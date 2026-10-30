//! Share and unshare.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::catalog;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::ports;
use crate::process as proc;
use crate::share_proxy;
use crate::state::{self, PendingShare, Phase, ShareRecord, WorktreeRecord};
use crate::tunnel;
use crate::worktree::Worktree;

use super::hooks::pando_env;
use super::lifecycle::{STOP_GRACE, sweep_orphaned_groups};
use super::refresh::{advance_before_reconcile, refresh};
use super::runtime::with_prelude;
use super::services::{
    UrlOwner, observed_port_for_role, resolved_env, share_owner, share_owner_not_running,
    share_role,
};
use super::worktree::{find_worktree, refuse_while_removed, refuse_while_removed_now};

/// Signals both halves of a worktree's share and drops the record, as part
/// of stopping it. Returns whether there was one.
///
/// Signal first, drop second, exactly as the process records are handled: a
/// share record cleared for a tunnel that was never signalled is a
/// cloudflared nothing can find again.
pub(super) fn take_share_down(
    record: &mut WorktreeRecord,
    stop: &impl Fn(proc::Group) -> Result<()>,
    failures: &mut Vec<String>,
) -> bool {
    let Some(share) = record.share.clone() else {
        return false;
    };
    match tunnel::stop_share_with(&share, stop) {
        Ok(()) => {
            record.share = None;
            true
        }
        Err(e) => {
            failures.push(format!(
                "its share (tunnel group {}): {e:#}",
                share.tunnel_pgid
            ));
            true
        }
    }
}

/// What a stop says of the public URL it closed. A quick tunnel's host is
/// random, so the URL the developer handed out is gone for good and the
/// next share is a different one: worth one line rather than a row that
/// quietly loses its URL.
pub(super) fn share_closed(name: &str, public_url: &str) -> String {
    format!(
        "{name}: its public URL {public_url} is closed — `pando share {name}` gives it a new one"
    )
}

/// How long `[share].auth_cmd` may take before the share gives up on it.
///
/// A script that waits on something that never comes would otherwise hold
/// a share open forever, and in the TUI that is a pending slot nothing can
/// clear. It is the same under test. No test waits it out — the one about
/// a budget running out ends it by an event instead, through
/// `process::with_budget_over_when` — and a shorter clock is one a login
/// shell on a loaded machine can spend before the script has printed
/// anything.
pub(super) const AUTH_CMD_TIMEOUT: Duration = Duration::from_secs(30);

/// How `auth_cmd` is told which port the proxy will listen on, in case it
/// wants to mint a session scoped to it.
pub const ENV_SHARE_PORT: &str = "PANDO_SHARE_PORT";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareOutcome {
    pub name: String,
    pub public_url: String,
    /// Whether a proxy is injecting a header in front of the application.
    pub pre_authed: bool,
    /// Whether the worktree was already shared when this was asked. A
    /// second `share` prints the URL rather than opening a second tunnel.
    pub already: bool,
}

/// What a share refused over a removal says it did not do.
const SHARED_NOTHING: &str = "nothing was shared";

/// Publishes a running worktree at a public URL.
///
/// The lock is held for the refusals and for recording the result, and
/// dropped for everything slow in between — the auth command, the proxy,
/// and a tunnel that takes up to thirty seconds to publish. That window is
/// why the second half re-checks: a `stop` in the meantime must not leave a
/// tunnel open onto nothing with no record of it.
///
/// A second share of the same worktree waits for the first to finish, and
/// then answers with its URL.
pub fn share(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    progress: &dyn Fn(&str),
) -> Result<ShareOutcome> {
    let provider = tunnel::provider_for(config.share.provider.as_deref())?;
    share_with(
        paths,
        config,
        name,
        provider.as_ref(),
        &|paths, name, listen, upstream, cookie| {
            share_proxy::spawn(paths, name, listen, upstream, cookie)
        },
        progress,
    )
}

/// How a proxy is started. Injected so a test can drive the path where a
/// tunnel fails with a proxy already running: the real one re-execs the
/// running binary, which inside a library test is the test harness.
pub type SpawnProxy<'a> =
    &'a dyn Fn(&PandoPaths, &str, u16, u16, &str) -> Result<share_proxy::ProxySpawn>;

/// [`share`] with the provider and the proxy injected, so a test can drive
/// a provider that is missing, and one whose tunnel fails after a proxy is
/// already running.
pub fn share_with(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    provider: &dyn tunnel::Provider,
    spawn_proxy: SpawnProxy<'_>,
    progress: &dyn Fn(&str),
) -> Result<ShareOutcome> {
    paths.ensure_home()?;
    let worktree = find_worktree(paths, name)?;
    let canonical = std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
    let shown = worktree.display_name();
    // Before the share's own lock, which is made in the worktree's logs: a
    // removal deletes those, and would leave the one made after.
    refuse_while_removed_now(paths, name, &shown, SHARED_NOTHING)?;

    // One share of a worktree at a time, for the whole of it. Each tunnel
    // of a worktree writes the same log, and two opening at once both read
    // the URL printed first: one recorded the other's, and the other,
    // giving way, closed the tunnel that served it. A second share waits
    // here, and then finds the first one's record.
    let lock_path = paths.share_lock_file(name);
    let _one_share = match state::try_lock(&lock_path)? {
        Some(held) => held,
        None => {
            progress(&format!("waiting for another share of {shown} to finish"));
            state::lock(&lock_path)?
        }
    };

    // Outside the state lock, because `refresh` takes it: a worktree
    // `start` returned from a moment ago is still `Starting`, and refusing
    // it is refusing the first thing anyone types.
    await_share_target(paths, name, &shown, progress);

    // Every refusal first, and the proxy's port, under the lock.
    let (target_port, share_port) = {
        let _lock = state::lock(&paths.lock_file())?;
        let mut store = state::load(&paths.state_file())?;
        for notice in sweep_orphaned_groups(&mut store)? {
            progress(&notice);
        }
        advance_before_reconcile(&mut store);
        state::reconcile(&mut store, proc::is_alive, proc::group_alive);
        // Before any of the refusals below, which leave without the save at
        // the end: the sweep marked every group it signalled, and a
        // refusal that forgot that signalled them all again on the next
        // call — an agent polling a share it already has, every time.
        state::save(&paths.state_file(), &store)?;

        let record = store
            .worktrees
            .get(name)
            .with_context(|| format!("pando has no record of {shown} — start it first"))?;
        if let Some(existing) = &record.share {
            return Ok(ShareOutcome {
                name: name.to_string(),
                public_url: existing.public_url.clone(),
                pre_authed: existing.proxy_pid.is_some(),
                already: true,
            });
        }
        let target_port = share_target_port(&shown, record)?;
        // Only when something will actually listen on it. A worktree that
        // is shared without an auth command needs no proxy and no port.
        let share_port = match config.share.auth_cmd {
            Some(_) => Some(ports::assign_share_port(paths, &mut store, name)?),
            None => None,
        };
        state::save(&paths.state_file(), &store)?;
        (target_port, share_port)
    };

    // Only now: a worktree that was never going to be shared must not be
    // told to install anything.
    provider.ensure_present(paths)?;

    // The cookie before anything is spawned, so a script that fails leaves
    // nothing behind to clean up.
    let cookie = match config.share.auth_cmd.as_deref() {
        Some(cmd) => {
            progress("running the auth command");
            Some(run_auth_cmd(
                paths,
                config,
                name,
                &worktree,
                &canonical,
                cmd,
                share_port.unwrap_or(target_port),
            )?)
        }
        None => None,
    };

    let proxy = match (&cookie, share_port) {
        (Some(cookie), Some(port)) => {
            progress("starting the share proxy");
            let proxy = spawn_proxy(paths, name, port, target_port, cookie)?;
            note_pending(paths, name, proxy.pgid);
            // Before a tunnel is published onto it, and the last moment
            // anything knows its process group if it never came up.
            if let Err(e) = share_proxy::await_listening(&proxy) {
                let _ = proc::stop(proxy.pgid, STOP_GRACE);
                forget_pending(paths, name);
                return Err(e);
            }
            Some(proxy)
        }
        _ => None,
    };
    // The proxy by the one address it binds; the dev server by the name
    // either loopback answers to.
    let (host, upstream) = match &proxy {
        Some(proxy) => (share_proxy::LISTEN_HOST, proxy.listen_port),
        None => (tunnel::DEV_SERVER_HOST, target_port),
    };

    progress(&format!("opening a {} tunnel", provider.name()));
    let noted = |pgid| note_pending(paths, name, pgid);
    let spawn = match provider.start(paths, name, host, upstream, &noted) {
        Ok(spawn) => spawn,
        Err(e) => {
            // Nothing has recorded the proxy yet, so this is the last
            // moment anything knows its process group.
            if let Some(proxy) = &proxy {
                let _ = proc::stop(proxy.pgid, STOP_GRACE);
            }
            forget_pending(paths, name);
            return Err(e);
        }
    };

    let record = ShareRecord {
        tunnel_pid: spawn.pid,
        tunnel_pgid: spawn.pgid,
        public_url: spawn.public_url.clone(),
        local_port: target_port,
        started_at: Utc::now(),
        log_path: spawn.log_path,
        proxy_pid: proxy.as_ref().map(|p| p.pid),
        proxy_pgid: proxy.as_ref().map(|p| p.pgid),
        proxy_port: proxy.as_ref().map(|p| p.listen_port),
    };

    // The worktree may have been stopped, removed, or shared by somebody
    // else while the tunnel was coming up. Anything that was spawned goes
    // down here rather than becoming a process with no record.
    let lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    // No longer pending from here, whichever way it goes: recorded, or
    // stopped. Every way out saves the store without it.
    drop_pending(&mut store, name);
    let Some(existing_record) = store.worktrees.get(name) else {
        let _ = tunnel::stop_share(&record);
        bail!("{shown} was removed while its tunnel was starting; the tunnel was closed again");
    };
    if let Err(e) = refuse_while_removed(paths, &store, name, &shown, SHARED_NOTHING) {
        let _ = tunnel::stop_share(&record);
        let _ = state::save(&paths.state_file(), &store);
        return Err(anyhow::anyhow!(
            "{e:#}; the tunnel that was starting was closed again"
        ));
    }
    if let Some(won) = &existing_record.share {
        let public_url = won.public_url.clone();
        let pre_authed = won.proxy_pid.is_some();
        let _ = tunnel::stop_share(&record);
        let _ = state::save(&paths.state_file(), &store);
        return Ok(ShareOutcome {
            name: name.to_string(),
            public_url,
            pre_authed,
            already: true,
        });
    }
    // Ours, once more, before it is recorded. Another share of this
    // worktree that started a moment earlier holds the proxy's port, so
    // this proxy can pass for listening and then lose the bind; recorded,
    // it is a share whose proxy is dead, and the other share, giving way
    // to it, stops the only proxy that worked.
    if let Some(proxy) = &proxy
        && !proc::is_alive(proxy.pid)
    {
        let said = share_proxy::last_words(proxy);
        let _ = tunnel::stop_share(&record);
        let _ = state::save(&paths.state_file(), &store);
        bail!(
            "the share proxy of {shown} exited while its tunnel was starting ({said}); the \
             tunnel was closed again"
        );
    }
    if let Err(e) = share_target_port(&shown, existing_record) {
        let _ = tunnel::stop_share(&record);
        let _ = state::save(&paths.state_file(), &store);
        return Err(e.context(format!(
            "{shown} stopped while its tunnel was starting; the tunnel was closed again"
        )));
    }
    store.worktrees.get_mut(name).expect("just read").share = Some(record.clone());
    if let Err(e) = state::save(&paths.state_file(), &store) {
        // A tunnel nothing has a record of is a tunnel nothing can close.
        let _ = tunnel::stop_share(&record);
        return Err(e);
    }
    drop(lock);
    // Shared, and said so, rather than refused: a slow edge is not a dead
    // one. But a URL that may not answer yet is not one to hand out
    // without the reason.
    if let Some(tail) = &spawn.unconnected {
        progress(&format!(
            "{} has not connected to its edge yet, so the URL may not answer — tail: {tail}",
            provider.name()
        ));
    }
    if let Some(refusal) = host_refusal(target_port, &record.public_url) {
        progress(&format!("{shown}: {refusal}"));
    }
    Ok(ShareOutcome {
        name: name.to_string(),
        public_url: record.public_url,
        pre_authed: proxy.is_some(),
        already: false,
    })
}

/// How long the host probe waits on the dev server. A host check is the
/// first thing such a server does with a request, so one that has not
/// answered by now let the host through and is building the page.
const HOST_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// How much of the answer the probe reads. A refusal says what it is in
/// its first few kilobytes.
const HOST_PROBE_BYTES: u64 = 16 * 1024;

/// What the dev server says to a visitor, asked as the tunnel will ask
/// it: with the public URL's own `Host`.
///
/// Several frameworks' dev servers refuse every host that is not
/// `localhost` or an address, so a share of one hands every visitor its
/// blocked-host page. The share still stands — it is the project's
/// setting to change, not pando's — so this names the change, from
/// [`catalog::host_checks`]. It asks the dev server's own port rather than
/// a proxy in front of it, so no cookie goes with it; an error, a timeout,
/// or any answer that is not a known refusal says nothing.
pub(super) fn host_refusal(target_port: u16, public_url: &str) -> Option<String> {
    use std::io::{Read, Write};
    let host = public_url
        .split_once("://")
        .map_or(public_url, |(_, rest)| rest);
    let mut stream = ports::connect_loopback(target_port, HOST_PROBE_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(HOST_PROBE_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(HOST_PROBE_TIMEOUT)).ok()?;
    let request =
        format!("GET / HTTP/1.1\r\nHost: {host}\r\nAccept: text/html\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).ok()?;
    let mut answer = Vec::new();
    // A timeout part-way through keeps what arrived before it.
    let _ = stream.take(HOST_PROBE_BYTES).read_to_end(&mut answer);
    let answer = String::from_utf8_lossy(&answer);
    let status = answer
        .strip_prefix("HTTP/")?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    let check = catalog::host_checks::refused_by(status, &answer)?;
    // Every host the provider hands out, not just this one: the next share
    // is given another.
    let suffix = host.find('.').map_or(host, |dot| &host[dot..]);
    Some(format!(
        "its dev server refuses the public URL's host, so visitors get {}'s blocked-host page — \
         to let shares in, {}",
        check.server,
        check.remedy.replace("{suffix}", suffix)
    ))
}

/// Writes down a process group this share has spawned, before anything
/// waits on it: what a sweep stops if this pando dies before the share is
/// recorded. Best effort, since the share itself does not depend on it.
fn note_pending(paths: &PandoPaths, name: &str, pgid: proc::Group) {
    let Ok(_lock) = state::lock(&paths.lock_file()) else {
        return;
    };
    let Ok(mut store) = state::load(&paths.state_file()) else {
        return;
    };
    let Some(record) = store.worktrees.get_mut(name) else {
        return;
    };
    let me = std::process::id();
    match record.pending_shares.iter_mut().find(|p| p.owner_pid == me) {
        Some(pending) => pending.pgids.push(pgid),
        None => record.pending_shares.push(PendingShare {
            owner_pid: me,
            since: Utc::now(),
            pgids: vec![pgid],
        }),
    }
    let _ = state::save(&paths.state_file(), &store);
}

/// Drops this pando's pending share of `name`, which is recorded or
/// stopped.
fn drop_pending(store: &mut state::State, name: &str) {
    if let Some(record) = store.worktrees.get_mut(name) {
        let me = std::process::id();
        record.pending_shares.retain(|p| p.owner_pid != me);
    }
}

/// [`drop_pending`] under the lock, for a share that stopped what it had
/// spawned before it got as far as recording anything.
fn forget_pending(paths: &PandoPaths, name: &str) {
    let Ok(_lock) = state::lock(&paths.lock_file()) else {
        return;
    };
    let Ok(mut store) = state::load(&paths.state_file()) else {
        return;
    };
    drop_pending(&mut store, name);
    let _ = state::save(&paths.state_file(), &store);
}

/// Takes a worktree's public URL down.
pub fn unshare(paths: &PandoPaths, name: &str) -> Result<()> {
    paths.ensure_home()?;
    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    let record = store
        .worktrees
        .get_mut(name)
        .with_context(|| format!("pando has no record of {name}"))?;
    let Some(share) = record.share.take() else {
        bail!("{name} is not shared");
    };
    if let Err(e) = tunnel::stop_share(&share) {
        // Still alive, so the record goes back: state has to match what is
        // really running, or a retry has no pgid to signal.
        record.share = Some(share);
        state::save(&paths.state_file(), &store)?;
        return Err(e.context(format!("could not take {name}'s share down")));
    }
    state::save(&paths.state_file(), &store)?;
    Ok(())
}

/// Clears a share whose tunnel or proxy has died, signalling whatever is
/// left of it *before* the record that names it is dropped.
///
/// The only place that signals. `state::reconcile` and
/// `state::advance_phases` also drop a dead share, and neither can signal
/// anything — `state` knows nothing about processes — so this runs first on
/// every path that reaches them, and they find the record already gone.
///
/// A share is only useful while both halves live: a tunnel whose proxy died
/// serves the wrong thing, and a proxy whose tunnel died is unreachable.
/// Either way both groups are signalled, because a dead leader is not a
/// dead group.
///
/// So is a share that never got as far as a record, because the pando
/// waiting on its tunnel died first: see [`PendingShare`].
pub(super) fn sweep_dead_shares(store: &mut state::State) -> Vec<String> {
    sweep_dead_shares_with(store, proc::is_alive, proc::group_alive, |pgid| {
        proc::stop(pgid, STOP_GRACE)
    })
}

/// [`sweep_dead_shares`] with liveness and the signal injected, so a test
/// can drive it without real process groups.
pub(super) fn sweep_dead_shares_with(
    store: &mut state::State,
    is_alive: impl Fn(u32) -> bool,
    group_alive: impl Fn(proc::Group) -> bool,
    stop: impl Fn(proc::Group) -> Result<()>,
) -> Vec<String> {
    let mut notices = Vec::new();
    for (name, record) in store.worktrees.iter_mut() {
        sweep_interrupted_shares(name, record, &is_alive, &group_alive, &stop, &mut notices);
        // A stop in flight takes the share down itself, once what it
        // points at is gone, and says so.
        if record.stop_in_flight(&is_alive) {
            continue;
        }
        let Some(share) = record.share.clone() else {
            continue;
        };
        let tunnel_dead = !is_alive(share.tunnel_pid);
        let proxy_dead = share.proxy_pid.is_some_and(|pid| !is_alive(pid));
        // `stop` and `rm` unshare first, so a *stopped* worktree never
        // keeps a public URL. A crashed one is the same thing without the
        // announcement: the URL answers, and a proxy in front of it keeps
        // injecting the auth cookie into requests aimed at a port whose
        // owner is gone.
        let nothing_serving = !share_target_is_up(record, &is_alive);
        if !tunnel_dead && !proxy_dead && !nothing_serving {
            continue;
        }
        let reason = if tunnel_dead {
            "the share's tunnel exited"
        } else if proxy_dead {
            "the share's proxy exited"
        } else {
            "nothing is serving what its public URL pointed at"
        };
        match tunnel::stop_share_with(&share, &stop) {
            Ok(()) => {
                record.share = None;
                notices.push(format!("{name}: {reason}, so the public URL is closed"));
            }
            // Not cleared: the record holds the only pgid anything can use
            // to try again.
            Err(e) => notices.push(format!(
                "{name}: {reason} and the rest of it would not stop ({e:#}) — \
                 `pando unshare {name}` to try again"
            )),
        }
    }
    notices
}

/// How long a share can be pending before the pando that wrote it down
/// cannot still be waiting on it: the proxy's time to listen, the tunnel's
/// time to publish, and a minute for the stops and locks either side.
const PENDING_SHARE_DEADLINE: Duration = Duration::from_secs(
    share_proxy::LISTEN_TIMEOUT.as_secs() + tunnel::READY_TIMEOUT.as_secs() + 60,
);

/// Stops what a share spawned when the pando waiting on it is gone, and
/// drops the [`PendingShare`] that named it.
///
/// That pando is gone when its pid has exited, or when the share has been
/// pending for longer than [`PENDING_SHARE_DEADLINE`]. A pid alone is no
/// proof: once its pando exits the system can hand it to anything, and a
/// long-lived process that got it kept an orphaned tunnel and its
/// cookie-holding proxy up for good.
///
/// Every group it names that is still alive is signalled, and the entry is
/// kept only when one would not stop: it holds the only pgids anything can
/// use to try again. One none of whose groups is alive goes without a
/// signal, whoever owns it: there is nothing left to stop, and its pgids
/// may name somebody else's groups by now.
fn sweep_interrupted_shares(
    name: &str,
    record: &mut WorktreeRecord,
    is_alive: &impl Fn(u32) -> bool,
    group_alive: &impl Fn(proc::Group) -> bool,
    stop: &impl Fn(proc::Group) -> Result<()>,
    notices: &mut Vec<String>,
) {
    let now = Utc::now();
    let mut kept = Vec::new();
    for pending in std::mem::take(&mut record.pending_shares) {
        let alive: Vec<proc::Group> = pending
            .pgids
            .iter()
            .copied()
            .filter(|&pgid| group_alive(pgid))
            .collect();
        if alive.is_empty() {
            continue;
        }
        let waited = now.signed_duration_since(pending.since).num_seconds();
        if is_alive(pending.owner_pid) && waited <= PENDING_SHARE_DEADLINE.as_secs() as i64 {
            kept.push(pending);
            continue;
        }
        let failures: Vec<String> = alive
            .iter()
            .filter_map(|&pgid| stop(pgid).err().map(|e| format!("group {pgid}: {e:#}")))
            .collect();
        if failures.is_empty() {
            notices.push(format!(
                "{name}: a share was interrupted before its tunnel was up, so what it had \
                 started is stopped"
            ));
        } else {
            notices.push(format!(
                "{name}: a share was interrupted before its tunnel was up, and what it had \
                 started would not stop ({})",
                failures.join("; ")
            ));
            kept.push(pending);
        }
    }
    record.pending_shares = kept;
}

/// How often the readiness wait asks whether the target is up yet. The
/// same poll a developer does by hand, and `refresh` is cheap.
const SHARE_READY_POLL: Duration = Duration::from_millis(250);

/// Waits for the process a share would publish to leave `Starting`,
/// bounded by that process's own readiness budget.
///
/// `start` records `Starting` and returns; the phase advances on the next
/// read path, once the port is really bound. So `pando start x && pando
/// share x` — the first thing anyone types — used to answer "x is not
/// running, start it first", which is advice the developer had just
/// followed. This is what they would do instead: look at `status` until
/// it says running.
///
/// Not a refusal of its own: the wait ends when the phase changes or the
/// budget runs out, and whatever the state is then goes through the same
/// refusals as before. `refresh` is a read path — it signals, it never
/// spawns — so polling it is safe from here.
fn await_share_target(paths: &PandoPaths, name: &str, shown: &str, progress: &dyn Fn(&str)) {
    let Some(budget) = share_ready_budget(&refresh(paths).state, name) else {
        return;
    };
    progress(&format!(
        "waiting up to {}s for {shown} to be ready",
        budget.as_secs().max(1)
    ));
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        std::thread::sleep(SHARE_READY_POLL);
        if share_ready_budget(&refresh(paths).state, name).is_none() {
            return;
        }
    }
}

/// How long the process a share would publish still has to become ready,
/// or `None` when there is nothing to wait for.
///
/// The budget `advance_phases` itself uses, so the wait ends when that
/// function gives up rather than a moment before or a minute after.
pub(super) fn share_ready_budget(store: &state::State, name: &str) -> Option<Duration> {
    let record = store.worktrees.get(name)?;
    let starting = |p: &state::ProcessRecord| match p.phase {
        Phase::Starting { since } => Some((p.ready_timeout_s, since)),
        _ => None,
    };
    let (timeout_s, since) = match share_owner(record) {
        UrlOwner::Recorded(process) => starting(process)?,
        // Nothing is coming up to serve it, whatever its siblings are doing.
        UrlOwner::Absent(_) => return None,
        UrlOwner::Unknown => {
            if record
                .processes
                .values()
                .any(|p| matches!(p.phase, Phase::Running { .. }))
            {
                return None;
            }
            record.processes.values().find_map(starting)?
        }
    };
    // The window *and* the grace an unanswerable port scan earns, because
    // that is how long the phase can really stay `Starting`. The wait still
    // ends as soon as the phase changes, so a conclusive scan costs
    // nothing extra; the window alone gave up on a healthy server whose
    // scan was merely slow.
    let budget = state::longest_starting_secs(
        timeout_s
            .map(|s| s as i64)
            .unwrap_or(state::START_TIMEOUT_SECS),
    );
    let left = budget - Utc::now().signed_duration_since(since).num_seconds();
    Some(Duration::from_secs(left.max(0) as u64) + SHARE_READY_POLL)
}

/// Whether anything is still up to serve what a share of this worktree
/// points at.
///
/// Liveness as well as phase, because this runs *before* `advance_phases`
/// on every path: a process that died a second ago still says `Running`,
/// and waiting a tick to notice is a tick of a public door onto nothing.
/// `Starting` counts — a worktree that is coming up is not one with
/// nothing serving, and tearing its share down would be the same mistake
/// inverted.
///
/// A sibling does not count. The URL is one process's port, so when that
/// process was stopped with `--only`, or never started by a `start --only`
/// of something else, nothing is serving it however much else is up.
pub(super) fn share_target_is_up(record: &WorktreeRecord, is_alive: &impl Fn(u32) -> bool) -> bool {
    let up = |p: &state::ProcessRecord| {
        matches!(p.phase, Phase::Running { .. } | Phase::Starting { .. }) && is_alive(p.pid)
    };
    match share_owner(record) {
        UrlOwner::Recorded(process) => up(process),
        UrlOwner::Absent(_) => false,
        // A record written before pando tracked who owns what: anything up
        // is as much as it can say.
        UrlOwner::Unknown => record.processes.values().any(up),
    }
}

/// The port a share points at: the one the worktree's own URL uses, and
/// only while the process that owns it is running.
///
/// The same rule and the same words as the TUI's open key, because
/// "shareable" and "openable" have to mean the same thing.
///
/// A refusal says which of the three states it found. They need different
/// answers — wait, read the log, start it — and one message for all three
/// sent a developer who had just run `start` back to run it again.
pub(super) fn share_target_port(name: &str, record: &WorktreeRecord) -> Result<u16> {
    let Some(role) = share_role(record) else {
        bail!("{name} has no port yet — start it first");
    };
    let Some(assigned) = record.ports.get(&role).copied() else {
        bail!("{name} has no port yet — start it first");
    };
    match share_state(record) {
        Some(Phase::Running { .. }) => {}
        Some(Phase::Starting { .. }) => bail!(
            "{name} is still starting — `pando status {name}` says when it is ready, and \
             `pando share {name}` then works"
        ),
        Some(Phase::Failed { reason, .. }) => bail!(
            "{name} failed to start ({reason}) — `pando logs {name}` says why; there is nothing \
             for a public URL to point at yet"
        ),
        None => match share_owner_not_running(record) {
            Some(owner) => bail!(
                "{name} is not running {owner}, the process its URL points at — start it, then \
                 share it"
            ),
            None => bail!("{name} is not running — start it first, then share it"),
        },
    }
    // What is really serving, not what pando asked for.
    Ok(observed_port_for_role(record, &role).unwrap_or(assigned))
}

/// The phase of the process a share would publish: the owner of the URL's
/// role, or — for a record written before pando tracked who owns what —
/// the best of what the worktree is running. `None` when the owner is not
/// running, whatever its siblings are.
fn share_state(record: &WorktreeRecord) -> Option<Phase> {
    match share_owner(record) {
        UrlOwner::Recorded(process) => return Some(process.phase.clone()),
        UrlOwner::Absent(_) => return None,
        UrlOwner::Unknown => {}
    }
    let best = |wanted: fn(&Phase) -> bool| {
        record
            .processes
            .values()
            .find(|p| wanted(&p.phase))
            .map(|p| p.phase.clone())
    };
    best(|p| matches!(p, Phase::Running { .. }))
        .or_else(|| best(|p| matches!(p, Phase::Starting { .. })))
        .or_else(|| best(|_| true))
}

/// Runs `[share].auth_cmd` and returns the `Cookie` header value it printed.
///
/// It runs in the worktree, with the same environment the processes get
/// plus the proxy's port, so a script can mint a session against the very
/// database the application is using.
fn run_auth_cmd(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    worktree: &Worktree,
    canonical: &Path,
    cmd: &str,
    share_port: u16,
) -> Result<String> {
    let mut env = pando_env(paths, name, worktree.branch.as_deref(), canonical);
    // Best effort: a worktree with no ports never reaches here, and a
    // template that will not render is not a reason to refuse a share the
    // script may not even need it for.
    if let Ok(resolved) = resolved_env(paths, config, name) {
        env.extend(resolved);
    }
    env.push((ENV_SHARE_PORT.to_string(), share_port.to_string()));

    let captured = proc::run_captured(
        &with_prelude(config, cmd),
        canonical,
        &env,
        AUTH_CMD_TIMEOUT,
    )
    .with_context(|| format!("the [share].auth_cmd of {name}"))?;
    if !captured.success() {
        let reason = match captured.last_stderr_line() {
            Some(line) => format!(" — {line}"),
            None => String::new(),
        };
        bail!(
            "[share].auth_cmd exited {}{reason} — it runs in {}, and its stdout is the Cookie \
             header pando injects",
            captured.code.unwrap_or(-1),
            canonical.display()
        );
    }
    let cookie = captured.stdout.trim().to_string();
    if cookie.is_empty() {
        bail!(
            "[share].auth_cmd printed nothing — its stdout is the Cookie header value pando \
             injects, so an empty one has nothing to inject"
        );
    }
    // A header is one line. A value carrying a newline would let a script
    // append headers of its own to every proxied request.
    if cookie.contains(|c: char| c.is_control()) {
        bail!(
            "[share].auth_cmd printed a value with a control character in it — a Cookie header \
             is a single line"
        );
    }
    Ok(cookie)
}
