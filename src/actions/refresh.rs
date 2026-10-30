//! Reading state: phases advanced, observed ports captured, failures
//! explained.

use std::collections::BTreeMap;
use std::path::Path;

use crate::paths::PandoPaths;
use crate::ports;
use crate::process as proc;
use crate::state::{self, Phase, PortCheck};

use super::share::sweep_dead_shares;

/// State as of right now: phases advanced, observed ports captured, and the
/// reason a dead process died written down.
///
/// This is the read path. It advances phases rather than reconciling,
/// because a crashed dev server has to stay visible as Failed until the
/// developer acts on it — `reconcile`, which drops dead records, would erase
/// exactly the thing worth showing. It never fails: a state file pando
/// cannot use becomes one warning line, the same one `rm` refuses with.
#[derive(Debug, Default, Clone)]
pub struct Refreshed {
    pub state: state::State,
    pub warning: Option<String>,
    /// Whether `warning` is why the state file could not be read at all,
    /// so `state` is empty for want of one rather than because nothing is
    /// recorded. A save that failed leaves it false: what was read stands.
    pub unreadable: bool,
    /// What the refresh itself did, one line each. A share whose tunnel
    /// died is closed here rather than in silence: the URL a developer had
    /// open stops working, and they should be told once rather than
    /// discover it from a browser.
    pub notices: Vec<String>,
}

pub fn refresh(paths: &PandoPaths) -> Refreshed {
    // Nothing has ever been started here, so there is nothing to advance and
    // no reason for a read-only command to create a home.
    if !paths.state_file().exists() {
        return Refreshed::default();
    }
    if let Err(e) = paths.ensure_home() {
        return unreadable(e);
    }
    // One scan of every live group, used for both questions this pass
    // answers: whether a starting process has opened its port yet, and what
    // every group is really listening on.
    //
    // Read and scanned before the lock is asked for. The scan is the slow
    // half — a `ps` and an `lsof` per live group, up to seconds on a busy
    // machine — and every mutation waits for whoever holds the lock.
    let seen = match state::load(&paths.state_file()) {
        Ok(store) => store,
        Err(e) => return unreadable(e),
    };
    let mut scans = scan_groups(&seen);
    // Never waited for: a `rm` or a stop holds it for as long as a
    // `docker compose down` takes, and `ls`, `status` and the TUI's first
    // frame waited all that time with nothing on screen. While someone
    // holds it, the answer is the same advance on the copy just read,
    // written nowhere — the holder's next save is the one that counts,
    // and the next read that finds the lock free saves this one's work.
    let _lock = match state::try_lock(&paths.lock_file()) {
        Ok(Some(lock)) => lock,
        Ok(None) => return advanced(seen, &scans),
        Err(e) => return unreadable(e),
    };
    // Read again under the lock: a mutation may have saved since.
    let mut store = match state::load(&paths.state_file()) {
        Ok(store) => store,
        Err(e) => return unreadable(e),
    };

    // Before anything advances or reconciles: both of those drop a share
    // whose tunnel has died, and neither can signal what is left of it.
    // Signalling is not spawning — a read path must not start a process,
    // but a tunnel nothing can reach again is exactly what it must not
    // leave behind either.
    let mut notices = sweep_dead_shares(&mut store);

    // A native service is pando's own child, so a crashed one is a
    // database that is gone: forgotten here rather than reported as up
    // until the next mutation. Its socket directory goes with it, the one
    // thing a server that died without cleaning up leaves outside the home.
    for (name, service) in
        state::forget_dead_native_services(&mut store, proc::is_alive, proc::group_alive)
    {
        let _ = std::fs::remove_dir_all(paths.service_socket_dir(&name, &service));
        notices.push(format!(
            "{name}: its {service} exited — `pando start {name}` brings it back"
        ));
    }

    // A group a start recorded between the two reads was not in the first
    // scan; only those are scanned under the lock.
    let unscanned: Vec<proc::Group> = live_groups(&store)
        .filter(|group| !scans.contains_key(group))
        .collect();
    if !unscanned.is_empty() {
        scans.extend(crate::platform::process::ports_by_group(&unscanned));
    }
    let mut changed = !notices.is_empty();
    changed |= advance_with(&mut store, &scans);
    changed |= capture_observed_ports(&mut store, &scans);
    if changed {
        // A read path that cannot write is still a read path: the phases are
        // right in memory either way, so a save that fails is not worth
        // failing the command the user actually ran.
        if let Err(e) = state::save(&paths.state_file(), &store) {
            return Refreshed {
                state: store,
                warning: Some(format!("{e:#}")),
                unreadable: false,
                notices,
            };
        }
    }
    Refreshed {
        state: store,
        warning: None,
        unreadable: false,
        notices,
    }
}

/// What a read says when there is no state to read: why, and nothing else.
fn unreadable(e: anyhow::Error) -> Refreshed {
    Refreshed {
        state: state::State::new(),
        warning: Some(format!("{e:#}")),
        unreadable: true,
        notices: Vec::new(),
    }
}

/// State as of right now, with nothing written and nothing signalled.
///
/// [`refresh`] is the read path every other command uses, and it takes the
/// lock, advances phases, sweeps a half-dead share — which *signals* the
/// surviving half — and saves. `doctor` may do none of those: it reports.
/// So this is the same advance, on a copy, with the sweep left out, so the
/// report can say a share is half dead rather than quietly finish it off.
pub fn inspect(paths: &PandoPaths) -> Refreshed {
    if !paths.state_file().exists() {
        return Refreshed::default();
    }
    let store = match state::load(&paths.state_file()) {
        Ok(store) => store,
        Err(e) => return unreadable(e),
    };
    let scans = scan_groups(&store);
    advanced(store, &scans)
}

/// The advance on a copy: [`inspect`]'s whole answer, and [`refresh`]'s
/// while another pando holds the lock.
fn advanced(mut store: state::State, scans: &BTreeMap<proc::Group, Option<Vec<u16>>>) -> Refreshed {
    // The same forgetting the read path does, on the copy: a report that
    // listed a database whose server is gone would contradict `status`.
    state::forget_dead_native_services(&mut store, proc::is_alive, proc::group_alive);
    advance_with(&mut store, scans);
    capture_observed_ports(&mut store, scans);
    Refreshed {
        state: store,
        warning: None,
        unreadable: false,
        notices: Vec::new(),
    }
}

/// Moves every process to the phase it is really in, and writes the reason
/// for each failure that is new. The one implementation of "advance", used
/// by the read path and by every mutation that is about to `reconcile`.
fn advance_with(store: &mut state::State, scans: &BTreeMap<proc::Group, Option<Vec<u16>>>) -> bool {
    let failed_before = failed_processes(store);
    let mut changed =
        state::advance_phases(store, proc::is_alive, proc::group_alive, |pgid, port| {
            port_is_bound(scans, pgid, port)
        });
    changed |= explain_new_failures(store, &failed_before, scans);
    changed
}

/// Advances phases before `reconcile` drops anything.
///
/// `reconcile` keeps a record that is already `Failed`, because a crash has
/// to stay visible until the developer acts on it. That only works if
/// something marked it failed first — and between a process dying and the
/// next read path, nothing has. Without this, a `start`, a `stop`, or a
/// `stop --only` of a *sibling* silently drops the crash it was about to
/// make visible. So every mutation advances with the same inputs the read
/// path uses, and only then reconciles.
///
/// The observed ports the same scan could capture are deliberately not
/// recorded here: `start` clears them for the processes it replaces, and
/// the next refresh says what is really listening.
pub(super) fn advance_before_reconcile(store: &mut state::State) {
    let scans = scan_groups(store);
    advance_with(store, &scans);
}

/// `(worktree, process)` pairs already in `Failed`, so a reason is explained
/// once — when it happens — rather than re-read from the log on every tick.
fn failed_processes(store: &state::State) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (name, record) in &store.worktrees {
        for (process, p) in &record.processes {
            if matches!(p.phase, Phase::Failed { .. }) {
                out.push((name.clone(), process.clone()));
            }
        }
    }
    out
}

/// The ports every live process group is listening on, scanned once.
///
/// `Starting` groups are scanned too, not only `Running` ones: the scan is
/// how a process *becomes* Running, and scanning only what is already
/// running is a deadlock. `None` against a pgid means the scan itself could
/// not run — no `lsof`, denied, or timed out — which is a different answer
/// from "listening on nothing".
pub(super) fn scan_groups(store: &state::State) -> BTreeMap<proc::Group, Option<Vec<u16>>> {
    let pgids: Vec<proc::Group> = live_groups(store).collect();
    crate::platform::process::ports_by_group(&pgids)
}

/// The groups [`scan_groups`] scans: every process `Starting` or `Running`.
fn live_groups(store: &state::State) -> impl Iterator<Item = proc::Group> + '_ {
    store
        .worktrees
        .values()
        .flat_map(|record| record.processes.values())
        .filter(|p| matches!(p.phase, Phase::Starting { .. } | Phase::Running { .. }))
        .map(|p| p.pgid)
}

/// Whether the process group has opened `port` yet.
///
/// Never by binding it: a probe that takes the port to find out whether it
/// is taken is one an `EADDRINUSE` away from killing the very server it is
/// waiting for, and it answers about the port rather than about *this*
/// process. The scan of the group's own sockets answers both properly; a
/// connection is the fallback for a machine where the scan cannot run.
///
/// A scan that could not run is not a scan that found nothing: when the
/// fallback connect gets no answer either, the honest reply is
/// [`PortCheck::Unknown`], which keeps the process waiting rather than
/// failing it with "nothing bound" — see [`state::PortCheck`].
pub(super) fn port_is_bound(
    scans: &BTreeMap<proc::Group, Option<Vec<u16>>>,
    pgid: proc::Group,
    port: u16,
) -> PortCheck {
    match scans.get(&pgid) {
        Some(Some(ports)) => ports.contains(&port).into(),
        _ if ports::something_is_listening(port) => PortCheck::Bound,
        _ => PortCheck::Unknown,
    }
}

/// Records the ports each live group is really listening on, per process.
///
/// Configured ports are what pando asked for; these are what happened. A
/// framework that ignores `PORT`, or one that opens a second socket for hot
/// reload, shows up here and nowhere else.
///
/// Per process, because a worktree-wide list cannot say which group opened
/// which socket: an `--inspect` port the api opened reads exactly like a
/// port the web server opened, and the worktree's URL then follows it.
/// The worktree's own list stays as the union of them, which is the shape
/// `status --json` publishes.
pub(super) fn capture_observed_ports(
    store: &mut state::State,
    scans: &BTreeMap<proc::Group, Option<Vec<u16>>>,
) -> bool {
    let mut changed = false;
    for record in store.worktrees.values_mut() {
        let mut union: Vec<u16> = Vec::new();
        let mut groups = 0usize;
        let mut scanned = false;
        for proc in record.processes.values_mut() {
            if !matches!(proc.phase, Phase::Starting { .. } | Phase::Running { .. }) {
                // Not up, so listening on nothing: its last sighting went
                // stale the moment it stopped.
                if !proc.observed_ports.is_empty() {
                    proc.observed_ports.clear();
                    changed = true;
                }
                continue;
            }
            groups += 1;
            let Some(Some(ports)) = scans.get(&proc.pgid) else {
                // This group could not be scanned: its last good answer
                // stands rather than being cleared by a missing `lsof`.
                union.extend(proc.observed_ports.iter().copied());
                continue;
            };
            scanned = true;
            let mut observed = ports.clone();
            observed.sort_unstable();
            observed.dedup();
            if proc.observed_ports != observed {
                proc.observed_ports = observed.clone();
                changed = true;
            }
            union.extend(observed);
        }
        union.sort_unstable();
        union.dedup();
        // Nothing could be scanned at all, so there is nothing to say.
        if groups > 0 && !scanned {
            continue;
        }
        if record.observed_ports != union {
            record.observed_ports = union;
            changed = true;
        }
    }
    changed
}

/// Appends the classifier's one-line hint to a failure that just happened.
///
/// pando is not the process's parent by the time it dies, so there is no
/// exit status to read. The last lines of its log are the only evidence, and
/// four patterns cover most of what actually goes wrong.
pub(super) fn explain_new_failures(
    store: &mut state::State,
    failed_before: &[(String, String)],
    scans: &BTreeMap<proc::Group, Option<Vec<u16>>>,
) -> bool {
    let mut changed = false;
    for (name, record) in store.worktrees.iter_mut() {
        let assigned: Vec<u16> = record.ports.values().copied().collect();
        for (process, p) in record.processes.iter_mut() {
            let Phase::Failed { at, reason } = &p.phase else {
                continue;
            };
            if failed_before
                .iter()
                .any(|(w, pr)| w == name && pr == process)
            {
                continue;
            }
            let alive = proc::group_alive(p.pgid);
            let (mut explained, log_explains) = explain(reason, &p.log_path, alive);
            let elsewhere = scans
                .get(&p.pgid)
                .and_then(|scan| scan.as_deref())
                .and_then(|listening| listening_elsewhere(reason, listening, &assigned));
            // Listening on another port, or a log that says what went
            // wrong, is the diagnosis; more time is not. Only a process
            // still up with nothing else to say may simply be slow.
            let hint = match elsewhere {
                Some(note) => Some(note),
                None if alive && !log_explains => {
                    slow_start_hint(reason, process, p.ready_timeout_s)
                }
                None => None,
            };
            if let Some(hint) = hint {
                explained = format!("{explained} — {hint}");
            }
            if &explained == reason {
                continue;
            }
            p.phase = Phase::Failed {
                at: *at,
                reason: explained,
            };
            changed = true;
        }
    }
    changed
}

/// What a readiness timeout leaves out when the process is up and serving,
/// just not where pando waited: the ports it did open.
///
/// "Nothing bound port 17002" over a server listening on 3000 sends a
/// developer looking for a crash that did not happen. The real failure is
/// that the port pando assigned never reached the process — its own env
/// file, its config, or a variable pando did not set chose another one —
/// and those ports are the whole of the evidence for that. A port another
/// role of the same worktree owns is not "elsewhere": a process that
/// serves two roles has opened one of its own.
pub(super) fn listening_elsewhere(
    reason: &str,
    listening: &[u16],
    assigned: &[u16],
) -> Option<String> {
    if !reason.starts_with("timeout") {
        return None;
    }
    let mut elsewhere: Vec<u16> = listening
        .iter()
        .copied()
        .filter(|port| !assigned.contains(port))
        .collect();
    elsewhere.sort_unstable();
    elsewhere.dedup();
    if elsewhere.is_empty() {
        return None;
    }
    let ports = elsewhere
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "it is listening on {ports} instead: the port pando assigned never reached it — \
         check which variable or flag it reads its port from, and whether a value in its own \
         env file or config wins over the one pando passed"
    ))
}

/// How many lines of a failed process's log a front end shows under its
/// reason: enough for a stack trace's closing frames, not a screenful.
pub const FAILURE_SHOWN_LINES: usize = 10;

/// The closing lines of a failed process's log, blank lines dropped, for a
/// front end to show under the reason `status` already carries.
///
/// The reason says what pando made of the failure — the exit status, and
/// the hint the classifier matched — and these are what it made it from,
/// so a developer can check the diagnosis without opening the file. The
/// CLI prints them when a `--wait` fails; the TUI can put them under its
/// error line the same way.
pub fn failure_tail(log_path: &Path) -> Vec<String> {
    let lines = crate::log_tail::snapshot(log_path, crate::log_tail::FAILURE_TAIL_LINES)
        .unwrap_or_default();
    let kept: Vec<String> = lines
        .into_iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let skip = kept.len().saturating_sub(FAILURE_SHOWN_LINES);
    kept.into_iter().skip(skip).collect()
}

/// Everything that can be said about one failure, in one line.
///
/// Three sources, in the order a reader wants them. What the phase knew —
/// that the process is gone, or that nothing bound the port it was waiting
/// for. Then the status its shell recorded on the way out, which is the
/// only thing that separates a crash from a command that did its job and
/// returned. Then either what the log says, or — when the log is empty —
/// that it is empty, which is a fact about the command and not an absence
/// of information.
///
/// The empty half is why this exists. A first run proposed a Makefile
/// guard as a dev command; it exited 0 in a millisecond because the tool
/// it guarded was installed, and `status`, `doctor` and the TUI all said
/// "failed — process exited" over a log file with nothing in it.
///
/// `group_alive` is the guard on the one claim here that is a judgement
/// rather than a fact: a command that backgrounds the server and returns
/// looks identical from the leader's exit status, and is not the wrong
/// command. Read after the phases are advanced, so the leader has already
/// been reaped and a zombie cannot hold its own group open.
///
/// With it, whether the log itself explained the failure: a line the
/// classifier knows, which is the diagnosis where there is one.
pub(super) fn explain(reason: &str, log_path: &Path, group_alive: bool) -> (String, bool) {
    let mut out = reason.to_string();
    let code = proc::recorded_exit_status(&crate::paths::exit_status_file(log_path));
    // Only onto the phase's own "it is gone": a timeout is a process that
    // is still running, and has no status to report.
    if out == state::EXITED
        && let Some(code) = code
    {
        out = format!("{out} with status {code}");
    }
    let lines = crate::log_tail::snapshot(log_path, crate::log_tail::FAILURE_TAIL_LINES)
        .unwrap_or_default();
    let printed_anything = std::fs::metadata(log_path).is_ok_and(|m| m.len() > 0);
    if let Some(note) = crate::observe::exit_note(code, printed_anything, group_alive) {
        out = format!("{out} — {note}");
    }
    let classified = crate::observe::classify_failure(&lines);
    if let Some(hint) = &classified {
        out = format!("{out} — {}", hint.hint);
    }
    (out, classified.is_some())
}

/// What to say of a process that outlived its readiness wait with nothing
/// bound and nothing else wrong: it may only be slow — a cold build, a JVM,
/// a server that waits for its database, a name lookup on a slow network —
/// and `ready.timeout_s` is how it gets longer. Twice the wait it had, so
/// the number is one worth trying. `None` for any other failure.
pub(super) fn slow_start_hint(
    reason: &str,
    process: &str,
    timeout_s: Option<u64>,
) -> Option<String> {
    if !reason.starts_with(state::NOTHING_BOUND) {
        return None;
    }
    let waited = timeout_s.unwrap_or(state::START_TIMEOUT_SECS.unsigned_abs());
    let longer = waited.saturating_mul(2);
    Some(format!(
        "if it is only slow to start, `ready = {{ timeout_s = {longer} }}` in the {process} \
         process's table in pando.toml gives it {longer}s"
    ))
}
