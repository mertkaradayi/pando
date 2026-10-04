//! Filling a new worktree by copy-on-write: the main checkout's files are
//! cloned in, and git writes only the ones that differ.
//!
//! `git worktree add` writes every tracked file from the object store, so
//! each worktree costs the whole checkout again on disk. A clone shares its
//! source's blocks until one side writes. What comes out has the same
//! content, modes and git state as git's own checkout — git's own `reset
//! --hard` makes it — and only the bytes it took differ.

use anyhow::{Result, anyhow, bail};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::config::Config;

/// How `new` checks a worktree out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CheckoutPlan {
    /// `git worktree add`, writing every file: what pando always did.
    Git,
    /// `git worktree add --no-checkout`, then [`fill`].
    CopyOnWrite(Source),
}

/// What the clones come from: the main checkout, at its `HEAD` commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Source {
    root: PathBuf,
    commit: String,
    /// The commit's regular files, by path, and whether each is
    /// executable. Symlinks and submodules are git's to write.
    files: Vec<(String, bool)>,
    /// The `post-checkout` hook `git worktree add` would run, resolved
    /// against the main checkout as it resolves it, when there is one.
    post_checkout: Option<PathBuf>,
}

/// What [`fill`] did, for the line `new` prints and for the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Filled {
    /// Files cloned from the main checkout.
    pub cloned: usize,
    /// Files git had to change: the branch's own changes, the main
    /// checkout's uncommitted ones, and anything not cloned.
    pub written: usize,
}

/// Whether a worktree goes into `worktrees_dir` by copy-on-write.
///
/// Every reason to say no keeps git's own checkout, which is always right:
///
/// - `copy_on_write = false`;
/// - a git older than the commands a copy-on-write checkout needs
///   (`check-attr --source`, `--attr-source`), asked of git, never
///   guessed from its version string;
/// - checkout settings that change bytes for every file, or that a raw
///   comparison cannot switch off ([`converts_everything`]);
/// - a sparse main checkout, whose files are not the commit's;
/// - no commit yet, or one git cannot list;
/// - a filesystem that cannot clone from the main checkout to where the
///   worktree goes, probed with one real clone.
///
/// Only the last is said, and only to a developer who asked for
/// copy-on-write by name: on a disk that cannot clone, a line on every
/// `new` is one nobody reads.
pub(super) fn plan(
    root: &Path,
    config: &Config,
    worktrees_dir: &Path,
    progress: &dyn Fn(&str),
) -> CheckoutPlan {
    if !config.project.copy_on_write() {
        return CheckoutPlan::Git;
    }
    if !git_can(root)
        || converts_everything(root)
        || git_text(
            root,
            &["config", "--type=bool", "--get", "core.sparseCheckout"],
        )
        .as_deref()
            == Some("true")
    {
        return CheckoutPlan::Git;
    }
    let Some(commit) = git_text(root, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
    else {
        return CheckoutPlan::Git;
    };
    let Some(files) = regular_files(root, &commit) else {
        return CheckoutPlan::Git;
    };
    match probe(root, &files, worktrees_dir) {
        Some(true) => CheckoutPlan::CopyOnWrite(Source {
            root: root.to_path_buf(),
            commit,
            files,
            post_checkout: post_checkout_hook(root),
        }),
        Some(false) => {
            if config.project.copy_on_write == Some(true) {
                progress(
                    "this filesystem cannot clone the main checkout's files into pando's \
                     worktrees directory, so git checks this out",
                );
            }
            CheckoutPlan::Git
        }
        None => CheckoutPlan::Git,
    }
}

/// Whether this git has what a copy-on-write checkout runs: attributes
/// read from a given tree, both for one command (`--attr-source`) and for
/// `check-attr` (`--source`). One harmless question asks for both.
fn git_can(root: &Path) -> bool {
    let Some(empty_tree) = empty_tree(root) else {
        return false;
    };
    let attr_source = format!("--attr-source={empty_tree}");
    let source = format!("--source={empty_tree}");
    git_ok(
        root,
        &[
            &attr_source,
            "check-attr",
            &source,
            "eol",
            "--",
            "pando-probe",
        ],
    )
}

/// Whether checkout in `dir` changes the bytes of files no attribute
/// names, or reads attributes the raw comparison cannot switch off.
///
/// - `core.autocrlf` true or input, as git reads it: a key with no value
///   and any non-zero number are true too;
/// - `core.eol = crlf`;
/// - `attr.tree`, or `GIT_ATTR_SOURCE` in pando's environment: checkout
///   then reads attributes from a tree the per-file check does not ask;
/// - `$GIT_DIR/info/attributes` with anything in it.
///
/// Asked of the main checkout before anything is made, and again of the
/// new worktree, whose config can differ: `includeIf "onbranch:…"`.
fn converts_everything(dir: &Path) -> bool {
    let autocrlf = match git(dir, &["config", "--type=bool", "--get", "core.autocrlf"]) {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim() == "true",
        // Not a boolean: `input` is the one other value git takes.
        Ok(out) if out.status.code() == Some(128) => {
            git_text(dir, &["config", "--get", "core.autocrlf"])
                .is_some_and(|v| v.eq_ignore_ascii_case("input"))
        }
        _ => false,
    };
    autocrlf
        || git_text(dir, &["config", "--get", "core.eol"])
            .is_some_and(|v| v.eq_ignore_ascii_case("crlf"))
        || git_text(dir, &["config", "--get", "attr.tree"]).is_some()
        || std::env::var_os("GIT_ATTR_SOURCE").is_some()
        || has_info_attributes(dir)
}

/// One real clone, from a file of the main checkout into the directory the
/// worktree will be made in, removed again at once. Whether two paths can
/// clone depends on both volumes and on the filesystem, and only the
/// filesystem knows. `None` when there was nothing to try with, or the
/// try failed for a reason that says nothing about cloning.
fn probe(root: &Path, files: &[(String, bool)], worktrees_dir: &Path) -> Option<bool> {
    let sample = files.iter().map(|(f, _)| root.join(f)).find(|p| {
        p.symlink_metadata()
            .is_ok_and(|m| m.is_file() && !crate::cow::must_not_clone(&m))
    })?;
    let dst = worktrees_dir.join(format!(".pando-clone-probe-{}", std::process::id()));
    let _ = std::fs::remove_file(&dst);
    let result = crate::cow::clone_file(&sample, &dst);
    let _ = std::fs::remove_file(&dst);
    match result {
        Ok(()) => Some(true),
        Err(e) if crate::cow::is_unsupported(&e) => Some(false),
        Err(_) => None,
    }
}

/// Fills the worktree `git worktree add --no-checkout` just made, then runs
/// the `post-checkout` hook `--no-checkout` skipped.
///
/// 1. The index is the main checkout's commit, before any file arrives, so
///    every file cloned next is tracked and the reset below removes the
///    ones the branch does not have.
/// 2. The commit's files are cloned from the main checkout's working tree —
///    the commit's list, never the main checkout's index, which can hold a
///    file staged but not committed — except a file whose checkout under
///    the branch is not its blob byte for byte: one with a filter, `ident`,
///    a working-tree encoding or CRLF endings. git writes those.
/// 3. A raw refresh — no attributes, no line-ending conversion — records a
///    clone as up to date only when its bytes are the blob's. git's usual
///    refresh compares the cleaned contents, and would keep a main
///    checkout's CRLF copy of an LF file.
/// 4. `reset --hard` writes what differs from the branch and removes what
///    it does not have — the same command whether every file was cloned or
///    none was. `--no-recurse-submodules`, as `worktree add` runs it.
///
/// Only a failed reset, a cloned file left that the branch does not have,
/// or a failed hook is an error, and the caller unwinds. Anything else
/// leaves more for git to write, which is slower and never wrong.
pub(super) fn fill(worktree: &Path, source: &Source, progress: &dyn Fn(&str)) -> Result<Filled> {
    let indexed = git_ok(worktree, &["read-tree", &source.commit]);
    let wanted = match indexed && !converts_everything(worktree) {
        true => clonable(worktree, &source.files),
        false => Vec::new(),
    };
    let cloned = match wanted.is_empty() {
        true => Vec::new(),
        false => {
            progress(&format!(
                "cloning {} from the main checkout (copy-on-write)",
                count(wanted.len(), "file")
            ));
            clone_all(worktree, &source.root, &wanted)
        }
    };
    if !cloned.is_empty() {
        raw_refresh(worktree);
    }
    let written = differing(worktree);
    let out = git(
        worktree,
        &[
            "reset",
            "--hard",
            "--quiet",
            "--no-recurse-submodules",
            "HEAD",
        ],
    )?;
    if !out.status.success() {
        bail!("git reset --hard failed: {}", last_line(&out.stderr));
    }
    if !cloned.is_empty() {
        progress(&format!(
            "git changed {} that {} from the main checkout",
            count(written, "file"),
            match written {
                1 => "differs",
                _ => "differ",
            }
        ));
    }
    // A file pando cloned that the branch does not have, still there, is
    // an untracked file pando wrote: exactly what Invariant 1 forbids. Only
    // pando's own files are asked about — anything else `git status`
    // shows, git's own checkout shows too.
    if let Some(stray) = stray(worktree, &cloned) {
        bail!(
            "the copy-on-write checkout left {stray:?}, which the branch does not have; \
             `copy_on_write = false` in pando.toml checks out with git"
        );
    }
    if let Some(hook) = &source.post_checkout {
        run_post_checkout(worktree, hook)?;
    }
    Ok(Filled {
        cloned: cloned.len(),
        written,
    })
}

/// The files worth cloning: those whose checkout under the worktree's
/// `HEAD` is the blob itself. Asked of git with the branch's own
/// attributes (`--source=HEAD`), since the worktree has no `.gitattributes`
/// on disk yet and the branch can carry different ones from main.
fn clonable(worktree: &Path, files: &[(String, bool)]) -> Vec<(String, bool)> {
    let mut input = Vec::new();
    for (path, _) in files {
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    let attrs = ["filter", "ident", "working-tree-encoding", "eol"];
    let mut args = vec!["check-attr", "--source=HEAD", "-z", "--stdin"];
    args.extend(attrs);
    let Ok(out) = git_with_input(worktree, &args, &input) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    // `<path> NUL <attribute> NUL <value> NUL`, one triple per attribute.
    let mut converted: HashSet<&[u8]> = HashSet::new();
    let fields: Vec<&[u8]> = out.stdout.split(|b| *b == 0).collect();
    for triple in fields.chunks(3) {
        let [path, attr, value] = triple else {
            continue;
        };
        let changes_bytes = match *attr {
            b"eol" => value.eq_ignore_ascii_case(b"crlf"),
            _ => !matches!(*value, b"unspecified" | b"unset"),
        };
        if changes_bytes {
            converted.insert(path);
        }
    }
    files
        .iter()
        .filter(|(path, _)| !converted.contains(path.as_bytes()))
        .cloned()
        .collect()
}

/// Clones each file on a few threads, each made to look the way git writes
/// one. Returns the paths that were cloned.
///
/// A file the main checkout lacks, has as something else, has locked or
/// has evicted is skipped, and so is any file that fails to clone: the
/// probe already said this filesystem can, so a failure is that file's,
/// and git writes it.
fn clone_all(worktree: &Path, root: &Path, files: &[(String, bool)]) -> Vec<String> {
    // Directories first, one pass, so the threads below only clone.
    let mut dirs: Vec<PathBuf> = files
        .iter()
        .filter_map(|(f, _)| worktree.join(f).parent().map(Path::to_path_buf))
        .collect();
    dirs.sort();
    dirs.dedup();
    for dir in &dirs {
        let _ = std::fs::create_dir_all(dir);
    }
    let umask = crate::cow::umask();
    let workers = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .clamp(1, 8);
    let chunk = files.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|part| {
                scope.spawn(move || {
                    part.iter()
                        .filter(|(file, executable)| {
                            clone_one(&root.join(file), &worktree.join(file), *executable, umask)
                        })
                        .map(|(file, _)| file.clone())
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    })
}

fn clone_one(src: &Path, dst: &Path, executable: bool, umask: u32) -> bool {
    let Ok(meta) = src.symlink_metadata() else {
        return false;
    };
    if !meta.is_file() || crate::cow::must_not_clone(&meta) {
        return false;
    }
    if crate::cow::clone_file(src, dst).is_err() {
        return false;
    }
    match crate::cow::as_git_writes(dst, executable, umask) {
        Ok(()) => true,
        // A clone that cannot be made to look git-written is not kept:
        // git writes it instead.
        Err(_) => {
            let _ = std::fs::remove_file(dst);
            false
        }
    }
}

/// `update-index --refresh` with nothing converting the files it reads, so
/// a clone counts as up to date only when its bytes are the blob's: no
/// attributes from the tree (`--attr-source` an empty one), the global file
/// (`core.attributesFile`) or the system one (`GIT_ATTR_NOSYSTEM`), and no
/// line-ending setting. A refresh git refuses still leaves an index the
/// reset can use: it only rewrites more.
fn raw_refresh(worktree: &Path) {
    let Some(empty_tree) = empty_tree(worktree) else {
        return;
    };
    let attr_source = format!("--attr-source={empty_tree}");
    let _ = git_command(worktree)
        .args([
            &attr_source,
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.eol=lf",
            "-c",
            "core.attributesFile=/dev/null",
            "update-index",
            "-q",
            "--refresh",
        ])
        .env("GIT_ATTR_NOSYSTEM", "1")
        .output();
}

/// How many files the reset will change: index entries that differ from
/// the branch's commit, and working files that differ from the index.
fn differing(worktree: &Path) -> usize {
    let mut paths: std::collections::BTreeSet<String> = Default::default();
    for args in [
        &["diff-index", "--cached", "--name-only", "-z", "HEAD"][..],
        &["diff-files", "--name-only", "-z"][..],
    ] {
        if let Ok(out) = git(worktree, args)
            && out.status.success()
        {
            paths.extend(
                String::from_utf8_lossy(&out.stdout)
                    .split('\0')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string),
            );
        }
    }
    paths.len()
}

/// The first cloned file the branch does not have that is still in the
/// worktree, under exactly its own name. Asked of the directory itself,
/// not by path: on a case-insensitive disk `A.txt` would answer for a
/// branch's `a.txt`.
fn stray(worktree: &Path, cloned: &[String]) -> Option<String> {
    if cloned.is_empty() {
        return None;
    }
    let out = git(worktree, &["ls-tree", "-r", "-z", "--name-only", "HEAD"]).ok()?;
    let tracked: HashSet<&[u8]> = out.stdout.split(|b| *b == 0).collect();
    cloned
        .iter()
        .filter(|path| !tracked.contains(path.as_bytes()))
        .find(|path| {
            let path = worktree.join(path);
            let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
                return false;
            };
            std::fs::read_dir(dir)
                .is_ok_and(|entries| entries.flatten().any(|e| e.file_name() == name))
        })
        .cloned()
}

/// Runs the `post-checkout` hook as `git worktree add` runs it: in the new
/// worktree, with the null commit, `HEAD` and the branch flag, and without
/// `GIT_DIR` or `GIT_WORK_TREE`, which `worktree add` unsets for it — a
/// hook that runs git in another repository must reach that repository.
/// A hook that fails is an error, as it is for the checkout it stands in
/// for.
fn run_post_checkout(worktree: &Path, hook: &Path) -> Result<()> {
    let head = git_text(worktree, &["rev-parse", "HEAD"])
        .ok_or_else(|| anyhow!("git could not read the new worktree's HEAD"))?;
    let null = "0".repeat(head.len());
    let cwd = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    let out = Command::new(hook)
        .args([null.as_str(), head.as_str(), "1"])
        .current_dir(&cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| anyhow!("run the post-checkout hook {}: {e}", hook.display()))?;
    if !out.status.success() {
        bail!(
            "the post-checkout hook failed: {}",
            match last_line(&out.stderr) {
                line if line.is_empty() => format!("exit {}", out.status.code().unwrap_or(-1)),
                line => line,
            }
        );
    }
    Ok(())
}

/// The executable `post-checkout` hook `git worktree add` would run, if
/// any. Absolute, because a relative `core.hooksPath` — husky's
/// `.husky/_` — means the main checkout's directory to `worktree add`, and
/// would mean the worktree's own to anything run there.
fn post_checkout_hook(root: &Path) -> Option<PathBuf> {
    let hook = PathBuf::from(git_text(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "hooks/post-checkout",
        ],
    )?);
    use std::os::unix::fs::PermissionsExt;
    hook.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .then_some(hook)
}

/// Whether `$GIT_DIR/info/attributes` says anything. The raw refresh
/// switches off the tree's, the global and the system attributes, but git
/// offers no way to switch off this file.
fn has_info_attributes(dir: &Path) -> bool {
    git_text(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/attributes",
        ],
    )
    .and_then(|p| std::fs::read_to_string(p).ok())
    .is_some_and(|text| {
        text.lines()
            .any(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
    })
}

/// The commit's regular files, `100644` and `100755` blobs, each with
/// whether it is executable. `None` when git cannot list them.
fn regular_files(root: &Path, commit: &str) -> Option<Vec<(String, bool)>> {
    let out = crate::project::git(root, ["ls-tree", "-r", "-z", "--full-tree", commit]).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.split('\0')
            .filter_map(|entry| entry.split_once('\t'))
            .filter_map(|(meta, path)| match meta.split(' ').next() {
                Some("100644") => Some((path.to_string(), false)),
                Some("100755") => Some((path.to_string(), true)),
                _ => None,
            })
            .collect(),
    )
}

/// The empty tree's id in this repository's hash: SHA-1 or SHA-256.
fn empty_tree(dir: &Path) -> Option<String> {
    git_text(dir, &["hash-object", "-t", "tree", "/dev/null"])
}

/// git in `dir`, as `git worktree add` runs: no deadline (a filter takes
/// what it takes), never prompting — a filter may ask for a password, over
/// the TUI, whose keys then go nowhere — and with its output captured,
/// never painted over the TUI's screen.
fn git_command(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    cmd
}

fn git(dir: &Path, args: &[&str]) -> Result<Output> {
    git_command(dir)
        .args(args)
        .output()
        .map_err(|e| anyhow!("spawn git {}: {e}", args.join(" ")))
}

fn git_with_input(dir: &Path, args: &[&str], input: &[u8]) -> std::io::Result<Output> {
    let mut child = git_command(dir)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Written from a thread: git answers as it reads, and a pipe buffer
    // full in both directions would stall both sides.
    let mut stdin = child.stdin.take().expect("stdin was piped");
    let input = input.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let out = child.wait_with_output()?;
    let _ = writer.join();
    Ok(out)
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git(dir, args).is_ok_and(|o| o.status.success())
}

/// A quick read-only answer from git, with pando's usual deadline.
fn git_text(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::project::git(dir, args).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn last_line(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().trim_start_matches("fatal: ").to_string())
        .unwrap_or_default()
}

fn count(n: usize, noun: &str) -> String {
    match n {
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}
