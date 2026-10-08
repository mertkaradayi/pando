//! What the menu offers on a checkout, and why not where it cannot: then,
//! for the one picked, the preview — the exact commands, what moves, and
//! what to know first.

use crate::worktree::InProgress;

use super::read::GitRead;
use super::run::commits;
use super::table::{ACTIONS, GitAction};

/// One line of the menu: an action, what it would do here, and the
/// reason it will not, when it will not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub action: GitAction,
    pub what: String,
    pub refused: Option<String>,
}

/// What the preview shows before an action runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub title: String,
    /// What runs, in order, as it would be typed.
    pub commands: Vec<String>,
    /// What moves, in numbers, as of the last fetch.
    pub moves: Option<String>,
    pub notes: Vec<String>,
    /// What to know before it runs: shown as a warning.
    pub warnings: Vec<String>,
}

/// Every action the menu shows for `read`, in the table's order. Abort is
/// shown only while a rebase or a merge is left half-done, and then the
/// moves are not: git will start none of them over it.
pub fn offers(read: &GitRead) -> Vec<Offer> {
    let abortable = matches!(
        read.in_progress,
        Some(InProgress::Rebase | InProgress::Merge)
    );
    ACTIONS
        .iter()
        .filter(|row| match row.action {
            GitAction::Abort => abortable,
            GitAction::Pull | GitAction::Rebase | GitAction::Merge => !abortable,
            GitAction::Fetch => true,
        })
        .map(|row| offer(read, row.action))
        .collect()
}

fn offer(read: &GitRead, action: GitAction) -> Offer {
    let refused = refusal(read, action);
    let base = read.base.as_deref().unwrap_or("its base");
    let what = match action {
        GitAction::Fetch => "bring origin up to date · no file changes".to_string(),
        GitAction::Pull => match (read.upstream.as_deref(), read.upstream_drift) {
            (Some(upstream), Some((0, behind))) if behind > 0 => {
                format!("fast-forward to {upstream} · {behind} new")
            }
            (Some(upstream), Some((ahead, behind))) if ahead > 0 && behind > 0 => {
                format!(
                    "{upstream} has diverged (↑{ahead} ↓{behind}) — only a fast-forward is taken"
                )
            }
            (Some(upstream), _) => format!("fetch {upstream}, then fast-forward to it"),
            (None, _) => "fast-forward to its upstream".to_string(),
        },
        GitAction::Rebase => match read.base_drift {
            Some((ahead, behind)) if behind > 0 => {
                format!("onto {base} · replays {} onto {behind} new", commits(ahead))
            }
            _ => format!("onto {base} · nothing new as of the last fetch"),
        },
        GitAction::Merge => format!("{base} into it · one merge commit, history kept"),
        GitAction::Abort => {
            let noun = read.in_progress.map_or("rebase", InProgress::noun);
            format!("a {noun} was left half-done — put it back as it was")
        }
    };
    Offer {
        action,
        what,
        refused,
    }
}

/// Why `action` will not run on `read`, or `None` when it will. Checked
/// again by [`super::run`] on a fresh read before anything runs.
pub(super) fn refusal(read: &GitRead, action: GitAction) -> Option<String> {
    let dirty = || match read.dirty {
        Some(0) => None,
        Some(n) => {
            let files = if n == 1 { "file" } else { "files" };
            Some(format!("✎ {n} uncommitted {files} — commit or stash first"))
        }
        None => Some("git status did not answer, so the tree may not be clean".to_string()),
    };
    let in_progress = || {
        read.in_progress
            .map(|op| format!("a {} is in progress — ! to finish it", op.noun()))
    };
    let detached = || {
        read.branch
            .is_none()
            .then(|| "no branch is checked out".to_string())
    };
    match action {
        GitAction::Fetch => (!read.has_origin).then(|| "there is no remote called origin".into()),
        GitAction::Pull => in_progress()
            .or_else(detached)
            .or_else(|| match &read.upstream {
                None => Some(format!(
                    "{} tracks no remote branch",
                    read.branch.as_deref().unwrap_or("it")
                )),
                Some(_) => dirty(),
            }),
        GitAction::Rebase | GitAction::Merge => {
            if read.main {
                return Some("not on the main checkout — pando only fast-forwards it".to_string());
            }
            in_progress().or_else(detached).or_else(dirty).or_else(|| {
                let Some(base) = &read.base else {
                    return Some("there is no base branch to move onto".to_string());
                };
                // A base on a remote may have moved since the last fetch,
                // which the run does first; a local one cannot have.
                let remote = remote_of(base).is_some();
                match read.base_drift {
                    Some((_, 0)) if !remote => Some(format!("already on top of {base}")),
                    None if !remote => Some(format!("{base} could not be read")),
                    _ => None,
                }
            })
        }
        GitAction::Abort => match read.in_progress {
            Some(InProgress::Rebase | InProgress::Merge) => None,
            _ => Some("nothing is left half-done here".to_string()),
        },
    }
}

/// The remote a base lives on, when it is a remote-tracking branch of a
/// remote the repository has: `origin/main` → (`origin`, `main`).
pub(super) fn remote_of(base: &str) -> Option<(&str, &str)> {
    base.split_once('/')
        .filter(|(remote, _)| *remote == "origin")
}

/// The preview of `action` on `read`.
pub fn plan(read: &GitRead, action: GitAction) -> Plan {
    let branch = read.branch.as_deref().unwrap_or("HEAD");
    let base = read.base.as_deref().unwrap_or("its base");
    let fetch_base = remote_of(base).map(|(remote, name)| format!("git fetch {remote} {name}"));
    let mut commands = Vec::new();
    let mut notes = Vec::new();
    let mut warnings = Vec::new();
    let (title, moves) = match action {
        GitAction::Fetch => {
            commands.push("git fetch origin".to_string());
            notes.push("brings origin's branches; no file in any checkout changes".to_string());
            notes.push("every row's ↓ is counted again afterwards".to_string());
            ("fetch origin".to_string(), None)
        }
        GitAction::Pull => {
            let upstream = read.upstream.as_deref().unwrap_or("its upstream");
            let remote = read.upstream_remote.as_deref().unwrap_or("origin");
            commands.push(format!("git fetch {remote}"));
            commands.push(format!("git merge --ff-only {upstream}"));
            notes.push("if it cannot fast-forward, it says so and changes nothing".to_string());
            let moves = match read.upstream_drift {
                Some((_, behind)) if behind > 0 => {
                    format!(
                        "moves {branch} forward {}, as of the last fetch",
                        commits(behind)
                    )
                }
                _ => "nothing new as of the last fetch".to_string(),
            };
            (format!("pull {branch}"), Some(moves))
        }
        GitAction::Rebase => {
            commands.extend(fetch_base);
            commands.push(format!("git rebase {base}"));
            notes.push("on a conflict: git rebase --abort, and nothing changes".to_string());
            if read.pushed > 0 {
                let upstream = read.upstream.as_deref().unwrap_or("its upstream");
                warnings.push(format!(
                    "{upstream} has {} of these commits: the next push needs --force-with-lease",
                    read.pushed
                ));
            }
            let moves = match read.base_drift {
                Some((ahead, behind)) if behind > 0 => {
                    format!("replays {} onto {behind} new", commits(ahead))
                }
                _ => "nothing new as of the last fetch".to_string(),
            };
            (format!("rebase {branch} onto {base}"), Some(moves))
        }
        GitAction::Merge => {
            commands.extend(fetch_base);
            commands.push(format!("git merge --no-edit {base}"));
            notes.push("on a conflict: git merge --abort, and nothing changes".to_string());
            let moves = match read.base_drift {
                Some((_, behind)) if behind > 0 => {
                    format!("brings in {} with one merge commit", commits(behind))
                }
                _ => "nothing new as of the last fetch".to_string(),
            };
            (format!("merge {base} into {branch}"), Some(moves))
        }
        GitAction::Abort => {
            let noun = read.in_progress.map_or("rebase", InProgress::noun);
            commands.push(format!("git {noun} --abort"));
            notes.push("back to exactly where it was before it started".to_string());
            warnings.push(format!(
                "conflicts resolved so far in that {noun} are dropped"
            ));
            (format!("abort the {noun} in {branch}"), None)
        }
    };
    Plan {
        title,
        commands,
        moves,
        notes,
        warnings,
    }
}
