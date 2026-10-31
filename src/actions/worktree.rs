//! Worktree creation and removal: `new`, `rm`, `ls`, `path`, the files
//! provisioned into a new worktree, and the git questions asked along the
//! way.

use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{self, Config, ProvisionMode};
use crate::paths::PandoPaths;
use crate::project::ProjectRef;
use crate::state::{self, WorktreeRecord};
use crate::worktree::{self, PrInfo, Worktree};

use super::checkout::CheckoutPlan;
use super::hooks::{HookContext, run_hooks};
use super::lifecycle::{
    STOP_GRACE, StopOutcome, mark_stop, settle_stop, stop_recorded, stop_recorded_with,
    sweep_orphaned_groups, unmark_stop, wait_for,
};
use super::namespaced::drop_namespaces;
use super::refresh::refresh;
use super::services::{clear_native_sockets, compose_projects, docker_down_for, remove_containers};
use crate::remedy;

/// What `new` says after the branch when the worktree was made and only
/// its install step failed. The worktree is kept, so a caller may treat
/// it as made.
pub const CREATED_BUT_INSTALL_FAILED: &str = "was created, but its install step failed";

/// What `new` says as its install starts, once it has written every ref it
/// writes: the branch, each checkout's entry, the record — and let go of
/// the state lock. What follows is the install and the create hooks, which
/// take minutes and write no ref, so the TUI lets the other git writers
/// run from this line on.
pub const INSTALLING: &str = "installing";

/// What `new` says when another command recorded the worktree while git
/// checked it out. The worktree is kept here too, so a caller may treat
/// it as made.
pub const KEPT_OVER_RACED_RECORD: &str = "the worktree and that record are kept";

pub use crate::worktree::sanitize_branch_to_dir;

/// Which of the three shapes `new` is in. Mirrors the decision git itself
/// would otherwise make implicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CreateSource {
    /// The branch already exists locally; check it out as-is.
    Local,
    /// The branch exists only on `origin`; track it.
    Remote,
    /// A new branch, forked from `base` and deliberately not tracking it.
    Fork { base: String },
    /// No branch at all: `commit` checked out detached. Only `pando check`
    /// makes one, for a worktree it removes again.
    Detached { commit: String },
}

/// Creates a worktree for `branch`, returning its directory name.
///
/// A rejected `new` leaves no directory, no branch, and no state behind.
/// Most refusals happen before git is asked to do anything; the ones that
/// cannot — the worktree's own gitignore has the last word on provisioning,
/// and it can only be read once the worktree exists — unwind what was
/// created and say so in the error. The one refusal that keeps the
/// worktree is over a record of it another command wrote while git
/// checked it out: that command may be running something in it. That
/// record is marked as pando's, and the error says what the worktree did
/// not get.
pub fn new(
    paths: &PandoPaths,
    config: &Config,
    branch: &str,
    base: Option<&str>,
    progress: &dyn Fn(&str),
) -> Result<String> {
    let root = paths.root().to_path_buf();
    validate_branch_name(&root, branch)?;

    let dir_name = sanitize_branch_to_dir(branch);
    // A set of this branch short of a repository's checkout — lost, or of
    // a repository that joined the project since — is given what it lacks
    // rather than refused: removing it to make it again would take the
    // other repositories' work with it.
    if let Some(set) = incomplete_set(paths, &dir_name)? {
        return complete_set(paths, config, &set, branch, base, progress);
    }
    let target = refuse_taken_target(paths, config, &dir_name)?;
    // `new` only: `pando check` never clones, so a `clone` path is no
    // reason to refuse one.
    // A path the main checkout does not have is nothing to clone, and git
    // cannot say whether a missing `node_modules` is one `node_modules/`
    // ignores.
    for rel in config
        .project
        .clones()
        .iter()
        .filter(|rel| is_present(&root.join(rel)))
    {
        ensure_ignored(&paths.project, &root, &as_ignore_query(&root, rel), "clone")?;
    }
    let sources = sources_for(paths, config, branch, base, &|_| true, progress)?;
    create(
        paths, config, &dir_name, &target, branch, &sources, progress,
    )
}

/// Each repository's part of a new worktree of `branch`, for the
/// repositories `wanted` takes: what it checks out, and from where.
fn sources_for(
    paths: &PandoPaths,
    config: &Config,
    branch: &str,
    base: Option<&str>,
    wanted: &dyn Fn(Option<&str>) -> bool,
    progress: &dyn Fn(&str),
) -> Result<Vec<RepoSource>> {
    let root = paths.root();
    let mut wanted_checkouts = Vec::new();
    for (repo, checkout) in paths.project.repo_checkouts_in(root) {
        if !wanted(repo) {
            continue;
        }
        // No base, no `--base` and no commit to name: the one way past it
        // for one repository of several is to leave it out.
        if let Some(repo) = repo
            && commit_of(&checkout, "HEAD").is_none()
        {
            bail!(
                "the repository {repo} has no commit yet, so a set has nothing of it to check \
                 out — commit to it, or leave it out of the project with `[repos] members` in \
                 {}",
                paths.config_file().display()
            );
        }
        let requested = base
            .or_else(|| config.base_for(repo, Some(branch)))
            .map(str::to_string);
        wanted_checkouts.push((repo.map(str::to_string), checkout, requested));
    }
    // Every repository's lookup at once, each on a thread of its own: a
    // lookup can be a `git fetch`, and one repository after another a set
    // of five waited on five. What each says is said here as it comes,
    // as its own, or a fetch per repository reads as one line said twice.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let resolved: Vec<Result<CreateSource>> = std::thread::scope(|scope| {
        let resolving: Vec<_> = wanted_checkouts
            .iter()
            .map(|(repo, checkout, requested)| {
                let tx = tx.clone();
                scope.spawn(move || {
                    let said = |line: &str| {
                        let _ = tx.send(match repo {
                            Some(repo) => format!("{repo}: {line}"),
                            None => line.to_string(),
                        });
                    };
                    resolve_create_source(checkout, branch, requested.clone(), &said)
                })
            })
            .collect();
        drop(tx);
        for line in rx {
            progress(&line);
        }
        resolving
            .into_iter()
            .map(|thread| {
                thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect()
    });
    wanted_checkouts
        .into_iter()
        .zip(resolved)
        .map(|((repo, root, _), source)| {
            Ok(RepoSource {
                source: in_repo(source, repo.as_deref())?,
                repo,
                root,
            })
        })
        .collect()
}

/// The set called `name` when it lacks a checkout of one of the project's
/// repositories, or has one whose directory is gone. `None` for a project
/// that is one repository, and when there is no such set.
fn incomplete_set(paths: &PandoPaths, name: &str) -> Result<Option<Worktree>> {
    if !paths.project.is_multi_repo() {
        return Ok(None);
    }
    let found = worktree::discover_all(&paths.project)?;
    Ok(found
        .worktrees
        .into_iter()
        .find(|w| w.name == name && w.is_set() && !missing_checkouts(paths, w).is_empty()))
}

/// The project's repositories `set` has no checkout of that it can use:
/// none at all, or one git still lists whose directory is gone.
fn missing_checkouts<'a>(paths: &'a PandoPaths, set: &Worktree) -> Vec<&'a str> {
    paths
        .project
        .repos
        .iter()
        .filter(|repo| {
            set.checkout(repo)
                .is_none_or(|c| c.prunable || !c.path.is_dir())
        })
        .map(String::as_str)
        .collect()
}

/// `new` of a set pando made that lacks some repositories' checkouts:
/// those are checked out into it on its branch as `new` would, each
/// forking from its repository's base where the branch is new there, and
/// provisioned and cloned like a new set's; the install runs again, over
/// the whole set. The checkouts it has are left as they are, and so is a
/// set that runs: its processes would see their dependencies change under
/// them.
fn complete_set(
    paths: &PandoPaths,
    config: &Config,
    set: &Worktree,
    branch: &str,
    base: Option<&str>,
    progress: &dyn Fn(&str),
) -> Result<String> {
    let shown = set.display_name();
    let store = state::load(&paths.state_file())?;
    let record = store
        .worktrees
        .get(&set.name)
        .filter(|record| record.created_by_pando && record_is_for(record, set));
    let Some(record) = record else {
        bail!(
            "{shown} is a set pando did not make, so it adds no checkout to it — `pando rm {shown} \
             --yes` removes it, and then `pando new {branch}` makes it whole"
        );
    };
    if !record.processes.is_empty() {
        bail!(
            "{shown} is running — `pando stop {shown}`, and then `pando new {branch}` gives it the \
             checkouts it lacks"
        );
    }
    let on = set
        .repos
        .iter()
        .filter(|c| !c.prunable)
        .filter_map(|c| c.branch.as_deref())
        .find(|b| *b != branch);
    if let Some(other) = on {
        bail!(
            "{shown} is a set on {other}, not {branch}: `pando new` adds to a set on its own branch"
        );
    }
    let missing = missing_checkouts(paths, set);
    clear_gone_checkouts(paths, set, &missing, progress)?;
    let sources = sources_for(
        paths,
        config,
        branch,
        base,
        &|repo| repo.is_some_and(|repo| missing.contains(&repo)),
        progress,
    )?;
    let claimed = claim_checkouts(paths, set, &sources)?;
    let worktrees_dir = config.worktrees_dir(paths);
    let mut added: Vec<(&RepoSource, PathBuf)> = Vec::new();
    for source in &sources {
        let at = source.target(&set.path);
        let made = add_checkout(source, &at, branch, config, &worktrees_dir, progress).and_then(
            |(plan, failed)| match (failed, &plan) {
                (Some(failed), _) => Err(failed),
                (None, CheckoutPlan::CopyOnWrite(from)) => {
                    super::checkout::fill(&at, from, progress).map(|_| ())
                }
                (None, CheckoutPlan::Git) => Ok(()),
            },
        );
        match made {
            Ok(()) => added.push((source, at)),
            Err(e) => {
                // The path was claimed by this call, so a checkout git
                // kept there is this call's to undo, with the rest.
                if is_registered(&source.root, &at) {
                    added.push((source, at));
                }
                let err = unwind_checkouts(&added, &set.path, branch, e);
                for dir in &claimed {
                    let _ = std::fs::remove_dir(dir);
                }
                return Err(err);
            }
        }
    }
    // Only into the checkouts just made: the others have what they had.
    let names: Vec<&str> = sources.iter().filter_map(|s| s.repo.as_deref()).collect();
    let theirs = |rel: &&String| {
        paths
            .project
            .repo_of(rel)
            .is_some_and(|repo| names.contains(&repo))
    };
    for rel in config.project.provision_paths().iter().filter(theirs) {
        provision_path(paths, config, &set.path, rel, progress)
            .map_err(|e| unwind_checkouts(&added, &set.path, branch, e))?;
    }
    record_bases(paths, set, &sources)?;
    let clones: Vec<String> = config
        .project
        .clones()
        .iter()
        .filter(theirs)
        .cloned()
        .collect();
    clone_ignored_paths(
        &paths.project,
        &clones,
        config.project.install.as_deref(),
        &set.path,
        progress,
    );
    progress(&format!("added {} to {shown}", names.join(", ")));
    if has_install_step(config) {
        progress(INSTALLING);
    }
    let no_ports: BTreeMap<String, u16> = BTreeMap::new();
    let no_services: BTreeMap<String, String> = BTreeMap::new();
    let ctx = HookContext {
        name: &set.name,
        branch: Some(branch),
        worktree: &set.path,
        ports: &no_ports,
        service_env: &no_services,
        own_data: false,
        not_own: None,
    };
    run_hooks(paths, config, config::HookPoint::Create, &ctx, progress)
        .with_context(|| format!("{branch} {CREATED_BUT_INSTALL_FAILED}"))?;
    Ok(set.name.clone())
}

/// Clears the entries git still lists for `missing` checkouts of `set`
/// whose directories are gone, as `rm` clears one: nothing is there to
/// lose, and `git worktree add` refuses a path git still lists.
fn clear_gone_checkouts(
    paths: &PandoPaths,
    set: &Worktree,
    missing: &[&str],
    progress: &dyn Fn(&str),
) -> Result<()> {
    for checkout in set
        .repos
        .iter()
        .filter(|c| missing.contains(&c.name.as_str()))
    {
        let root = paths.root().join(&checkout.name);
        let path = checkout
            .path
            .to_str()
            .context("worktree path is not utf-8")?;
        if !git_succeeds(&root, &["worktree", "remove", "--force", path]) {
            bail!(
                "git still lists {}'s checkout at {path}, and would not let it go — `git -C {} \
                 worktree prune` clears it",
                checkout.name,
                crate::process::shell_word(&root.display().to_string())
            );
        }
        // git hands the new checkout the entry the lost one had, so a copy
        // of it moved somewhere else would share the new one's index.
        progress(&format!(
            "{} had no checkout at {path} any more; if it was moved rather than deleted, delete \
             the moved copy, which git would take for the new one",
            checkout.name
        ));
    }
    Ok(())
}

/// Claims the path of each checkout `sources` will add to `set`, under the
/// state lock and before git runs, as `new` claims a worktree's: a second
/// `new` of the same set finds them taken and refuses, so the one that
/// loses never undoes what the winner made.
fn claim_checkouts(
    paths: &PandoPaths,
    set: &Worktree,
    sources: &[RepoSource],
) -> Result<Vec<PathBuf>> {
    let _lock = state::lock(&paths.lock_file())?;
    // A set being removed gets no checkout added to it.
    refuse_while_removed(
        paths,
        &state::load(&paths.state_file())?,
        &set.name,
        &set.display_name(),
        "nothing was added to it",
    )?;
    let mut claimed = Vec::new();
    for source in sources {
        let at = source.target(&set.path);
        if let Err(e) = std::fs::create_dir(&at) {
            for dir in &claimed {
                let _ = std::fs::remove_dir(dir);
            }
            return Err(match e.kind() {
                std::io::ErrorKind::AlreadyExists => anyhow::anyhow!(
                    "{} already exists — another `pando new` may be giving {} its checkout",
                    at.display(),
                    set.display_name()
                ),
                _ => anyhow::Error::from(e).context(format!("create {}", at.display())),
            });
        }
        claimed.push(at);
    }
    Ok(claimed)
}

/// Records the base each forked checkout of `sources` went onto, in
/// `set`'s record.
fn record_bases(paths: &PandoPaths, set: &Worktree, sources: &[RepoSource]) -> Result<()> {
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    let Some(record) = store.worktrees.get_mut(&set.name) else {
        return Ok(());
    };
    for source in sources {
        if let CreateSource::Fork { base } = &source.source {
            record.set_base(source.repo.as_deref(), base);
        }
    }
    state::save(&paths.state_file(), &store)
}

/// One repository's part of a new worktree: which repository (none for a
/// project that is one), its main checkout, and what is checked out.
#[derive(Debug, Clone)]
struct RepoSource {
    repo: Option<String>,
    root: PathBuf,
    source: CreateSource,
}

impl RepoSource {
    /// Where its checkout goes in the worktree at `target`.
    fn target(&self, target: &Path) -> PathBuf {
        ProjectRef::checkout_of(target, self.repo.as_deref())
    }
}

/// `result`, an answer about one repository of a project, saying which
/// when the project has several.
fn in_repo<T>(result: Result<T>, repo: Option<&str>) -> Result<T> {
    match repo {
        Some(repo) => result.with_context(|| format!("in the repository {repo}")),
        None => result,
    }
}

/// Commits as a message names them: seven characters each, one beside
/// the next.
pub(super) fn short_commits(commits: &[String]) -> String {
    commits
        .iter()
        .map(|commit| commit.chars().take(7).collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Creates the worktree `pando check` tests in, at [`crate::paths::CHECK_WORKTREE`]
/// under the worktrees directory, with `commit` checked out detached:
/// through `new`'s own steps — the refusals, the provisioning, the state
/// record and the install — less the branch, which a check never makes,
/// and the fetch, which the commit it was handed does not need.
///
/// `commits` is one commit per repository, in the project's order: the one
/// commit of a project that is one repository.
pub(super) fn new_detached(
    paths: &PandoPaths,
    config: &Config,
    commits: &[String],
    progress: &dyn Fn(&str),
) -> Result<String> {
    let dir_name = crate::paths::CHECK_WORKTREE.to_string();
    let target = refuse_taken_target(paths, config, &dir_name)?;
    let checkouts = paths.project.repo_checkouts_in(paths.root());
    if checkouts.len() != commits.len() {
        bail!(
            "{} commits for {} repositories",
            commits.len(),
            checkouts.len()
        );
    }
    let short = short_commits(commits);
    let sources: Vec<RepoSource> = checkouts
        .into_iter()
        .zip(commits)
        .map(|((repo, root), commit)| RepoSource {
            repo: repo.map(str::to_string),
            root,
            source: CreateSource::Detached {
                commit: commit.clone(),
            },
        })
        .collect();
    create(
        paths, config, &dir_name, &target, &short, &sources, progress,
    )
}

/// Where a worktree called `dir_name` goes, once nothing is in the way:
/// no worktree git lists under that name, no provisioned path the main
/// checkout does not ignore, and nothing on disk there yet. Asks git
/// nothing that writes.
fn refuse_taken_target(paths: &PandoPaths, config: &Config, dir_name: &str) -> Result<PathBuf> {
    let root = paths.root();
    let existing = worktree::discover_all(&paths.project)?;
    if existing.main.name == dir_name {
        bail!("{dir_name:?} is the main checkout's directory name");
    }
    if let Some(found) = existing.worktrees.iter().find(|w| w.name == dir_name) {
        // A prunable entry is one whose directory is gone, and "already
        // exists at" a path that is not there says nothing about the one
        // command that clears it.
        if found.prunable {
            bail!(
                "git still lists a worktree named {dir_name:?} at {}, but a prunable one ({}) — \
                 `pando rm {}` clears that entry, and then this can run again",
                found.path.display(),
                found
                    .prunable_reason
                    .as_deref()
                    .unwrap_or("its directory is gone"),
                found.display_name()
            );
        }
        // One with no record is either made by hand or a `new` that was
        // stopped before it recorded it — a TUI quit abandons one in
        // flight. Either way the way past it is the same command.
        let recorded = state::load(&paths.state_file())
            .is_ok_and(|store| store.worktrees.contains_key(dir_name));
        if !recorded {
            bail!(
                "a worktree named {dir_name:?} already exists at {}, and pando has no record of \
                 it — if a `pando new` was stopped while making it, `pando rm {}` removes it",
                found.path.display(),
                found.display_name()
            );
        }
        bail!(
            "a worktree named {dir_name:?} already exists at {}",
            found.path.display()
        );
    }

    // The cheap early refusal: a project that needs an untracked,
    // non-ignored file is refused with the path named, not fixed up. The
    // worktree gets asked again once it exists, because it may have a
    // different `.gitignore` checked out.
    for rel in config.project.provision_paths() {
        ensure_ignored(&paths.project, root, rel, "provision")?;
    }

    let target = config.worktrees_dir(paths).join(dir_name);
    if target.exists() {
        // `new` claims the target as an empty directory before git runs; a
        // `new` stopped in between leaves it. Never taken back here: a
        // `new` running now holds it the same way.
        let empty = std::fs::read_dir(&target).is_ok_and(|mut entries| entries.next().is_none());
        if empty {
            bail!(
                "{} already exists, empty and not a worktree — a `pando new` stopped before \
                 git ran leaves one; if none is running, `rmdir {}` and this can run again",
                target.display(),
                crate::process::shell_word(&target.display().to_string())
            );
        }
        bail!("{} already exists", target.display());
    }
    Ok(target)
}

/// `new` from the checkout on: git makes the worktree at `target` from
/// `sources`, and it is provisioned, recorded and installed. `branch` is
/// what the messages call it — the short commit for a detached one.
///
/// `sources` is one per repository: the worktree itself for a project
/// that is one repository, and for several a set, the directory `target`
/// holding each repository's checkout under the repository's name.
fn create(
    paths: &PandoPaths,
    config: &Config,
    dir_name: &str,
    target: &Path,
    branch: &str,
    sources: &[RepoSource],
    progress: &dyn Fn(&str),
) -> Result<String> {
    let Some(first) = sources.first() else {
        bail!("{} has no repository to check out", paths.root().display());
    };
    let root = paths.root().to_path_buf();
    let dir_name = dir_name.to_string();
    let target = target.to_path_buf();
    let worktrees_dir = config.worktrees_dir(paths);

    paths.ensure_home()?;
    // How to run the project with pando, above the worktree an agent will
    // be opened in, so it finds it there with nothing saved. Here rather
    // than where config is written: a project whose config is committed,
    // or was set up by an older pando, never writes config again, and
    // every worktree pando makes, `check`'s own included, comes through
    // here. A convenience, so a `new` is never failed over it. The device
    // line comes from what config runs: by now it runs what `new` will.
    let device_note = crate::setup::device_note(config, &[]);
    let _ = crate::setup::write_memory_files(paths, device_note);
    // State is locked and read *before* git creates anything: a state file
    // pando cannot use has to refuse while there is still nothing to undo.
    // Not held through the checkout, which runs the repository's filters
    // and hooks — an LFS download takes minutes, and every `ls` and every
    // TUI worker would wait on it.
    std::fs::create_dir_all(&worktrees_dir)
        .with_context(|| format!("create {}", worktrees_dir.display()))?;
    // Before the directory is claimed: each checkout's path is asked again
    // where git is given it, by which time there is a claim to undo.
    target.to_str().context("worktree path is not utf-8")?;
    let left = {
        let _lock = state::lock(&paths.lock_file())?;
        let mut store = state::load(&paths.state_file())?;
        let loaded = store.clone();
        // Under the lock, so the porcelain read cannot race a concurrent
        // `new` whose record is already saved but whose worktree this
        // process has not seen yet. The sweep signals only groups whose
        // leader is dead: a record of a worktree removed outside pando while
        // its processes still run is dropped with them unsignalled. A known
        // gap, left because a path comparison that went wrong here would
        // kill a healthy server.
        for notice in sweep_orphaned_groups(&mut store)? {
            progress(&notice);
        }
        drop_stale_worktree_records(&mut store, &paths.project);
        if store != loaded {
            state::save(&paths.state_file(), &store)?;
        }
        // A worktree of this name is not gone until its removal drops its
        // record: made now, the new one would be checked out where the old
        // one's tree is still being deleted, and lose its record to that
        // removal.
        refuse_while_removed(paths, &store, &dir_name, &dir_name, "nothing was made")?;
        // The target is claimed here, under the lock, as an empty directory
        // `git worktree add` accepts: a second `new` of the same branch
        // finds it taken and refuses before git runs, so the one that
        // loses never unwinds what the winner made.
        match std::fs::create_dir(&target) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!("{} already exists", target.display())
            }
            Err(e) => return Err(e).with_context(|| format!("create {}", target.display())),
        }
        // What the record under this name is as the lock goes: none, unless
        // a porcelain that could not be read kept a stale one.
        store.worktrees.get(&dir_name).cloned()
    };

    // Each repository's checkout in turn, into the directory claimed
    // above. A failure in any is undone with every one made before it,
    // below, under the lock.
    let mut added: Vec<(&RepoSource, PathBuf)> = Vec::new();
    let mut filled = None;
    for source in sources {
        let at = source.target(&target);
        match add_checkout(source, &at, branch, config, &worktrees_dir, progress) {
            Ok((plan, failed_add)) => {
                added.push((source, at.clone()));
                // Still part of the checkout, so still without the lock:
                // git writes the files a copy-on-write checkout could not
                // clone. Its result waits for the lock, like every other
                // failure here: a failed fill is undone with the lock
                // held, and never under a record a `start` wrote.
                filled = match (failed_add, &plan) {
                    (Some(failed), _) => Some(failed),
                    (None, CheckoutPlan::CopyOnWrite(from)) => {
                        super::checkout::fill(&at, from, progress).err()
                    }
                    (None, CheckoutPlan::Git) => None,
                }
                // Told to stop during the checkout: what it made goes, as
                // `git worktree add` takes its own half-made worktree
                // down.
                .or_else(|| {
                    super::checkout::told_to_stop()
                        .then(|| anyhow::anyhow!(super::checkout::INTERRUPTED))
                });
            }
            // Nothing of this checkout is left: with nothing made before
            // it, the claimed directory goes too and that is the whole
            // undo, with no lock to take.
            Err(e) if added.is_empty() => {
                let _ = std::fs::remove_dir(&target);
                return Err(e);
            }
            Err(e) => filled = Some(e),
        }
        if filled.is_some() {
            break;
        }
    }

    // Past this point the worktree exists, so every failure has something to
    // undo before it is reported. The state is read again under the lock:
    // other commands have saved theirs while git checked out. And the undo
    // runs with the lock still held: let go first, a `start` waiting on it
    // could record a dev server in the worktree the undo then removes, and
    // the next sweep drops that record as stale with the server running.
    let undo = |e| unwind_checkouts(&added, &target, branch, e);
    let lock = state::lock(&paths.lock_file()).map_err(undo)?;
    let mut store = state::load(&paths.state_file()).map_err(undo)?;
    // A `start` from another terminal can find the worktree while git is
    // still checking it out, and record its dev server and ports under
    // this name. Written over, they drop out of pando's state while they
    // run; unwound, the worktree goes from under them. So neither: the
    // worktree is kept, and so is what that command recorded in it.
    if store.worktrees.get(&dir_name) != left.as_ref() {
        let refused =
            refuse_over_raced_record(paths, config, &mut store, &dir_name, branch, &target);
        return Err(match filled {
            Some(e) => anyhow::anyhow!("{refused:#}; its checkout did not finish either: {e:#}"),
            None => refused,
        });
    }
    if let Some(e) = filled {
        return Err(undo(e));
    }
    if !config.project.provision_paths().is_empty() {
        progress("provisioning");
    }
    provision_worktree_files(paths, config, &target, progress).map_err(undo)?;
    // The clones themselves come after the lock, but the check that allows
    // them is made here, where a refusal still unwinds: the worktree's own
    // gitignore has the last word, and nothing else writes it meanwhile.
    let clones = clones_for(config, &first.source);
    for rel in clones.iter().filter(|rel| is_present(&root.join(rel))) {
        ensure_ignored(
            &paths.project,
            &target,
            &as_ignore_query(&root, rel),
            "clone",
        )
        .map_err(undo)?;
    }
    let canonical = std::fs::canonicalize(&target).unwrap_or_else(|_| target.clone());
    let mut record = WorktreeRecord::new(canonical, true);
    for source in sources {
        if let CreateSource::Fork { base } = &source.source {
            record.set_base(source.repo.as_deref(), base);
        }
    }
    store.worktrees.insert(dir_name.clone(), record);
    state::save(&paths.state_file(), &store).map_err(undo)?;
    // The lock goes before the install runs: `npm ci` takes minutes, and
    // holding the state lock through it would stall every `ls` and freeze
    // the TUI's tick.
    drop(lock);

    // A failed install keeps the worktree. The branch is checked out, the
    // files are provisioned, and the next `start` tries the install again —
    // so the error is worth an exit code, but not an unwind.
    //
    // No ports yet: they are allocated at `start`, so a create hook that
    // names one fails here by name rather than silently rendering the
    // wrong number. Almost none do; the install step never does.
    // Said only when there is something to install: a project with no
    // install command and no create hook used to print it anyway.
    clone_ignored_paths(
        &paths.project,
        clones,
        config.project.install.as_deref(),
        &target,
        progress,
    );
    let installs = has_install_step(config);
    for (_, at) in &added {
        for line in uninitialised_submodules(at) {
            progress(&line);
        }
    }
    if installs {
        progress(INSTALLING);
    }
    let no_ports: BTreeMap<String, u16> = BTreeMap::new();
    let no_services: BTreeMap<String, String> = BTreeMap::new();
    let ctx = HookContext {
        name: &dir_name,
        branch: match first.source {
            CreateSource::Detached { .. } => None,
            _ => Some(branch),
        },
        worktree: &target,
        ports: &no_ports,
        service_env: &no_services,
        own_data: false,
        not_own: None,
    };
    run_hooks(paths, config, config::HookPoint::Create, &ctx, progress)
        .with_context(|| format!("{branch} {CREATED_BUT_INSTALL_FAILED}"))?;
    Ok(dir_name)
}

/// `git worktree add` of one repository's checkout at `at`, copy-on-write
/// where the filesystem clones: the plan it was made with, and the error
/// of an add git kept the worktree after — its `post-checkout` hook
/// failed — for the caller to unwind with the rest.
///
/// An error is an add that left nothing: git removes a worktree whose
/// checkout failed, and the branch `-b` made for it is deleted here.
fn add_checkout(
    source: &RepoSource,
    at: &Path,
    branch: &str,
    config: &Config,
    worktrees_dir: &Path,
    progress: &dyn Fn(&str),
) -> Result<(CheckoutPlan, Option<anyhow::Error>)> {
    let root = &source.root;
    let at_str = at.to_str().context("worktree path is not utf-8")?;
    let checkout = super::checkout::plan(root, config, worktrees_dir, progress);
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(root).args(["worktree", "add"]);
    if matches!(checkout, CheckoutPlan::CopyOnWrite(_)) {
        cmd.arg("--no-checkout");
    }
    match &source.source {
        CreateSource::Local => {
            cmd.args([at_str, branch]);
        }
        CreateSource::Detached { commit } => {
            cmd.args(["--detach", at_str, commit]);
        }
        CreateSource::Remote => {
            cmd.args(["--track", "-b", branch, at_str, &format!("origin/{branch}")]);
        }
        CreateSource::Fork { base } => {
            // --no-track, or the new branch's upstream becomes the base and a
            // later `git pull` merges the base into the feature branch.
            cmd.args(["--no-track", "-b", branch, at_str, base]);
        }
    }
    // What a failed add may delete: a branch `-b` made in this call, and
    // only while it is still where `-b` put it.
    let made = match &source.source {
        CreateSource::Fork { base } => Some(base.clone()),
        CreateSource::Remote => Some(format!("origin/{branch}")),
        _ => None,
    }
    .filter(|_| !ref_exists(root, &format!("refs/heads/{branch}")))
    .and_then(|start| commit_of(root, &start));
    progress(&match &source.repo {
        Some(repo) => format!("checking out {branch} in {repo}"),
        None => format!("checking out {branch}"),
    });
    // Captured, not inherited: `git worktree add` narrates on stdout and
    // stderr, which would paint over the TUI's alternate screen. And never
    // prompting, like the fetches: the checkout runs the LFS filter, whose
    // download can ask for a password — over the TUI, whose keys then went
    // nowhere, and for ever.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    // Again while another git holds the config or a ref a moment — the
    // upstream `--track` writes, beside an install's `husky install` — once
    // the failed add is undone: git kept the branch `-b` made.
    let out = retrying_git_locks(
        || cmd.output(),
        |_| {
            !is_registered(root, at)
                && made
                    .as_deref()
                    .is_none_or(|start| delete_made_branch(root, branch, start) != Some(false))
        },
    )
    .context("spawn git worktree add")?;
    // git removes a worktree whose checkout failed, but keeps the branch
    // `-b` made for it; and it keeps the worktree itself when only its
    // `post-checkout` hook failed. A worktree still there is the caller's
    // to unwind, like any failure after the add.
    if out.status.success() {
        return Ok((checkout, None));
    }
    let failed = anyhow::anyhow!("git worktree add failed: {}", git_failure_reason(&out));
    let failed = match &source.repo {
        Some(repo) => failed.context(format!("in the repository {repo}")),
        None => failed,
    };
    if !is_registered(root, at) {
        return Err(drop_made_branch(root, branch, made.as_deref(), failed));
    }
    Ok((checkout, Some(failed)))
}

/// [`unwind_new`] of every checkout `new` made, the last first; for a set,
/// its directory after them once they are gone. The error says what was
/// undone in each.
fn unwind_checkouts(
    added: &[(&RepoSource, PathBuf)],
    target: &Path,
    branch: &str,
    err: anyhow::Error,
) -> anyhow::Error {
    let mut err = err;
    for (source, at) in added.iter().rev() {
        err = unwind_new(
            &source.root,
            source.repo.as_deref(),
            at,
            branch,
            &source.source,
            err,
        );
    }
    if added.iter().any(|(source, _)| source.repo.is_some()) {
        // Empty once every checkout is gone, and kept when one is not: a
        // checkout git still lists is not pando's to delete by hand.
        let _ = std::fs::remove_dir(target);
    }
    err
}

/// Whether `new` has an install step to run: the project's install
/// command, or a hook after create.
fn has_install_step(config: &Config) -> bool {
    config
        .project
        .install
        .as_deref()
        .is_some_and(|cmd| !cmd.trim().is_empty())
        || config
            .hooks
            .iter()
            .any(|hook| hook.after == config::HookPoint::Create)
}

/// The paths `clone` names, for a worktree `new` makes. None for the
/// worktree `pando check` makes: a check proves the install builds the
/// dependencies from nothing, and a cloned tree could hide one that no
/// longer does.
fn clones_for<'a>(config: &'a Config, source: &CreateSource) -> &'a [String] {
    match source {
        CreateSource::Detached { .. } => &[],
        _ => config.project.clones(),
    }
}

/// `rel` as `git check-ignore` must be asked about it: with a trailing
/// slash when the main checkout has a directory there. `node_modules/` in
/// a gitignore matches only a directory, and in a new worktree, where
/// nothing is there yet, git cannot tell that `node_modules` would be one.
fn as_ignore_query(root: &Path, rel: &str) -> String {
    let rel = rel.trim_end_matches('/');
    match root.join(rel).symlink_metadata().is_ok_and(|m| m.is_dir()) {
        true => format!("{rel}/"),
        false => rel.to_string(),
    }
}

/// Clones each `clone` path from the main checkout into a new worktree,
/// copy-on-write, so the install that follows only fixes what differs.
///
/// Whatever cannot be cloned is left to the install, with a line saying
/// so — never copied in full, which would cost the disk this exists to
/// save. Nothing is written over a path the worktree already has, nor
/// into a directory the branch does not have: a branch made before
/// `apps/web` was has nothing that reads `apps/web/node_modules`.
///
/// Not cloned, each with a line: a path the install deletes before it
/// installs (`npm ci` and `node_modules`); a Python virtualenv, whose
/// scripts name the main checkout's interpreter by its absolute path; and
/// a path the worktree's gitignore stopped ignoring since the check made
/// under the lock. A cloned directory with a link out of the worktree is
/// removed again: the worktree would run the main checkout's files
/// through it, which is the sharing a worktree exists to avoid. npm's
/// workspace links are relative and resolve inside the worktree, so they
/// pass.
fn clone_ignored_paths(
    project: &ProjectRef,
    clones: &[String],
    install: Option<&str>,
    worktree: &Path,
    progress: &dyn Fn(&str),
) {
    let root = project.root.as_path();
    let cleared = install.and_then(crate::catalog::package_managers::cleared_by);
    for rel in clones {
        let src = root.join(rel);
        let dst = worktree.join(rel);
        let Ok(meta) = src.symlink_metadata() else {
            continue;
        };
        if is_present(&dst) || !dst.parent().is_some_and(Path::is_dir) {
            continue;
        }
        if let Some(dir) = cleared
            && Path::new(rel).file_name() == Some(std::ffi::OsStr::new(dir))
        {
            progress(&format!(
                "not cloning {rel}: the install deletes it before it installs"
            ));
            continue;
        }
        if meta.is_dir() && src.join("pyvenv.cfg").exists() {
            progress(&format!(
                "not cloning {rel}: a virtualenv names the main checkout's paths in its scripts, \
                 so the install builds it"
            ));
            continue;
        }
        // Invariant 1, immediately before the write, as provisioning asks.
        if let Err(e) = ensure_ignored(project, worktree, &as_ignore_query(root, rel), "clone") {
            progress(&format!("not cloning {rel}: {e:#}"));
            continue;
        }
        let cloned = match (meta.is_dir(), meta.is_file()) {
            (true, _) => crate::platform::cow::clone_tree(&src, &dst),
            (_, true) => crate::platform::cow::clone_file(&src, &dst),
            _ => {
                progress(&format!(
                    "not cloning {rel}: in the main checkout it is neither a file nor a directory"
                ));
                continue;
            }
        };
        match cloned {
            Ok(()) => {}
            Err(e) if crate::platform::cow::is_unsupported(&e) => {
                progress(&format!(
                    "this filesystem cannot clone {rel}, so the install builds it"
                ));
                continue;
            }
            Err(e) => {
                progress(&format!(
                    "could not clone {rel} ({e}), so the install builds it"
                ));
                continue;
            }
        }
        if meta.is_dir() {
            match crate::platform::cow::link_out_of(&dst, worktree, root) {
                Ok(None) => {}
                Ok(Some(link)) => {
                    if let Err(e) = crate::platform::cow::remove_tree(&dst) {
                        progress(&format!(
                            "the clone of {rel} links out of the worktree and could not be \
                             removed ({e}); remove {} before the install runs",
                            dst.display()
                        ));
                        continue;
                    }
                    progress(&format!(
                        "not cloning {rel}: {} links out of the worktree, so the install builds \
                         it",
                        link.strip_prefix(worktree).unwrap_or(&link).display()
                    ));
                    continue;
                }
                Err(e) => {
                    let removed = crate::platform::cow::remove_tree(&dst).is_ok();
                    progress(&format!(
                        "could not read the clone of {rel} ({e}), so the install builds it{}",
                        match removed {
                            true => String::new(),
                            false => format!("; remove {} first", dst.display()),
                        }
                    ));
                    continue;
                }
            }
        }
        progress(&format!(
            "cloned {rel} from the main checkout (copy-on-write)"
        ));
    }
}

/// The refusal `new` gives over a record another command wrote while git
/// checked the worktree out, having marked that record as pando's.
///
/// It did: the worktree is the one `new` just created. Left as a start
/// writes it, the record is an adopted worktree's, and `rm` and the TUI
/// would treat it as not pando's to remove. Only a record of this
/// directory is marked — one under the same name for another path is not
/// this worktree's. The error names what `new` did not give the worktree,
/// and the commands that do.
fn refuse_over_raced_record(
    paths: &PandoPaths,
    config: &Config,
    store: &mut state::State,
    dir_name: &str,
    branch: &str,
    target: &Path,
) -> anyhow::Error {
    let head = format!(
        "{branch} was checked out at {}, but another pando command recorded {dir_name:?} \
         while git was checking it out — {KEPT_OVER_RACED_RECORD}",
        target.display()
    );
    if let Some(record) = store.worktrees.get_mut(dir_name)
        && !record.created_by_pando
        && crate::paths::resolve_for_compare(&record.path)
            == crate::paths::resolve_for_compare(target)
    {
        record.created_by_pando = true;
        if let Err(e) = state::save(&paths.state_file(), store) {
            return e.context(head);
        }
    }
    // What provisioning would have written: the paths not there yet that
    // have something to come from.
    let missing: Vec<&str> = config
        .project
        .provision_paths()
        .iter()
        .filter(|rel| !target.join(rel).exists() && provision_source(paths, config, rel).is_some())
        .chain(
            config.project.clones().iter().filter(|rel| {
                !is_present(&target.join(rel)) && is_present(&paths.root().join(rel))
            }),
        )
        .map(String::as_str)
        .collect();
    let lacks = match (missing.is_empty(), has_install_step(config)) {
        (false, true) => format!(
            "its provisioned files ({}) or its install step",
            missing.join(", ")
        ),
        (false, false) => format!("its provisioned files ({})", missing.join(", ")),
        (true, true) => "its install step".to_string(),
        (true, false) => {
            return anyhow::anyhow!(
                "{head}, and there was nothing to provision or install into it"
            );
        }
    };
    anyhow::anyhow!(
        "{head}, but it did not get {lacks}; `pando rm {branch}` and then `pando new {branch}` \
         make it again with nothing missing"
    )
}

/// What `new` says about submodules it left empty, one line, or nothing.
///
/// `git worktree add` checks out the superproject only, so every submodule
/// of a new worktree is an empty directory — and a build that needs one
/// fails far from the cause. pando does not fill them: `submodule update`
/// clones into `.git`, beyond what `worktree add` records, which Invariant
/// 1 leaves to the developer. So it says which, and the command that does.
pub(super) fn uninitialised_submodules(worktree: &Path) -> Vec<String> {
    if !worktree.join(".gitmodules").is_file() {
        return Vec::new();
    }
    let Ok(out) = crate::project::git(worktree, ["submodule", "status"]) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    // `-<sha> <path>` is a submodule that was never initialised here.
    let empty: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('-'))
        .filter_map(|rest| rest.split_whitespace().nth(1))
        .map(str::to_string)
        .collect();
    if empty.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "submodules left empty: {} — pando does not fill them; `git -C {} submodule update \
         --init --recursive` does, if this worktree needs them",
        empty.join(", "),
        worktree.display()
    )]
}

/// The first thing `git worktree remove` would refuse over, when there is
/// one: a tracked file that differs, or an untracked file the project does
/// not ignore. `None` means git has no objection.
///
/// A prunable worktree has no directory left to look in, and clearing its
/// entry removes nothing, so there is nothing to refuse.
///
/// A git that could not answer — timed out, failed, never ran — is an
/// error, never "clean": `rm` acts on this answer by stopping processes
/// and removing volumes, and only then finds out from `git worktree
/// remove` that the tree was dirty after all.
///
/// `--no-optional-locks`, as `worktree::is_dirty` has: a plain status
/// refreshes a stale index, which takes `index.lock` and rewrites the
/// index inside `.git` — of a worktree `rm --yes` may not own, and one a
/// refused `rm` leaves in place.
fn dirty_entry(worktree: &Worktree) -> Result<Option<String>> {
    if worktree.prunable {
        return Ok(None);
    }
    let out = crate::project::git(
        &worktree.path,
        ["--no-optional-locks", "status", "--porcelain"],
    )?;
    if !out.status.success() {
        bail!(
            "git status failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|entry| entry.trim().to_string()))
}

/// The submodule `git worktree remove` would refuse over, when there is
/// one: without `--force`, git removes no worktree with a submodule checked
/// out in it, or with a `modules` directory in its git directory, however
/// clean its status is. `new` itself says how to fill a worktree's
/// submodules, so a clean worktree that has them is an ordinary one.
///
/// A prunable worktree has nothing to refuse, and a git that could not
/// answer is an error, both as in [`dirty_entry`].
fn submodule_in(worktree: &Worktree) -> Result<Option<String>> {
    if worktree.prunable {
        return Ok(None);
    }
    let answer = |args: &[&str]| -> Result<String> {
        let out = crate::project::git(&worktree.path, args)?;
        if !out.status.success() {
            bail!(
                "git {} failed: {}",
                args[0],
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };
    // `<mode> <object> <stage>\t<path>`, and a submodule's mode is 160000.
    // git counts one as checked out when its directory has a `.git`.
    let index = answer(&["ls-files", "--stage", "-z"])?;
    let checked_out = index
        .split('\0')
        .filter_map(|entry| entry.split_once('\t'))
        .filter(|(meta, _)| meta.starts_with("160000 "))
        .map(|(_, path)| path)
        .find(|path| worktree.path.join(path).join(".git").exists());
    if let Some(path) = checked_out {
        return Ok(Some(path.to_string()));
    }
    let modules =
        PathBuf::from(answer(&["rev-parse", "--absolute-git-dir"])?.trim()).join("modules");
    Ok(modules.is_dir().then(|| modules.display().to_string()))
}

/// The git checkouts among what a set's directory holds beside its own,
/// [`beside_checkouts`]: never pando's to remove, forced or not.
pub fn foreign_checkouts(target: &Worktree) -> Vec<String> {
    beside_checkouts(target)
        .into_iter()
        .filter(|name| target.path.join(name).join(".git").exists())
        .collect()
}

/// What `rm` refuses over even when forced, before it stops or removes
/// anything: a git checkout of its own in a set's directory beside the
/// checkouts the set is made of — a repository cloned there, or the
/// checkout of one the project no longer counts — which is never pando's
/// to delete, and which would keep the set's directory there after it.
fn refuse_foreign_checkouts(shown: &str, target: &Worktree) -> Result<()> {
    let foreign = foreign_checkouts(target);
    if foreign.is_empty() {
        return Ok(());
    }
    bail!(
        "{shown} holds {}, a git checkout pando does not count as one of the set's, in {} — \
         nothing was stopped or removed; move it out of the set, or remove it with git, and \
         then this can run again",
        foreign.join(", "),
        target.path.display()
    )
}

/// What `rm` refuses over unless forced, before it stops or removes
/// anything: git's own refusals of each checkout, and for a set anything
/// in its directory beside its checkouts.
fn refuse_unforced(shown: &str, target: &Worktree) -> Result<()> {
    for checkout in target.checkouts() {
        refuse_what_git_would(shown, target, checkout)?;
    }
    // A set's directory may hold more than its checkouts: notes, a
    // repository cloned into it by hand, or the checkout of one the
    // project no longer counts. None of it is git's to judge, and all of
    // it would go with the set.
    let strangers = beside_checkouts(target);
    if !strangers.is_empty() {
        bail!(
            "{shown} holds {} beside its repositories' checkouts, in {} — nothing was stopped \
             or removed; move it out, or {}",
            strangers.join(", "),
            target.path.display(),
            remedy::REMOVE_BESIDE
        );
    }
    Ok(())
}

/// `git worktree remove` of each checkout of `target`, in its own
/// repository: the worktree itself, or each of a set's. A removal git
/// refuses says what was and was not removed — what was running has been
/// stopped by then, and saved so, and that cannot be taken back.
fn remove_checkouts(
    paths: &PandoPaths,
    target: &Worktree,
    force: bool,
    stopped: &StopOutcome,
) -> Result<()> {
    let mut removed: Vec<&str> = Vec::new();
    for checkout in target.checkouts() {
        let repo = target.is_set().then_some(checkout.name.as_str());
        let root = ProjectRef::checkout_of(paths.root(), repo);
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&root).args(["worktree", "remove"]);
        if force {
            cmd.arg("--force");
        }
        cmd.arg(
            checkout
                .path
                .to_str()
                .context("worktree path is not utf-8")?,
        );
        let out = cmd.output().context("spawn git worktree remove")?;
        if out.status.success() {
            removed.push(&checkout.name);
            continue;
        }
        // The message says what actually happened.
        let reason = match repo {
            Some(repo) => format!("in {repo}: {}", git_failure_reason(&out)),
            None => git_failure_reason(&out),
        };
        let kept = match removed.is_empty() {
            true => "the worktree was kept".to_string(),
            false => format!(
                "the checkouts of {} were removed, the rest kept",
                removed.join(", ")
            ),
        };
        if matches!(stopped, StopOutcome::Stopped(_)) {
            bail!(
                "git worktree remove failed: {reason} — what was running was stopped, and any \
                 public URL closed; {kept}"
            );
        }
        match removed.is_empty() {
            true => bail!("git worktree remove failed: {reason}"),
            false => bail!("git worktree remove failed: {reason} — {kept}"),
        }
    }
    Ok(())
}

/// git's two refusals of `git worktree remove` without `--force`, asked of
/// one checkout of `target` — the worktree itself, or one of a set's —
/// before `rm` stops or removes anything.
fn refuse_what_git_would(shown: &str, target: &Worktree, checkout: &Worktree) -> Result<()> {
    let shown = match target.is_set() {
        true => format!("{shown} ({})", checkout.name),
        false => shown.to_string(),
    };
    match submodule_in(checkout) {
        Ok(None) => {}
        Ok(Some(what)) => bail!(
            "{shown} has submodules in it ({what}), and git removes such a worktree only \
             when forced — nothing was stopped or removed; {}",
            remedy::DISCARD_CHANGES
        ),
        Err(e) => bail!(
            "could not tell whether {shown} has submodules in it: {e:#} — nothing was \
             stopped or removed; check it with `git submodule status`, or {}",
            remedy::DISCARD_CHANGES
        ),
    }
    match dirty_entry(checkout) {
        Ok(None) => Ok(()),
        Ok(Some(entry)) => bail!(
            "{shown} contains modified or untracked files ({entry}) — commit or remove \
             them, or {}",
            remedy::DISCARD_CHANGES
        ),
        // Before anything destructive: not knowing is not "clean".
        Err(e) => bail!(
            "could not tell whether {shown} has changes: {e:#} — nothing was stopped or \
             removed; check it with `git status`, or {}",
            remedy::DISCARD_CHANGES
        ),
    }
}

/// What a set's directory holds besides the checkouts git lists in it, by
/// name, sorted. Empty for a worktree of one repository.
pub fn beside_checkouts(target: &Worktree) -> Vec<String> {
    if !target.is_set() {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(&target.path) else {
        return Vec::new();
    };
    let mut strangers: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| !target.repos.iter().any(|checkout| checkout.name == *name))
        .collect();
    strangers.sort();
    strangers
}

/// Removes a set's directory once its checkouts are gone, saying what
/// went with it and what kept it. What is left in one pando made is no
/// repository's, and goes as `--force` takes a checkout's ignored files —
/// except a git checkout of its own, which `rm` refuses before it gets
/// here and which is kept should one have appeared since. One pando did
/// not make goes only while empty.
fn clear_set_dir(target: &Worktree, ours: bool) -> Vec<String> {
    let mut said = Vec::new();
    let mut gone = Vec::new();
    if ours {
        for name in beside_checkouts(target) {
            let path = target.path.join(&name);
            if path.join(".git").exists() {
                continue;
            }
            let removed = match path.symlink_metadata().is_ok_and(|m| m.is_dir()) {
                true => std::fs::remove_dir_all(&path),
                false => std::fs::remove_file(&path),
            };
            if removed.is_ok() {
                gone.push(name);
            }
        }
    }
    if !gone.is_empty() {
        said.push(format!("removed {} with the set", gone.join(", ")));
    }
    match std::fs::remove_dir(&target.path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            let left = beside_checkouts(target);
            said.push(format!(
                "kept {}: {} in it {} not pando's to remove",
                target.path.display(),
                left.join(", "),
                if left.len() == 1 { "is" } else { "are" }
            ));
        }
    }
    said
}

/// Undoes a `new` that failed after `git worktree add`. The worktree goes;
/// so does the branch, but only when pando created it in this same call —
/// a branch that existed before is the user's work, not pando's to delete.
///
/// The returned error is the original one plus what was actually undone, so
/// a partial unwind never reads as a clean one.
///
/// `repo` names the repository of a multi-repo project's set it was one
/// checkout of, so each of a set's unwinds says whose it was.
fn unwind_new(
    root: &Path,
    repo: Option<&str>,
    target: &Path,
    branch: &str,
    source: &CreateSource,
    err: anyhow::Error,
) -> anyhow::Error {
    let Some(target_str) = target.to_str() else {
        return err;
    };
    let (partial, git) = match repo {
        Some(repo) => (
            format!("the partial checkout of {repo}"),
            format!(
                "git -C {}",
                crate::process::shell_word(&root.display().to_string())
            ),
        ),
        None => ("the partial worktree".to_string(), "git".to_string()),
    };
    if !git_succeeds(root, &["worktree", "remove", "--force", target_str]) {
        return anyhow::anyhow!(
            "{err:#} — {partial} at {} could not be removed; remove it with `{git} worktree \
             remove --force` and delete the branch if it is new",
            target.display()
        );
    }
    if matches!(source, CreateSource::Local | CreateSource::Detached { .. }) {
        return anyhow::anyhow!("{err:#} — {partial} was removed");
    }
    if !git_succeeds(root, &["branch", "-D", branch]) {
        return anyhow::anyhow!(
            "{err:#} — {partial} was removed, but the new branch {branch} is still there; \
             delete it with `{git} branch -D {branch}`"
        );
    }
    anyhow::anyhow!("{err:#} — {partial} and the new branch {branch} were removed")
}

/// Whether git lists a worktree at `target`.
fn is_registered(root: &Path, target: &Path) -> bool {
    worktree::porcelain_paths(root).is_ok_and(|listed| {
        listed.iter().any(|p| {
            crate::paths::resolve_for_compare(p) == crate::paths::resolve_for_compare(target)
        })
    })
}

/// Deletes the branch a failed `git worktree add -b` made, when this call
/// made it: `made` is the commit it started at, known only when the branch
/// did not exist before. `update-ref -d` with that commit deletes it only
/// while it is still there, so a branch anything else has moved is kept.
fn drop_made_branch(
    root: &Path,
    branch: &str,
    made: Option<&str>,
    err: anyhow::Error,
) -> anyhow::Error {
    let Some(start) = made else {
        return err;
    };
    match delete_made_branch(root, branch, start) {
        None => err,
        Some(true) => anyhow::anyhow!("{err:#} — the new branch {branch} was removed"),
        Some(false) => anyhow::anyhow!(
            "{err:#} — the new branch {branch} is still there; delete it with `git branch -D \
             {branch}`"
        ),
    }
}

/// Deletes `branch` while it is still at `start`, the commit a failed
/// `git worktree add -b` made it at: `None` when there is no such branch,
/// else whether it went.
fn delete_made_branch(root: &Path, branch: &str, start: &str) -> Option<bool> {
    let refname = format!("refs/heads/{branch}");
    ref_exists(root, &refname).then(|| git_succeeds(root, &["update-ref", "-d", &refname, start]))
}

/// How long a git step that writes the repository's shared config or refs
/// tries again while another git holds their lock. git holds one for
/// milliseconds; one held past this is one a git that crashed left, which
/// git's own message then names.
const GIT_LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_secs(3);

/// Runs a git step that writes the repository's shared config or refs
/// again while it fails only because another git held a lock it needed
/// ([`git_lock_busy`]), for up to [`GIT_LOCK_PATIENCE`]. git waits for
/// none of them, so two steps that would each have passed alone fail side
/// by side: a create beside another's install, whose `husky install` sets
/// `core.hooksPath`.
///
/// `undo` runs after each such failure, before the step is tried again,
/// and says whether it left nothing behind: only then is it. The failure
/// it gave up on is the one returned.
pub(super) fn retrying_git_locks(
    mut run: impl FnMut() -> std::io::Result<std::process::Output>,
    mut undo: impl FnMut(&std::process::Output) -> bool,
) -> std::io::Result<std::process::Output> {
    let began = std::time::Instant::now();
    let mut pause = std::time::Duration::from_millis(25);
    loop {
        let out = run()?;
        if out.status.success()
            || !git_lock_busy(&out)
            || began.elapsed() >= GIT_LOCK_PATIENCE
            || !undo(&out)
        {
            return Ok(out);
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(std::time::Duration::from_millis(400));
    }
}

/// Whether git failed for a lock another git held a moment, in git's own
/// words: the config's, a ref's or an index's lock file already there, or
/// a ref another git moved under it. Never a lock git could not take for
/// any other reason.
pub(super) fn git_lock_busy(out: &std::process::Output) -> bool {
    String::from_utf8_lossy(&out.stderr).lines().any(|line| {
        let held = line.contains("File exists");
        (line.contains("could not lock config file") && held)
            || (line.contains("Unable to create '") && line.contains(".lock'") && held)
            || (line.contains("cannot lock ref") && line.contains("but expected"))
    })
}

/// The commit `rev` names, in full.
fn commit_of(root: &Path, rev: &str) -> Option<String> {
    let out = crate::project::git(
        root,
        [
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_succeeds(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Removes a worktree, its logs, and its data directory. The branch is kept;
/// deleting it is a separate decision.
///
/// A worktree git no longer lists — its directory deleted and `git
/// worktree prune` run — that pando still has a record for is removed the
/// same way, less the git removal: what the record names is taken down
/// and the record forgotten. Otherwise the record would wait for an
/// unrelated `new` to drop it, and nothing would take down what it named.
///
/// In the shape `start` has, so removals of several worktrees run side by
/// side and nothing else waits behind one: under the state lock, every
/// refusal is decided, what runs is stopped and the record is marked as
/// being removed ([`state::Removal`]); without it, the containers, the
/// tree and the namespaces go — the tree alone is minutes for a big
/// `node_modules`; under it again, the record goes. The questions put to
/// git and Docker are asked before the lock, and the ones state answers
/// asked again under it. A removal stopped half way leaves its record
/// marked, and the next `rm` of the name finishes it.
///
/// What runs is stopped as `stop` stops it: marked and signalled under the
/// lock, waited for without it, settled under it again. A dev server that
/// ignores SIGTERM takes its whole grace before its SIGKILL, and several
/// removals of running worktrees wait out theirs together.
pub fn rm(
    paths: &PandoPaths,
    name: &str,
    yes: bool,
    force: bool,
    progress: &dyn Fn(&str),
) -> Result<()> {
    rm_with(
        paths,
        name,
        yes,
        force,
        |pgid| crate::process::stop(pgid, STOP_GRACE),
        progress,
    )
}

/// [`rm`] with the signal injected, so a test can give a stop the grace a
/// stubborn dev server takes without spending it.
pub(super) fn rm_with(
    paths: &PandoPaths,
    name: &str,
    yes: bool,
    force: bool,
    stop: impl Fn(crate::process::Group) -> Result<()> + Sync,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let discovery = worktree::discover_all(&paths.project)?;
    if discovery.main.name == name {
        bail!(
            "{name:?} is the main checkout — pando runs it, but never removes it; `pando stop \
             {name}` stops what runs there"
        );
    }
    let target = discovery.worktrees.iter().find(|w| w.name == name);
    // Read without the lock, and before the home is made: this only asks
    // whether there is anything to remove at all.
    if target.is_none()
        && !state::load(&paths.state_file())
            .is_ok_and(|store| forgotten_record(&discovery, &store, name).is_some())
    {
        bail!("no worktree named {name:?}");
    }

    // Locked worktrees are always refused. pando never unlocks, and never
    // passes --force twice to talk git out of it.
    if let Some(target) = target
        && target.locked
    {
        let reason = target
            .lock_reason
            .as_deref()
            .unwrap_or("no reason recorded");
        bail!(
            "{} is locked ({reason}) — unlock it with `git worktree unlock` first",
            target.display_name()
        );
    }

    // `rm` can be the first command a project ever sees (an adopted
    // worktree), and taking the lock creates the project directory.
    paths.ensure_home()?;
    let shown = target.map_or_else(|| name.to_string(), Worktree::display_name);
    // A removal of this one already under way, first: git's questions
    // below, put to a tree being deleted, would fail with less to say.
    let seen = {
        let _lock = state::lock(&paths.lock_file())?;
        let seen = state::load(&paths.state_file())?;
        if let Some(RemovalState::UnderWay(mark)) = removal_of(paths, &seen, name) {
            bail!(being_removed(&shown, &mark, ALREADY_REMOVING));
        }
        seen
    };
    // Every refusal before anything is stopped, in the order they are put
    // again under the lock below, where state answers the ones it can.
    let recorded_at = match target {
        Some(target) => Some(target.path.clone()),
        None => forgotten_record(&discovery, &seen, name).map(|record| record.path.clone()),
    };
    if let Some(at) = &recorded_at {
        refuse_unowned(&seen, target, name, yes, &shown, at)?;
    }

    // git's own refusal is the last one, so it is asked first: stopping the
    // dev server and *then* being told the worktree stays leaves a process
    // that is gone, a worktree that is not, and a record blaming the
    // process for pando's kill. The questions are the two git asks without
    // `--force`, in its order; ignored files never block a removal and do
    // not show here. Not under the lock: they read the tree, which no lock
    // of pando's keeps anyone from writing, and `git status` of a big one
    // takes seconds every other command would wait out.
    if let Some(target) = target {
        refuse_foreign_checkouts(&shown, target)?;
    }
    if !force && let Some(target) = target {
        refuse_unforced(&shown, target)?;
    }

    // And Docker's, for the same reason. `rm` is what takes a worktree's
    // compose volumes with it, and the record naming the project is about
    // to go: removed while the daemon is down, the volumes survive with
    // nothing in pando able to find them again. So that is refused unless
    // forced, before anything is stopped — and forced, it goes ahead and
    // says how to remove them by hand.
    let asked: Vec<String> = seen
        .worktrees
        .get(name)
        .map(compose_projects)
        .unwrap_or_default();
    if !force {
        refuse_docker_down(paths, &shown, &asked)?;
    }

    let mut claim = claim_removal(
        paths, &discovery, target, name, yes, force, &asked, &stop, progress,
    )?;
    if let Err(e) = stop_claimed(paths, &mut claim, &stop) {
        return Err(abandon_removal(paths, claim, e));
    }
    match take_down(paths, &claim, target, force, progress) {
        Ok(()) => finish_removal(paths, claim, progress),
        Err(e) => Err(abandon_removal(paths, claim, e)),
    }
}

/// What `rm` says it did not do when another `rm` is removing the same
/// worktree.
const ALREADY_REMOVING: &str = "nothing was stopped or removed here";

/// Refuses `rm` of a worktree pando did not create, unless `--yes` says
/// to; whether pando created it.
fn refuse_unowned(
    store: &state::State,
    target: Option<&Worktree>,
    name: &str,
    yes: bool,
    shown: &str,
    recorded_at: &Path,
) -> Result<bool> {
    let created_by_pando = store.worktrees.get(name).is_some_and(|r| {
        r.created_by_pando && target.is_none_or(|target| record_is_for(r, target))
    });
    if !created_by_pando && !yes {
        bail!(
            "pando did not create {shown} ({}) — {}",
            recorded_at.display(),
            remedy::REMOVE_ANYWAY
        );
    }
    Ok(created_by_pando)
}

/// Refuses `rm` of a worktree whose compose projects Docker cannot be
/// asked to take down — see the comment where `rm` asks.
fn refuse_docker_down(paths: &PandoPaths, shown: &str, projects: &[String]) -> Result<()> {
    if let Some((project, how)) = docker_down_for(paths, projects) {
        bail!(
            "Docker {how}, so the services of {shown} cannot be removed with it — {}; \
             its data volumes then stay until `docker compose -p {project} down -v`",
            remedy::REMOVE_WITHOUT_DOCKER
        );
    }
    Ok(())
}

/// What `rm` holds from the lock it marked the record under to the one it
/// drops it under.
struct Claim {
    name: String,
    shown: String,
    /// State as it was saved with the mark. Its record of this worktree
    /// names what the slow part takes down; the rest is what the guard on
    /// each namespace drop reads.
    marked: state::State,
    /// Whether the mark made the record: an adopted worktree pando had
    /// none of, which a removal that fails leaves with none again.
    made_record: bool,
    created_by_pando: bool,
    /// The process groups the claim marked as its stop's and signals once
    /// the lock is let go; none when nothing was running.
    planned: Vec<crate::process::Group>,
    stopped: StopOutcome,
    /// The compose projects the record named, taken down before git runs.
    projects: Vec<String>,
    /// The worktree's removal lock, held to the end. Let go any other way
    /// — a crash, a Ctrl-C, a panic on a TUI worker — it says the removal
    /// was interrupted.
    _held: state::StateLock,
}

impl Claim {
    fn record(&self) -> Option<&WorktreeRecord> {
        self.marked.worktrees.get(&self.name)
    }

    /// Whether `store`'s record of this worktree still carries this
    /// removal's mark.
    fn still_ours(&self, store: &state::State) -> bool {
        let mark = self.record().and_then(|record| record.removing.as_ref());
        mark.is_some()
            && store
                .worktrees
                .get(&self.name)
                .is_some_and(|record| record.removing.as_ref() == mark)
    }
}

/// `rm`'s first step, under the state lock: the refusals state answers,
/// asked of it as it is now; then what runs is marked as this removal's
/// stop, the record marked as being removed, and both saved — with the
/// compose projects the record names, so the `rm` that finishes an
/// interrupted removal has them. Nothing is signalled here unless nothing
/// is running, when it is all settled at once.
#[allow(clippy::too_many_arguments)]
fn claim_removal(
    paths: &PandoPaths,
    discovery: &worktree::Discovery,
    target: Option<&Worktree>,
    name: &str,
    yes: bool,
    force: bool,
    asked: &[String],
    stop: &(impl Fn(crate::process::Group) -> Result<()> + Sync),
    progress: &dyn Fn(&str),
) -> Result<Claim> {
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    // Before any record is dropped, whichever worktree it belongs to.
    for notice in sweep_orphaned_groups(&mut store)? {
        progress(&notice);
    }
    // And saved before any of the refusals below: the sweep marked every
    // group it signalled, and a refused `rm` that forgot that signalled
    // them all again on the next mutation.
    state::save(&paths.state_file(), &store)?;
    let (shown, recorded_at) = match target {
        Some(target) => {
            drop_stale_worktree_records(&mut store, &paths.project);
            (target.display_name(), target.path.clone())
        }
        // Not dropped as stale: that record is the one this removes.
        None => match forgotten_record(discovery, &store, name) {
            Some(record) => {
                progress(&format!(
                    "git no longer lists {name} — taking down what pando still has for it"
                ));
                (name.to_string(), record.path.clone())
            }
            None => bail!("no worktree named {name:?}"),
        },
    };
    let interrupted = match removal_of(paths, &store, name) {
        Some(RemovalState::UnderWay(mark)) => {
            bail!(being_removed(&shown, &mark, ALREADY_REMOVING))
        }
        Some(RemovalState::Interrupted(_)) => true,
        None => false,
    };
    let created_by_pando = refuse_unowned(&store, target, name, yes, &shown, &recorded_at)?;
    // Docker again, for a compose project recorded since it was asked.
    if !force {
        let since: Vec<String> = store
            .worktrees
            .get(name)
            .map(compose_projects)
            .unwrap_or_default()
            .into_iter()
            .filter(|project| !asked.contains(project))
            .collect();
        refuse_docker_down(paths, &shown, &since)?;
    }
    // The removal lock, before anything is stopped. Nobody holds it now:
    // `removal_of` said so above, under the lock everything reads the mark
    // under.
    let lock_file = paths.removal_lock_file(name);
    let Some(held) = take_removal_lock(&lock_file)? else {
        bail!("{shown} {BEING_REMOVED} — {ALREADY_REMOVING}");
    };
    if interrupted {
        progress(&format!(
            "a removal of {shown} was interrupted — finishing it"
        ));
    }

    // Whatever is running goes next. A dev server whose working directory
    // has just been deleted is not a process anyone can do anything with,
    // and `rm` removes the record that is the only way to find it again.
    let mut projects: Vec<String> = Vec::new();
    // Said before it happens: `rm` of a running worktree takes its dev
    // server down, and a bare "removed" afterwards hid that it had.
    let running: Vec<String> = store
        .worktrees
        .get(name)
        .map(|record| record.processes.keys().cloned().collect())
        .unwrap_or_default();
    if !running.is_empty() {
        progress(&format!("stopping {}", running.join(", ")));
    }
    // Marked, and signalled and waited for once the lock is let go
    // ([`stop_claimed`]). With nothing to wait on — no process up, only the
    // records a stopped worktree keeps — settled here at once.
    let stopping = mark_stop(&mut store, name, None).and_then(|planned| {
        match planned.is_empty() {
            true => stop_recorded_with(&mut store, name, None, stop, &mut projects),
            false => Ok(StopOutcome::NotRunning),
        }
        .map(|stopped| (planned, stopped))
    });
    let (planned, stopped) = match stopping {
        Ok(stopping) => stopping,
        Err(e) => {
            let _ = std::fs::remove_file(&lock_file);
            return Err(e);
        }
    };

    // And the removal's mark, saved with the stop's.
    let made_record = !store.worktrees.contains_key(name);
    store
        .worktrees
        .entry(name.to_string())
        .or_insert_with(|| WorktreeRecord::new(recorded_at, false))
        .removing = Some(state::Removal {
        owner_pid: std::process::id(),
        since: chrono::Utc::now(),
    });
    state::save(&paths.state_file(), &store)?;
    Ok(Claim {
        name: name.to_string(),
        shown,
        marked: store,
        made_record,
        created_by_pando,
        planned,
        stopped,
        projects,
        _held: held,
    })
}

/// `rm`'s stop of what was running, when its claim marked any: every
/// group signalled and waited for together, with no lock held — a dev
/// server that ignores SIGTERM takes its whole grace, and every other
/// command, and every other removal, used to wait it out too — then the
/// records settled under the lock by what the wait found, as `stop`
/// settles them. A group that would not end fails the removal before
/// anything is removed, its record kept for a retry to signal.
fn stop_claimed(
    paths: &PandoPaths,
    claim: &mut Claim,
    stop: &(impl Fn(crate::process::Group) -> Result<()> + Sync),
) -> Result<()> {
    if claim.planned.is_empty() {
        return Ok(());
    }
    let waited = wait_for(&claim.planned, stop);
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    let before = store.clone();
    let mut projects = Vec::new();
    match settle_stop(
        &mut store,
        &claim.name,
        None,
        &claim.planned,
        &waited,
        stop,
        &mut projects,
    ) {
        Ok(stopped) => {
            state::save(&paths.state_file(), &store)?;
            claim.stopped = stopped;
            claim.projects = projects;
            claim.marked = store;
            Ok(())
        }
        Err(e) => {
            let mut kept = before;
            unmark_stop(&mut kept, &claim.name, &claim.planned);
            state::save(&paths.state_file(), &kept)?;
            Err(e)
        }
    }
}

/// A removal's lock, which nobody holds by the time `rm` takes it. Taken
/// at once almost always: but a process another thread of this pando is
/// spawning holds a copy of every descriptor open at its fork until its
/// exec — a look at this lock among them — so one that reads as taken is
/// asked again, for a moment, before it is believed.
fn take_removal_lock(lock_file: &Path) -> Result<Option<state::StateLock>> {
    for _ in 0..10 {
        if let Some(held) = state::try_lock(lock_file)? {
            return Ok(Some(held));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(None)
}

/// `rm`'s slow part, without the state lock: the containers and their
/// volumes, then git's removal of the tree, then pando's own files of the
/// worktree, then its namespaces. An error is a removal that kept the
/// worktree.
fn take_down(
    paths: &PandoPaths,
    claim: &Claim,
    target: Option<&Worktree>,
    force: bool,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let (name, shown) = (claim.name.as_str(), claim.shown.as_str());
    // The containers, the network, and the volumes, before git is asked to
    // remove anything: the record that names the compose project is about
    // to be dropped, and a `down -v` that failed after the worktree was
    // gone would leave a database nothing could ever find. The mark's save
    // kept the record, so a retry of `rm` has what it needs.
    //
    // Forced past a Docker that cannot be asked, it is not asked again: a
    // wedged daemon would hold `down -v` for the whole teardown deadline
    // and then fail the removal `--force` was passed to get. Said instead,
    // with the command that finishes it.
    if !claim.projects.is_empty() {
        match force
            .then(|| docker_down_for(paths, &claim.projects))
            .flatten()
        {
            Some((_, how)) => {
                for project in &claim.projects {
                    progress(&format!(
                        "Docker {how}, so the services of {project} could not be removed — \
                         their data volumes survive; once Docker is up, `docker compose -p \
                         {project} down -v` removes them"
                    ));
                }
            }
            None => remove_containers(paths, &claim.projects, progress).with_context(|| {
                format!("{shown} was left in place; its services could not be taken down")
            })?,
        }
    }

    // Nothing is unlinked first. Verified against git 2.51: an ignored file
    // does not block `git worktree remove`, and `--force` does not follow a
    // symlink out of the worktree — so unlinking bought nothing, and a
    // removal git then refused (a dirty tree without `--force`) left the
    // worktree alive without the `.env` pando had provisioned for it.
    //
    // Always run, even when the directory is already gone: the same command
    // clears a prunable entry, and only that one. `git worktree prune` is
    // global and would sweep entries pando has no business touching. A
    // worktree git no longer lists has nothing left for it to remove.
    //
    // Two at once, of two worktrees, are safe: each deletes its own tree
    // and its own `.git/worktrees/<id>`, and the one shared write — the
    // rmdir of `.git/worktrees` once it is empty — is one git tolerates
    // failing.
    if let Some(target) = target {
        remove_checkouts(paths, target, force, &claim.stopped)?;
        if target.is_set() {
            for line in clear_set_dir(target, claim.created_by_pando) {
                progress(&line);
            }
        }
    }

    let _ = std::fs::remove_dir_all(paths.logs_dir(name));
    // Every native service's data directory is under this one, so this is
    // also what makes "`rm` wipes the database" true for them.
    let _ = std::fs::remove_dir_all(paths.data_dir(name));
    if let Some(record) = claim.record() {
        clear_native_sockets(paths, name, record);
    }
    // And the compose override, which is regenerated on every isolated
    // start and would otherwise outlive every worktree that ever had one.
    let _ = std::fs::remove_file(paths.compose_override_file(name));
    // Its namespaces last, once the worktree is gone for certain: a
    // removal git refused must not have cost the worktree its database.
    // Each goes through the guard, on the server it was made on; one pando
    // cannot drop is said, with the command that does. Read from state as
    // it was marked: the record is still there, naming them, so no other
    // worktree is given one of them meanwhile.
    drop_namespaces(paths, &claim.marked, name, progress);
    Ok(())
}

/// `rm`'s last step, under the state lock again: the record goes, and
/// with it the mark.
///
/// What a start that raced the removal recorded after the mark — one
/// whose own lock-free part began before it — is taken down first, the
/// way the rest was. Almost always there is nothing.
fn finish_removal(paths: &PandoPaths, claim: Claim, progress: &dyn Fn(&str)) -> Result<()> {
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    if claim.still_ours(&store) {
        take_down_since(paths, &mut store, &claim, progress)?;
        store.worktrees.remove(&claim.name);
        state::save(&paths.state_file(), &store)?;
    }
    let _ = std::fs::remove_file(paths.removal_lock_file(&claim.name));
    Ok(())
}

/// What was recorded for the worktree after its mark: a process or a
/// share a start spawned, stopped; a compose project it named, taken down
/// with its volumes; a namespace it made, dropped through the guard.
fn take_down_since(
    paths: &PandoPaths,
    store: &mut state::State,
    claim: &Claim,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let name = claim.name.as_str();
    let mut projects = Vec::new();
    stop_recorded(store, name, None, &mut projects)?;
    projects.retain(|project| !claim.projects.contains(project));
    remove_containers(paths, &projects, progress)?;
    let dropped = claim
        .record()
        .map(|r| r.namespaces.as_slice())
        .unwrap_or_default();
    let Some(record) = store.worktrees.get(name) else {
        return Ok(());
    };
    let since: Vec<state::NamespaceRecord> = record
        .namespaces
        .iter()
        .filter(|ns| {
            !dropped
                .iter()
                .any(|was| crate::namespace::same_namespace(was, ns))
        })
        .cloned()
        .collect();
    if since.is_empty() {
        return Ok(());
    }
    // Those alone: the rest were dropped, or said, once already.
    let mut only = store.clone();
    if let Some(record) = only.worktrees.get_mut(name) {
        record.namespaces = since;
        record.mode = None;
    }
    drop_namespaces(paths, &only, name, progress);
    Ok(())
}

/// Takes the mark off a worktree whose removal failed and kept it — git
/// refused, or its services would not go down — so it reads as what it
/// is: a worktree, there, that a start or the next `rm` can act on. The
/// error is the removal's, and says so when the mark stayed.
fn abandon_removal(paths: &PandoPaths, claim: Claim, err: anyhow::Error) -> anyhow::Error {
    let cleared = (|| -> Result<()> {
        let _lock = state::lock(&paths.lock_file())?;
        let mut store = state::load(&paths.state_file())?;
        if claim.still_ours(&store)
            && let Some(record) = store.worktrees.get_mut(&claim.name)
        {
            record.removing = None;
            if claim.made_record && *record == WorktreeRecord::new(record.path.clone(), false) {
                store.worktrees.remove(&claim.name);
            }
            state::save(&paths.state_file(), &store)?;
        }
        let _ = std::fs::remove_file(paths.removal_lock_file(&claim.name));
        Ok(())
    })();
    match cleared {
        Ok(()) => err,
        Err(e) => anyhow::anyhow!(
            "{err:#} — and it is still marked as being removed ({e:#}); `pando rm {}` finishes \
             the removal",
            claim.shown
        ),
    }
}

/// Where a removal of a worktree stands, for one a removal has begun on,
/// as its record's mark and its removal lock say. Each carries the mark:
/// which pando began it, and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemovalState {
    /// An `rm` is at it now. Everything else asked of the worktree is
    /// refused, and the record goes when it is done.
    UnderWay(state::Removal),
    /// One began and stopped — a Ctrl-C, a crash, a TUI quit — leaving
    /// the record for the next `rm` of the name to finish, containers
    /// and namespaces included. A start, a share and a `new` of the name
    /// are refused until then.
    Interrupted(state::Removal),
}

/// Every worktree of the project a removal has begun on, by name, and
/// where each stands: the one rule anything outside `rm` reads it by.
///
/// Asked under the state lock, which asking needs: finding out whether a
/// removal holds its lock means taking that lock for a moment, and done
/// outside the state lock, the moment could fall on an `rm` claiming it,
/// which would then refuse as if another were under way
/// ([`state::is_held`]). Never waited for, as no read waits: while
/// another pando holds the state lock, the answer is the state as it
/// stands with nothing asked of any removal's lock, and every mark in it
/// reads as under way — something holds the lock right now — until a read
/// that finds it free. For a caller that holds no lock — the TUI's
/// refresh worker, after `refresh` — and so never for one that does.
/// Nothing is made, and nothing is asked, where there is no state yet.
pub fn removals(paths: &PandoPaths) -> Result<BTreeMap<String, RemovalState>> {
    if !paths.state_file().exists() {
        return Ok(BTreeMap::new());
    }
    let Some(_lock) = state::try_lock(&paths.lock_file())? else {
        let store = state::load(&paths.state_file())?;
        return Ok(store
            .worktrees
            .into_iter()
            .filter_map(|(name, record)| Some((name, RemovalState::UnderWay(record.removing?))))
            .collect());
    };
    let store = state::load(&paths.state_file())?;
    Ok(store
        .worktrees
        .keys()
        .filter_map(|name| Some((name.clone(), removal_of(paths, &store, name)?)))
        .collect())
}

/// Where a removal of `name` stands, if one has begun. Asked under the
/// state lock: see [`state::is_held`].
pub(super) fn removal_of(
    paths: &PandoPaths,
    store: &state::State,
    name: &str,
) -> Option<RemovalState> {
    let mark = store.worktrees.get(name)?.removing.clone()?;
    Some(match state::is_held(&paths.removal_lock_file(name)) {
        true => RemovalState::UnderWay(mark),
        false => RemovalState::Interrupted(mark),
    })
}

/// The refusal of anything asked of a worktree an `rm` is removing.
fn being_removed(shown: &str, mark: &state::Removal, nothing: &str) -> String {
    format!(
        "{shown} {BEING_REMOVED} (pid {}) — {nothing}",
        mark.owner_pid
    )
}

/// The words every refusal of a worktree an `rm` is removing has in it.
pub const BEING_REMOVED: &str = "is being removed";

/// Refuses what would run in, or make, a worktree a removal has begun on:
/// a start, a share, a `new` of the name. `nothing` says what was not
/// done. Asked under the state lock.
pub(super) fn refuse_while_removed(
    paths: &PandoPaths,
    store: &state::State,
    name: &str,
    shown: &str,
    nothing: &str,
) -> Result<()> {
    match removal_of(paths, store, name) {
        None => Ok(()),
        Some(RemovalState::UnderWay(mark)) => bail!(being_removed(shown, &mark, nothing)),
        Some(RemovalState::Interrupted(_)) => bail!(
            "a removal of {shown} was interrupted — {nothing}; `pando rm {shown}` finishes it"
        ),
    }
}

/// [`refuse_while_removed`] under a moment of the state lock, for a
/// command about to do something slow before it takes the lock itself.
/// Nothing to ask, and no home made, where there is no state yet.
pub(super) fn refuse_while_removed_now(
    paths: &PandoPaths,
    name: &str,
    shown: &str,
    nothing: &str,
) -> Result<()> {
    if !paths.state_file().exists() {
        return Ok(());
    }
    let _lock = state::lock(&paths.lock_file())?;
    let store = state::load(&paths.state_file())?;
    refuse_while_removed(paths, &store, name, shown, nothing)
}

/// A checkout pando can run: a worktree, or the main checkout itself.
#[derive(Debug, Clone)]
pub(super) struct Checkout {
    pub(super) worktree: Worktree,
    /// The main checkout: the developer's own, set up by them. pando runs
    /// its processes and nothing else there — see [`MAIN_RUNS_ONLY`].
    pub(super) main: bool,
}

/// What a start of the main checkout says it does, once. Invariant 1 has
/// no exception for it: an install, a hook or a probe runs a command in
/// the repository, and what that command writes is not pando's to know.
pub const MAIN_RUNS_ONLY: &str =
    "the main checkout: pando runs its processes and nothing else — no install, no hooks";

/// The checkout called `name` — a managed worktree, or the main checkout
/// by its directory name — or an error naming what is there.
pub(super) fn find_checkout(paths: &PandoPaths, name: &str) -> Result<Checkout> {
    let discovery = worktree::discover_all(&paths.project)?;
    if discovery.main.name == name {
        return Ok(Checkout {
            worktree: discovery.main,
            main: true,
        });
    }
    discovery
        .worktrees
        .into_iter()
        .find(|w| w.name == name)
        .map(|worktree| Checkout {
            worktree,
            main: false,
        })
        .with_context(|| format!("no worktree named {name:?}"))
}

/// [`find_checkout`], for a caller that runs or reads a checkout the same
/// way whichever it is.
pub(super) fn find_worktree(paths: &PandoPaths, name: &str) -> Result<Worktree> {
    Ok(find_checkout(paths, name)?.worktree)
}

/// [`find_checkout`] for a start or restart, which runs things in the
/// checkout: see [`refuse_a_gone_directory`].
pub(super) fn find_live_checkout(paths: &PandoPaths, name: &str) -> Result<Checkout> {
    let checkout = find_checkout(paths, name)?;
    refuse_a_gone_directory(paths.root(), &checkout.worktree)?;
    refuse_a_missing_repo(paths, &checkout.worktree)?;
    Ok(checkout)
}

/// Refuses a set with no checkout of a repository the project has: one
/// that joined the project after the set was made. A process whose `cwd`
/// is that repository's would fail with a bare "does not exist".
fn refuse_a_missing_repo(paths: &PandoPaths, worktree: &Worktree) -> Result<()> {
    let Some(missing) = paths.project.repos.iter().find(|repo| {
        !worktree
            .repos
            .iter()
            .any(|checkout| checkout.name == **repo)
    }) else {
        return Ok(());
    };
    let shown = worktree.display_name();
    // A set pando did not make it adds nothing to: `rm --yes` first.
    let ours = state::load(&paths.state_file()).is_ok_and(|store| {
        store
            .worktrees
            .get(&worktree.name)
            .is_some_and(|record| record.created_by_pando && record_is_for(record, worktree))
    });
    if !ours {
        bail!(
            "{shown} has no checkout of {missing}, and pando did not make it — `pando rm {shown} \
             --yes` removes it, and then `pando new {}` makes it with every repository",
            worktree.new_arg()
        );
    }
    bail!(
        "{shown} has no checkout of {missing} — {} adds it to the set, and leaves the others \
         as they are",
        worktree.checkout_hint(paths.root(), missing)
    )
}

/// Refuses a worktree git still lists but whose directory is gone, naming
/// the `rm` that clears its entry.
///
/// Nothing else would say so. A start made its namespaces, assigned its
/// ports and saved them, and then failed in the first hook or spawn with a
/// bare "No such file or directory" that read as the install command or
/// bash being missing. `rm` keeps the plain lookup: clearing such an entry
/// is what it is for.
pub(super) fn refuse_a_gone_directory(root: &Path, worktree: &Worktree) -> Result<()> {
    // A set whose own directory is there can still have lost a checkout:
    // a process whose `cwd` is that repository's would fail the same way.
    if !worktree.prunable && worktree.path.is_dir() {
        return match worktree
            .repos
            .iter()
            .find(|checkout| checkout.prunable || !checkout.path.is_dir())
        {
            None => Ok(()),
            Some(gone) => bail!(
                "the checkout of {} in {} is gone: git still lists it at {}, but nothing can \
                 run there — {} checks it out again, and leaves the others as they are",
                gone.name,
                worktree.display_name(),
                gone.path.display(),
                worktree.checkout_hint(root, &gone.name)
            ),
        };
    }
    let why = worktree
        .prunable_reason
        .as_deref()
        .map(|reason| format!(" ({reason})"))
        .unwrap_or_default();
    bail!(
        "the directory of {} is gone{why}: git still lists it at {}, but nothing can run there \
         — `pando rm {}` clears that entry",
        worktree.display_name(),
        worktree.path.display(),
        worktree.display_name()
    )
}

/// Refuses, once per run and before anything is written, a pando home or a
/// `worktrees_dir` that lies inside the repository or any worktree git knows
/// about.
///
/// `config::validate` already checks `worktrees_dir` against the repository
/// root, which is all it can do without git. This is the version that has
/// the porcelain list, so it also covers linked worktrees, and it covers the
/// home — which nothing validates, and which is where state, caches, logs
/// and every worktree pando creates would land.
pub fn guard_write_locations(paths: &PandoPaths, config: &Config) -> Result<()> {
    let discovery = worktree::discover_all(&paths.project)?;
    let worktrees: Vec<PathBuf> = discovery
        .worktrees
        .iter()
        .map(|w| w.path.clone())
        .chain(std::iter::once(discovery.main.path.clone()))
        .collect();
    crate::paths::ensure_outside_repository("pando home", &paths.home, paths.root(), &worktrees)?;
    crate::paths::ensure_outside_repository(
        "worktrees_dir",
        &config.worktrees_dir(paths),
        paths.root(),
        &worktrees,
    )
}

/// Every managed worktree, enriched with git metadata. A check's
/// throwaway worktree is not one: see [`worktree::is_check`].
pub fn ls(paths: &PandoPaths) -> Result<Vec<Worktree>> {
    let mut worktrees = worktree::discover(&paths.project)?;
    worktrees.retain(|w| !worktree::is_check(&w.name));
    worktree::enrich_from_git(&mut worktrees, &paths.project).ok();
    Ok(worktrees)
}

/// [`ls`], with the main checkout beside the worktrees, enriched in the
/// same pass: what a listing that puts main first shows.
pub fn ls_all(paths: &PandoPaths) -> Result<worktree::Discovery> {
    let mut discovery = worktree::discover_all(&paths.project)?;
    discovery.worktrees.retain(|w| !worktree::is_check(&w.name));
    let mut all: Vec<Worktree> = std::iter::once(discovery.main)
        .chain(discovery.worktrees)
        .collect();
    worktree::enrich_from_git(&mut all, &paths.project).ok();
    let main = all.remove(0);
    Ok(worktree::Discovery {
        main,
        worktrees: all,
    })
}

/// The absolute, canonical path of a worktree.
pub fn path(paths: &PandoPaths, name: &str) -> Result<PathBuf> {
    path_in(worktree::discover_all(&paths.project)?, name)
}

/// [`path`], from a listing already made: the one the name was resolved
/// against, so `pando path` lists the worktrees once for both.
pub fn path_in(discovery: worktree::Discovery, name: &str) -> Result<PathBuf> {
    if discovery.main.name == name {
        return Ok(discovery.main.path);
    }
    discovery
        .worktrees
        .into_iter()
        .find(|w| w.name == name)
        .map(|w| w.path)
        .with_context(|| format!("no worktree named {name:?}"))
}

/// Which worktrees pando created, from a state it has already read.
pub fn ownership(
    store: &state::State,
    worktrees: &[Worktree],
) -> std::collections::BTreeMap<String, bool> {
    store
        .worktrees
        .iter()
        .map(|(name, record)| {
            let ours = record.created_by_pando
                && worktrees
                    .iter()
                    .any(|w| &w.name == name && record_is_for(record, w));
            (name.clone(), ours)
        })
        .collect()
}

/// Which worktrees pando created, and anything that stopped the answer
/// being certain.
#[derive(Debug, Default, Clone)]
pub struct Ownership {
    pub by_name: std::collections::BTreeMap<String, bool>,
    /// The one-line reason the map may be empty or wrong — the same message
    /// `rm` refuses with, so a listing never says "adopted" about a state
    /// file `rm` would not touch.
    pub warning: Option<String>,
}

/// Which worktrees pando created, from state. A read path, so it goes
/// through [`refresh`]: phases are advanced and a crashed process stays
/// visible as Failed.
///
/// `worktrees` is what git currently reports: a record only vouches for a
/// worktree at the same path it was written for, so a record left behind by
/// a worktree removed outside pando cannot adopt a later namesake.
pub fn created_by_pando(paths: &PandoPaths, worktrees: &[Worktree]) -> Ownership {
    let refreshed = refresh(paths);
    Ownership {
        by_name: ownership(&refreshed.state, worktrees),
        warning: refreshed.warning,
    }
}

/// Creates a worktree for a pull request, returning its directory name.
///
/// One opened from a branch on `origin` is `new` of that branch, which
/// fetches it when it has not been yet. One from a fork has no branch on
/// `origin`, only `refs/pull/<number>/head`, so that is fetched into
/// [`PrInfo::local_branch`] first — once: a local branch of that name
/// already is somebody's work on it, and is checked out as it stands.
pub fn new_for_pr(
    paths: &PandoPaths,
    config: &Config,
    pr: &PrInfo,
    progress: &dyn Fn(&str),
) -> Result<String> {
    let (name, remote) = checkout_for_pr(paths, config, pr, progress)?;
    // The branch it is going onto is the pull request's, not the config's.
    if !pr.base.is_empty() {
        remember_base(
            paths,
            &name,
            pr_repo(paths, pr),
            &format!("{remote}/{}", pr.base),
        );
    }
    Ok(name)
}

/// The repository of a multi-repo project a pull request was opened in,
/// when it is one of the project's.
fn pr_repo<'a>(paths: &PandoPaths, pr: &'a PrInfo) -> Option<&'a str> {
    pr.repo
        .as_deref()
        .filter(|repo| paths.project.repos.iter().any(|r| r == repo))
}

/// Writes down the base a worktree goes onto — for a set, the base of its
/// checkout of `repo`. Best effort: a worktree that has not got one falls
/// back to the config's rule.
fn remember_base(paths: &PandoPaths, name: &str, repo: Option<&str>, base: &str) {
    let Ok(_lock) = state::lock(&paths.lock_file()) else {
        return;
    };
    let Ok(mut store) = state::load(&paths.state_file()) else {
        return;
    };
    if let Some(record) = store.worktrees.get_mut(name) {
        record.set_base(repo, base);
        let _ = state::save(&paths.state_file(), &store);
    }
}

/// [`new_for_pr`]'s worktree, and the remote its branch came from.
fn checkout_for_pr(
    paths: &PandoPaths,
    config: &Config,
    pr: &PrInfo,
    progress: &dyn Fn(&str),
) -> Result<(String, String)> {
    let branch = pr.local_branch();
    // A multi-repo project's pull request is fetched in its own
    // repository, and the set is `new` of its branch: the other
    // repositories take that branch where they have it.
    let root = match (paths.project.is_multi_repo(), pr_repo(paths, pr)) {
        (false, _) => paths.root().to_path_buf(),
        (true, Some(repo)) => paths.root().join(repo),
        (true, None) => bail!(
            "#{} names no repository of this project ({}) — refresh the pull requests and \
             try again",
            pr.number,
            paths.project.repos.join(", ")
        ),
    };
    validate_branch_name(&root, &branch)?;
    let origin = || "origin".to_string();
    if ref_exists(&root, &format!("refs/heads/{branch}")) {
        let remote = match pr.cross_repository {
            true => pr_remote(&root, &pr.url).unwrap_or_else(origin),
            false => origin(),
        };
        return Ok((new(paths, config, &branch, None, progress)?, remote));
    }
    if !pr.cross_repository {
        // `new` of a name it cannot find forks a new branch of that name,
        // which here would be an empty worktree that looks like the pull
        // request. `gh` may be answering for a remote other than `origin`
        // — `upstream`, with `origin` somebody's fork — so it is looked
        // for first, and its absence said.
        let tracking = format!("refs/remotes/origin/{branch}");
        if has_origin(&root) && !ref_exists(&root, &tracking) {
            progress(&format!("looking for origin/{branch}"));
            fetch_branch(&root, &branch, crate::project::GIT_TIMEOUT)?;
        }
        if !ref_exists(&root, &tracking) {
            bail!(
                "#{}'s branch {branch} is not on origin — gh may list pull requests from \
                 another remote; fetch the branch, then try again",
                pr.number
            );
        }
        return Ok((new(paths, config, &branch, None, progress)?, origin()));
    }
    let Some(remote) = pr_remote(&root, &pr.url) else {
        bail!(
            "#{} is from a fork, and pando fetches it from the remote of the repository it was \
             opened against, or from origin, and this repository has neither",
            pr.number
        );
    };
    progress(&format!("fetching #{} from {remote}", pr.number));
    fetch_pr_head(
        &root,
        &remote,
        pr.number,
        &branch,
        crate::project::GIT_TIMEOUT,
    )?;
    // A rejected `new` leaves no branch behind, and the one fetched here
    // is part of what it made.
    new(paths, config, &branch, None, progress)
        .inspect_err(|_| {
            let _ = crate::project::git(&root, ["branch", "-D", branch.as_str()]);
        })
        .map(|name| (name, remote))
}

/// The remote a pull request's `refs/pull/<n>/head` is fetched from: the
/// one whose URL names the repository the pull request was opened
/// against, as `gh` chose it — `upstream` in a contributor's clone, where
/// `origin` is their fork and its #n is another pull request or none —
/// else `origin`. Matched on the owner and name alone, so an ssh host
/// alias still matches.
fn pr_remote(root: &Path, url: &str) -> Option<String> {
    let base = repository_of(url.split("/pull/").next().unwrap_or_default());
    let names = remotes(root);
    let matching = base.and_then(|base| {
        names.iter().find(|name| {
            crate::project::git(root, ["remote", "get-url", name.as_str()])
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| repository_of(String::from_utf8_lossy(&out.stdout).trim()))
                .is_some_and(|theirs| theirs == base)
        })
    });
    matching
        .or_else(|| names.iter().find(|name| *name == "origin"))
        .cloned()
}

/// `owner/name`, lower-cased, from a repository's URL in any of git's
/// spellings: `https://host/owner/name(.git)`, `git@host:owner/name.git`,
/// `ssh://git@host/owner/name`, or a path.
pub(super) fn repository_of(url: &str) -> Option<String> {
    let path = url.trim().trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = match path.split_once("://") {
        Some((_, rest)) => rest,
        // scp-like: `host:owner/name`, but not a Windows drive.
        None => match path.split_once(':') {
            Some((host, rest)) if host.len() > 1 && !host.contains('/') => rest,
            _ => path,
        },
    };
    let mut parts = path.rsplit('/').filter(|part| !part.is_empty());
    let name = parts.next()?;
    let owner = parts.next()?;
    Some(format!("{owner}/{name}").to_ascii_lowercase())
}

/// `git fetch <remote> refs/pull/<number>/head` into a new local branch.
/// Bounded and never prompting, as [`fetch_branch`] is; unlike it, a
/// failure is an error, because there is no other place the branch could be.
fn fetch_pr_head(
    root: &Path,
    remote: &str,
    number: u32,
    branch: &str,
    timeout: std::time::Duration,
) -> Result<()> {
    let refspec = format!("refs/pull/{number}/head:refs/heads/{branch}");
    let fetch = || {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(root)
            .args(["fetch", "--quiet", remote, &refspec])
            .env("GIT_TERMINAL_PROMPT", "0");
        crate::platform::process::output_within(command, timeout)
    };
    match retrying_git_locks(fetch, |_| true) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => bail!(
            "could not fetch #{number} from {remote}: {}",
            git_failure_reason(&out)
        ),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => bail!(
            "`git fetch {remote} {refspec}` did not answer in {}s",
            timeout.as_secs()
        ),
        Err(e) => Err(e).context("spawn git fetch"),
    }
}

/// Whether a state record is really about this worktree. Keying on the
/// directory basename alone is not enough: the record a worktree removed
/// outside pando leaves behind would otherwise vouch for any later worktree
/// of the same name, anywhere on disk.
fn record_is_for(record: &WorktreeRecord, wt: &Worktree) -> bool {
    // A prunable entry's directory is gone, so `Worktree::path` is whatever
    // git recorded rather than a canonical path. Comparing it would start
    // demanding `--yes` for pando's own prunable worktrees, so the name is
    // trusted for those — clearing a prunable entry removes no directory.
    wt.prunable
        || crate::paths::resolve_for_compare(&record.path)
            == crate::paths::resolve_for_compare(&wt.path)
}

/// The record called `name` when it is for a worktree git no longer lists
/// at all, under that name or any other: the one thing `rm` of a name git
/// does not know removes.
fn forgotten_record<'a>(
    discovery: &worktree::Discovery,
    store: &'a state::State,
    name: &str,
) -> Option<&'a WorktreeRecord> {
    let record = store.worktrees.get(name)?;
    let path = crate::paths::resolve_for_compare(&record.path);
    let listed = std::iter::once(&discovery.main)
        .chain(&discovery.worktrees)
        .any(|w| crate::paths::resolve_for_compare(&w.path) == path);
    (!listed).then_some(record)
}

/// Drops records for worktrees git no longer lists. Callers hold the flock;
/// best effort, because a porcelain that cannot be read is not a reason to
/// refuse the command that is running.
///
/// Never a record a removal has marked: git stops listing a worktree half
/// way through its `rm`, which drops the record itself once the rest is
/// down; and one an interrupted removal left names the containers and
/// namespaces the `rm` that finishes it takes down, which nothing else
/// could find once it was gone.
fn drop_stale_worktree_records(store: &mut state::State, project: &ProjectRef) {
    let Ok(live) = worktree::listed_paths(project) else {
        return;
    };
    store.worktrees.retain(|_, record| {
        record.removing.is_some() || live.contains(&crate::paths::resolve_for_compare(&record.path))
    });
}

/// `requested` is the base the caller or config named, if any; none forks
/// from the repository's own.
fn resolve_create_source(
    root: &Path,
    branch: &str,
    requested: Option<String>,
    progress: &dyn Fn(&str),
) -> Result<CreateSource> {
    if ref_exists(root, &format!("refs/heads/{branch}")) {
        return Ok(CreateSource::Local);
    }
    if has_origin(root) {
        if ref_exists(root, &format!("refs/remotes/origin/{branch}")) {
            return Ok(CreateSource::Remote);
        }
        // The branch may exist on the remote but not be fetched yet — a PR
        // opened since the last fetch. A failure here just means it does not
        // exist there either, so this falls through to a new branch.
        progress(&format!("looking for origin/{branch}"));
        let fetched = fetch_branch(root, branch, crate::project::GIT_TIMEOUT)?;
        if fetched && ref_exists(root, &format!("refs/remotes/origin/{branch}")) {
            return Ok(CreateSource::Remote);
        }
    } else if has_any_remote(root) {
        // Named explicitly rather than guessed at: pando only knows how to
        // look a branch up on a remote called `origin`.
        progress("no remote named origin; treating this as a new branch");
    }

    let base = match requested {
        Some(b) => {
            let resolved = resolve_create_base(root, &b);
            if !ref_exists(root, &resolved) {
                bail!("base {b:?} does not exist in this repository");
            }
            resolved
        }
        None => worktree::resolve_base_branch(root).context(format!(
            "cannot work out a base branch (no origin/HEAD, main, or master) — {}",
            remedy::NAME_A_BASE
        ))?,
    };
    Ok(CreateSource::Fork { base })
}

/// `git fetch origin <branch>`: whether it fetched.
///
/// Bounded, and never allowed to ask for anything. A remote that wants a
/// password used to prompt on the terminal — over the TUI, whose keys
/// then went nowhere — and one that never answered held `new` for ever.
/// A fetch that *fails* is still "not on the remote", as before; one that
/// runs out of time is not an answer, and forking a new branch under a
/// name the remote may well have is worse than saying so.
pub(super) fn fetch_branch(
    root: &Path,
    branch: &str,
    timeout: std::time::Duration,
) -> Result<bool> {
    // A fetch another git's lock failed is no answer either, and would
    // read as "not on the remote": tried again a moment later.
    let fetch = || {
        let mut command = Command::new("git");
        command
            .arg("-C")
            .arg(root)
            .args(["fetch", "--quiet", "origin", branch])
            .env("GIT_TERMINAL_PROMPT", "0");
        crate::platform::process::output_within(command, timeout)
    };
    match retrying_git_locks(fetch, |_| true) {
        Ok(out) => Ok(out.status.success()),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => bail!(
            "`git fetch origin {branch}` did not answer in {}s, so pando cannot tell whether \
             origin already has {branch} — fetch it yourself, then run this again",
            timeout.as_secs()
        ),
        Err(_) => Ok(false),
    }
}

/// A bare base name would fork from the possibly stale local branch, so a
/// worktree created weeks after the last fetch would silently miss
/// everything merged since. A name already qualified with its remote is
/// used untouched; that is told from the refs, not from a slash, because
/// `release/1.2` is a branch name too.
pub(super) fn resolve_create_base(root: &Path, base: &str) -> String {
    if base.contains('/') && ref_exists(root, &format!("refs/remotes/{base}")) {
        return base.to_string();
    }
    if ref_exists(root, &format!("refs/remotes/origin/{base}")) {
        return format!("origin/{base}");
    }
    base.to_string()
}

/// Whether `checkout`'s repository has `base`, read as `new` reads it.
pub(super) fn base_exists(checkout: &Path, base: &str) -> bool {
    ref_exists(checkout, &resolve_create_base(checkout, base))
}

fn validate_branch_name(root: &Path, branch: &str) -> Result<()> {
    if branch.trim().is_empty() {
        bail!("a branch name is required");
    }
    let ok = crate::project::git(root, ["check-ref-format", "--branch", branch])
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        bail!("{branch:?} is not a valid branch name");
    }
    Ok(())
}

/// Exit 0 means ignored, 1 means not ignored (including a tracked file),
/// 128 is a git error worth surfacing. `dir` is whichever checkout has the
/// last word: the main one for the pre-flight, the new worktree for the
/// check that actually authorises a write. `key` is the setting that named
/// the path, `provision` or `clone`.
fn ensure_gitignored(dir: &Path, rel: &str, key: &str) -> Result<()> {
    let out = crate::project::git(dir, ["check-ignore", "-q", "--", rel])
        .context("run git check-ignore")?;
    // Named as the config names it: a directory is asked about with a
    // trailing slash, which is the query's, not the developer's.
    let rel = rel.trim_end_matches('/');
    match out.status.code() {
        Some(0) => Ok(()),
        Some(1) => bail!(
            "{key} path {rel:?} is not ignored in {} — pando only creates files your project \
             already ignores. Add it to .gitignore, or drop it from {key}.",
            dir.display()
        ),
        Some(128) => bail!(
            "git check-ignore failed for {rel:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        other => bail!("git check-ignore exited with {other:?} for {rel:?}"),
    }
}

/// [`ensure_gitignored`] for `rel`, a path relative to `dir`, a checkout
/// of the project: asked in the repository it is in, which for a
/// multi-repo project is the checkout of the repository the path starts
/// with. A path in none of them is refused: it would be a file of no
/// repository, which nothing in the project reads.
fn ensure_ignored(project: &ProjectRef, dir: &Path, rel: &str, key: &str) -> Result<()> {
    let Some((checkout, rel)) = project.repo_path(dir, rel) else {
        bail!(
            "{key} path {:?} is in none of the repositories ({}) — in a project of several \
             repositories, a {key} path starts with the one it is in, as `{}/.env` does",
            rel.trim_end_matches('/'),
            project.repos.join(", "),
            project.example_repo()
        );
    };
    ensure_gitignored(&checkout, &rel, key)
}

/// Links (or copies) the configured files into a new worktree.
///
/// The caller proved every path gitignored in the main checkout, but this
/// worktree has a different commit checked out and can have a different
/// `.gitignore` — an older branch, or an uncommitted edit the pre-flight
/// read. Invariant 1 is about the worktree the file lands in, so the
/// authorising `check-ignore` is re-run there, immediately before each
/// write.
fn provision_worktree_files(
    paths: &PandoPaths,
    config: &Config,
    worktree: &Path,
    progress: &dyn Fn(&str),
) -> Result<()> {
    for rel in config.project.provision_paths() {
        provision_path(paths, config, worktree, rel, progress)?;
    }
    Ok(())
}

/// One provisioned path, into a worktree pando created: nothing when the
/// worktree has something there already, a dangling link included, or
/// when the main checkout has nothing to give it.
fn provision_path(
    paths: &PandoPaths,
    config: &Config,
    worktree: &Path,
    rel: &str,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let dst = worktree.join(rel);
    if is_present(&dst) {
        return Ok(());
    }
    let Some((src, seeded)) = provision_source(paths, config, rel) else {
        return Ok(());
    };
    // Invariant 1, checked in the worktree the file lands in and
    // immediately before the write — a seeded file is no different, and
    // the example it comes from being tracked buys it nothing.
    ensure_ignored(&paths.project, worktree, rel, "provision")?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create dir {}", parent.display()))?;
    }
    // A seed is always copied, whatever the mode says. A symlink to the
    // tracked example would make every edit inside the worktree a write
    // into the repository, which is the invariant this whole path exists
    // to keep.
    let mode = match seeded {
        true => {
            // Every guess is visible, and this one is a file being created
            // out of contents pando did not write: the notice names the
            // source, so the developer can go and read it.
            let from = config
                .project
                .provision_from
                .get(rel)
                .map(String::as_str)
                .unwrap_or_default();
            progress(&format!("seeding {rel} from {from}"));
            ProvisionMode::Copy
        }
        false => config.project.provision_mode,
    };
    match mode {
        ProvisionMode::Link => {
            crate::platform::files::symlink(&src, &dst)
                .with_context(|| format!("symlink {} → {}", src.display(), dst.display()))?;
            // Said per file, and said to be a link: an edit in the worktree
            // edits the main checkout's file.
            progress(&format!(
                "linked {rel} → {} (symlink; `provision_mode = \"copy\"` gives each worktree \
                 its own)",
                src.display()
            ));
        }
        ProvisionMode::Copy => {
            copy_new(&src, &dst)
                .with_context(|| format!("copy {} → {}", src.display(), dst.display()))?;
            if !seeded {
                progress(&format!("copied {rel} from {}", src.display()));
            }
        }
    }
    Ok(())
}

/// Whether anything is at `path`, a link whose target is gone included:
/// a write there would follow the link to wherever it points.
fn is_present(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}

/// A copy that never replaces a file: one that appeared since the check
/// above is somebody's, and is left as it is.
fn copy_new(src: &Path, dst: &Path) -> std::io::Result<()> {
    let mut from = std::fs::File::open(src)?;
    let mut to = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    std::io::copy(&mut from, &mut to)?;
    to.set_permissions(from.metadata()?.permissions())
}

/// A provisioned path a worktree does not have and the main checkout can
/// give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unprovisioned {
    /// The path, as `provision` names it: `.env`, `apps/web/.env`.
    pub rel: String,
    /// What it would come from: the main checkout's own file, or the
    /// example `provision_from` names.
    pub source: PathBuf,
    /// Where it would go.
    pub target: PathBuf,
    /// Whether `new` would link it rather than copy it: `provision_mode =
    /// "link"`, and a local file rather than an example, which is always
    /// copied.
    pub link: bool,
    /// Whether the worktree's own gitignore ignores it. pando writes only
    /// a path that is, so a command for one that is not is a file that
    /// shows up in `git status`.
    pub ignored: bool,
    /// The example it is seeded from, as `provision_from` names it, when
    /// the main checkout has no file of its own to give.
    pub seeded_from: Option<String>,
}

impl Unprovisioned {
    /// The command that gives the worktree what `new` would have: `cp`,
    /// or `ln -s` for a link.
    pub fn command(&self) -> String {
        format!(
            "{} {} {}",
            self.verb(),
            crate::process::shell_word(&self.source.display().to_string()),
            crate::process::shell_word(&self.target.display().to_string())
        )
    }

    /// `cp` or `ln -s`.
    pub fn verb(&self) -> &'static str {
        match self.link {
            true => "ln -s",
            false => "cp",
        }
    }

    /// Whether the directory it goes in is there. A branch without the
    /// app a path belongs to — one made before `apps/mobile` was — has
    /// nothing that reads it, and a `cp` into it fails.
    pub fn has_place(&self) -> bool {
        self.target.parent().is_some_and(Path::is_dir)
    }

    /// What `start` says about it in a worktree pando did not create,
    /// before it starts the app without it.
    pub fn adopted_line(&self) -> String {
        match self.ignored {
            true => format!(
                "{} is not in this worktree, and pando writes only into worktrees it created — \
                 `{}` gives it {}",
                self.rel,
                self.command(),
                match &self.seeded_from {
                    Some(from) => format!("a copy of the main checkout's {from}"),
                    None => "the main checkout's".to_string(),
                }
            ),
            false => format!(
                "{} is not in this worktree, and this worktree's .gitignore does not ignore it, \
                 so pando would not write it even into a worktree of its own — a copy would show \
                 in `git status` until it is ignored there",
                self.rel
            ),
        }
    }
}

/// Every provisioned path `worktree` lacks that the main checkout can give
/// it, in `provision`'s order. Reads, and asks git whether each is ignored
/// there; writes nothing.
pub fn unprovisioned(paths: &PandoPaths, config: &Config, worktree: &Path) -> Vec<Unprovisioned> {
    config
        .project
        .provision_paths()
        .iter()
        .filter(|rel| !is_present(&worktree.join(rel)))
        .filter_map(|rel| {
            let (source, seeded) = provision_source(paths, config, rel)?;
            Some(Unprovisioned {
                rel: rel.clone(),
                target: worktree.join(rel),
                link: !seeded && config.project.provision_mode == ProvisionMode::Link,
                ignored: crate::detect::is_ignored_in(&paths.project, worktree, rel),
                seeded_from: seeded
                    .then(|| config.project.provision_from.get(rel).cloned())
                    .flatten(),
                source,
            })
        })
        .collect()
}

/// What `start` does about provisioned paths a worktree lacks, before
/// anything runs in it.
///
/// A worktree pando created gets them, the way `new` gives them: never
/// over anything already there, and with the gitignore check made in the
/// worktree immediately before each write. `provision` answered after the
/// worktree was made, or a file deleted since, is otherwise an app that
/// starts with no `.env` and nothing saying why. One that cannot be
/// written is said and the start goes on, as it would have without it.
///
/// A worktree pando did not create is never written into — Invariant 1
/// names the worktrees pando created — so each path it lacks is said, with
/// the command that gives it one.
pub(super) fn provision_at_start(
    paths: &PandoPaths,
    config: &Config,
    worktree: &Path,
    created_by_pando: bool,
    progress: &dyn Fn(&str),
) {
    let missing = unprovisioned(paths, config, worktree);
    if missing.is_empty() {
        return;
    }
    if !created_by_pando {
        for path in missing.iter().filter(|path| path.has_place()) {
            progress(&path.adopted_line());
        }
        return;
    }
    progress("provisioning");
    for path in &missing {
        if let Err(e) = provision_path(paths, config, worktree, &path.rel, progress) {
            progress(&format!("{e:#} — starting without it"));
        }
    }
}

/// Where a provisioned path's contents come from, and whether that is the
/// project's own example rather than a local file.
///
/// The main checkout's own file first: an example is the fallback for a
/// clone that has none, and the moment the developer writes their real one
/// it is what every new worktree gets. `None` when there is nothing to copy
/// from, which is skipped rather than invented.
///
/// A source that is itself a gitignored local file — the root `.env` given
/// to a workspace app — is not a seed: it is the developer's own file under
/// another path, and it is linked or copied as the mode says, like the
/// root one is.
fn provision_source(paths: &PandoPaths, config: &Config, rel: &str) -> Option<(PathBuf, bool)> {
    let src = paths.root().join(rel);
    if src.exists() {
        return Some((src, false));
    }
    let from = config.project.provision_from.get(rel)?;
    let seed = paths.root().join(from);
    if !seed.exists() {
        return None;
    }
    let seeded = !crate::detect::is_ignored_in(&paths.project, paths.root(), from);
    Some((seed, seeded))
}

pub(super) fn ref_exists(root: &Path, refname: &str) -> bool {
    crate::project::git(root, ["rev-parse", "--verify", "--quiet", refname])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn has_origin(root: &Path) -> bool {
    remotes(root).iter().any(|r| r == "origin")
}

fn has_any_remote(root: &Path) -> bool {
    !remotes(root).is_empty()
}

fn remotes(root: &Path) -> Vec<String> {
    crate::project::git(root, ["remote"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// The last non-empty stderr line that is not a hint: git narrates before
/// it fails, so the closing line is the reason and everything above it is
/// progress noise — and the hints it can print after it, a branch whose
/// upstream could not be written, are advice for git's own command line.
pub(super) fn git_failure_reason(out: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    match stderr
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty() && !l.starts_with("hint:"))
    {
        Some(line) => line.trim().trim_start_matches("fatal: ").to_string(),
        None => format!("exit {}", out.status.code().unwrap_or(-1)),
    }
}
