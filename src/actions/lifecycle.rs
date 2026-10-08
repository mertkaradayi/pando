//! Start, stop, restart.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{self, Config, ProcessConfig};
use crate::detect;
use crate::paths::PandoPaths;
use crate::ports;
use crate::process::{self as proc, SpawnOptions};
use crate::state::{self, Phase, ProcessRecord, ServiceMode, WorktreeRecord};
use crate::template;
use crate::worktree::{self, Worktree};

use super::hooks::{HookContext, pando_env, run_hooks, run_probes};
use super::namespaced::{self, Ready};
use super::refresh::advance_before_reconcile;
use super::runtime::with_prelude;
use super::services::{
    FRESH_DATA_DIR, Fresh, bring_up_services, changed_kinds, clear_native_sockets,
    compose_projects, forget_hooks_after_services, forget_unstarted_services,
    has_interrupted_compose, has_live_services, leave_changed_kinds, planned_services,
    preflight_isolation, replace_stopped_containers, resolve_service_env, service_roles,
    shared_service_env, stop_containers, stop_service_containers, stop_service_pumps,
    undo_failed_isolation, url_owner_not_running, worktree_url,
};
use super::share::{share_closed, share_target_is_up, sweep_dead_shares_with, take_share_down};
// Only for the intra-doc link above `sweep_orphaned_groups`.
#[cfg(doc)]
use super::share::sweep_dead_shares;
use super::worktree::{
    Checkout, MAIN_RUNS_ONLY, find_live_checkout, find_worktree, provision_at_start,
};

/// How long a process group gets to exit on its own before SIGKILL.
pub(super) const STOP_GRACE: Duration = Duration::from_secs(5);

/// The role `share` and the browser-open key default to, and the role a
/// readiness rule watches when none is named.
pub(super) const DEFAULT_READY_ROLE: &str = "web";

/// What a start does about this worktree's services.
///
/// One value rather than two flags, because "isolated and shared" is not a
/// state: the command line refuses the pair, and nothing downstream has to
/// decide what it would have meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Whatever this worktree already does: private copies if it has them,
    /// the project's shared services if it does not. A plain `start`.
    #[default]
    Remembered,
    /// Private copies of the project's services, for this worktree alone.
    Isolated,
    /// The project's shared services, and the private copies stopped. The
    /// way back.
    Shared,
    /// The main checkout's own servers, with a namespace of this
    /// worktree's own in each: a database, a numbered slot.
    Namespaced,
}

impl From<ServiceMode> for Mode {
    /// The start that puts a worktree in this mode.
    fn from(mode: ServiceMode) -> Mode {
        match mode {
            ServiceMode::Shared => Mode::Shared,
            ServiceMode::Namespaced => Mode::Namespaced,
            ServiceMode::Isolated => Mode::Isolated,
        }
    }
}

impl Mode {
    /// The mode three flags mean. `start` and `restart` refuse any two of
    /// them together before this is ever called.
    pub fn of(isolated: bool, namespaced: bool, shared: bool) -> Mode {
        match (isolated, namespaced, shared) {
            (true, _, _) => Mode::Isolated,
            (_, true, _) => Mode::Namespaced,
            (_, _, true) => Mode::Shared,
            _ => Mode::Remembered,
        }
    }
}

/// One process a start brought up, or found already up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedProcess {
    /// The process's name in config — `dev` for the `[dev]` shorthand.
    pub process: String,
    pub record: ProcessRecord,
}

/// What one `start` did to a worktree.
///
/// A worktree has as many processes as its config declares, so a start is
/// never one thing: some come up, some were already running, and the ports
/// and the URL belong to the worktree rather than to any one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartReport {
    pub worktree: String,
    /// Spawned by this call, in the order they were spawned.
    pub started: Vec<StartedProcess>,
    /// Already up, and left exactly as they were.
    pub already_running: Vec<StartedProcess>,
    /// Every role of the worktree, whatever `--only` asked for: ports are
    /// reserved for the whole worktree at once, so starting one process
    /// never moves another one's port.
    pub ports: BTreeMap<String, u16>,
    /// The `web` role's URL when any process owns that role, else the
    /// first role of the alphabetically first process that owns one — the
    /// same rule, through the same function, that `status`, `ls` and the
    /// TUI use. See [`worktree_url`]. `None` while the process it points at
    /// is not running.
    pub url: Option<String>,
    /// Ports this worktree owned had been taken, so it moved. Worth saying
    /// out loud: a URL the developer had bookmarked just changed.
    pub reassigned: bool,
}

impl StartReport {
    /// Every process this call has something to say about.
    pub fn processes(&self) -> impl Iterator<Item = &StartedProcess> {
        self.started.iter().chain(self.already_running.iter())
    }

    /// Nothing was spawned: everything asked for was already up.
    pub fn started_nothing(&self) -> bool {
        self.started.is_empty()
    }

    /// The names of the processes this call spawned.
    pub fn spawned(&self) -> Vec<String> {
        self.started.iter().map(|p| p.process.clone()).collect()
    }
}

/// The names of a list of processes, for a message.
pub fn process_names(list: &[StartedProcess]) -> String {
    list.iter()
        .map(|p| p.process.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// The processes whose groups were signalled.
    Stopped(Vec<String>),
    /// Nothing was running. Not an error: `stop` is how you make sure.
    NotRunning,
}

/// What a stop of every worktree did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopAllReport {
    /// The worktrees it stopped.
    pub stopped: Vec<String>,
    /// The worktrees up that the list it was given did not name: they came
    /// up after it was shown, and were left running.
    pub kept: Vec<String>,
}

/// Everything a start needs to know about one process before anything is
/// spawned.
///
/// Planned in full first, and spawned only afterwards: a second process
/// with an unrenderable command or a missing `cwd` must not leave the first
/// one running behind a failed start.
struct Planned {
    process: String,
    shell_cmd: String,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    log_file: PathBuf,
    ready_port: Option<u16>,
    ready_timeout_s: Option<u64>,
}

/// Starts a worktree's processes — every one its config declares, or the
/// one `only` names.
///
/// The shape is `new`'s: take the lock, decide, act, record, save. A
/// process that is already running is reported rather than started twice; a
/// record left over from one that died is signalled and cleared first,
/// because a dead leader does not mean a dead process group.
///
/// Every process is spawned before any of them is waited on. Dependencies
/// between them are the app's problem: a web server that needs its api up
/// first retries, as every dev server does.
pub fn start(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    only: Option<&str>,
    mode: Mode,
    progress: &dyn Fn(&str),
) -> Result<StartReport> {
    start_checked(
        paths,
        config,
        name,
        only,
        mode,
        Preflight::default(),
        progress,
    )
}

/// What a caller has already checked or made ready before [`start`] —
/// `restart` has to, before its stop, and doing either twice in one
/// command buys nothing.
#[derive(Default)]
struct Preflight {
    /// [`preflight_isolation`] has run.
    isolation: bool,
    /// The namespaces a namespaced start needs are made and recorded.
    namespaces: Option<Ready>,
    /// This start is a `restart --only`: the process it names is replaced
    /// even while it runs, rather than reported as already up.
    restarting: bool,
    /// This start is `pando check`'s on the shared services: the hooks
    /// after `services` and after `dev` are not run. There they would run
    /// against the developer's own data — a migration marked `on =
    /// "always"`, or any such hook in a project with no services pando
    /// runs — and a test must not change what it tests on. The check says
    /// so itself.
    skip_after_services: bool,
}

/// [`start`] for `pando check`: every process of `name`. With no
/// namespaces, on the shared services, with the hooks after services left
/// out — see [`Preflight::skip_after_services`]. With the namespaces the
/// check made first, namespaced, and those hooks run in them: that is the
/// schema step proved on data that is the check's own.
pub(super) fn start_for_check(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    namespaces: Option<Ready>,
    progress: &dyn Fn(&str),
) -> Result<StartReport> {
    let mode = match namespaces {
        Some(_) => Mode::Namespaced,
        None => Mode::Shared,
    };
    let preflight = Preflight {
        skip_after_services: namespaces.is_none(),
        namespaces,
        ..Preflight::default()
    };
    start_checked(paths, config, name, None, mode, preflight, progress)
}

fn start_checked(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    only: Option<&str>,
    mode: Mode,
    preflight: Preflight,
    progress: &dyn Fn(&str),
) -> Result<StartReport> {
    let Preflight {
        isolation: mut preflighted,
        namespaces: mut ready,
        restarting,
        skip_after_services,
    } = preflight;
    let Checkout { worktree, main } = find_live_checkout(paths, name)?;
    if main {
        refuse_main_mode(name, mode)?;
    }
    // Before anything is installed, signalled or spawned: a `--only` naming
    // a process that does not exist must have no side effects at all.
    let selection = selected_processes(config, only)?;
    // And before a namespace is made or a container checked: a `--only`
    // through a mode change is refused under the lock below whatever
    // happens, and a database made first would outlive the refusal.
    refuse_only_on_a_mode_change(paths, config, name, only, mode)?;
    refuse_losing_namespaces(paths, config, name, mode)?;
    let canonical = std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());

    // A project with nothing to isolate is not an error: `--isolated` on a
    // Go service with no compose file runs shared and says so, because the
    // flag is a wish about this project and the project has no services.
    let isolatable = !service_roles(config).is_empty();
    if mode == Mode::Isolated && !isolatable {
        progress("no services are configured for this project — starting in shared mode");
    }
    // The same for namespaces: a project none of whose services pando can
    // give a worktree a namespace in starts on the main checkout's data,
    // and says why for each of them.
    let namespaceable = can_namespace(paths, config, name, mode);
    if mode == Mode::Namespaced && !namespaceable {
        progress("no service here can have a namespace of its own — starting in shared mode");
        for line in namespaced::plan(paths, config).shared_lines() {
            progress(&line);
        }
    }
    // A start that will only report what is already up must not install
    // first: `npm ci` inside a worktree whose dev server is live is a
    // surprise nobody asked for. Starting one process beside a live one is
    // not that case — it is about to run code. Read without the lock, like
    // the hook's own fingerprint: the decision it guards is "can this step
    // be skipped", and the authoritative one is made under the lock below.
    // A restart is never that case: what it names is about to be replaced.
    let mut everything_up = !restarting && every_process_running(paths, name, &selection);
    // Said once, before anything runs: a developer who expected the
    // install to run in their own checkout reads why it does not here.
    if main && !everything_up {
        progress(MAIN_RUNS_ONLY);
    }

    // A start that switches this worktree to its own services replaces
    // every process it is running. So everything that switch needs —
    // Docker answering, the compose files, the recipes and their engines,
    // the env the app will be given — is checked first, while there is
    // still nothing to lose: a start that stopped a working dev server and
    // *then* found Docker was off had destroyed an environment over a
    // request that could never succeed. Read without the lock, like
    // `mode_would_change`; the decision is made again under it below.
    //
    // The same for an isolated worktree one of whose services config now
    // runs as another kind: the start stops the server it ran on before
    // the new one is brought up, and an engine that is not installed, or a
    // recipe that does not resolve, left its app with no database at all.
    if !preflighted
        && (would_switch_to_isolated(paths, config, name, mode)
            || (would_isolate(paths, config, name, mode)
                && would_change_kinds(paths, config, name)))
    {
        preflight_isolation(
            paths,
            config,
            name,
            &canonical,
            &worktree_roles(config, true),
        )?;
        preflighted = true;
    }
    // A namespaced start makes its namespaces now, before anything is
    // stopped: a server that does not answer, or a login it will not let
    // make a database, costs nothing here — the processes stay as they
    // were, and nothing was made.
    if ready.is_none() && target_of(paths, config, name, mode) == ServiceMode::Namespaced {
        ready = Some(namespaced::prepare(paths, config, name, progress)?);
    }

    paths.ensure_home()?;
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;

    // A record only vouches for the worktree it was written for; a stale
    // one at another path is replaced rather than inherited. Every group it
    // recorded is signalled first: those processes are running somewhere
    // else entirely, and dropping the record would leave nothing able to
    // find them again. Not one the orphan sweep has already signalled,
    // though: its pgid may name somebody else's group by now.
    let stale = store.worktrees.get(name).is_some_and(|record| {
        crate::paths::resolve_for_compare(&record.path)
            != crate::paths::resolve_for_compare(&canonical)
    });
    // What a stale record knew about this worktree's name rather than its
    // path outlives it: the compose project its containers and volumes are
    // under, and the namespaces pando made in the main checkout's servers.
    // Both are named for the project and the worktree's name, so they are
    // this worktree's, and a record that no longer names them is one `rm`
    // can never take down or drop.
    let mut inherited: Option<WorktreeRecord> = None;
    if stale {
        let record = store.worktrees.get_mut(name).expect("just read");
        let groups: Vec<proc::Group> = record
            .processes
            .values()
            .filter(|p| !p.swept)
            .map(|p| p.pgid)
            .collect();
        for pgid in groups {
            proc::stop(pgid, STOP_GRACE)?;
        }
        // Its native servers, log pumps and share are process groups too,
        // and a public tunnel onto a worktree that is not there any more is
        // the worst of them to leave with no record.
        let stop = |pgid| proc::stop(pgid, STOP_GRACE);
        let mut failures = stop_service_pumps(record, &stop);
        take_share_down(record, &stop, &mut failures);
        if !failures.is_empty() {
            bail!("{name}: {}", failures.join("; "));
        }
        inherited = store.worktrees.remove(name);
    }
    let inherited_projects = inherited.as_ref().map(compose_projects).unwrap_or_default();

    // What this worktree does about services now, against what it did
    // before. Decided here rather than with the ports below, because a
    // start that changes the answer is a start that has to replace the
    // processes: an application still pointed at a database that is going
    // away is not running in the mode it was asked for.
    let was = store
        .worktrees
        .get(name)
        .map(WorktreeRecord::mode)
        .unwrap_or_default();
    let was_isolated = was == ServiceMode::Isolated;
    let target = decide(mode, was, isolatable, namespaceable);
    let isolate = target == ServiceMode::Isolated;
    let mode_changed = was != target;
    if mode_changed {
        refuse_only_across_a_mode_change(only, target)?;
    }
    // The mode flipped to namespaced under a concurrent command between
    // the read above and this one, and nothing was made for it — or, for a
    // start that keeps that mode, nothing was asked of the plan: leaving
    // namespaces for the main checkout's data is never decided by a race.
    let raced = match target {
        ServiceMode::Namespaced => ready.is_none(),
        _ => keeps_namespaces(mode, was),
    };
    if raced {
        bail!(
            "another start changed this worktree's mode while this one was starting, so nothing \
             was started here — start it again with the mode you want"
        );
    }
    let ready: Option<Ready> = ready.take().filter(|_| target == ServiceMode::Namespaced);
    // A slot this start was given that another worktree's record names by
    // now, and this one's no longer does, was freed from this one while it
    // was starting and given out again: writing it back below would hand
    // one slot to two worktrees. One both records still name is one
    // `prepare` kept, and said so, because this worktree runs on it.
    let ours = |namespace: &state::NamespaceRecord| {
        store
            .worktrees
            .get(name)
            .or(inherited.as_ref())
            .is_some_and(|record| {
                record
                    .namespaces
                    .iter()
                    .any(|ns| crate::namespace::same_namespace(ns, namespace))
            })
    };
    for namespace in ready
        .iter()
        .flat_map(|ready| &ready.namespaces)
        .filter(|namespace| namespace.kind == state::NamespaceKind::Slot && !ours(namespace))
    {
        if let Some(other) = namespaced::recorded_elsewhere(&store, name, namespace) {
            bail!(
                "{} was given to {other} while this start was starting, so nothing was started \
                 here — start it again, and it gets one of its own",
                crate::namespace::describe(namespace)
            );
        }
    }
    // The mode flipped under a concurrent command between the lock-free
    // read above and this one, or the kind of a service its record runs
    // did. Rare, so checked here, under the lock, rather than not at all.
    let kinds_change = isolate
        && store
            .worktrees
            .get(name)
            .or(inherited.as_ref())
            .is_some_and(|record| !changed_kinds(config, record).is_empty());
    if (mode_changed || kinds_change) && isolate && !preflighted {
        preflight_isolation(
            paths,
            config,
            name,
            &canonical,
            &worktree_roles(config, true),
        )?;
    }
    // And the lifecycle runs again for it: a worktree whose services are
    // being swapped underneath it is not one where "everything is already
    // up" means there is nothing to do.
    everything_up = everything_up && !mode_changed;

    // A worktree becoming isolated in this start. Anything it writes down
    // for that before the services are really up is undone if the start
    // fails first — see `undo_failed_isolation`.
    let becoming_isolated = isolate && !was_isolated;
    // A worktree moving onto data of its own — its own servers, or its own
    // namespaces — from services that stay up whatever happens: the main
    // checkout's, or namespaces that are kept until `rm`. Its processes can
    // go on serving until the new data is ready; leaving isolated they
    // cannot, because their servers are about to stop.
    let keeps_serving = mode_changed && target != ServiceMode::Shared && !was_isolated;

    // Decided before anything is touched. A process that is alive is
    // reported and left exactly as it is; one whose leader is gone is
    // signalled before its record goes, because a dead leader is not a dead
    // process group and its child may still hold the port.
    //
    // A live process of a worktree moving onto data of its own is the
    // exception to both. It has to be replaced, since it talks to the
    // services it is leaving, but not yet: it keeps serving while the
    // private services or the namespaces come up and the hooks run against
    // them, and is replaced only once all of that has worked. A start that
    // fails there — a container that never gets ready, a migration that
    // fails — then costs nothing: the dev server the developer had is
    // still the one they have. Nothing fights over a port meanwhile,
    // because the services get roles of their own after the processes'
    // and the processes keep the ports they hold.
    //
    // A live process a `restart --only` names is left as it is too, and
    // replaced under the lock its successor is spawned under. Stopped
    // here, it would be out of the record until then, and a share it
    // serves would have nothing behind it: the next sweep, this start's
    // own among them, closes a URL whose process is coming back on the
    // same port.
    let mut already: Vec<String> = Vec::new();
    let mut clear: Vec<(String, proc::Group, bool)> = Vec::new();
    let mut kept_serving: Vec<String> = Vec::new();
    if let Some(record) = store.worktrees.get(name) {
        for (process, existing) in &record.processes {
            let selected = selection.iter().any(|(n, _)| n == process);
            let live = matches!(
                existing.phase,
                Phase::Starting { .. } | Phase::Running { .. }
            ) && existing.alive(proc::is_alive, proc::group_alive);
            if selected && live && restarting {
                // Replaced below, under the second lock.
            } else if selected && live && !mode_changed {
                already.push(process.clone());
            } else if selected && live && keeps_serving {
                kept_serving.push(process.clone());
            } else if selected || only.is_none() {
                // Selected and not live: this start replaces it. Not
                // selected, with nothing asking for a subset: a name config
                // no longer has — state a newer pando wrote, or a process
                // the developer renamed — whose live child would otherwise
                // be left holding a port nothing could find again.
                //
                // This is also where a `Failed` record goes: it survives
                // `reconcile` on purpose, so the mutation that acts on that
                // process is the one that has to clear it.
                //
                // A `--only` start leaves every other record alone. One
                // whose leader is dead is still signalled, by the sweep
                // below, before `reconcile` drops it.
                //
                // One the sweep has already signalled is cleared without a
                // second signal: its group had its SIGTERM and SIGKILL, and
                // by now its pgid may name somebody else's.
                clear.push((process.clone(), existing.pgid, existing.swept));
            }
        }
    }
    for process in &already {
        progress(&format!("{process} is already running"));
    }
    if !kept_serving.is_empty() {
        progress(&format!(
            "{} keeps running on the {} until this worktree's own are ready",
            kept_serving.join(", "),
            match was {
                ServiceMode::Namespaced => "namespaces it has",
                _ => "shared services",
            }
        ));
    }
    if !clear.is_empty() {
        progress(match mode_changed {
            true => "the services this worktree talks to are changing, so its processes restart",
            false => "clearing what is left of the last run",
        });
    }
    for (_, pgid, swept) in &clear {
        if !swept {
            proc::stop(*pgid, STOP_GRACE)?;
        }
    }
    if let Some(record) = store.worktrees.get_mut(name) {
        for (process, _, _) in &clear {
            record.processes.remove(process);
        }
    }
    // And every *other* worktree's dead-leader group, because `reconcile`
    // drops those records too — a half-dead share among them.
    for notice in sweep_orphaned_groups(&mut store)? {
        progress(&notice);
    }
    advance_before_reconcile(&mut store);
    state::reconcile(&mut store, proc::is_alive, proc::group_alive);

    let record = store
        .worktrees
        .entry(name.to_string())
        .or_insert_with(|| WorktreeRecord::new(canonical.clone(), false));
    if let Some(stale) = inherited {
        // Without a port: the window this start re-derives is its own.
        record.services.extend(
            stale
                .services
                .into_iter()
                .filter(|s| s.kind == state::ServiceKind::Compose && s.compose_project.is_some())
                .map(|s| state::ServiceRecord { port: None, ..s }),
        );
        for namespace in &stale.namespaces {
            namespaced::keep(record, namespace);
        }
    }
    // A whole-worktree start replaces everything that was observed; a
    // `--only` start leaves the sibling's ports alone and lets the next
    // refresh say what is really listening.
    if only.is_none() {
        record.observed_ports.clear();
    }
    // Isolation is per start and remembered per worktree: `--isolated`
    // turns it on, and a later plain `start` of the same worktree keeps
    // the services it already has rather than quietly pointing its
    // processes back at the shared database.
    //
    // Turned *on* only once the services are up and ready, right before
    // the processes that use them are spawned: a start that failed before
    // then — a mapping the env rewriter cannot satisfy, a container that
    // never got ready — must leave a worktree that still starts, shared,
    // with no edit to `pando.toml`. Turned off here and now, because a
    // worktree pando can no longer isolate is one whose next start is a
    // shared one, and there is no container to contradict that.
    //
    // Asked before the ports go: a compose record with one is the only
    // trace of a switch interrupted before any pump was recorded.
    let interrupted = has_interrupted_compose(record);
    if !isolate {
        if record.mode == Some(ServiceMode::Isolated) {
            record.mode = Some(ServiceMode::Shared);
        }
        // A worktree on the shared services owns no service ports: the
        // window this start re-derives has no room for them. And a record
        // whose service was never brought up — what a failed isolated
        // start used to leave behind — is forgotten, rather than shown as
        // a database that is down on a port nothing reserved.
        for service in record.services.iter_mut() {
            service.port = None;
        }
    }
    // `--shared` is the way back, and taking it means taking the private
    // copies down. The records stay: they carry the compose project, which
    // is the only thing that can find those containers and their volumes
    // again. Their *ports* do not, because the window this start is about
    // to re-derive no longer has room for them, and a status line claiming
    // a service is on a port another worktree now owns is worse than one
    // that says nothing.
    //
    // A record that is not isolated but has live service pumps or native
    // servers is the same case: a switch to isolated interrupted between
    // its services coming up and its second lock — Ctrl-C, a crash —
    // whose undo never ran. Its containers and servers are orphans, and
    // this start, which is a shared one, is the one that has to take them
    // down; dropping their ports while they run leaves them unfindable.
    // So is one interrupted earlier, during `up` or the wait for the
    // containers to be ready, before any pump was recorded.
    let mut going_shared: Vec<String> = Vec::new();
    if !isolate && (was_isolated || has_live_services(record) || interrupted) {
        let failures = stop_service_pumps(record, &|pgid| proc::stop(pgid, STOP_GRACE));
        if !failures.is_empty() {
            bail!("{name}: {}", failures.join("; "));
        }
        clear_native_sockets(paths, name, record);
        going_shared = compose_projects(record);
    }
    if !isolate {
        // After the pumps and native servers above are stopped, so a
        // native record that was live is only dropped once it is not.
        forget_unstarted_services(record);
    }
    // Service roles are reserved with the process roles, in one window, so
    // `{port:postgres}` resolves in any template and the number is the
    // same on every restart.
    let roles = worktree_roles(config, isolate);
    // Ports this worktree's own surviving processes are holding. They will
    // not pass a freeness probe, and they are not somebody else's either.
    let mut keep: Vec<u16> = record
        .processes
        .keys()
        .filter_map(|process| config.processes.get(process))
        .flat_map(|process| process.roles())
        .filter_map(|role| record.ports.get(&role).copied())
        .collect();
    // And the ports its own *containers* are holding, for exactly the same
    // reason. Without these, a second isolated start of a running worktree
    // reads its own database as somebody else's listener, decides the
    // window was taken, and moves every port — web included — leaving the
    // live application pointed at ports nothing is on.
    if isolate {
        keep.extend(record.services.iter().filter_map(|service| service.port));
    }
    // The shared window, as it was before this start re-derives it. A
    // switch that fails puts it back: the processes kept serving through
    // it are still on these numbers, whatever the new window said.
    let shared_ports = record.ports.clone();

    let assignment = ports::assign_keeping(paths, &mut store, name, &roles, &keep)?;

    // Which process owns which role, for the whole worktree and whatever
    // `--only` asked for, recorded beside the ports themselves. The URL
    // rule has to be answerable from the record alone — `status` and the
    // TUI never see config — and it has to give the same answer after a
    // stop as `start` gave, so this outlives the processes exactly as the
    // ports do.
    let owners: BTreeMap<String, Vec<String>> = config
        .processes
        .iter()
        .map(|(process, config)| (process.clone(), config.roles()))
        .filter(|(_, roles)| !roles.is_empty())
        .collect();
    // And which of them serve no page, so the URL skips their roles the
    // same way from the record alone.
    let pageless: std::collections::BTreeSet<String> = config
        .processes
        .iter()
        .filter(|(process, config)| owners.contains_key(*process) && !config.serves_page())
        .map(|(process, _)| process.clone())
        .collect();
    // The containers of services that were compose and are native now,
    // stopped once the lock is let go: the port the native server is about
    // to be given is the one they publish. Their compose records stay
    // until they are stopped, and a project no other record names is one
    // `rm` cannot take the data of after that, so the start says how.
    let mut changed_kind: Vec<(String, Vec<String>)> = Vec::new();
    let mut unnamed: Vec<String> = Vec::new();
    // Every service that changed kind, in either direction: each is on a
    // server of the other kind now, with other data.
    let mut moved_kinds: Vec<(String, state::ServiceKind)> = Vec::new();
    if let Some(record) = store.worktrees.get_mut(name) {
        record.roles = owners;
        record.pageless = pageless;
        if isolate {
            moved_kinds = changed_kinds(config, record);
            changed_kind = leave_changed_kinds(paths, config, name, record)?;
            record.services = planned_services(config, &assignment.ports, record);
            unnamed = changed_kind
                .iter()
                .filter(|(project, replaced)| {
                    !record.services.iter().any(|s| {
                        s.kind == state::ServiceKind::Compose
                            && s.compose_project.as_ref() == Some(project)
                            && !replaced.contains(&s.name)
                    })
                })
                .map(|(project, _)| project.clone())
                .collect();
        }
        // Written into the record this start saves and spawns from, not
        // left to what `prepare` wrote: a namespace nothing records is one
        // `rm` can never drop.
        for namespace in ready.iter().flat_map(|ready| &ready.namespaces) {
            namespaced::keep(record, namespace);
        }
    }
    // The files `provision` names, before anything runs that reads them:
    // given to a worktree pando created that lacks one, and said of one it
    // did not. Under the lock, as `new` gives them, so two starts do not
    // both decide a file is missing. Not the main checkout, which is where
    // they come from, nor a start with nothing to start.
    if !everything_up && !main {
        let created = store
            .worktrees
            .get(name)
            .is_some_and(|record| record.created_by_pando);
        provision_at_start(paths, config, &canonical, created, progress);
    }
    // Written down before anything is brought up: a start that fails
    // halfway must still leave `rm` able to name the compose project and
    // take its volumes with it.
    state::save(&paths.state_file(), &store)?;
    // And unlocked from here to the spawn. An install takes minutes, a
    // database takes seconds to become ready, and a migration takes as
    // long as it takes; holding the state lock through any of them would
    // freeze `pando ls` and the TUI's tick.
    drop(_lock);

    // Every failure between here and the services being up leaves a
    // worktree that is still the shared one it was: nothing of the
    // isolated attempt may outlive it.
    let undo = |e: anyhow::Error| -> anyhow::Error {
        if becoming_isolated {
            undo_failed_isolation(paths, config, name, &shared_ports);
        }
        e
    };

    // A service on a server of another kind is on other data, which a
    // hook's fingerprint cannot see any more than it sees a mode change —
    // see `forget_hooks_after_services`. Forgotten now, before anything
    // that can fail: the record already says the new kind, so the start
    // that retries a failure below would not see the change again.
    let moved_why = moved_kinds_why(&moved_kinds);
    if let Some(why) = &moved_why {
        forget_hooks_after_services(paths, config, name, why, progress).map_err(undo)?;
    }

    // Outside the lock, like every other compose call: `docker compose
    // stop` takes seconds, and holding the state lock through it would
    // freeze `pando ls` and the TUI's tick.
    if !going_shared.is_empty() {
        progress(&match was_isolated {
            true if target == ServiceMode::Namespaced => format!(
                "{} moves to namespaces of its own in the project's services — stopping its \
                 private ones",
                worktree.display_name()
            ),
            true => format!(
                "{} is going back to the project's shared services — stopping its own",
                worktree.display_name()
            ),
            false => format!(
                "{} runs on the project's shared services — stopping the private ones an \
                 interrupted start left running",
                worktree.display_name()
            ),
        });
        stop_containers(paths, &going_shared, progress)?;
    }
    for (project, services) in &changed_kind {
        let (verb, whose) = match services.len() {
            1 => ("runs", "its compose container"),
            _ => ("run", "their compose containers"),
        };
        let mut line = format!(
            "{}: {} {verb} natively now — stopping {whose}",
            worktree.display_name(),
            services.join(", ")
        );
        if unnamed.contains(project) {
            line.push_str(&format!(
                "; nothing else of it is in {project}, so the data stays until `docker compose \
                 -p {project} down -v` removes it"
            ));
        }
        progress(&line);
    }
    let unasked = stop_service_containers(paths, &changed_kind, progress).map_err(undo)?;
    replace_stopped_containers(paths, name, &changed_kind, &unasked).map_err(undo)?;
    // And the containers a stale record replaced above left up, on a start
    // that is not about to bring that compose project up again itself.
    if !isolate && !inherited_projects.is_empty() {
        progress(&format!(
            "{} was last started at another path — stopping the private services it left running",
            worktree.display_name()
        ));
        stop_containers(paths, &inherited_projects, progress)?;
    }

    // Everything the app is told about where its services are. Computed
    // from the allocated ports alone, so a hook that runs before the
    // containers exist sees exactly what the processes will.
    let service_env = match (&ready, isolate) {
        (_, true) => {
            resolve_service_env(paths, config, &canonical, &assignment.ports).map_err(undo)?
        }
        (Some(ready), false) => {
            namespaced::namespaced_env(paths, config, &ready.plan, &ready.namespaces, name)?
        }
        (None, false) => shared_service_env(paths, config),
    };

    // The lifecycle in order: create (which is where the install step
    // lives), install, the services coming up, then services. Every hook
    // is gated by its own fingerprint, so a start that changes nothing
    // runs none of them.
    //
    // A worktree that is already running every process it was asked for
    // is not starting anything, so nothing is re-run for it either: `npm
    // ci` inside a live worktree is a surprise nobody asked for.
    //
    // A namespaced start is on data of its own only where its database is:
    // one that got a Redis slot and left its database on the main
    // checkout's would otherwise run the branch's migrations against main.
    //
    // The main checkout runs none of them, nor a probe: each is a command
    // run in the repository, and Invariant 1 has no exception for it. It
    // is the developer's own checkout, installed and migrated by them.
    let not_own = ready.as_ref().and_then(Ready::not_own_data);
    let hook_ctx = HookContext {
        name,
        branch: worktree.branch.as_deref(),
        worktree: &canonical,
        ports: &assignment.ports,
        service_env: &service_env,
        own_data: isolate || (ready.is_some() && not_own.is_none()),
        not_own: not_own.as_deref(),
    };
    if !everything_up && !main {
        run_hooks(
            paths,
            config,
            config::HookPoint::Create,
            &hook_ctx,
            progress,
        )
        .map_err(undo)?;
        run_hooks(
            paths,
            config,
            config::HookPoint::Install,
            &hook_ctx,
            progress,
        )
        .map_err(undo)?;
    }

    let mut fresh = Fresh(false);
    if isolate {
        fresh = bring_up_services(paths, config, name, &canonical, &assignment.ports, progress)
            .map_err(undo)?;
    }

    // The database the hooks after this point run against has changed, and
    // a fingerprint cannot see a database. A mode change swaps it in either
    // direction; a data directory initialised just now is a new and empty
    // one even without a mode change, which is what a developer who
    // deleted it by hand gets. Either way the recorded answer to "have the
    // inputs changed" is wrong, so it is discarded rather than trusted —
    // and the hooks are let through even on a worktree that was otherwise
    // fully up, because an empty schema is not "nothing to do".
    let made_now = ready.as_ref().is_some_and(|ready| ready.fresh);
    let why = match (mode_changed, fresh.0, made_now) {
        (true, _, _) if isolate => Some("this worktree now runs its own services"),
        (true, _, _) if target == ServiceMode::Namespaced => {
            Some("this worktree now runs on namespaces of its own")
        }
        (true, _, _) => Some("this worktree is back on the project's shared services"),
        (_, true, _) => Some(FRESH_DATA_DIR),
        (_, _, true) => Some("a database of this worktree's own was made just now"),
        _ => None,
    };
    if let Some(why) = why {
        forget_hooks_after_services(paths, config, name, why, progress).map_err(undo)?;
        everything_up = false;
    }
    // A service that changed kind had its hooks forgotten above, and its
    // new server gets them and the probes even under live processes.
    if moved_why.is_some() {
        everything_up = false;
    }

    if !everything_up && !main {
        if !skip_after_services {
            run_hooks(
                paths,
                config,
                config::HookPoint::Services,
                &hook_ctx,
                progress,
            )
            .map_err(undo)?;
        }
        // The last gate before anything is spawned: a probe that
        // recognises the failure stops the start and says how to fix it,
        // rather than letting the dev server die of it thirty seconds
        // later with the reason buried in a log.
        run_probes(paths, config, &hook_ctx, progress).map_err(undo)?;
    }

    let _lock = state::lock(&paths.lock_file()).map_err(undo)?;
    // The undo takes this same lock, and a second `flock` from this process
    // on a second descriptor waits for the first one: undoing with it held
    // is a start that never returns. Every undo below lets go first.
    let mut store = match state::load(&paths.state_file()) {
        Ok(store) => store,
        Err(e) => {
            drop(_lock);
            return Err(undo(e));
        }
    };
    // The record this call left behind, unless something removed the
    // worktree while the services were coming up.
    let record = store
        .worktrees
        .entry(name.to_string())
        .or_insert_with(|| WorktreeRecord::new(canonical.clone(), false));

    // And the namespaces this start was given have to still be this
    // worktree's. Its record named them when the lock was let go, with none
    // of its processes up, so another worktree's namespaced start that found
    // every slot held could offer it as stopped, empty its slot, forget it
    // and give it out again. Spawning past that put this app on a slot only
    // another record names, whose `rm` empties it under the app.
    if let Some(gone) = ready
        .iter()
        .flat_map(|ready| &ready.namespaces)
        .find(|namespace| {
            !record
                .namespaces
                .iter()
                .any(|ns| crate::namespace::same_namespace(ns, namespace))
        })
    {
        return Err(anyhow::anyhow!(
            "{} was freed from this worktree while this start was starting, so nothing was \
             started here — start it again, and it gets one of its own",
            crate::namespace::describe(gone)
        ));
    }

    // The lock was let go for the hooks and the services, and another start
    // of this worktree — the TUI's key pressed twice, or the TUI and the
    // CLI at once — may have spawned a process in the meantime. Spawning a
    // second one would overwrite that record, and the first group would go
    // on holding the port with nothing in pando able to find it again.
    //
    // But only a process running in *this* start's mode is one to keep. A
    // worktree's processes always run in the mode its record says — the
    // flag is set, under this lock, right before any of them is spawned —
    // so a live process under a flag that disagrees with this start talks
    // to the other services: the ones this start kept serving while it
    // brought its own up, or a plain start's that raced it through the
    // unlocked window. Those are replaced, never adopted, or an isolated
    // worktree would be running an application on the shared database.
    //
    // And the services this start brought up have to still be the ones on
    // record. A start on the shared services that ran through the unlocked
    // window — the TUI and the CLI at once — takes down whatever private
    // services it finds on a worktree that is not isolated yet, and blanks
    // their ports. Spawning past that would set the flag on a worktree
    // with no services at all, its application pointed at a database that
    // had just been stopped. Nothing is undone: the start that took them
    // down left a consistent shared worktree — its own ports, its own
    // processes — and an undo would write this start's older picture over it.
    //
    // A `restart --only` keeps nothing it names, whatever mode it runs in:
    // replacing it is what the restart is for.
    if isolate {
        let lost: Vec<String> = service_roles(config)
            .into_iter()
            .filter(|role| {
                let wanted = assignment.ports.get(role).copied();
                !record
                    .services
                    .iter()
                    .any(|s| &s.name == role && s.port.is_some() && s.port == wanted)
            })
            .collect();
        if !lost.is_empty() {
            return Err(anyhow::anyhow!(
                "another start of this worktree took down or moved {} while this one was \
                 waiting on {} — so nothing was started here; start it again with the mode you \
                 want",
                lost.join(", "),
                match lost.len() {
                    1 => "it",
                    _ => "them",
                }
            ));
        }
    }
    let running = record.mode();
    let mut replace: Vec<(String, proc::Group)> = Vec::new();
    for (process_name, _) in &selection {
        if already.contains(process_name) {
            continue;
        }
        let Some(p) = record.processes.get(process_name) else {
            continue;
        };
        let live = matches!(p.phase, Phase::Starting { .. } | Phase::Running { .. })
            && p.alive(proc::is_alive, proc::group_alive);
        if !live {
            continue;
        }
        if running == target && !restarting {
            progress(&format!("{process_name} is already running"));
            already.push(process_name.clone());
        } else {
            replace.push((process_name.clone(), p.pgid));
        }
    }

    // Planned in full before anything is stopped: a command that does not
    // render must not cost the processes this start was about to replace.
    let planned = match plan_processes(
        paths,
        config,
        name,
        &worktree,
        &canonical,
        &selection,
        &already,
        &assignment.ports,
        &service_env,
    ) {
        Ok(planned) => planned,
        Err(e) => {
            drop(_lock);
            return Err(undo(e));
        }
    };

    if !replace.is_empty() && running != target {
        progress(match target {
            ServiceMode::Isolated => {
                "its own services are ready, so its processes restart against them"
            }
            ServiceMode::Namespaced => {
                "its own namespaces are ready, so its processes restart against them"
            }
            ServiceMode::Shared => {
                "the services this worktree talks to changed, so its processes restart"
            }
        });
    }
    for (process_name, pgid) in &replace {
        if let Err(e) = proc::stop(*pgid, STOP_GRACE) {
            drop(_lock);
            return Err(undo(e.context(format!("stopping {process_name}"))));
        }
        if let Some(record) = store.worktrees.get_mut(name) {
            record.processes.remove(process_name);
        }
    }
    // Isolation is remembered from here on: the services are up and ready,
    // and every process spawned below is given their addresses. Set only
    // now, not when the services came up, because a worktree's processes
    // run in the mode this flag says — see above — and until this point
    // they were the shared ones.
    if let Some(record) = store.worktrees.get_mut(name) {
        record.mode = Some(target);
    }

    let mut started: Vec<StartedProcess> = Vec::new();
    let mut failure: Option<anyhow::Error> = None;
    for plan in planned {
        progress(&format!("starting {}", plan.process));
        // Truncated, not appended: the classifier reads the tail of this
        // file to explain a failure, and the closing lines of the
        // *previous* run would be a confident wrong answer.
        let spawned = reset_log(&plan.log_file).and_then(|()| {
            proc::spawn_detached(SpawnOptions {
                shell_cmd: &plan.shell_cmd,
                cwd: &plan.cwd,
                log_file: &plan.log_file,
                env: &plan.env,
                status_file: Some(&crate::paths::exit_status_file(&plan.log_file)),
            })
        });
        let spawn = match spawned {
            Ok(spawn) => spawn,
            // Whatever came up before this one is already running and
            // already in the map. The state is saved below either way, so
            // the failure never leaves a process nothing can stop.
            Err(e) => {
                failure = Some(e.context(format!("starting process {}", plan.process)));
                break;
            }
        };
        let now = Utc::now();
        let record = ProcessRecord {
            pid: spawn.pid,
            pgid: spawn.pgid,
            started_at: now,
            log_path: plan.log_file,
            ready_port: plan.ready_port,
            ready_timeout_s: plan.ready_timeout_s,
            observed_ports: Vec::new(),
            swept: false,
            phase: Phase::Starting { since: now },
        };
        let worktree = store
            .worktrees
            .get_mut(name)
            .expect("the record was just inserted");
        worktree.last_started = Some(now);
        worktree
            .processes
            .insert(plan.process.clone(), record.clone());
        started.push(StartedProcess {
            process: plan.process,
            record,
        });
    }
    state::save(&paths.state_file(), &store)?;
    if let Some(e) = failure {
        return Err(e);
    }

    let already_running: Vec<StartedProcess> = already
        .iter()
        .filter_map(|process| {
            let record = store.worktrees.get(name)?.processes.get(process)?;
            Some(StartedProcess {
                process: process.clone(),
                record: record.clone(),
            })
        })
        .collect();
    // The last point, and the only one that runs with the processes up.
    // Outside the lock, because a `dev` hook is a command like any other
    // and holding the lock through it would freeze the TUI's tick.
    drop(_lock);
    if !everything_up && !skip_after_services && !main {
        run_hooks(paths, config, config::HookPoint::Dev, &hook_ctx, progress)?;
    }
    // From the record, through the one function every read path uses, so
    // that `pando start` and a `pando status` a second later cannot
    // disagree about the URL of the same worktree. None when a `--only`
    // left out the process it points at: nothing answers it.
    let url = store
        .worktrees
        .get(name)
        .filter(|record| url_owner_not_running(record).is_none())
        .and_then(worktree_url);
    Ok(StartReport {
        worktree: name.to_string(),
        started,
        already_running,
        url,
        ports: assignment.ports,
        reassigned: assignment.reassigned,
    })
}

/// Renders every process a start is about to spawn — its command, its
/// directory, its environment — without spawning anything. `already` are
/// the ones left as they are.
#[allow(clippy::too_many_arguments)]
fn plan_processes(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    worktree: &Worktree,
    canonical: &Path,
    selection: &[(String, &ProcessConfig)],
    already: &[String],
    ports: &BTreeMap<String, u16>,
    service_env: &BTreeMap<String, String>,
) -> Result<Vec<Planned>> {
    let mut planned: Vec<Planned> = Vec::new();
    for (process_name, process) in selection {
        if already.contains(process_name) {
            continue;
        }
        let process_roles = process.roles();
        let ready_role = ready_role(process, &process_roles)
            .with_context(|| format!("in process {process_name}"))?;
        let ready_port = ready_role.as_deref().and_then(|r| ports.get(r)).copied();
        let log_file = paths.log_file(name, process_name);
        let ctx = template::Context {
            name,
            branch: worktree.branch.as_deref(),
            worktree: canonical,
            root: paths.root(),
            project: paths.project_id(),
            // Every role of the worktree, not only this process's own:
            // `{port:api}` inside the web process's env is how one process
            // is told where another one is listening.
            ports,
            default_role: ready_role.as_deref(),
            log: Some(&log_file),
        };
        let cmd = template::render_shell(&process.cmd, &ctx)
            .with_context(|| format!("in the command for process {process_name}"))?;
        let cwd = process_cwd(canonical, process_name, process, &ctx)?;
        let env = process_env(paths, name, worktree, process, service_env, &ctx)?;
        planned.push(Planned {
            process: process_name.clone(),
            shell_cmd: with_prelude(config, &cmd),
            cwd,
            env,
            log_file,
            ready_port,
            ready_timeout_s: process.ready.as_ref().and_then(|r| r.timeout_s),
        });
    }
    Ok(planned)
}

/// Whether every process a start was asked for is already up. Best effort
/// and lock-free: every caller re-decides under the lock.
fn every_process_running(
    paths: &PandoPaths,
    name: &str,
    selection: &[(String, &ProcessConfig)],
) -> bool {
    let Ok(store) = state::load(&paths.state_file()) else {
        return false;
    };
    let Some(record) = store.worktrees.get(name) else {
        return false;
    };
    selection.iter().all(|(process, _)| {
        record.processes.get(process).is_some_and(|p| {
            matches!(p.phase, Phase::Starting { .. } | Phase::Running { .. })
                && p.alive(proc::is_alive, proc::group_alive)
        })
    })
}

/// Stops a worktree's processes — every one it is running, or the one
/// `only` names. The worktree, its ports, and its logs survive.
pub fn stop(
    paths: &PandoPaths,
    name: &str,
    only: Option<&str>,
    progress: &dyn Fn(&str),
) -> Result<StopOutcome> {
    paths.ensure_home()?;
    let mut projects: Vec<String> = Vec::new();
    let outcome = {
        let _lock = state::lock(&paths.lock_file())?;
        let mut store = state::load(&paths.state_file())?;
        let shared = public_url(&store, name);
        let outcome = stop_recorded(&mut store, name, only, &mut projects)?;
        say_if_closed(&store, name, shared, progress);
        // `reconcile` drops dead-leader records for every worktree in the
        // project, not only this one, so every one is signalled first — and
        // a sibling's half-dead share along with them.
        for notice in sweep_orphaned_groups(&mut store)? {
            progress(&notice);
        }
        advance_before_reconcile(&mut store);
        state::reconcile(&mut store, proc::is_alive, proc::group_alive);
        if let Some(record) = store.worktrees.get(name) {
            clear_native_sockets(paths, name, record);
        }
        state::save(&paths.state_file(), &store)?;
        outcome
    };
    // Outside the lock: `docker compose stop` takes as long as the
    // containers take to shut down, and the records that name the project
    // are already saved, so a failure here is recoverable by running
    // `stop` again.
    stop_containers(paths, &projects, progress)?;
    Ok(outcome)
}

/// Stops every worktree pando has a process for, returning their names.
pub fn stop_all(paths: &PandoPaths, progress: &dyn Fn(&str)) -> Result<Vec<String>> {
    stop_all_with(paths, None, |pgid| proc::stop(pgid, STOP_GRACE), progress)
        .map(|report| report.stopped)
}

/// [`stop_all`] once a confirmation has shown `listed` as what is up. A
/// worktree up now that it did not name came up after it was shown — an
/// agent started or shared it while the question was open — and is left
/// running, said to be, and reported as kept. What is not up goes as it
/// does for [`stop_all`]: a crashed dev server's containers were never
/// listed.
pub fn stop_all_listed(
    paths: &PandoPaths,
    listed: &[String],
    progress: &dyn Fn(&str),
) -> Result<StopAllReport> {
    stop_all_with(
        paths,
        Some(listed),
        |pgid| proc::stop(pgid, STOP_GRACE),
        progress,
    )
}

/// [`stop_all`] or [`stop_all_listed`] with the signal injected, so a
/// test can drive the path where a group refuses to die without needing
/// one that really does.
pub fn stop_all_with(
    paths: &PandoPaths,
    listed: Option<&[String]>,
    stop: impl Fn(proc::Group) -> Result<()>,
    progress: &dyn Fn(&str),
) -> Result<StopAllReport> {
    paths.ensure_home()?;
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    // Services as well as processes: a worktree whose dev server crashed
    // still has a database up, and `stop` with no name is how you make
    // sure nothing of pando's is left running. A share too: its tunnel
    // outlives the processes it pointed at.
    let mut names: Vec<String> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    for (name, record) in &store.worktrees {
        if record.processes.is_empty() && record.services.is_empty() && record.share.is_none() {
            continue;
        }
        if listed.is_some_and(|listed| !listed.contains(name)) && record.is_live() {
            progress(&format!(
                "{} came up after the list of what stops was shown — left running",
                worktree::label(name)
            ));
            kept.push(name.clone());
            continue;
        }
        names.push(name.clone());
    }
    let mut stopped = Vec::new();
    let mut failures = Vec::new();
    let mut projects: Vec<String> = Vec::new();
    for name in names {
        // One worktree that will not die must not leave the rest running —
        // and must not lose its record either. The failures are collected
        // and reported once every other group has been signalled.
        let shared = public_url(&store, &name);
        match stop_recorded_with(&mut store, &name, None, &stop, &mut projects) {
            Ok(StopOutcome::Stopped(_)) => {
                say_if_closed(&store, &name, shared, progress);
                stopped.push(name);
            }
            Ok(StopOutcome::NotRunning) => {}
            Err(e) => failures.push(format!("stopping {}: {e:#}", worktree::label(&name))),
        }
    }
    // Nothing is dropped while a group is still unaccounted for: the pgid
    // in that record is the only way back to it.
    let mut sweep_failed = None;
    if failures.is_empty() {
        match sweep_orphaned_groups_with(&mut store, &stop) {
            Ok(notices) => {
                for notice in notices {
                    progress(&notice);
                }
                advance_before_reconcile(&mut store);
                state::reconcile(&mut store, proc::is_alive, proc::group_alive);
            }
            Err(e) => sweep_failed = Some(e),
        }
    }
    for name in &stopped {
        if let Some(record) = store.worktrees.get(name) {
            clear_native_sockets(paths, name, record);
        }
    }
    // Saved either way, so the groups that *were* signalled do not come
    // back as phantom records on the next read.
    state::save(&paths.state_file(), &store)?;
    drop(_lock);
    if !failures.is_empty() {
        bail!("{}", failures.join("; "));
    }
    if let Some(e) = sweep_failed {
        return Err(e);
    }
    stop_containers(paths, &projects, progress)?;
    Ok(StopAllReport { stopped, kept })
}

/// The public URL `name` is shared at, read before a stop so that one which
/// closes it can say which.
fn public_url(store: &state::State, name: &str) -> Option<String> {
    let share = store.worktrees.get(name)?.share.as_ref()?;
    Some(share.public_url.clone())
}

/// Says so when the stop that just ran closed the share `shared` was read
/// from, restart's stop half included: a URL that stops answering with
/// nothing said is one the developer finds out about from whoever they
/// sent it to.
fn say_if_closed(
    store: &state::State,
    name: &str,
    shared: Option<String>,
    progress: &dyn Fn(&str),
) {
    let Some(url) = shared else { return };
    if store.worktrees.get(name).is_some_and(|r| r.share.is_none()) {
        progress(&share_closed(name, &url));
    }
}

/// Signals the process groups recorded for `name` and drops their records.
/// The caller holds the lock and saves.
///
/// The signal is unconditional, Failed records included. pando is not the
/// process's parent by then, so "failed" only ever meant "its leader is
/// gone" — the group can still be serving. Clearing them is this path's job
/// too: a `Failed` record survives `reconcile` on purpose, and a stop of
/// the process it belongs to is one of the three things that ends it (the
/// others being a start of it and an `rm` of its worktree).
///
/// The one record dropped without a signal is one the orphan sweep has
/// already signalled: its group had its SIGTERM and SIGKILL then, and this
/// stop can come days later, when its pgid names somebody else's group.
pub(super) fn stop_recorded(
    store: &mut state::State,
    name: &str,
    only: Option<&str>,
    services_to_stop: &mut Vec<String>,
) -> Result<StopOutcome> {
    stop_recorded_with(
        store,
        name,
        only,
        |pgid| proc::stop(pgid, STOP_GRACE),
        services_to_stop,
    )
}

/// What `--only <name>` gets when the worktree is not running that
/// process: an error naming what it *is* running, because "not running"
/// would read as "nothing to do" for a typo — and a typo must never be
/// answered by taking the database down. `stop` cannot see config — it
/// has to work when `pando.toml` is broken — so the record is the only
/// thing it can check a name against.
fn missing_only(only: Option<&str>, record: &WorktreeRecord) -> Result<StopOutcome> {
    let running: Vec<&str> = record.processes.keys().map(String::as_str).collect();
    bail!(
        "this worktree is not running a process named {:?} — it is running: {}",
        only.unwrap_or_default(),
        if running.is_empty() {
            "nothing".to_string()
        } else {
            running.join(", ")
        }
    )
}

pub(super) fn stop_recorded_with(
    store: &mut state::State,
    name: &str,
    only: Option<&str>,
    stop: impl Fn(proc::Group) -> Result<()>,
    services_to_stop: &mut Vec<String>,
) -> Result<StopOutcome> {
    let Some(record) = store.worktrees.get_mut(name) else {
        return Ok(StopOutcome::NotRunning);
    };
    // A worktree whose processes are all down may still have containers
    // up: it was started isolated and then every process crashed. `stop`
    // is how you make sure, so the services are taken down either way —
    // but only when nothing asked for a subset. `--only dev` is about one
    // process, and the database its siblings use is not that process; a
    // name the worktree is not running is the same error it is when
    // something *is* running, not a silent whole-worktree stop.
    if record.processes.is_empty() {
        if only.is_some() {
            return missing_only(only, record);
        }
        // Read before anything is signalled: a native service *is* its
        // process, so a worktree whose only service is a database has
        // something to stop even though it has no compose project and no
        // process record at all. A compose service is up while its log
        // pump is; a record that only names the project is what every
        // stopped isolated worktree keeps for `rm`, until `rm`, and is not
        // something running.
        let had_service = record.services.iter().any(|s| s.pgid.is_some());
        let mut failures = stop_service_pumps(record, &stop);
        // A worktree whose every process crashed can still be shared: the
        // tunnel outlives them, and a public URL onto nothing is the worst
        // of both worlds.
        let was_shared = take_share_down(record, &stop, &mut failures);
        let projects = compose_projects(record);
        if !failures.is_empty() {
            bail!("{name}: {}", failures.join("; "));
        }
        // Asked of Docker either way, because a container can outlive the
        // pump in front of it; but saying "stopped" of a worktree that was
        // stopped last week is a report of something that did not happen.
        services_to_stop.extend(projects);
        if !was_shared && !had_service {
            return Ok(StopOutcome::NotRunning);
        }
        return Ok(StopOutcome::Stopped(Vec::new()));
    }
    let groups: Vec<(String, proc::Group, bool)> = record
        .processes
        .iter()
        .filter(|(process, _)| only.is_none_or(|wanted| process.as_str() == wanted))
        .map(|(process, p)| (process.clone(), p.pgid, p.swept))
        .collect();
    if groups.is_empty() {
        return missing_only(only, record);
    }
    let mut stopped = Vec::new();
    let mut failures = Vec::new();
    for (process, pgid, swept) in groups {
        // Signal first, drop second. A record cleared for a group that was
        // never signalled is a process nothing can find again.
        let signalled = match swept {
            true => Ok(()),
            false => stop(pgid),
        };
        match signalled {
            Ok(()) => {
                record.processes.remove(&process);
                stopped.push(process);
            }
            Err(e) => failures.push(format!("{process} (group {pgid}): {e:#}")),
        }
    }
    if record.processes.is_empty() {
        record.observed_ports.clear();
    }
    // A worktree-wide stop takes its services with it; `--only` is about
    // one process and leaves the database its siblings are still using.
    if only.is_none() {
        failures.extend(stop_service_pumps(record, &stop));
        services_to_stop.extend(compose_projects(record));
    }
    // And the public URL, once nothing is left for it to point at. A
    // `--only` stop of a process the URL does not point at leaves the
    // share up, because what it publishes is still serving; a `--only`
    // stop of the one it does point at, or of the last one, does not,
    // because a tunnel onto nothing is worse than no tunnel. Phase alone:
    // what this stop signalled is already out of the record.
    let still_serving = share_target_is_up(record, &|_| true);
    if only.is_none() || !still_serving {
        take_share_down(record, &stop, &mut failures);
    }
    if !failures.is_empty() {
        bail!("{name}: {}", failures.join("; "));
    }
    Ok(StopOutcome::Stopped(stopped))
}

/// Signals every process group in the project whose leader is dead, so that
/// no record is ever dropped without being signalled first.
///
/// `reconcile` drops dead-leader records for *every* worktree in the state
/// file, while an action only signals the worktree it was asked about. That
/// seam is how a sibling worktree — one whose `bash -lc` exited while a
/// child it backgrounded still holds a port — loses its record and leaves a
/// process nothing can find again. So the sweep is global, and runs before
/// anything that drops records.
///
/// A half-dead share is the same failure with a public URL attached, so it
/// is swept here too: [`sweep_dead_shares`] is the only thing that can
/// signal one, and `reconcile` would otherwise drop the record holding the
/// surviving half's pgid. Its notices come back to the caller, which is the
/// only place that knows where to print them.
///
/// One group that will not die does not stop the sweep: the rest are still
/// signalled and the failures are reported together. A caller that gets an
/// error must not go on to drop records.
pub(super) fn sweep_orphaned_groups(store: &mut state::State) -> Result<Vec<String>> {
    sweep_orphaned_groups_with(store, |pgid| proc::stop(pgid, STOP_GRACE))
}

/// [`sweep_orphaned_groups`] with the signal injected, so a test can watch
/// which groups it decides to signal without needing real ones.
pub(super) fn sweep_orphaned_groups_with(
    store: &mut state::State,
    stop: impl Fn(proc::Group) -> Result<()>,
) -> Result<Vec<String>> {
    // First, because a share is the one record whose survivor is a public
    // door: a tunnel nobody can name again is worse than a dev server
    // nobody can name again.
    let notices = sweep_dead_shares_with(store, proc::is_alive, proc::group_alive, &stop);
    let mut failures = Vec::new();
    for (name, record) in &mut store.worktrees {
        for (process, p) in &mut record.processes {
            // Once, not on every mutation. A dead leader is not a dead
            // group, so the group is signalled — but a `Failed` record now
            // survives `reconcile` until its own worktree is started,
            // stopped or removed (each of which clears it on its own path,
            // and signals it only if this has not), and re-sending
            // SIGTERM/SIGKILL to that pgid on every later mutation in the
            // project is how a pid that has since wrapped around onto an
            // unrelated session leader gets killed. One signal per record
            // bounds that to the window between the leader dying and the
            // first mutation after it — and so does saving the flag: a
            // mutation that sweeps and then refuses still writes it down.
            //
            // Alive by the read path's rule, not the leader alone: a
            // process that owns no port and backgrounded itself is one
            // `status` calls Running, and `reconcile` keeps its record, so
            // there is no orphan here to signal — only an app to kill.
            if p.swept || p.alive(proc::is_alive, proc::group_alive) {
                continue;
            }
            match stop(p.pgid) {
                // Recorded only once the signal actually went out: a group
                // that could not be signalled has to be tried again.
                Ok(()) => p.swept = true,
                Err(e) => failures.push(format!("{name}/{process} (group {}): {e:#}", p.pgid)),
            }
        }
        // A log pump is a process group like any other, and `reconcile`
        // forgets its pid the moment its leader dies — so it is signalled
        // here first, or a `docker compose logs -f` whose leader exited
        // keeps a child attached to the daemon with nothing able to name
        // it again.
        for service in record.services.iter_mut() {
            let (Some(pid), Some(pgid)) = (service.pid, service.pgid) else {
                continue;
            };
            if proc::is_alive(pid) {
                continue;
            }
            match stop(pgid) {
                Ok(()) => {
                    service.pid = None;
                    service.pgid = None;
                }
                Err(e) => failures.push(format!(
                    "{name}/{} log pump (group {pgid}): {e:#}",
                    service.name
                )),
            }
        }
    }
    if failures.is_empty() {
        return Ok(notices);
    }
    bail!(
        "could not signal {} process group(s) before dropping their records: {}",
        failures.len(),
        failures.join("; ")
    )
}

/// Stop, then start. The ports come back from the record `stop` left
/// behind, so a restart keeps the URL — and `--only` restarts one process
/// while the rest keep serving on the ports they already have, as a start
/// that replaces it: its public URL, if it has one, stays up.
pub fn restart(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    only: Option<&str>,
    mode: Mode,
    progress: &dyn Fn(&str),
) -> Result<StartReport> {
    // Against config, and before anything is signalled: `stop` can only
    // check a name against what is running, so a typo at `--only` used to
    // get an error about the record — a different message depending on
    // unrelated state, and never the one that lists the names config
    // declares.
    selected_processes(config, only)?;
    // And a worktree whose directory is gone, which the start half would
    // refuse only after the stop half had run and the namespaces were made.
    // And the main checkout in a mode it cannot run in, for the same reason.
    let Checkout { worktree, main } = find_live_checkout(paths, name)?;
    if main {
        refuse_main_mode(name, mode)?;
    }
    refuse_losing_namespaces(paths, config, name, mode)?;
    let was = recorded_mode(paths, name);
    let target = target_of(paths, config, name, mode);
    // Before the stop below, not after it: a refusal that has already
    // taken the process down is not a refusal.
    if mode_would_change(paths, config, name, mode) {
        refuse_only_across_a_mode_change(only, target)?;
    }
    // And every precondition of the isolated start that follows, for the
    // same reason: a restart whose start half cannot succeed must not get
    // to run its stop half. Asked of every isolated restart, not only a
    // mode change, because the stop takes the containers down too.
    let preflighted = target == ServiceMode::Isolated;
    if preflighted {
        let canonical =
            std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
        preflight_isolation(
            paths,
            config,
            name,
            &canonical,
            &worktree_roles(config, true),
        )?;
    }
    // A `--only` restart has no stop half: its start replaces the process
    // it names, under the lock the successor is spawned under, so a share
    // of it never has nothing behind it. And a name config does know,
    // whose process is simply not up, is just started. Bringing a stopped
    // process back is the one thing `restart --only` exists for, and it
    // used to be the one thing it could not do — but only while a sibling
    // was still running, which made the failure look random.
    //
    // Nor does one on the way onto data of its own: that start replaces
    // every process anyway, and only once the worktree's own services or
    // namespaces are ready. A stop first would throw that away — a restart
    // whose services then failed to come up would have cost the developer
    // the dev server it could not replace.
    //
    // A namespaced restart's namespaces are made before the stop, for the
    // same reason the isolated preflight runs before it.
    let namespaces = match target {
        ServiceMode::Namespaced => Some(namespaced::prepare(paths, config, name, progress)?),
        _ => None,
    };
    let onto_own_data =
        target != was && target != ServiceMode::Shared && was != ServiceMode::Isolated;
    if only.is_none() && !onto_own_data {
        stop(paths, name, None, progress)?;
    }
    start_checked(
        paths,
        config,
        name,
        only,
        mode,
        Preflight {
            isolation: preflighted,
            namespaces,
            restarting: only.is_some(),
            skip_after_services: false,
        },
        progress,
    )
}

/// Refuses a start of the main checkout in a mode that would give it data
/// of its own.
///
/// The project's shared services are the main checkout's own — they are
/// what main is — and `--isolated` and `--namespaced` exist to give a
/// worktree data apart from main's. Checked before anything is asked,
/// made or stopped.
pub(super) fn refuse_main_mode(name: &str, mode: Mode) -> Result<()> {
    let flag = match mode {
        Mode::Isolated => "--isolated",
        Mode::Namespaced => "--namespaced",
        Mode::Remembered | Mode::Shared => return Ok(()),
    };
    bail!(
        "{name} is the main checkout, and it runs on the project's own services, which hold \
         its own data — {flag} gives a worktree data apart from main's, so it is for worktrees"
    )
}

/// Refuses `--only` on a start that would change which services the
/// worktree talks to.
///
/// A mode change replaces every process's environment: the database
/// address they were given is about to point somewhere else. `--only dev`
/// through that change restarts `dev` against the new services and leaves
/// `api` running against the old ones — two halves of one application
/// talking to two different databases, with nothing saying so. The other
/// way out is to carry every process across, but that restarts processes
/// the developer did not name, which is its own surprise; refusing says
/// what is true and costs one word on the command line.
///
/// Checked before anything is stopped or spawned, and again under the
/// lock where the decision is actually made.
fn refuse_only_across_a_mode_change(only: Option<&str>, going: ServiceMode) -> Result<()> {
    let Some(only) = only else { return Ok(()) };
    bail!(
        "this worktree is switching to {} services, and `--only {only}` cannot do that for one \
         process: the others would keep talking to the services that are going away. Run it \
         without `--only`, or leave the mode as it is",
        match going {
            ServiceMode::Isolated => "its own",
            ServiceMode::Namespaced => "namespaces of its own in the project's",
            ServiceMode::Shared => "the project's shared",
        }
    )
}

/// [`refuse_only_across_a_mode_change`] for a start or restart in `mode`,
/// read without the lock: what the CLI asks before any question, so that
/// a refusal has asked for no login, freed no slot and made nothing.
pub fn refuse_only_on_a_mode_change(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    only: Option<&str>,
    mode: Mode,
) -> Result<()> {
    if only.is_some() && mode_would_change(paths, config, name, mode) {
        refuse_only_across_a_mode_change(only, target_of(paths, config, name, mode))?;
    }
    Ok(())
}

/// Whether a start in this mode would change what the worktree's
/// processes talk to, read without the lock.
///
/// The answer the refusal above needs *before* `restart` stops anything.
/// `start` makes the same decision again under the lock, where it is
/// authoritative; this one only has to be right often enough to refuse
/// before a side effect, and a mode that is flipping under a concurrent
/// command is caught there.
fn mode_would_change(paths: &PandoPaths, config: &Config, name: &str, mode: Mode) -> bool {
    mode != Mode::Remembered && target_of(paths, config, name, mode) != recorded_mode(paths, name)
}

/// The mode this worktree runs in, or last ran in, read without the lock:
/// shared when nothing says.
///
/// Only a record written for this worktree says. One left at another path
/// by a worktree of the same name, removed and made again, is replaced by
/// the start that finds it under the lock; read as this one's mode, it
/// made a plain start prepare namespaces in the developer's own servers
/// for a mode it would not run in, and fail when they did not answer. The
/// path is asked of git only for a record that says anything but shared,
/// the one answer it cannot change.
pub(super) fn recorded_mode(paths: &PandoPaths, name: &str) -> ServiceMode {
    let Some(record) = state::load(&paths.state_file())
        .ok()
        .and_then(|mut store| store.worktrees.remove(name))
    else {
        return ServiceMode::default();
    };
    let mode = record.mode();
    if mode == ServiceMode::default() {
        return mode;
    }
    match find_worktree(paths, name) {
        Ok(worktree)
            if crate::paths::resolve_for_compare(&record.path)
                == crate::paths::resolve_for_compare(&worktree.path) =>
        {
            mode
        }
        _ => ServiceMode::default(),
    }
}

/// The mode a start asked for `mode` puts a worktree in that was in
/// `was`. A wish the project cannot grant — isolated with no services,
/// namespaced with none pando can give a namespace in — is a shared start.
fn decide(mode: Mode, was: ServiceMode, isolatable: bool, namespaceable: bool) -> ServiceMode {
    match mode {
        Mode::Shared => ServiceMode::Shared,
        Mode::Isolated if isolatable => ServiceMode::Isolated,
        Mode::Namespaced if namespaceable => ServiceMode::Namespaced,
        Mode::Isolated | Mode::Namespaced => ServiceMode::Shared,
        Mode::Remembered => match was {
            ServiceMode::Isolated if isolatable => ServiceMode::Isolated,
            ServiceMode::Namespaced if namespaceable => ServiceMode::Namespaced,
            _ => ServiceMode::Shared,
        },
    }
}

/// Whether a start in `mode` could give this worktree namespaces. Asked
/// only of a start that wants them, because answering reads the recipes
/// and the main checkout's env files.
fn can_namespace(paths: &PandoPaths, config: &Config, name: &str, mode: Mode) -> bool {
    let wants = match mode {
        Mode::Namespaced => true,
        Mode::Remembered => recorded_mode(paths, name) == ServiceMode::Namespaced,
        Mode::Shared | Mode::Isolated => false,
    };
    wants && namespaced::plan(paths, config).gives_own()
}

/// Refuses a start of a worktree that runs namespaced, plain or asking for
/// namespaces again, when no service here can have a namespace of its own
/// any more.
///
/// Such a start went on shared: a recipe of the developer's own with no
/// `[namespace]` table, or a main checkout whose env stopped naming its
/// database, and the branch's app, its workers and every hook run again
/// wrote into the main checkout's data — with nothing said on a plain
/// start, and only a progress line on a namespaced one, which is what the
/// TUI's quick start of such a worktree is. A worktree that runs on
/// namespaces of its own leaves them for the main checkout's data only
/// when that is asked for, never because of what the plan lost. One that
/// never ran namespaced is on the main checkout's data already, and a
/// namespaced start of it still goes on shared and says why. Read without
/// the lock, like [`target_of`], so nothing is asked, made or stopped
/// first. The worktree's namespaces are kept either way.
pub(super) fn refuse_losing_namespaces(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    mode: Mode,
) -> Result<()> {
    if !keeps_namespaces(mode, recorded_mode(paths, name)) {
        return Ok(());
    }
    let plan = namespaced::plan(paths, config);
    // Every service the worktree has a namespace of its own in has to
    // keep one: a prefix elsewhere is no reason to run a branch's app on
    // the main checkout's database.
    let lost: Vec<String> = crate::state::load(&paths.state_file())
        .ok()
        .and_then(|store| store.worktrees.get(name).cloned())
        .map(|record| record.namespaces)
        .unwrap_or_default()
        .into_iter()
        .map(|ns| ns.service)
        .filter(|service| !plan.targets.iter().any(|t| t.service == *service))
        .collect();
    if plan.gives_own() && lost.is_empty() {
        return Ok(());
    }
    if !lost.is_empty() && plan.gives_own() {
        let why: Vec<String> = plan
            .shared_lines()
            .into_iter()
            .filter(|line| lost.iter().any(|s| line.starts_with(&format!("{s}:"))))
            .collect();
        bail!(
            "{name} runs on a namespace of its own in {}, which can have none now{}, so nothing \
             was started: it does not move onto the main checkout's data unless that is asked \
             for, and its namespaces are kept. Fix that, or {}",
            lost.join(", "),
            match why.is_empty() {
                true => String::new(),
                false => format!(" ({})", why.join("; ")),
            },
            crate::remedy::SHARED_ON_PURPOSE
        );
    }
    let why = match plan.shared_lines() {
        lines if lines.is_empty() => "no service is configured".to_string(),
        lines => lines.join("; "),
    };
    bail!(
        "{name} runs namespaced, but no service here can have a namespace of its own now \
         ({why}), so nothing was started: it does not move onto the main checkout's data \
         unless that is asked for, and its namespaces are kept. Fix that, or {}",
        crate::remedy::SHARED_ON_PURPOSE
    )
}

/// Whether a start in `mode` of a worktree that was in `was` has to stay
/// on namespaces of its own: see [`refuse_losing_namespaces`].
fn keeps_namespaces(mode: Mode, was: ServiceMode) -> bool {
    matches!(mode, Mode::Remembered | Mode::Namespaced) && was == ServiceMode::Namespaced
}

/// The mode a start in `mode` would leave this worktree in, read without
/// the lock. `start` decides again under it, where it is authoritative.
pub(super) fn target_of(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    mode: Mode,
) -> ServiceMode {
    decide(
        mode,
        recorded_mode(paths, name),
        !service_roles(config).is_empty(),
        can_namespace(paths, config, name, mode),
    )
}

/// Whether a start in this mode would run this worktree on its own
/// services, read without the lock.
pub(super) fn would_isolate(paths: &PandoPaths, config: &Config, name: &str, mode: Mode) -> bool {
    target_of(paths, config, name, mode) == ServiceMode::Isolated
}

/// Whether a start in this mode would move a worktree onto private
/// services it does not have yet — the one start that replaces every
/// running process for the sake of services that do not exist yet. Read
/// without the lock.
fn would_switch_to_isolated(paths: &PandoPaths, config: &Config, name: &str, mode: Mode) -> bool {
    mode == Mode::Isolated
        && would_isolate(paths, config, name, mode)
        && recorded_mode(paths, name) != ServiceMode::Isolated
}

/// Why the hooks after the services run again when services changed
/// kind, each said with how it runs now; `None` when none did.
fn moved_kinds_why(moved: &[(String, state::ServiceKind)]) -> Option<String> {
    let said: Vec<String> = [
        (state::ServiceKind::Native, "natively"),
        (state::ServiceKind::Compose, "in a container"),
    ]
    .into_iter()
    .filter_map(|(kind, how)| {
        let names: Vec<&str> = moved
            .iter()
            .filter(|(_, to)| *to == kind)
            .map(|(service, _)| service.as_str())
            .collect();
        let verb = if names.len() == 1 { "runs" } else { "run" };
        (!names.is_empty()).then(|| format!("{} {verb} {how} now", names.join(", ")))
    })
    .collect();
    (!said.is_empty()).then(|| format!("{}, on other data", said.join(" and ")))
}

/// Whether this worktree's record runs a service as another kind than
/// config gives it now — the start that stops the old server before the
/// new one comes up. Read without the lock.
fn would_change_kinds(paths: &PandoPaths, config: &Config, name: &str) -> bool {
    state::load(&paths.state_file())
        .ok()
        .and_then(|mut store| store.worktrees.remove(name))
        .is_some_and(|record| !changed_kinds(config, &record).is_empty())
}

/// The processes a start, stop or restart acts on: every one config
/// declares, or the one `only` names.
///
/// Alphabetically by process name, which is the order they are spawned in:
/// nothing in the loader preserves the file's own order.
fn selected_processes<'a>(
    config: &'a Config,
    only: Option<&str>,
) -> Result<Vec<(String, &'a ProcessConfig)>> {
    if config.processes.is_empty() {
        bail!("no processes configured; add [dev] to pando.toml");
    }
    let chosen: Vec<(String, &ProcessConfig)> = match only {
        Some(wanted) => {
            let process = config.processes.get(wanted).with_context(|| {
                let known: Vec<&str> = config.processes.keys().map(String::as_str).collect();
                format!(
                    "no process named {wanted:?} in pando.toml — it configures: {}",
                    known.join(", ")
                )
            })?;
            vec![(wanted.to_string(), process)]
        }
        None => config
            .processes
            .iter()
            .map(|(name, process)| (name.clone(), process))
            .collect(),
    };
    // `cmd` is optional so that a half-written process table does not take
    // every other command down with it; this is where it has to be there.
    // Refused before anything starts: half a worktree is worse than none.
    for (name, process) in &chosen {
        if process.cmd.trim().is_empty() {
            let table = if name == detect::DEV && config.processes.len() == 1 {
                "[dev]".to_string()
            } else {
                format!("[processes.{name}]")
            };
            bail!("{table} in pando.toml has no cmd — add the command that starts this process");
        }
    }
    Ok(chosen)
}

/// Every role every process of the worktree owns, alphabetically by
/// process name — which is the order `Config.processes` keeps them in, and
/// the order they are spawned in.
///
/// Ports are reserved for all of them at once, whatever `--only` asked
/// for: a role belongs to the worktree, and starting one process must
/// never move another one's port. `config::validate` has already refused
/// two processes claiming one role, so this only deduplicates defensively.
/// Every role the worktree needs a port for: the processes' first, then
/// the services' when this worktree runs private copies of them.
///
/// Services last, deliberately. Ports are handed out in role order from
/// one window, so putting them after the processes means a worktree that
/// switches from shared to isolated keeps the web port it already had and
/// simply grows the window — no bookmarked URL changes for turning
/// isolation on.
pub(super) fn worktree_roles(config: &Config, isolated: bool) -> Vec<String> {
    let mut roles: Vec<String> = Vec::new();
    for process in config.processes.values() {
        for role in process.roles() {
            if !roles.contains(&role) {
                roles.push(role);
            }
        }
    }
    if isolated {
        for role in service_roles(config) {
            if !roles.contains(&role) {
                roles.push(role);
            }
        }
    }
    roles
}

/// The role whose port has to bind before the process counts as running.
///
/// `web` when it owns one, because that is the role everything else defaults
/// to; otherwise its first role. A process with no ports has none, and is
/// running as soon as it is alive.
fn ready_role(process: &ProcessConfig, roles: &[String]) -> Result<Option<String>> {
    if let Some(named) = process.ready.as_ref().and_then(|r| r.role.as_deref()) {
        if !roles.iter().any(|r| r == named) {
            bail!(
                "ready.role = {named:?} names a role this process does not own — it owns {}",
                if roles.is_empty() {
                    "none".to_string()
                } else {
                    roles.join(", ")
                }
            );
        }
        return Ok(Some(named.to_string()));
    }
    Ok(roles
        .iter()
        .find(|r| r.as_str() == DEFAULT_READY_ROLE)
        .or_else(|| roles.first())
        .cloned())
}

/// The directory the process runs in: the worktree, or the subdirectory
/// config names. A monorepo app sets `cwd = "apps/web"`.
fn process_cwd(
    worktree: &Path,
    name: &str,
    process: &ProcessConfig,
    ctx: &template::Context<'_>,
) -> Result<PathBuf> {
    let Some(relative) = process.cwd.as_deref() else {
        return Ok(worktree.to_path_buf());
    };
    let rendered = template::render(relative, ctx).context("in cwd")?;
    let dir = worktree.join(&rendered);
    if !dir.is_dir() {
        bail!(
            "cwd {rendered:?} does not exist in this worktree ({}) — check the cwd of process \
             {name:?}",
            dir.display()
        );
    }
    // `config::validate` refuses the literal ways out — an absolute path, a
    // `..` — but a template renders at start time and a symlink resolves
    // later still, so the directory that will really be entered is compared
    // against the worktree that owns it. A process that ran outside its own
    // worktree would be writing into a repository, which Invariant 1
    // forbids.
    let resolved = crate::paths::resolve_for_compare(&dir);
    let owner = crate::paths::resolve_for_compare(worktree);
    if !resolved.starts_with(&owner) {
        bail!(
            "cwd {rendered:?} for process {name:?} resolves to {}, which is outside the worktree \
             ({})",
            resolved.display(),
            owner.display()
        );
    }
    Ok(dir)
}

/// The environment the process is started with: the ports the map form of
/// `ports` is sugar for, then whatever `env` sets, then pando's own
/// variables.
///
/// `PANDO_*` last and unconditional: a hook or a script needs to be able to
/// find out which worktree it is in, and config cannot be allowed to lie
/// about that.
pub(super) fn process_env(
    paths: &PandoPaths,
    name: &str,
    worktree: &Worktree,
    process: &ProcessConfig,
    service_env: &BTreeMap<String, String>,
    ctx: &template::Context<'_>,
) -> Result<Vec<(String, String)>> {
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for (var, tmpl) in process.port_env() {
        env.insert(
            var.clone(),
            template::render(&tmpl, ctx).with_context(|| format!("in ports.{var}"))?,
        );
    }
    // Between the port sugar and the process's own `env`, so a developer
    // who spells a service URL out by hand still wins: pando's rewrite is
    // the default, not the law.
    for (var, value) in service_env {
        env.insert(var.clone(), value.clone());
    }
    for (var, tmpl) in &process.env {
        env.insert(
            var.clone(),
            template::render(tmpl, ctx).with_context(|| format!("in env.{var}"))?,
        );
    }
    // The same variables a hook gets, from the one list.
    env.extend(pando_env(
        paths,
        name,
        worktree.branch.as_deref(),
        ctx.worktree,
    ));
    Ok(env.into_iter().collect())
}

/// Empties a log file before a run, creating its directory.
pub(super) fn reset_log(log_file: &Path) -> Result<()> {
    if let Some(parent) = log_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create log dir {}", parent.display()))?;
    }
    // The exit status beside it belongs to the run being replaced. A run
    // that dies before its own shell can record one would otherwise be
    // explained by the previous run's status.
    let _ = std::fs::remove_file(crate::paths::exit_status_file(log_file));
    std::fs::write(log_file, b"").with_context(|| format!("truncate {}", log_file.display()))
}
