//! Worktree discovery and git enrichment.
//!
//! The managed list comes from `git worktree list --porcelain`, not from a
//! directory scan: that is what makes "pando manages every worktree git
//! reports" true, and it means adopted worktrees can live anywhere on disk.
//! The first porcelain entry is the main checkout and is never part of the
//! managed list.

use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::SystemTime;

use crate::project::ProjectRef;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Directory basename, as-is. Worktrees pando creates are named by
    /// sanitizing the branch; adopted ones keep whatever they have.
    pub name: String,
    /// Canonical path, except for a prunable entry whose directory is gone.
    pub path: PathBuf,
    /// Full sha of the checked-out commit, from porcelain.
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub prunable: bool,
    pub prunable_reason: Option<String>,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub bare: bool,
    /// Directory birthtime; drives the newest-first list order. `None` when
    /// the filesystem cannot say, or the directory is gone.
    pub created_at: Option<SystemTime>,
    /// Short sha, filled by enrichment.
    pub head_sha: Option<String>,
    pub head_subject: Option<String>,
    pub head_age: Option<String>,
    pub dirty: Option<bool>,
    pub ahead_behind: Option<(u32, u32)>,
}

/// Whether `name` is the throwaway worktree `pando check` makes and
/// removes, [`crate::paths::CHECK_WORKTREE`].
///
/// Hidden where worktrees are listed for a person — `ls`, `status`, the
/// TUI's rows, completion and the names a command takes — and nowhere
/// else: discovery still finds it, because the check's own start, `new`
/// and `rm` need what git lists, and `doctor` shows it.
pub fn is_check(name: &str) -> bool {
    name == crate::paths::CHECK_WORKTREE
}

/// What `pando check`'s worktree is called wherever a person reads its
/// name: its directory name is nobody's word for it.
pub const CHECK_LABEL: &str = "the running pando check";

/// `name` as a message says it: the check's worktree by what it is, any
/// other by its own name.
pub fn label(name: &str) -> &str {
    match is_check(name) {
        true => CHECK_LABEL,
        false => name,
    }
}

/// Directory name for a branch: `feat/checkout` becomes `feat+checkout`.
/// Slashes are the only thing that cannot appear in a directory name, and a
/// plus reads as a join rather than an escape.
pub fn sanitize_branch_to_dir(branch: &str) -> String {
    branch.replace('/', "+")
}

impl Worktree {
    /// Whether the directory is its branch spelled as a directory: then
    /// the branch *is* the name.
    pub fn named_for_branch(&self) -> bool {
        self.branch
            .as_deref()
            .is_some_and(|branch| sanitize_branch_to_dir(branch) == self.name)
    }

    /// The name a person knows this worktree by: its branch when the
    /// directory was named for it — `feat/one`, which every command
    /// accepts, rather than the `feat+one` it became on disk — and the
    /// directory's own name otherwise.
    ///
    /// For text a person reads. Anything a program parses — the stdout
    /// lines of `new`, `start`, `stop` and `rm`, and every JSON shape —
    /// carries the directory name, which is the published contract.
    pub fn display_name(&self) -> String {
        match (self.named_for_branch(), &self.branch) {
            (true, Some(branch)) => branch.clone(),
            _ => self.name.clone(),
        }
    }

    fn from_entry(entry: PorcelainEntry) -> Self {
        let name = entry
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| entry.path.to_string_lossy().to_string());
        let path = std::fs::canonicalize(&entry.path).unwrap_or(entry.path);
        let created_at = std::fs::metadata(&path).ok().and_then(|m| m.created().ok());
        Self {
            name,
            path,
            head: entry.head,
            branch: entry.branch,
            detached: entry.detached,
            prunable: entry.prunable,
            prunable_reason: entry.prunable_reason,
            locked: entry.locked,
            lock_reason: entry.lock_reason,
            bare: entry.bare,
            created_at,
            head_sha: None,
            head_subject: None,
            head_age: None,
            dirty: None,
            ahead_behind: None,
        }
    }

    /// The porcelain entry this worktree was read from.
    fn entry(&self) -> PorcelainEntry {
        PorcelainEntry {
            path: self.path.clone(),
            head: self.head.clone(),
            branch: self.branch.clone(),
            detached: self.detached,
            prunable: self.prunable,
            prunable_reason: self.prunable_reason.clone(),
            locked: self.locked,
            lock_reason: self.lock_reason.clone(),
            bare: self.bare,
        }
    }
}

/// The main checkout plus every worktree pando manages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub main: Worktree,
    pub worktrees: Vec<Worktree>,
}

/// Every worktree git reports except the main checkout.
pub fn discover(project: &ProjectRef) -> Result<Vec<Worktree>> {
    Ok(discover_all(project)?.worktrees)
}

pub fn discover_all(project: &ProjectRef) -> Result<Discovery> {
    let text = porcelain_text(&project.root)?;
    let mut entries = parse_porcelain(&text);
    if entries.is_empty() {
        anyhow::bail!("git listed no worktrees for {}", project.root.display());
    }
    let main = Worktree::from_entry(entries.remove(0));
    let mut worktrees: Vec<Worktree> = entries.into_iter().map(Worktree::from_entry).collect();

    // Two worktrees sharing a basename would silently overwrite each other in
    // every name-keyed map pando has, so it is a hard error that names both.
    let mut seen: HashMap<String, PathBuf> = HashMap::new();
    for wt in &worktrees {
        if let Some(first) = seen.insert(wt.name.clone(), wt.path.clone()) {
            anyhow::bail!(
                "two worktrees are both named {:?}: {} and {} — rename one, pando keys worktrees by directory name",
                wt.name,
                first.display(),
                wt.path.display()
            );
        }
    }

    // Newest first: the worktree just created is the one being reached for.
    // Name breaks ties and orders entries with no birthtime, which the
    // Option ordering already puts last.
    worktrees.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(Discovery { main, worktrees })
}

fn porcelain_text(root: &Path) -> Result<String> {
    let out = crate::project::git(root, ["worktree", "list", "--porcelain"])
        .context("run git worktree list")?;
    if !out.status.success() {
        anyhow::bail!(
            "git worktree list failed in {}: {}",
            root.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Every path `git worktree list` reports, the main checkout included,
/// resolved the way paths are compared elsewhere. A state record whose path
/// is not in here belongs to a worktree git has forgotten; a prunable entry,
/// whose directory is gone, is still listed and so still counts.
pub fn porcelain_paths(root: &Path) -> Result<Vec<PathBuf>> {
    Ok(parse_porcelain(&porcelain_text(root)?)
        .iter()
        .map(|e| crate::paths::resolve_for_compare(&e.path))
        .collect())
}

/// The git directory a linked checkout's `.git` file names —
/// `<repository>/.git/worktrees/<name>` — as an absolute path. `None` when
/// its `.git` is not such a file.
///
/// Git writes it absolute, or relative to the checkout when
/// `worktree.useRelativePaths` is set (git 2.48 and later), and resolves a
/// relative one from the checkout's own directory, so that is where it is
/// resolved here. The `..` in it are folded by hand, because what a caller
/// wants to know is often that the directory is no longer there.
pub fn linked_gitdir(checkout: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(checkout.join(".git")).ok()?;
    let named = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    if named.is_empty() {
        return None;
    }
    let named = Path::new(named);
    if named.is_absolute() {
        return Some(named.to_path_buf());
    }
    let mut out = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    for part in named.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            part => out.push(part),
        }
    }
    Some(out)
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PorcelainEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub prunable: bool,
    pub prunable_reason: Option<String>,
    pub locked: bool,
    pub lock_reason: Option<String>,
    pub bare: bool,
}

/// Entries in the order git printed them, so the caller can rely on the
/// first being the main checkout. Unknown lines are ignored: git adds new
/// ones and an unrecognised attribute is not a reason to fail.
pub fn parse_porcelain(text: &str) -> Vec<PorcelainEntry> {
    let mut out = Vec::new();
    let mut current: Option<PorcelainEntry> = None;
    for line in text.lines() {
        if line.is_empty() {
            if let Some(entry) = current.take() {
                out.push(entry);
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                out.push(entry);
            }
            current = Some(PorcelainEntry {
                path: PathBuf::from(rest),
                ..Default::default()
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("HEAD ") {
            entry.head = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("branch refs/heads/") {
            entry.branch = Some(rest.to_string());
        } else if line == "detached" {
            entry.detached = true;
        } else if line == "bare" {
            entry.bare = true;
        } else if let Some(rest) = strip_attribute(line, "locked") {
            entry.locked = true;
            entry.lock_reason = rest;
        } else if let Some(rest) = strip_attribute(line, "prunable") {
            entry.prunable = true;
            entry.prunable_reason = rest;
        }
    }
    if let Some(entry) = current.take() {
        out.push(entry);
    }
    out
}

/// `locked` and `prunable` appear bare or with a free-text reason after one
/// space. Returns `Some(reason)` for a match, `None` for a non-match.
#[allow(clippy::manual_map)]
fn strip_attribute(line: &str, name: &str) -> Option<Option<String>> {
    if line == name {
        Some(None)
    } else if let Some(rest) = line.strip_prefix(&format!("{name} ")) {
        let reason = rest.trim();
        Some(if reason.is_empty() {
            None
        } else {
            Some(unquote_c_style(reason))
        })
    } else {
        None
    }
}

/// Decodes the C-quoting git uses for a reason containing non-ASCII, a
/// quote, or a control character. A value that does not start with a quote
/// is already literal and is returned untouched.
///
/// The escapes are bytes, not characters — `\303\251` is one `é` — so this
/// decodes into a byte buffer and reads UTF-8 out of it at the end.
fn unquote_c_style(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    else {
        return value.to_string();
    };
    let mut out: Vec<u8> = Vec::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    let mut buf = [0u8; 4];
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('r') => out.push(b'\r'),
            Some('"') => out.push(b'"'),
            Some('\\') => out.push(b'\\'),
            // `\NNN`: up to three octal digits, one byte.
            Some(digit) if digit.is_digit(8) => {
                let mut byte = digit.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    let Some(next) = chars.peek().and_then(|c| c.to_digit(8)) else {
                        break;
                    };
                    byte = byte * 8 + next;
                    chars.next();
                }
                out.push(byte.min(u8::MAX as u32) as u8);
            }
            // Not an escape git produces: keep both characters rather than
            // silently eating one.
            Some(other) => {
                out.push(b'\\');
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichUpdate {
    pub name: String,
    pub branch: Option<String>,
    pub prunable: bool,
    pub head_sha: Option<String>,
    pub head_subject: Option<String>,
    pub head_age: Option<String>,
    pub dirty: Option<bool>,
    pub ahead_behind: Option<(u32, u32)>,
}

/// Enriches worktrees from the listing they were read from: their own
/// heads and branches feed the batched reads, so no second
/// `git worktree list` runs.
pub fn enrich_from_git(worktrees: &mut [Worktree], root: &Path) -> Result<()> {
    if worktrees.is_empty() {
        return Ok(());
    }
    let items: Vec<(String, PathBuf)> = worktrees
        .iter()
        .map(|w| (w.name.clone(), w.path.clone()))
        .collect();
    let listed: HashMap<PathBuf, PorcelainEntry> = worktrees
        .iter()
        .map(|w| (listed_key(&w.path), w.entry()))
        .collect();
    let (tx, rx) = mpsc::channel();
    enrich_listed(root, items, listed, resolve_base_branch(root), tx, 16);

    let mut updates: HashMap<String, EnrichUpdate> = HashMap::new();
    while let Ok(u) = rx.recv() {
        updates.insert(u.name.clone(), u);
    }
    for wt in worktrees.iter_mut() {
        if let Some(u) = updates.remove(&wt.name) {
            apply_update(wt, u);
        }
    }
    Ok(())
}

/// `pool_cap` bounds worker parallelism: every job forks `git status` against
/// a full working tree, so a wide pool saturates the disk for the whole run.
/// Callers that block on the result want it wide; the TUI keeps it narrow so
/// startup enrichment does not starve the UI.
///
/// `known_base` is a base the caller resolved already, which saves the
/// one to five git calls of resolving it again; `None` resolves it here.
pub fn enrich_stream(
    root: &Path,
    items: Vec<(String, PathBuf)>,
    known_base: Option<String>,
    sender: mpsc::Sender<EnrichUpdate>,
    pool_cap: usize,
) {
    if items.is_empty() {
        return;
    }
    let porcelain = porcelain_by_path(root);
    let base = known_base.or_else(|| resolve_base_branch(root));
    enrich_listed(root, items, porcelain, base, sender, pool_cap);
}

/// [`enrich_stream`] with the listing and the base already in hand.
fn enrich_listed(
    root: &Path,
    items: Vec<(String, PathBuf)>,
    porcelain: HashMap<PathBuf, PorcelainEntry>,
    base: Option<String>,
    sender: mpsc::Sender<EnrichUpdate>,
    pool_cap: usize,
) {
    let count = items.len();

    // Two batched calls replace two forks per worktree: commit meta for
    // each enriched worktree's HEAD, and ahead/behind for each one's
    // branch — only theirs, so re-reading one row walks one branch, not
    // every local one. Misses fall back to a per-worktree fork.
    let listed: Vec<&PorcelainEntry> = items
        .iter()
        .filter_map(|(_, path)| porcelain.get(&listed_key(path)))
        .collect();
    let shas: Vec<&str> = listed.iter().filter_map(|e| e.head.as_deref()).collect();
    let branches: Vec<&str> = listed.iter().filter_map(|e| e.branch.as_deref()).collect();
    let commit_meta = batch_commit_meta(root, &shas);
    let branch_counts = batch_ahead_behind(root, base.as_deref(), &branches);

    let pool_size = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8)
        .min(pool_cap)
        .min(count)
        .max(1);

    let queue: Mutex<VecDeque<(String, PathBuf)>> = Mutex::new(items.into_iter().collect());
    let porcelain_ref = &porcelain;
    let base_ref = base.as_deref();
    let commit_meta_ref = &commit_meta;
    let branch_counts_ref = &branch_counts;
    let queue_ref = &queue;

    std::thread::scope(|s| {
        for _ in 0..pool_size {
            let tx = sender.clone();
            s.spawn(move || {
                loop {
                    let item = {
                        let mut g = queue_ref.lock().unwrap();
                        g.pop_front()
                    };
                    let Some((name, path)) = item else { break };
                    let update = enrich_one(
                        name,
                        &path,
                        porcelain_ref,
                        base_ref,
                        commit_meta_ref,
                        branch_counts_ref,
                    );
                    if tx.send(update).is_err() {
                        break;
                    }
                }
            });
        }
    });
}

fn porcelain_by_path(root: &Path) -> HashMap<PathBuf, PorcelainEntry> {
    let Ok(text) = porcelain_text(root) else {
        return HashMap::new();
    };
    parse_porcelain(&text)
        .into_iter()
        .map(|e| (listed_key(&e.path), e))
        .collect()
}

/// How enrichment keys a listed path: canonical where the directory is
/// there, as git printed it where it is gone.
fn listed_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Short sha, subject, and relative age for every given commit from one
/// `git log --no-walk`, keyed by full sha. An empty map on failure just
/// routes callers to the per-worktree fallback.
///
/// A sha that names no commit — the null sha git lists for a worktree on
/// an unborn branch, or a HEAD whose object is gone — is left out of the
/// answer rather than failing it for every other worktree.
fn batch_commit_meta(root: &Path, shas: &[&str]) -> HashMap<String, (String, String, String)> {
    if shas.is_empty() {
        return HashMap::new();
    }
    let args = [
        "log",
        "--no-walk=unsorted",
        "--ignore-missing",
        "--format=%H%x1f%h%x1f%s%x1f%cr",
    ]
    .into_iter()
    .chain(shas.iter().copied());
    let out = crate::project::git(root, args);
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = HashMap::new();
    for line in text.lines() {
        let parts: Vec<&str> = line.splitn(4, '\x1f').collect();
        if let [full, short, subject, age] = parts[..] {
            map.insert(
                full.to_string(),
                (short.to_string(), subject.to_string(), age.to_string()),
            );
        }
    }
    map
}

/// Ahead/behind against the base for each of `branches`, from one
/// `git for-each-ref`. Requires git >= 2.41 for `%(ahead-behind:...)`; on
/// failure the empty map routes callers to the per-worktree fallback.
///
/// A branch name is matched as a literal ref: git refuses the glob
/// characters in one, and `a` and `a/b` cannot both be branches.
fn batch_ahead_behind(
    root: &Path,
    base: Option<&str>,
    branches: &[&str],
) -> HashMap<String, (u32, u32)> {
    let Some(base) = base else {
        return HashMap::new();
    };
    if branches.is_empty() {
        return HashMap::new();
    }
    let format = format!("--format=%(refname:short)\x1f%(ahead-behind:{base})");
    let refs = branches.iter().map(|b| format!("refs/heads/{b}"));
    let args = ["for-each-ref".to_string(), format].into_iter().chain(refs);
    let out = crate::project::git(root, args);
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return HashMap::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split('\x1f');
        let Some(branch) = fields.next() else {
            continue;
        };
        let Some(counts) = fields.next() else {
            continue;
        };
        let mut nums = counts.split_whitespace();
        let parsed = (|| {
            let ahead: u32 = nums.next()?.parse().ok()?;
            let behind: u32 = nums.next()?.parse().ok()?;
            Some((ahead, behind))
        })();
        if let Some(pair) = parsed {
            map.insert(branch.to_string(), pair);
        }
    }
    map
}

fn enrich_one(
    name: String,
    path: &Path,
    porcelain: &HashMap<PathBuf, PorcelainEntry>,
    base: Option<&str>,
    commit_meta: &HashMap<String, (String, String, String)>,
    branch_counts: &HashMap<String, (u32, u32)>,
) -> EnrichUpdate {
    let entry = porcelain.get(&listed_key(path));
    let (branch, prunable) = match entry {
        Some(e) => (e.branch.clone(), e.prunable),
        None => (None, false),
    };
    let batched_meta = entry
        .and_then(|e| e.head.as_ref())
        .and_then(|sha| commit_meta.get(sha));
    let (head_sha, head_subject, head_age) = match batched_meta {
        Some((short, subject, age)) => (
            Some(short.clone()),
            Some(subject.clone()),
            Some(age.clone()),
        ),
        None => match last_commit(path) {
            Ok((sha, subject, age)) => (Some(sha), Some(subject), Some(age)),
            Err(_) => (None, None, None),
        },
    };
    let dirty = is_dirty(path);
    let ahead_behind = match branch.as_deref().and_then(|b| branch_counts.get(b)) {
        Some(counts) => Some(*counts),
        // Detached, or the batch call failed — one fork for this worktree.
        None => base.and_then(|b| ahead_behind(path, b)),
    };

    EnrichUpdate {
        name,
        branch,
        prunable,
        head_sha,
        head_subject,
        head_age,
        dirty,
        ahead_behind,
    }
}

pub fn apply_update(wt: &mut Worktree, u: EnrichUpdate) {
    wt.branch = u.branch;
    wt.prunable = u.prunable;
    wt.head_sha = u.head_sha;
    wt.head_subject = u.head_subject;
    wt.head_age = u.head_age;
    wt.dirty = u.dirty;
    wt.ahead_behind = u.ahead_behind;
}

fn last_commit(path: &Path) -> Result<(String, String, String)> {
    let out = crate::project::git(path, ["log", "-1", "--format=%h%x1f%s%x1f%cr", "HEAD"])?;
    if !out.status.success() {
        anyhow::bail!("git log failed at {}", path.display());
    }
    let text = String::from_utf8(out.stdout)?;
    let parts: Vec<&str> = text.trim_end_matches('\n').splitn(3, '\x1f').collect();
    if parts.len() != 3 {
        anyhow::bail!("unexpected git log output: {text:?}");
    }
    Ok((
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    ))
}

/// The branch new worktrees fork from and existing ones are measured
/// against: what `origin/HEAD` points at, else `main`, else `master`.
/// Generic on purpose — no project-specific branch convention lives here.
pub fn resolve_base_branch(root: &Path) -> Option<String> {
    if let Some(head) = origin_head(root) {
        return Some(head);
    }
    for candidate in ["main", "master"] {
        if let Some(found) = first_existing_ref(root, &[&format!("origin/{candidate}"), candidate])
        {
            return Some(found);
        }
    }
    None
}

/// How many commits the main checkout's branch must have that origin/HEAD
/// lacks before [`base_drift`] says origin/HEAD is not where work starts.
///
/// A feature branch rarely carries a hundred commits of its own; a branch
/// a team merges into, beside a default branch nobody moves, soon does.
pub const FAR_AHEAD: u32 = 100;

/// How many days older origin/HEAD's last commit must be than the main
/// checkout's. Both have to hold: a long feature branch on a repository
/// whose default branch is alive, or a quiet repository whose branches all
/// sit still, never raises it.
pub const STALE_DAYS: i64 = 30;

/// origin/HEAD, far behind the branch the main checkout is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseDrift {
    /// origin/HEAD as git spells it short: `origin/develop`.
    pub default: String,
    /// The branch it points at, as a base is written: `develop`.
    pub default_branch: String,
    /// The main checkout's branch.
    pub current: String,
    /// Commits on `current` that `default` does not have.
    pub ahead: u32,
    /// How many days before `current`'s last commit `default`'s was made.
    pub days_older: i64,
}

/// origin/HEAD and the main checkout's branch, when origin/HEAD is so far
/// behind — [`FAR_AHEAD`] commits and [`STALE_DAYS`] days — that it is
/// unlikely to be the branch work starts from, though `new` and `check`
/// fork from it. `None` when it is not, when there is no origin/HEAD, and
/// when the main checkout is on that branch or on none.
///
/// Read from the repository alone, with no clock: the same refs give the
/// same answer on any day, which is what lets `signals` publish it.
pub fn base_drift(root: &Path) -> Option<BaseDrift> {
    let default = origin_head(root)?;
    let default_branch = default.strip_prefix("origin/")?.to_string();
    let current = checked_out_branch(root)?;
    if current == default_branch {
        return None;
    }
    let count = crate::project::git(
        root,
        ["rev-list", "--count", &format!("{default}..HEAD"), "--"],
    )
    .ok()
    .filter(|out| out.status.success())?;
    let ahead: u32 = String::from_utf8_lossy(&count.stdout).trim().parse().ok()?;
    let times = crate::project::git(root, ["show", "-s", "--format=%ct", &default, "HEAD", "--"])
        .ok()
        .filter(|out| out.status.success())?;
    let times: Vec<i64> = String::from_utf8_lossy(&times.stdout)
        .lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect();
    let [default_at, head_at] = times[..] else {
        return None;
    };
    let days_older = (head_at - default_at) / 86_400;
    (ahead >= FAR_AHEAD && days_older >= STALE_DAYS).then_some(BaseDrift {
        default,
        default_branch,
        current,
        ahead,
        days_older,
    })
}

/// The branch a checkout is on; `None` when its HEAD is detached.
pub fn checked_out_branch(root: &Path) -> Option<String> {
    let out = crate::project::git(root, ["symbolic-ref", "--short", "--quiet", "HEAD"]).ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty()).then_some(name)
}

fn origin_head(root: &Path) -> Option<String> {
    let out = crate::project::git(
        root,
        [
            "symbolic-ref",
            "--short",
            "--quiet",
            "refs/remotes/origin/HEAD",
        ],
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn first_existing_ref(root: &Path, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .find(|c| ref_resolves(root, c))
        .map(|c| c.to_string())
}

fn ref_resolves(root: &Path, refname: &str) -> bool {
    crate::project::git(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{refname}^{{commit}}"),
        ],
    )
    .map(|o| o.status.success() && !o.stdout.is_empty())
    .unwrap_or(false)
}

/// `--no-optional-locks` so a status probe never writes an index lock into
/// the worktree — invariant 1 covers `.git` too.
fn is_dirty(wt_path: &Path) -> Option<bool> {
    let out = crate::project::git(
        wt_path,
        ["--no-optional-locks", "status", "--porcelain", "-z"],
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!out.stdout.is_empty())
}

fn ahead_behind(wt_path: &Path, base: &str) -> Option<(u32, u32)> {
    let out = crate::project::git(
        wt_path,
        [
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{base}"),
        ],
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut parts = text.split_whitespace();
    let ahead: u32 = parts.next()?.parse().ok()?;
    let behind: u32 = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchSource {
    Local,
    Remote,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchEntry {
    pub name: String,
    pub source: BranchSource,
}

/// Local branches, then remote-only branches on `origin`, each alphabetical.
/// Empty on any git failure — the create modal degrades to typing a name.
pub fn list_branches(root: &Path) -> Vec<BranchEntry> {
    let out = crate::project::git(
        root,
        [
            "for-each-ref",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes/origin",
        ],
    );
    let out = match out {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };

    let text = String::from_utf8_lossy(&out.stdout);
    let mut local: Vec<String> = Vec::new();
    let mut remote: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("refs/heads/") {
            if !rest.is_empty() {
                local.push(rest.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("refs/remotes/origin/") {
            if rest.is_empty() || rest == "HEAD" {
                continue;
            }
            remote.push(rest.to_string());
        }
    }
    local.sort();
    remote.sort();

    let local_set: HashSet<&String> = local.iter().collect();
    let mut entries: Vec<BranchEntry> = Vec::with_capacity(local.len() + remote.len());
    for name in &local {
        entries.push(BranchEntry {
            name: name.clone(),
            source: BranchSource::Local,
        });
    }
    for name in &remote {
        if local_set.contains(name) {
            continue;
        }
        entries.push(BranchEntry {
            name: name.clone(),
            source: BranchSource::Remote,
        });
    }
    entries
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrInfo {
    pub number: u32,
    pub title: String,
    pub branch: String,
    pub author: String,
    pub draft: bool,
    pub state: PrState,
    pub url: String,
    /// Opened from a fork: its branch lives in another repository, so
    /// `origin` has it only as `refs/pull/<number>/head`. Absent from a
    /// cache written before pando asked, which reads as not a fork.
    #[serde(default)]
    pub cross_repository: bool,
}

impl PrInfo {
    /// The local branch a worktree for this pull request checks out: its
    /// own branch when that lives on `origin`, and `pr-<number>/<branch>`
    /// for a fork's, whose name — often `main` or `patch-1` — says nothing
    /// and may already be taken here.
    pub fn local_branch(&self) -> String {
        if self.cross_repository {
            format!("pr-{}/{}", self.number, self.branch)
        } else {
            self.branch.clone()
        }
    }
}

/// Pull requests via the `gh` CLI, which resolves the repository from the
/// checkout's origin remote. Errors carry enough context to explain a
/// missing or unauthenticated `gh`; callers treat that as "no chips".
///
/// Every open one, and the most recent of the rest: one list of every
/// state would cut off an open pull request older than the newest few
/// hundred merged ones, and the picker lists only the open.
pub fn list_prs(root: &Path) -> Result<Vec<PrInfo>> {
    let open = gh_pr_list(root, "open", "1000")?;
    let recent = gh_pr_list(root, "all", "300")?;
    Ok(merge_pr_lists(open, recent))
}

/// A list is pages of API calls, a round trip each, so it is given longer
/// than [`GH_TIMEOUT`]. Bounded all the same: the TUI asks for the next
/// list only once this one has answered, so a `gh` that never did left
/// the picker asking GitHub until the TUI was restarted.
const GH_LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn gh_pr_list(root: &Path, state: &str, limit: &str) -> Result<Vec<PrInfo>> {
    gh_pr_list_with(Path::new("gh"), root, state, limit, GH_LIST_TIMEOUT)
}

/// [`gh_pr_list`] with the program and the deadline named, for a test's
/// stand-in. Never prompts, as [`gh_account_with`] does not.
fn gh_pr_list_with(
    program: &Path,
    root: &Path,
    state: &str,
    limit: &str,
    timeout: std::time::Duration,
) -> Result<Vec<PrInfo>> {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .args([
            "pr",
            "list",
            "--state",
            state,
            "--limit",
            limit,
            "--json",
            "number,title,headRefName,author,isDraft,state,url,isCrossRepository",
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    let out = match crate::project::output_within(command, timeout) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            anyhow::bail!("gh pr list did not answer in {}s", timeout.as_secs())
        }
        Err(e) => return Err(e).context("spawn gh — is the GitHub CLI installed?"),
    };
    if !out.status.success() {
        anyhow::bail!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_pr_list(&String::from_utf8_lossy(&out.stdout))
}

/// Both lists as one, each pull request once, newest first — the order
/// `gh` gives each of them.
fn merge_pr_lists(open: Vec<PrInfo>, recent: Vec<PrInfo>) -> Vec<PrInfo> {
    let mut merged = open;
    for pr in recent {
        if !merged.iter().any(|p| p.number == pr.number) {
            merged.push(pr);
        }
    }
    merged.sort_by_key(|p| std::cmp::Reverse(p.number));
    merged
}

/// Pull requests by the local branch a worktree for each checks out, so
/// a fork's `main` is not the main checkout's. Where two share a branch
/// — one merged, a later one opened from the same name — the newest wins.
pub fn prs_by_branch(prs: &[PrInfo]) -> std::collections::BTreeMap<String, PrInfo> {
    let mut by_branch = std::collections::BTreeMap::new();
    for pr in prs {
        let newer = by_branch
            .get(&pr.local_branch())
            .is_none_or(|kept: &PrInfo| kept.number < pr.number);
        if newer {
            by_branch.insert(pr.local_branch(), pr.clone());
        }
    }
    by_branch
}

/// Which GitHub account `gh` acts as for a checkout.
///
/// Asked from the checkout itself, because that is where it can differ: a
/// `gh` that picks its account by directory — a wrapper, `GH_TOKEN` set
/// per project, a `GH_CONFIG_DIR` from direnv — answers for this project
/// and not for the shell pando was started from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GhAccount {
    /// Signed in, as this login.
    Login(String),
    /// `gh` is installed and has no account to act as here.
    SignedOut,
    /// No `gh` on PATH.
    Missing,
    /// `gh` could not say: offline, no answer in time, an API error.
    Unknown(String),
}

/// A network round trip to the API; a slow link is not a hung `gh`.
const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// What `gh` prints when it has no account to use.
const GH_SIGNED_OUT: [&str; 3] = ["auth login", "not logged in", "authentication required"];

/// The account `gh` acts as from `root`. One API call, bounded; never
/// prompts.
pub fn gh_account(root: &Path) -> GhAccount {
    gh_account_with(Path::new("gh"), root)
}

/// [`gh_account`] with the program named, for a test's stand-in.
pub fn gh_account_with(program: &Path, root: &Path) -> GhAccount {
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .args(["api", "user", "--jq", ".login"])
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1");
    let out = match crate::project::output_within(command, GH_TIMEOUT) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return GhAccount::Missing,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
            return GhAccount::Unknown(format!("gh did not answer in {}s", GH_TIMEOUT.as_secs()));
        }
        Err(e) => return GhAccount::Unknown(format!("gh: {e}")),
    };
    if out.status.success() {
        let login = String::from_utf8_lossy(&out.stdout).trim().to_string();
        return if login.is_empty() {
            GhAccount::Unknown("gh named no account".to_string())
        } else {
            GhAccount::Login(login)
        };
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lower = stderr.to_lowercase();
    if GH_SIGNED_OUT.iter().any(|needle| lower.contains(needle)) {
        return GhAccount::SignedOut;
    }
    let reason = stderr
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("gh failed")
        .trim()
        .to_string();
    GhAccount::Unknown(reason)
}

fn parse_pr_list(json: &str) -> Result<Vec<PrInfo>> {
    #[derive(serde::Deserialize)]
    struct RawAuthor {
        login: String,
    }
    #[derive(serde::Deserialize)]
    struct RawPr {
        number: u32,
        title: String,
        #[serde(rename = "headRefName")]
        head_ref_name: String,
        author: RawAuthor,
        #[serde(rename = "isDraft")]
        is_draft: bool,
        state: String,
        url: String,
        #[serde(rename = "isCrossRepository", default)]
        is_cross_repository: bool,
    }
    let raw: Vec<RawPr> = serde_json::from_str(json).context("parse gh pr list JSON")?;
    Ok(raw
        .into_iter()
        .map(|p| PrInfo {
            number: p.number,
            title: p.title,
            branch: p.head_ref_name,
            author: p.author.login,
            draft: p.is_draft,
            state: match p.state.as_str() {
                "OPEN" => PrState::Open,
                "MERGED" => PrState::Merged,
                _ => PrState::Closed,
            },
            url: p.url,
            cross_repository: p.is_cross_repository,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{git, init_repo};
    use tempfile::{TempDir, tempdir};

    // The check's worktree is named for what it is wherever a person reads
    // a name; every other worktree keeps its own.
    #[test]
    fn a_label_names_the_check_for_what_it_is_and_nothing_else() {
        assert_eq!(label(crate::paths::CHECK_WORKTREE), CHECK_LABEL);
        assert_eq!(label("feat+login"), "feat+login");
        assert_eq!(label("x+.pando-check"), "x+.pando-check");
    }

    fn project_at(path: &Path) -> ProjectRef {
        ProjectRef::from_root(path).unwrap()
    }

    /// A repo with one commit and a place to put linked worktrees.
    fn repo_with_worktrees(names: &[(&str, &str)]) -> (TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let base = dir.path().join("trees");
        std::fs::create_dir_all(&base).unwrap();
        for (dir_name, branch) in names {
            let path = base.join(dir_name);
            git(
                &repo,
                &["worktree", "add", "-b", branch, path.to_str().unwrap()],
            );
        }
        (dir, repo)
    }

    #[test]
    fn parses_every_entry_shape_git_prints() {
        let text = "\
worktree /repo
HEAD abc123
branch refs/heads/main

worktree /trees/feat+detached
HEAD def456
detached

worktree /trees/feat+gone
HEAD abc123
branch refs/heads/feat/gone
prunable gitdir file points to non-existent location

worktree /trees/feat+locked
HEAD abc123
branch refs/heads/feat/locked
locked busy testing

worktree /trees/feat+plainlock
HEAD abc123
locked

worktree /bare.git
bare

";
        let entries = parse_porcelain(text);
        assert_eq!(entries.len(), 6);

        assert_eq!(entries[0].path, PathBuf::from("/repo"));
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(entries[0].head.as_deref(), Some("abc123"));

        assert!(entries[1].detached);
        assert!(entries[1].branch.is_none());

        assert!(entries[2].prunable);
        assert_eq!(
            entries[2].prunable_reason.as_deref(),
            Some("gitdir file points to non-existent location")
        );

        assert!(entries[3].locked);
        assert_eq!(entries[3].lock_reason.as_deref(), Some("busy testing"));

        assert!(entries[4].locked, "a bare `locked` line still means locked");
        assert_eq!(entries[4].lock_reason, None);

        assert!(entries[5].bare);
    }

    #[test]
    fn parsing_ignores_unknown_lines_and_survives_a_missing_trailing_blank() {
        let entries = parse_porcelain(
            "worktree /repo\nsomething-git-added-later value\nHEAD abc\nbranch refs/heads/main",
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn discover_excludes_the_main_checkout() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+one", "feat/one")]);
        let project = project_at(&repo);
        let discovery = discover_all(&project).unwrap();

        assert_eq!(discovery.main.path, project.root);
        assert_eq!(discovery.main.branch.as_deref(), Some("main"));
        let names: Vec<&str> = discovery
            .worktrees
            .iter()
            .map(|w| w.name.as_str())
            .collect();
        assert_eq!(names, vec!["feat+one"]);
    }

    #[test]
    fn discover_is_empty_when_the_repository_has_only_a_main_checkout() {
        let (_dir, repo) = repo_with_worktrees(&[]);
        assert!(discover(&project_at(&repo)).unwrap().is_empty());
    }

    #[test]
    fn discovered_names_are_basenames_and_paths_are_canonical() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+checkout", "feat/checkout")]);
        let wts = discover(&project_at(&repo)).unwrap();
        assert_eq!(wts[0].name, "feat+checkout");
        assert_eq!(
            wts[0].path,
            std::fs::canonicalize(&wts[0].path).unwrap(),
            "paths must already be canonical — git prints /private/var on macOS"
        );
        assert!(wts[0].created_at.is_some());
    }

    #[test]
    fn discover_sorts_newest_first() {
        let (_dir, repo) = repo_with_worktrees(&[]);
        let base = repo.parent().unwrap().join("trees");
        std::fs::create_dir_all(&base).unwrap();
        // Creation order deliberately opposes alphabetical order.
        for n in ["mango", "apple", "zed"] {
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &format!("feat/{n}"),
                    base.join(n).to_str().unwrap(),
                ],
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let names: Vec<String> = discover(&project_at(&repo))
            .unwrap()
            .into_iter()
            .map(|w| w.name)
            .collect();
        assert_eq!(names, vec!["zed", "apple", "mango"]);
    }

    #[test]
    fn two_worktrees_with_the_same_basename_are_an_error_naming_both() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let a = dir.path().join("a").join("shared");
        let b = dir.path().join("b").join("shared");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        git(
            &repo,
            &["worktree", "add", "-b", "one", a.to_str().unwrap()],
        );
        git(
            &repo,
            &["worktree", "add", "-b", "two", b.to_str().unwrap()],
        );

        let err = discover(&project_at(&repo)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("shared"), "{msg}");
        assert!(
            msg.contains(&a.canonicalize().unwrap().display().to_string()),
            "{msg}"
        );
        assert!(
            msg.contains(&b.canonicalize().unwrap().display().to_string()),
            "{msg}"
        );
    }

    #[test]
    fn discover_reports_prunable_and_locked_entries_with_their_reasons() {
        let (_dir, repo) =
            repo_with_worktrees(&[("feat+gone", "feat/gone"), ("feat+lk", "feat/lk")]);
        let base = repo.parent().unwrap().join("trees");
        std::fs::remove_dir_all(base.join("feat+gone")).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "lock",
                "--reason",
                "holding this one",
                base.join("feat+lk").to_str().unwrap(),
            ],
        );

        let wts = discover(&project_at(&repo)).unwrap();
        let gone = wts.iter().find(|w| w.name == "feat+gone").unwrap();
        assert!(gone.prunable);
        assert!(gone.prunable_reason.is_some());
        let locked = wts.iter().find(|w| w.name == "feat+lk").unwrap();
        assert!(locked.locked);
        assert_eq!(locked.lock_reason.as_deref(), Some("holding this one"));
    }

    #[test]
    fn enrichment_fills_branch_sha_subject_and_age() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+x", "feat/x")]);
        let path = repo.parent().unwrap().join("trees").join("feat+x");
        git(&path, &["commit", "--allow-empty", "-m", "add feature x"]);

        let mut wts = discover(&project_at(&repo)).unwrap();
        enrich_from_git(&mut wts, &repo).unwrap();

        let w = &wts[0];
        assert_eq!(w.branch.as_deref(), Some("feat/x"));
        assert!(w.head_sha.as_deref().is_some_and(|s| !s.is_empty()));
        assert_eq!(w.head_subject.as_deref(), Some("add feature x"));
        assert!(w.head_age.is_some());
        assert_eq!(w.dirty, Some(false));
    }

    #[test]
    fn dirty_is_true_for_an_untracked_or_modified_file() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+u", "feat/u"), ("feat+m", "feat/m")]);
        let trees = repo.parent().unwrap().join("trees");
        std::fs::write(trees.join("feat+u").join("scratch.txt"), "hello").unwrap();

        let modified = trees.join("feat+m");
        std::fs::write(modified.join("a.txt"), "first").unwrap();
        git(&modified, &["add", "a.txt"]);
        git(&modified, &["commit", "-m", "add a"]);
        std::fs::write(modified.join("a.txt"), "changed").unwrap();

        let mut wts = discover(&project_at(&repo)).unwrap();
        enrich_from_git(&mut wts, &repo).unwrap();
        for w in &wts {
            assert_eq!(w.dirty, Some(true), "{} should be dirty", w.name);
        }
    }

    #[test]
    fn ahead_and_behind_are_counted_against_the_base() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+ab", "feat/ab")]);
        let path = repo.parent().unwrap().join("trees").join("feat+ab");
        git(&path, &["commit", "--allow-empty", "-m", "ahead 1"]);
        git(&path, &["commit", "--allow-empty", "-m", "ahead 2"]);
        git(&repo, &["commit", "--allow-empty", "-m", "main 1"]);

        let mut wts = discover(&project_at(&repo)).unwrap();
        enrich_from_git(&mut wts, &repo).unwrap();
        assert_eq!(wts[0].ahead_behind, Some((2, 1)));
    }

    // A detached-HEAD worktree has no branch for the batched lookups, so it
    // exercises the per-worktree fallbacks — and must enrich just as fully.
    #[test]
    fn a_detached_worktree_enriches_through_the_fallbacks() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+d", "feat/d")]);
        let path = repo.parent().unwrap().join("trees").join("feat+d");
        git(&path, &["commit", "--allow-empty", "-m", "on branch"]);
        git(&path, &["checkout", "--detach"]);

        let mut wts = discover(&project_at(&repo)).unwrap();
        assert!(wts[0].detached);
        enrich_from_git(&mut wts, &repo).unwrap();

        let w = &wts[0];
        assert!(w.branch.is_none());
        assert_eq!(w.head_subject.as_deref(), Some("on branch"));
        assert_eq!(w.ahead_behind, Some((1, 0)));
        assert_eq!(w.dirty, Some(false));
    }

    // A worktree on an unborn branch lists the null sha as its HEAD, and a
    // HEAD whose object is gone lists a sha git cannot read: neither may
    // send every other worktree to a `git log` of its own.
    #[test]
    fn the_batched_commit_read_answers_past_a_head_that_names_no_commit() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+x", "feat/x")]);
        let unborn = repo.parent().unwrap().join("trees").join("pages");
        git(
            &repo,
            &["worktree", "add", "--detach", unborn.to_str().unwrap()],
        );
        git(&unborn, &["checkout", "--quiet", "--orphan", "pages"]);

        let wts = discover(&project_at(&repo)).unwrap();
        let head_of = |name: &str| {
            wts.iter()
                .find(|w| w.name == name)
                .and_then(|w| w.head.clone())
                .unwrap()
        };
        let null = head_of("pages");
        assert!(null.bytes().all(|b| b == b'0'), "unborn HEAD is {null}");
        let feature = head_of("feat+x");
        let gone = "1".repeat(40);

        let meta = batch_commit_meta(&repo, &[&null, &gone, &feature]);
        assert!(meta.contains_key(&feature), "{meta:?}");
        assert_eq!(meta.len(), 1, "{meta:?}");
    }

    #[test]
    fn enrich_stream_emits_one_update_per_worktree() {
        let (_dir, repo) = repo_with_worktrees(&[
            ("alpha", "feat/alpha"),
            ("bravo", "feat/bravo"),
            ("charlie", "feat/charlie"),
            ("delta", "feat/delta"),
        ]);
        let wts = discover(&project_at(&repo)).unwrap();
        let items: Vec<(String, PathBuf)> = wts
            .iter()
            .map(|w| (w.name.clone(), w.path.clone()))
            .collect();

        let (tx, rx) = mpsc::channel();
        enrich_stream(&repo, items, None, tx, 16);
        let mut names: Vec<String> = rx.iter().map(|u| u.name).collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "bravo", "charlie", "delta"]);
    }

    fn stream_updates(
        repo: &Path,
        wts: &[Worktree],
        names: &[&str],
        known_base: Option<&str>,
    ) -> HashMap<String, EnrichUpdate> {
        let items: Vec<(String, PathBuf)> = wts
            .iter()
            .filter(|w| names.contains(&w.name.as_str()))
            .map(|w| (w.name.clone(), w.path.clone()))
            .collect();
        let (tx, rx) = mpsc::channel();
        enrich_stream(repo, items, known_base.map(str::to_string), tx, 4);
        rx.iter().map(|u| (u.name.clone(), u)).collect()
    }

    // The TUI re-reads the selected row on its own, with the base it
    // already holds: the batched reads then cover that row only, and must
    // give it exactly what the pass over every row does.
    #[test]
    fn enriching_one_worktree_gives_what_enriching_all_of_them_gives() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+a", "feat/a"), ("feat+b", "feat/b")]);
        let trees = repo.parent().unwrap().join("trees");
        git(
            &trees.join("feat+a"),
            &["commit", "--allow-empty", "-m", "a 1"],
        );
        git(
            &trees.join("feat+a"),
            &["commit", "--allow-empty", "-m", "a 2"],
        );
        git(
            &trees.join("feat+b"),
            &["commit", "--allow-empty", "-m", "b 1"],
        );
        git(&repo, &["commit", "--allow-empty", "-m", "main 1"]);
        let wts = discover(&project_at(&repo)).unwrap();

        let all = stream_updates(&repo, &wts, &["feat+a", "feat+b"], None);
        let one = stream_updates(&repo, &wts, &["feat+a"], Some("main"));
        assert_eq!(one.len(), 1);
        // head_age is git's relative time ("N seconds ago"), read at each
        // call: two reads can straddle a second, so it is compared apart.
        let without_age = |u: &EnrichUpdate| EnrichUpdate {
            head_age: None,
            ..u.clone()
        };
        assert_eq!(without_age(&one["feat+a"]), without_age(&all["feat+a"]));
        assert!(one["feat+a"].head_age.is_some());
        assert_eq!(one["feat+a"].ahead_behind, Some((2, 1)));
        assert_eq!(one["feat+a"].head_subject.as_deref(), Some("a 2"));
    }

    #[test]
    fn a_known_base_is_what_ahead_and_behind_are_counted_against() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+a", "feat/a"), ("feat+b", "feat/b")]);
        let trees = repo.parent().unwrap().join("trees");
        git(
            &trees.join("feat+a"),
            &["commit", "--allow-empty", "-m", "a 1"],
        );
        git(
            &trees.join("feat+b"),
            &["commit", "--allow-empty", "-m", "b 1"],
        );
        git(
            &trees.join("feat+b"),
            &["commit", "--allow-empty", "-m", "b 2"],
        );
        let wts = discover(&project_at(&repo)).unwrap();

        let resolved = stream_updates(&repo, &wts, &["feat+a"], None);
        assert_eq!(
            resolved["feat+a"].ahead_behind,
            Some((1, 0)),
            "against main"
        );
        let known = stream_updates(&repo, &wts, &["feat+a"], Some("feat/b"));
        assert_eq!(known["feat+a"].ahead_behind, Some((1, 2)), "against feat/b");
    }

    /// Whether this machine's git has `%(ahead-behind:…)` (2.41 and later).
    /// An older one sends [`batch_ahead_behind`] to its empty map on
    /// purpose, which is the fallback working, not the batch failing.
    fn git_counts_ahead_behind(repo: &Path) -> bool {
        Command::new("git")
            .current_dir(repo)
            .args([
                "for-each-ref",
                "--format=%(ahead-behind:main)",
                "refs/heads/main",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[test]
    fn the_batched_ahead_behind_walks_only_the_branches_asked_about() {
        let (_dir, repo) = repo_with_worktrees(&[("feat+a", "feat/a"), ("feat+b", "feat/b")]);
        git(&repo, &["branch", "feat/idle"]);
        git(&repo, &["branch", "feat/a-sibling"]);

        assert!(batch_ahead_behind(&repo, Some("main"), &[]).is_empty());
        if !git_counts_ahead_behind(&repo) {
            eprintln!("skipping: this git has no %(ahead-behind:…), which needs 2.41");
            return;
        }
        let counts = batch_ahead_behind(&repo, Some("main"), &["feat/a"]);
        assert_eq!(counts.keys().collect::<Vec<_>>(), vec!["feat/a"]);
    }

    #[test]
    fn enrichment_degrades_to_none_when_git_cannot_answer() {
        let dir = tempdir().unwrap();
        let ghost = dir.path().join("ghost");
        std::fs::create_dir_all(&ghost).unwrap();
        let mut wts = vec![Worktree {
            name: "ghost".into(),
            path: ghost,
            head: None,
            branch: None,
            detached: false,
            prunable: false,
            prunable_reason: None,
            locked: false,
            lock_reason: None,
            bare: false,
            created_at: None,
            head_sha: None,
            head_subject: None,
            head_age: None,
            dirty: None,
            ahead_behind: None,
        }];
        enrich_from_git(&mut wts, dir.path()).unwrap();
        assert!(wts[0].dirty.is_none());
        assert!(wts[0].ahead_behind.is_none());
    }

    /// A bare remote plus a clone, so remote-tracking refs exist.
    fn repo_with_origin(dir: &Path, extra_remote_branches: &[&str]) -> PathBuf {
        let bare = dir.join("origin.git");
        git(
            dir,
            &[
                "init",
                "--bare",
                "--quiet",
                "--initial-branch=main",
                bare.to_str().unwrap(),
            ],
        );
        // A push's receive-pack gets no config from the environment: see
        // `testutil::no_auto_maintenance`.
        git(&bare, &["config", "maintenance.auto", "false"]);
        let seed = dir.join("seed");
        git(
            dir,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        git(&seed, &["commit", "--allow-empty", "-m", "origin main 1"]);
        git(&seed, &["push", "--quiet", "origin", "main"]);
        for branch in extra_remote_branches {
            git(&seed, &["checkout", "--quiet", "-b", branch]);
            git(&seed, &["commit", "--allow-empty", "-m", "remote work"]);
            git(&seed, &["push", "--quiet", "origin", branch]);
        }
        let repo = dir.join("repo");
        git(
            dir,
            &[
                "clone",
                "--quiet",
                bare.to_str().unwrap(),
                repo.to_str().unwrap(),
            ],
        );
        repo
    }

    #[test]
    fn the_base_branch_comes_from_origin_head_then_main_then_master() {
        let dir = tempdir().unwrap();
        let cloned = repo_with_origin(dir.path(), &[]);
        assert_eq!(
            resolve_base_branch(&cloned).as_deref(),
            Some("origin/main"),
            "a clone knows origin/HEAD"
        );

        let local = dir.path().join("local");
        init_repo(&local);
        assert_eq!(resolve_base_branch(&local).as_deref(), Some("main"));

        let master = dir.path().join("master-repo");
        std::fs::create_dir_all(&master).unwrap();
        git(&master, &["init", "--quiet", "--initial-branch=master"]);
        git(
            &master,
            &["commit", "--quiet", "--allow-empty", "-m", "root"],
        );
        assert_eq!(resolve_base_branch(&master).as_deref(), Some("master"));

        let empty = dir.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        git(&empty, &["init", "--quiet", "--initial-branch=trunk"]);
        git(
            &empty,
            &["commit", "--quiet", "--allow-empty", "-m", "root"],
        );
        assert_eq!(resolve_base_branch(&empty), None);
    }

    // Far behind is both at once: a hundred commits, and a month. Either
    // alone is a long feature branch, or a quiet repository.
    #[test]
    fn origin_head_is_far_behind_only_by_both_measures_at_once() {
        let dir = tempdir().unwrap();
        let far = dir.path().join("far");
        crate::testutil::drifted_repo(&far, FAR_AHEAD, STALE_DAYS, None);
        assert_eq!(
            base_drift(&far),
            Some(BaseDrift {
                default: "origin/develop".to_string(),
                default_branch: "develop".to_string(),
                current: "work".to_string(),
                ahead: FAR_AHEAD,
                days_older: STALE_DAYS,
            })
        );

        let few = dir.path().join("few");
        crate::testutil::drifted_repo(&few, FAR_AHEAD - 1, 400, None);
        assert_eq!(base_drift(&few), None, "a long feature branch");

        let recent = dir.path().join("recent");
        crate::testutil::drifted_repo(&recent, 500, STALE_DAYS - 1, None);
        assert_eq!(base_drift(&recent), None, "a default branch still moving");

        // On origin/HEAD's own branch, or on none, there is nothing to say.
        git(&far, &["checkout", "--quiet", "develop"]);
        assert_eq!(base_drift(&far), None);
        git(&far, &["checkout", "--quiet", "--detach", "work"]);
        assert_eq!(base_drift(&far), None);

        // And with no origin/HEAD at all.
        let local = dir.path().join("local");
        init_repo(&local);
        assert_eq!(base_drift(&local), None);
    }

    #[test]
    fn ahead_behind_measures_against_origin_not_a_stale_local_branch() {
        let dir = tempdir().unwrap();
        let repo = repo_with_origin(dir.path(), &[]);
        git(&repo, &["commit", "--allow-empty", "-m", "local extra 1"]);
        git(&repo, &["commit", "--allow-empty", "-m", "local extra 2"]);

        let trees = dir.path().join("trees");
        std::fs::create_dir_all(&trees).unwrap();
        let path = trees.join("feat+o");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--no-track",
                "-b",
                "feat/o",
                path.to_str().unwrap(),
                "origin/main",
            ],
        );
        git(&path, &["commit", "--allow-empty", "-m", "wt c1"]);

        let mut wts = discover(&project_at(&repo)).unwrap();
        enrich_from_git(&mut wts, &repo).unwrap();
        assert_eq!(
            wts[0].ahead_behind,
            Some((1, 0)),
            "counts must be against origin/main, not the local main (which would be (1, 2))"
        );
    }

    #[test]
    fn list_branches_returns_locals_then_remote_only_branches() {
        let dir = tempdir().unwrap();
        let repo = repo_with_origin(dir.path(), &["remote-only"]);
        git(&repo, &["branch", "local-only"]);

        let entries = list_branches(&repo);
        let names: Vec<(&str, &BranchSource)> = entries
            .iter()
            .map(|e| (e.name.as_str(), &e.source))
            .collect();
        assert_eq!(
            names,
            vec![
                ("local-only", &BranchSource::Local),
                ("main", &BranchSource::Local),
                ("remote-only", &BranchSource::Remote),
            ]
        );
    }

    #[test]
    fn list_branches_is_empty_outside_a_repository() {
        let dir = tempdir().unwrap();
        assert!(list_branches(dir.path()).is_empty());
    }

    #[test]
    fn parse_pr_list_maps_the_gh_json_fields() {
        let json = r#"[
            {"author":{"login":"dev"},"headRefName":"fix/one","isDraft":true,"number":435,
             "title":"fix: one","state":"OPEN","url":"https://github.com/org/repo/pull/435"},
            {"author":{"login":"dev"},"headRefName":"docs/two","isDraft":false,"number":427,
             "title":"docs: two","state":"MERGED","url":"https://github.com/org/repo/pull/427"},
            {"author":{"login":"dev"},"headRefName":"feat/three","isDraft":false,"number":400,
             "title":"abandoned","state":"CLOSED","url":"https://github.com/org/repo/pull/400",
             "isCrossRepository":true}
        ]"#;
        let prs = parse_pr_list(json).unwrap();
        assert_eq!(prs.len(), 3);
        assert_eq!(prs[0].number, 435);
        assert_eq!(prs[0].branch, "fix/one");
        assert!(prs[0].draft);
        assert_eq!(prs[0].state, PrState::Open);
        assert_eq!(prs[1].state, PrState::Merged);
        assert_eq!(prs[2].state, PrState::Closed);
        assert!(!prs[0].cross_repository);
        assert!(prs[2].cross_repository);
    }

    fn pr(number: u32, branch: &str, state: PrState, fork: bool) -> PrInfo {
        PrInfo {
            number,
            title: String::new(),
            branch: branch.into(),
            author: String::new(),
            draft: false,
            state,
            url: String::new(),
            cross_repository: fork,
        }
    }

    // An open pull request older than the newest few hundred is in the
    // open list only; one in both is kept once.
    #[test]
    fn the_open_list_and_the_recent_one_merge_newest_first() {
        let open = vec![
            pr(40, "a", PrState::Open, false),
            pr(3, "old", PrState::Open, false),
        ];
        let recent = vec![
            pr(41, "b", PrState::Merged, false),
            pr(40, "a", PrState::Open, false),
        ];
        let numbers: Vec<u32> = merge_pr_lists(open, recent)
            .iter()
            .map(|p| p.number)
            .collect();
        assert_eq!(numbers, vec![41, 40, 3]);
    }

    #[test]
    fn prs_by_branch_keeps_the_newest_and_keys_a_fork_by_its_own_branch() {
        let prs = [
            pr(20, "feat/x", PrState::Open, false),
            pr(15, "main", PrState::Open, true),
            pr(9, "feat/x", PrState::Merged, false),
        ];
        let by_branch = prs_by_branch(&prs);
        assert_eq!(
            by_branch["feat/x"].number, 20,
            "not the merged one before it"
        );
        assert!(!by_branch.contains_key("main"), "a fork's main is not ours");
        assert_eq!(by_branch["pr-15/main"].number, 15);
    }

    #[test]
    fn a_forks_pull_request_gets_a_local_branch_of_its_own() {
        let mut pr = PrInfo {
            number: 12,
            title: "fix".into(),
            branch: "main".into(),
            author: "someone".into(),
            draft: false,
            state: PrState::Open,
            url: String::new(),
            cross_repository: false,
        };
        assert_eq!(pr.local_branch(), "main");
        pr.cross_repository = true;
        assert_eq!(pr.local_branch(), "pr-12/main");
    }

    /// A stand-in `gh` that prints `stdout` and `stderr` and exits `code`,
    /// and records the directory it was asked from.
    fn fake_gh(dir: &Path, stdout: &str, stderr: &str, code: i32) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("gh");
        let script = format!(
            "#!/bin/sh\npwd > '{}'\nprintf '%s' '{stdout}'\nprintf '%s' '{stderr}' >&2\nexit {code}\n",
            dir.join("asked-from").display()
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    // Asked from the project, so a `gh` that picks its account by
    // directory answers for this project.
    #[test]
    fn the_gh_account_is_asked_from_the_project_and_read_as_a_login() {
        let bin = tempdir().unwrap();
        let project = tempdir().unwrap();
        let gh = fake_gh(bin.path(), "someone\n", "", 0);
        assert_eq!(
            gh_account_with(&gh, project.path()),
            GhAccount::Login("someone".to_string())
        );
        let asked = std::fs::read_to_string(bin.path().join("asked-from")).unwrap();
        assert_eq!(
            Path::new(asked.trim()).canonicalize().unwrap(),
            project.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn a_gh_with_no_account_is_signed_out_and_no_gh_is_missing() {
        let bin = tempdir().unwrap();
        let gh = fake_gh(
            bin.path(),
            "",
            "To get started with GitHub CLI, please run:  gh auth login",
            4,
        );
        assert_eq!(gh_account_with(&gh, bin.path()), GhAccount::SignedOut);
        assert_eq!(
            gh_account_with(&bin.path().join("no-such-gh"), bin.path()),
            GhAccount::Missing
        );
        let gh = fake_gh(bin.path(), "", "error connecting to api.github.com", 1);
        assert_eq!(
            gh_account_with(&gh, bin.path()),
            GhAccount::Unknown("error connecting to api.github.com".to_string())
        );
    }

    // A `gh pr list` with no deadline that never answered left the TUI's
    // picker asking GitHub, with `p` refused, until the TUI was restarted.
    #[test]
    fn a_gh_pr_list_that_does_not_answer_is_an_error_in_time() {
        use std::os::unix::fs::PermissionsExt;
        let bin = tempdir().unwrap();
        let gh = bin.path().join("gh");
        std::fs::write(&gh, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let asked = std::time::Instant::now();
        let err = gh_pr_list_with(
            &gh,
            bin.path(),
            "open",
            "10",
            std::time::Duration::from_millis(300),
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("did not answer"), "{err:#}");
        assert!(asked.elapsed() < std::time::Duration::from_secs(10));

        let gh = fake_gh(bin.path(), "[]", "", 0);
        let listed = gh_pr_list_with(
            &gh,
            bin.path(),
            "open",
            "10",
            std::time::Duration::from_secs(10),
        )
        .unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn parse_pr_list_rejects_malformed_json_with_context() {
        let err = parse_pr_list("not json").unwrap_err();
        assert!(format!("{err:#}").contains("parse gh pr list JSON"));
    }

    // Every git question this module asks is read-only, and each one used
    // to be a bare `Command::output`: a git that hung on a lock or a
    // network filesystem hung `ls` and the TUI's enrichment with it. They
    // all go through `project::git`, which has a deadline. A new bare call
    // is the regression, so it is what this looks for.
    #[test]
    fn every_git_call_here_is_bounded() {
        let source = include_str!("worktree.rs");
        let body = source.split("#[cfg(test)]").next().unwrap();
        assert!(
            !body.contains(concat!("Command::new(", "\"git\")")),
            "a git call in worktree.rs bypasses project::git's deadline"
        );
    }

    // git C-quotes a reason with non-ASCII, a quote, or a control character
    // in it. Stored raw, the escapes reach the CLI error, the TUI modal and
    // `ls --json` verbatim.
    #[test]
    fn a_c_quoted_lock_reason_is_decoded() {
        let text = concat!(
            "worktree /a\n",
            "HEAD 0123456789012345678901234567890123456789\n",
            "branch refs/heads/x\n",
            r#"locked "h\303\251llo \"x\" tab\tend""#,
            "\n\n"
        );
        let entries = parse_porcelain(text);
        assert_eq!(
            entries[0].lock_reason.as_deref(),
            Some("h\u{e9}llo \"x\" tab\tend")
        );
    }

    #[test]
    fn an_unquoted_reason_is_left_exactly_as_git_printed_it() {
        let text = concat!(
            "worktree /a\n",
            "HEAD 0123456789012345678901234567890123456789\n",
            "prunable gitdir file points to non-existent location\n\n"
        );
        let entries = parse_porcelain(text);
        assert_eq!(
            entries[0].prunable_reason.as_deref(),
            Some("gitdir file points to non-existent location")
        );
    }

    // `git worktree add --relative-paths`, or `worktree.useRelativePaths`,
    // writes the gitdir relative to the worktree, and git resolves it from
    // there — whether or not the repository is still where it names.
    #[test]
    fn a_relative_gitdir_is_resolved_from_the_worktree_it_is_in() {
        let dir = tempdir().unwrap();
        let base = std::fs::canonicalize(dir.path()).unwrap();
        let checkout = base.join("home/worktrees/feat+one");
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(
            checkout.join(".git"),
            "gitdir: ../../../repo/.git/worktrees/feat+one\n",
        )
        .unwrap();
        assert_eq!(
            linked_gitdir(&checkout),
            Some(base.join("repo/.git/worktrees/feat+one"))
        );

        std::fs::write(
            checkout.join(".git"),
            "gitdir: /abs/repo/.git/worktrees/x\n",
        )
        .unwrap();
        assert_eq!(
            linked_gitdir(&checkout),
            Some(PathBuf::from("/abs/repo/.git/worktrees/x"))
        );

        std::fs::write(checkout.join(".git"), "gitdir: \n").unwrap();
        assert_eq!(linked_gitdir(&checkout), None, "an empty one names nothing");
        std::fs::remove_file(checkout.join(".git")).unwrap();
        std::fs::create_dir(checkout.join(".git")).unwrap();
        assert_eq!(
            linked_gitdir(&checkout),
            None,
            "a main checkout's is a directory"
        );
    }

    // The form git itself writes, end to end.
    #[test]
    fn the_gitdir_git_writes_with_relative_paths_is_the_one_it_uses() {
        let (_dir, repo) = repo_with_worktrees(&[]);
        let path = repo.parent().unwrap().join("trees/rel");
        git(
            &repo,
            &[
                "-c",
                "worktree.useRelativePaths=true",
                "worktree",
                "add",
                "-b",
                "rel",
                path.to_str().unwrap(),
            ],
        );
        let gitdir = linked_gitdir(&path).expect("a linked worktree");
        assert!(
            gitdir.is_dir(),
            "{} is where git keeps it",
            gitdir.display()
        );
        assert_eq!(
            std::fs::canonicalize(&gitdir).unwrap(),
            std::fs::canonicalize(repo.join(".git/worktrees/rel")).unwrap()
        );
    }
}
