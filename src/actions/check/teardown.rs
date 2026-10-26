//! Taking a check's worktree down: at the end of every check, and at the
//! start of the next one for what a killed check left behind. A
//! namespaced check's database and slot go with it, through the same
//! drop `rm` makes.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::paths::{CHECK_WORKTREE, PandoPaths, resolve_for_compare};
use crate::state;
use crate::worktree;

use super::super::lifecycle::stop;
use super::super::namespaced::drop_namespaces;
use super::run::Narration;

/// What a check that did not finish left behind: its worktree, its
/// record, or both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftoverCheck {
    /// Where its worktree is, or was.
    pub path: PathBuf,
    /// Whether that is not where this project's checks go now — a check
    /// made before `worktrees_dir` changed. The sweep leaves such a one to
    /// the developer: it removes nothing it cannot prove is the check's.
    pub elsewhere: bool,
    /// What its record says pando made for it in the main checkout's
    /// servers — `database shop__pando_check`, `redis slot 3` — which the
    /// sweep drops with it.
    pub namespaces: Vec<String>,
}

/// A worktree or a record of a check that is not running: what a check
/// killed before its teardown leaves, or one whose teardown failed. `None`
/// while a check runs, whose worktree is not left over but in use.
///
/// Read-only, for `doctor` as much as for the sweep: the lock is asked
/// about only when its file exists, as the setup state asks it.
pub fn leftover_check(paths: &PandoPaths, config: &Config) -> Option<LeftoverCheck> {
    if crate::setup::check_running(paths) {
        return None;
    }
    leftover(paths, config)
}

/// [`leftover_check`], for a caller that holds the check lock itself.
fn leftover(paths: &PandoPaths, config: &Config) -> Option<LeftoverCheck> {
    let expected = config.check_worktree_path(paths);
    let record = state::load(&paths.state_file())
        .ok()
        .and_then(|mut store| store.worktrees.remove(CHECK_WORKTREE));
    let namespaces: Vec<String> = record
        .iter()
        .flat_map(|record| &record.namespaces)
        .map(crate::namespace::describe)
        .collect();
    let listed = worktree::discover(&paths.project)
        .unwrap_or_default()
        .into_iter()
        .find(|w| worktree::is_check(&w.name));
    if let Some(listed) = listed {
        return Some(LeftoverCheck {
            elsewhere: !same_path(&listed.path, &expected),
            path: listed.path,
            namespaces,
        });
    }
    if let Some(record) = record {
        return Some(LeftoverCheck {
            elsewhere: !same_path(&record.path, &expected),
            path: record.path,
            namespaces,
        });
    }
    is_real_dir(&expected).then_some(LeftoverCheck {
        path: expected,
        elsewhere: false,
        namespaces,
    })
}

/// Sweeps what a check that did not finish left behind, before a new one
/// starts: its processes stopped, its worktree removed and pruned, its
/// record dropped. The caller holds the check lock, so nothing it touches
/// is in use. Says that it does as a step, always: a server that was left
/// running is being stopped.
///
/// A leftover somewhere other than where this project's checks go is not
/// touched, and not a reason to refuse: the new check has its own place.
pub(super) fn sweep_leftover_check(
    paths: &PandoPaths,
    config: &Config,
    say: &Narration<'_>,
) -> Result<()> {
    let progress = say.step;
    let Some(found) = leftover(paths, config) else {
        return Ok(());
    };
    if found.elsewhere {
        progress(&format!(
            "a check that did not finish left a worktree at {}, which is not where this \
             project's checks go now — left alone; `git worktree remove --force {}` removes it",
            found.path.display(),
            found.path.display()
        ));
        return Ok(());
    }
    progress(&match found.namespaces.is_empty() {
        true => {
            "a check that did not finish left its worktree behind — sweeping it first".to_string()
        }
        false => format!(
            "a check that did not finish left its worktree and its {} behind — sweeping them \
             first",
            found.namespaces.join(", ")
        ),
    });
    for line in tear_down(paths, config, say.detail).context("sweeping what the last check left")? {
        progress(&line);
    }
    Ok(())
}

/// Stops everything the check's worktree runs, removes the worktree and
/// prunes git's entry for it, drops the namespaces its record says pando
/// made for it, and drops the record. Its logs stay: they are what a
/// failed check is read from afterwards.
///
/// Touches one path, the configured check directory, canonicalised and
/// compared before anything is removed. The record goes only once its
/// processes are stopped: a record dropped over a live group is a process
/// nothing can find again. Its namespaces go the way `rm` drops a
/// worktree's, once the worktree is gone, each through the guard
/// `namespace::may_drop` holds every drop to — never the main checkout's,
/// never one pando has no record of making for the check. Returns what
/// could not be dropped, each with the command that drops it by hand.
pub(super) fn tear_down(
    paths: &PandoPaths,
    config: &Config,
    progress: &dyn Fn(&str),
) -> Result<Vec<String>> {
    stop(paths, CHECK_WORKTREE, None, progress).context("stopping the test worktree")?;
    remove_worktree(paths, config)?;
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    let left = drop_namespaces(paths, &store, CHECK_WORKTREE, progress);
    if store.worktrees.remove(CHECK_WORKTREE).is_some() {
        state::save(&paths.state_file(), &store)?;
    }
    Ok(left)
}

/// `git worktree remove --force` on the check's worktree, then `git
/// worktree prune` — of each repository's checkout in it, for a multi-repo
/// project's set. A worktree git will not remove — a submodule checked
/// out in it, a lock — is deleted by path instead, which is safe for this
/// one directory alone: it is pando's, at a name no branch can have. So is
/// what is left of a set once its checkouts are gone.
fn remove_worktree(paths: &PandoPaths, config: &Config) -> Result<()> {
    let expected = config.check_worktree_path(paths);
    let roots = paths.project.checkouts_in(paths.root());
    let listed = worktree::discover(&paths.project)?
        .into_iter()
        .find(|w| worktree::is_check(&w.name) && same_path(&w.path, &expected));
    if listed.is_some() {
        for (root, checkout) in roots.iter().zip(paths.project.checkouts_in(&expected)) {
            let _ = crate::project::git(
                root,
                [
                    "worktree",
                    "remove",
                    "--force",
                    checkout.to_str().context("the check's path is not UTF-8")?,
                ],
            );
        }
    }
    // A directory git no longer lists: a check killed between the checkout
    // and its entry, or one whose entry was pruned by hand.
    if is_real_dir(&expected) {
        std::fs::remove_dir_all(&expected)
            .with_context(|| format!("remove {}", expected.display()))?;
    }
    for root in &roots {
        let pruned =
            crate::project::git(root, ["worktree", "prune"]).is_ok_and(|out| out.status.success());
        if !pruned {
            bail!("`git worktree prune` failed in {}", root.display());
        }
    }
    if is_real_dir(&expected) {
        bail!("{} is still there", expected.display());
    }
    Ok(())
}

/// A directory of its own at `path`, named as a check's is, and not a
/// link to one somewhere else: the only thing the teardown deletes by path.
fn is_real_dir(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == CHECK_WORKTREE)
        && std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

fn same_path(a: &Path, b: &Path) -> bool {
    resolve_for_compare(a) == resolve_for_compare(b)
}
