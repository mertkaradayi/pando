//! Running one action: the network steps bounded as every fetch pando
//! makes is, the local ones given long enough to finish, and a rebase or
//! a merge that stops on a conflict aborted again before it is reported.

use anyhow::{Result, bail};
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

use crate::worktree::{self, InProgress};

use super::offer::{refusal, remote_of};
use super::read::{self, drift, text};
use super::table::GitAction;

/// How long a rebase, a merge or an abort may take before it is stopped.
/// They run hooks and touch every changed file, so far longer than a git
/// question; bounded, because a hook waiting on a terminal it will never
/// get is a menu that never comes back.
const LOCAL_TIMEOUT: Duration = Duration::from_secs(600);

/// How long a fetch the developer asked for may take. Far longer than
/// the 30 seconds a background git question gets: a large repository,
/// or one not fetched for weeks, takes longer than that every time, and
/// a fetch killed while it writes refs leaves their lock files behind for
/// the next one to trip on. Still bounded, because an ssh prompt for a
/// host key waits on a terminal the menu never shows.
const NETWORK_TIMEOUT: Duration = Duration::from_secs(300);

/// What an action did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// What is checked out changed: the branch moved, or an abort put it
    /// back. A process running on the old files wants a restart.
    Moved(String),
    /// It ran, and nothing checked out changed: a fetch, or nothing new.
    Unchanged(String),
    /// A pull that would not fast-forward, so changed nothing.
    Diverged {
        upstream: String,
        ahead: u32,
        behind: u32,
    },
    /// A rebase or a merge that stopped on a conflict, aborted again: the
    /// files it stopped on, and for a rebase the commit it stopped at.
    Conflict {
        op: InProgress,
        files: Vec<String>,
        at: Option<String>,
    },
}

impl Ran {
    /// Whether what is checked out changed.
    pub fn moved(&self) -> bool {
        matches!(self, Ran::Moved(_))
    }

    /// One line, for the status line and `m`.
    pub fn summary(&self) -> String {
        match self {
            Ran::Moved(said) | Ran::Unchanged(said) => said.clone(),
            Ran::Diverged {
                upstream,
                ahead,
                behind,
            } => format!(
                "{upstream} has diverged (↑{ahead} ↓{behind}) — not a fast-forward, nothing changed"
            ),
            Ran::Conflict { op, files, .. } => format!(
                "conflict in {} — {} aborted, nothing changed",
                file_list(files),
                op.noun()
            ),
        }
    }
}

/// `1 commit`, `2 commits`.
pub(crate) fn commits(n: u32) -> String {
    match n {
        1 => "1 commit".to_string(),
        n => format!("{n} commits"),
    }
}

/// `a.ts`, `a.ts and b.ts`, `a.ts, b.ts and 3 more`.
pub fn file_list(files: &[String]) -> String {
    match files {
        [] => "the files it touched".to_string(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [one, two, rest @ ..] => format!("{one}, {two} and {} more", rest.len()),
    }
}

/// Runs `action` on `checkout`, after reading it again: what the menu
/// showed may be minutes old, and a file saved since is a refusal now.
pub fn run(
    checkout: &Path,
    main: bool,
    base: Option<&str>,
    action: GitAction,
    progress: &dyn Fn(&str),
) -> Result<Ran> {
    let now = read::read(checkout, main, base);
    if let Some(why) = refusal(&now, action) {
        bail!("{why}");
    }
    let branch = now.branch.as_deref().unwrap_or("HEAD");
    match action {
        GitAction::Fetch => fetch_origin(checkout, progress),
        GitAction::Pull => {
            let upstream = now.upstream.as_deref().unwrap_or("@{upstream}");
            let remote = now.upstream_remote.as_deref().unwrap_or("origin");
            pull(checkout, branch, remote, upstream, progress)
        }
        GitAction::Rebase | GitAction::Merge => {
            let Some(base) = now.base.as_deref() else {
                bail!("there is no base branch to move onto");
            };
            move_onto(checkout, branch, base, action, progress)
        }
        GitAction::Abort => abort(checkout, now.in_progress),
    }
}

/// git with nothing to ask anybody: no password prompt, and an editor
/// that accepts whatever message git proposes.
fn git(dir: &Path, args: &[&str], timeout: Duration) -> std::io::Result<Output> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no");
    crate::platform::process::output_within(command, timeout)
}

/// The last line git gave as its reason: stderr's, else stdout's, where
/// a merge says its conflicts.
fn reason(out: &Output) -> String {
    [&out.stderr, &out.stdout]
        .iter()
        .find_map(|bytes| {
            String::from_utf8_lossy(bytes)
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .map(|l| l.trim().to_string())
        })
        .unwrap_or_else(|| format!("exit {}", out.status))
}

/// A step that reaches the network: bounded by [`NETWORK_TIMEOUT`], and
/// nothing checked out has changed when it fails.
fn network(dir: &Path, args: &[&str], shown: &str) -> Result<()> {
    match git(dir, args, NETWORK_TIMEOUT) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => bail!("`{shown}` failed: {} — nothing changed", reason(&out)),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => bail!(
            "`{shown}` did not answer in {}s — nothing changed",
            NETWORK_TIMEOUT.as_secs()
        ),
        Err(e) => bail!("could not run `{shown}`: {e}"),
    }
}

/// Every branch origin has, as this repository last saw it.
fn origin_refs(dir: &Path) -> Vec<String> {
    text(
        dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/remotes/origin",
        ],
    )
    .map(|t| {
        t.lines()
            // A symbolic ref: it moves with the branch it names.
            .filter(|line| !line.starts_with("refs/remotes/origin/HEAD "))
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

fn fetch_origin(dir: &Path, progress: &dyn Fn(&str)) -> Result<Ran> {
    let before = origin_refs(dir);
    progress("fetching origin");
    network(dir, &["fetch", "--quiet", "origin"], "git fetch origin")?;
    let moved = origin_refs(dir)
        .iter()
        .filter(|line| !before.contains(line))
        .count();
    Ok(Ran::Unchanged(match moved {
        0 => "fetched origin · nothing new".to_string(),
        1 => "fetched origin · 1 branch moved".to_string(),
        n => format!("fetched origin · {n} branches moved"),
    }))
}

fn pull(
    dir: &Path,
    branch: &str,
    remote: &str,
    upstream: &str,
    progress: &dyn Fn(&str),
) -> Result<Ran> {
    progress(&format!("fetching {remote}"));
    network(
        dir,
        &["fetch", "--quiet", remote],
        &format!("git fetch {remote}"),
    )?;
    let Some((ahead, behind)) = drift(dir, upstream) else {
        bail!("could not compare {branch} with {upstream} — nothing changed");
    };
    match (ahead, behind) {
        (_, 0) => Ok(Ran::Unchanged(format!(
            "{branch} is up to date with {upstream}"
        ))),
        (0, behind) => {
            refuse_ignored_in_the_way(dir, upstream)?;
            progress(&format!("fast-forwarding to {upstream}"));
            local(dir, &["merge", "--ff-only", "--quiet", upstream], "merge")?;
            Ok(Ran::Moved(format!(
                "{branch} fast-forwarded {} to {upstream}",
                commits(behind)
            )))
        }
        (ahead, behind) => Ok(Ran::Diverged {
            upstream: upstream.to_string(),
            ahead,
            behind,
        }),
    }
}

/// A local step that either worked or changed nothing: a fast-forward
/// refused for a file in its way, an abort.
fn local(dir: &Path, args: &[&str], what: &str) -> Result<()> {
    match git(dir, args, LOCAL_TIMEOUT) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => bail!("git {what} failed: {}", reason(&out)),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => bail!(
            "git {what} did not finish in {} minutes and was stopped",
            LOCAL_TIMEOUT.as_secs() / 60
        ),
        Err(e) => bail!("could not run git {what}: {e}"),
    }
}

/// Refuses a move onto `target` that would write over a file this
/// checkout ignores.
///
/// `git status` lists no ignored file, so the dirty check passes over a
/// provisioned `.env`; and git treats an ignored file as expendable, so a
/// merge, a fast-forward or a rebase onto a commit that starts tracking
/// that path replaces it — and the abort after a conflict then deletes
/// it. Checked before anything runs, so "nothing changed" stays true.
fn refuse_ignored_in_the_way(dir: &Path, target: &str) -> Result<()> {
    let in_the_way = ignored_in_the_way(dir, target);
    if in_the_way.is_empty() {
        return Ok(());
    }
    let (is, them) = match in_the_way.len() {
        1 => ("is", "it"),
        _ => ("are", "them"),
    };
    bail!(
        "{target} tracks {}, which {is} ignored here and would be overwritten — move {them} \
         aside first; nothing changed",
        file_list(&in_the_way)
    )
}

/// The paths `target` tracks and HEAD does not that are ignored files,
/// or under ignored directories, in `dir`.
pub(super) fn ignored_in_the_way(dir: &Path, target: &str) -> Vec<String> {
    let lines = |args: &[&str]| -> Vec<String> {
        text(dir, args)
            .map(|t| t.lines().map(str::to_string).collect())
            .unwrap_or_default()
    };
    let added = lines(&[
        "-c",
        "core.quotePath=false",
        "diff",
        "--name-only",
        "--no-renames",
        "--diff-filter=A",
        "HEAD",
        target,
        "--",
    ]);
    if added.is_empty() {
        return Vec::new();
    }
    // `--directory` keeps an ignored `node_modules` one line.
    let ignored = lines(&[
        "-c",
        "core.quotePath=false",
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "--directory",
    ]);
    added
        .into_iter()
        .filter(|path| {
            ignored.iter().any(|entry| match entry.strip_suffix('/') {
                Some(dir) => path.starts_with(entry.as_str()) || path == dir,
                None => path == entry,
            })
        })
        .collect()
}

fn move_onto(
    dir: &Path,
    branch: &str,
    base: &str,
    action: GitAction,
    progress: &dyn Fn(&str),
) -> Result<Ran> {
    if let Some((remote, name)) = remote_of(base) {
        progress(&format!("fetching {remote} {name}"));
        network(
            dir,
            &["fetch", "--quiet", remote, name],
            &format!("git fetch {remote} {name}"),
        )?;
    }
    let Some((ahead, behind)) = drift(dir, base) else {
        bail!("could not compare {branch} with {base} — nothing changed");
    };
    if behind == 0 {
        return Ok(Ran::Unchanged(format!(
            "{branch} is already on top of {base}"
        )));
    }
    refuse_ignored_in_the_way(dir, base)?;
    let before = text(dir, &["rev-parse", "HEAD"]);
    let (op, args): (InProgress, &[&str]) = match action {
        GitAction::Rebase => {
            progress(&format!("rebasing onto {base}"));
            (InProgress::Rebase, &["rebase", "--quiet", base])
        }
        _ => {
            progress(&format!("merging {base}"));
            (InProgress::Merge, &["merge", "--no-edit", "--quiet", base])
        }
    };
    let out = match git(dir, args, LOCAL_TIMEOUT) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => bail!(
            "git {} did not finish in {} minutes and was stopped — u shows what it left",
            op.noun(),
            LOCAL_TIMEOUT.as_secs() / 60
        ),
        Err(e) => bail!("could not run git {}: {e}", op.noun()),
    };
    if out.status.success() {
        return Ok(Ran::Moved(match op {
            InProgress::Rebase => {
                format!(
                    "rebased {branch} onto {base} · {} on top of {behind} new",
                    commits(ahead)
                )
            }
            _ => format!(
                "merged {base} into {branch} · brought in {}",
                commits(behind)
            ),
        }));
    }
    if worktree::in_progress(dir) != Some(op) {
        // It never started: a file in the way, a hook that refused. git
        // leaves nothing behind then, which HEAD shows.
        let unchanged = text(dir, &["rev-parse", "HEAD"]) == before;
        let after = if unchanged {
            "nothing changed"
        } else {
            "HEAD moved — look before you go on"
        };
        bail!("git {} failed: {} — {after}", op.noun(), reason(&out));
    }
    let files: Vec<String> = text(dir, &["diff", "--name-only", "--diff-filter=U"])
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let at = match op {
        InProgress::Rebase => text(dir, &["log", "-1", "--format=%h %s", "REBASE_HEAD"]),
        _ => None,
    };
    if let Err(e) = local(
        dir,
        &[op.noun(), "--abort"],
        &format!("{} --abort", op.noun()),
    ) {
        bail!(
            "the {} stopped on a conflict in {}, and aborting it failed ({e:#}) — ! to finish \
             it by hand",
            op.noun(),
            file_list(&files)
        );
    }
    if text(dir, &["rev-parse", "HEAD"]) != before {
        bail!(
            "the {} stopped on a conflict and was aborted, but HEAD is not where it was — look \
             before you go on",
            op.noun()
        );
    }
    Ok(Ran::Conflict { op, files, at })
}

fn abort(dir: &Path, in_progress: Option<InProgress>) -> Result<Ran> {
    let Some(op @ (InProgress::Rebase | InProgress::Merge)) = in_progress else {
        bail!("nothing is left half-done here");
    };
    let noun = op.noun();
    local(dir, &[noun, "--abort"], &format!("{noun} --abort"))?;
    Ok(Ran::Moved(format!(
        "aborted the {noun} · back to where it was"
    )))
}
