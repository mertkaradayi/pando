//! Where a checkout stands, as the menu's header shows it: against the
//! base new worktrees fork from, against its own upstream, and whether
//! anything in it would stop a move.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::config::Config;
use crate::worktree::{self, InProgress};

use super::super::worktree::resolve_create_base;

/// One read of a checkout, taken off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRead {
    pub checkout: PathBuf,
    /// The main checkout, which pando only ever fast-forwards.
    pub main: bool,
    /// `None` on a detached HEAD.
    pub branch: Option<String>,
    /// What a rebase goes onto and a merge brings in, as git names it:
    /// `origin/main`.
    pub base: Option<String>,
    /// Commits ahead of the base, and behind it.
    pub base_drift: Option<(u32, u32)>,
    /// The branch's upstream: `origin/feat/x`.
    pub upstream: Option<String>,
    /// The remote the upstream is on, which a pull fetches.
    pub upstream_remote: Option<String>,
    /// Commits ahead of the upstream, and behind it.
    pub upstream_drift: Option<(u32, u32)>,
    /// How many of the commits a rebase would rewrite the upstream has
    /// already: the next push of a rebased branch needs force.
    pub pushed: u32,
    /// Entries `git status` lists: uncommitted changes and untracked
    /// files alike. `None` when `git status` failed or did not answer in
    /// time, which no move takes for clean.
    pub dirty: Option<usize>,
    pub in_progress: Option<InProgress>,
    pub has_origin: bool,
    /// When the repository last fetched, from `FETCH_HEAD`'s time.
    pub fetched: Option<SystemTime>,
}

/// The base a branch is measured against and moved onto: the one its
/// worktree was made to go onto (`recorded`), while that still names a
/// commit; then the config's rule for it, then the project's base, each
/// preferring `origin`'s copy as `new` does, then the repository's default.
pub fn base_for(
    root: &Path,
    config: &Config,
    branch: Option<&str>,
    recorded: Option<&str>,
) -> Option<String> {
    if let Some(recorded) = recorded
        && text(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{recorded}^{{commit}}"),
            ],
        )
        .is_some()
    {
        return Some(recorded.to_string());
    }
    let configured = match branch {
        Some(branch) => config.base_for_branch(branch),
        None => config.project.base.as_deref(),
    };
    configured
        .map(|base| resolve_create_base(root, base))
        .or_else(|| worktree::resolve_base_branch(root))
}

/// Where `checkout` stands. Never fails: what git does not answer is left
/// unknown, and the menu says so rather than offering a move on a guess.
pub fn read(checkout: &Path, main: bool, base: Option<&str>) -> GitRead {
    let branch = worktree::checked_out_branch(checkout);
    let upstream = branch.as_ref().and_then(|_| {
        text(
            checkout,
            &[
                "rev-parse",
                "--abbrev-ref",
                "--symbolic-full-name",
                "@{upstream}",
            ],
        )
    });
    let upstream_remote = branch
        .as_ref()
        .filter(|_| upstream.is_some())
        .and_then(|b| text(checkout, &["config", &format!("branch.{b}.remote")]));
    let base_drift = base.and_then(|b| drift(checkout, b));
    let upstream_drift = upstream.as_deref().and_then(|u| drift(checkout, u));
    let pushed = match (base, upstream.as_deref()) {
        (Some(base), Some(upstream)) => pushed(checkout, base, upstream),
        _ => 0,
    };
    GitRead {
        checkout: checkout.to_path_buf(),
        main,
        branch,
        base: base.map(str::to_string),
        base_drift,
        upstream,
        upstream_remote,
        upstream_drift,
        pushed,
        dirty: dirty(checkout),
        in_progress: worktree::in_progress(checkout),
        has_origin: text(checkout, &["remote", "get-url", "origin"]).is_some(),
        fetched: fetched(checkout),
    }
}

/// Trimmed stdout of a git command that succeeded with something to say.
pub(super) fn text(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::project::git(dir, args).ok()?;
    let said = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !said.is_empty()).then_some(said)
}

/// Commits on HEAD that `other` lacks, and on `other` that HEAD lacks.
pub(super) fn drift(dir: &Path, other: &str) -> Option<(u32, u32)> {
    let counts = text(
        dir,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{other}"),
            "--",
        ],
    )?;
    let mut parts = counts.split_whitespace();
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

/// The commits between the base and where HEAD and its upstream last
/// met: those a rebase rewrites that a push already published.
fn pushed(dir: &Path, base: &str, upstream: &str) -> u32 {
    let Some(met) = text(dir, &["merge-base", "HEAD", upstream]) else {
        return 0;
    };
    text(
        dir,
        &["rev-list", "--count", &format!("{base}..{met}"), "--"],
    )
    .and_then(|n| n.parse().ok())
    .unwrap_or(0)
}

/// `--no-optional-locks`, as the list's own status read: a read never
/// writes an index lock into the checkout.
pub(super) fn dirty(dir: &Path) -> Option<usize> {
    crate::project::git(dir, ["--no-optional-locks", "status", "--porcelain"])
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).lines().count())
}

/// The newer of this checkout's `FETCH_HEAD` and the repository's: a
/// fetch writes the one of the checkout it ran in, and every checkout
/// shares the refs it brought.
fn fetched(dir: &Path) -> Option<SystemTime> {
    let paths = text(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "FETCH_HEAD",
            "--git-common-dir",
        ],
    )?;
    let mut lines = paths.lines();
    let own = lines.next().map(PathBuf::from);
    let common = lines.next().map(|d| Path::new(d).join("FETCH_HEAD"));
    [own, common]
        .into_iter()
        .flatten()
        .filter_map(|p| std::fs::metadata(p).ok()?.modified().ok())
        .max()
}
