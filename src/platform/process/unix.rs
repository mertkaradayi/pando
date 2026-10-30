//! Process groups on Unix: sessions, `killpg`, `waitpid` and `waitid`.

use super::Group;
use std::collections::BTreeMap;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Starts the child `command` spawns in a session of its own: the leader of
/// a new process group, with no controlling terminal.
///
/// The one place pando asks for that, because it is `setsid` in the child
/// between fork and exec, so the child is forked rather than spawned, and
/// a fork has to be made safe first: [`settle_before_fork`].
fn new_session(command: &mut Command) -> &mut Command {
    settle_before_fork();
    // SAFETY: `setsid` is async-signal-safe, and it is all the closure does.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(|e| io::Error::from_raw_os_error(e as i32))
        });
    }
    command
}

pub(super) fn spawn_group(command: &mut Command) -> io::Result<(Child, Group)> {
    let child = new_session(command).spawn()?;
    // After `setsid` the child leads a group whose id is its pid.
    let group = Group::from_raw(child.id() as i32);
    Ok((child, group))
}

pub(super) fn spawn_in_group(command: &mut Command) -> io::Result<(Child, Group)> {
    let child = command.process_group(0).spawn()?;
    // A group of 0 is one led by the child, whose id is its pid.
    let group = Group::from_raw(child.id() as i32);
    Ok((child, group))
}

pub(super) fn ports_by_group(groups: &[Group]) -> BTreeMap<Group, Option<Vec<u16>>> {
    super::scan::observed_ports_by_group(groups)
}

/// Finishes libnotify's one-time set-up in this process before any fork,
/// so no fork can land in the middle of it.
///
/// On macOS a forked child runs libSystem's fork handlers before it execs,
/// and libnotify's reaches its globals through a once-gate. When another
/// thread is setting them up at the moment of the fork — the first FSEvents
/// watcher in a process does, the TUI's among them — the child finds the
/// gate held by a thread it does not have and is killed before `exec`
/// ("os_once_t is corrupt", in its crash report): a dev server, a tunnel or
/// a hook that never ran, with nothing in its log. The set-up happens once
/// per process, so finishing it here, on the thread about to fork, closes
/// the window for every fork after. Any libnotify call does it; this one
/// asks nothing of the daemon.
///
/// `platform::init` calls it first, before the TUI or anything else starts
/// a thread, so a fork std makes for any reason finds it settled, not only
/// the ones [`new_session`] asks for. `new_session` calls it as well, for
/// the test binaries, which never run `main`.
#[cfg(target_os = "macos")]
pub(super) fn settle_before_fork() {
    LIBNOTIFY_SETTLED.call_once(|| {
        unsafe extern "C" {
            fn notify_is_valid_token(token: libc::c_int) -> bool;
        }
        // SAFETY: it takes any integer, and reads nothing but its argument
        // and libnotify's own globals.
        unsafe { notify_is_valid_token(0) };
    });
}

/// Done once [`settle_before_fork`] has run in this process.
#[cfg(target_os = "macos")]
pub(super) static LIBNOTIFY_SETTLED: std::sync::Once = std::sync::Once::new();

/// Nothing to settle: the handlers that make a fork unsafe mid-set-up are
/// macOS's.
#[cfg(not(target_os = "macos"))]
pub(super) fn settle_before_fork() {}

pub(super) fn is_alive(pid: u32) -> bool {
    // Reap zombie first if we're the parent (TUI session). Returns
    // ECHILD when pid isn't our child — fall through to signal probe.
    match waitpid(Pid::from_raw(pid as i32), Some(WaitPidFlag::WNOHANG)) {
        Ok(WaitStatus::Exited(..)) | Ok(WaitStatus::Signaled(..)) => return false,
        Ok(WaitStatus::StillAlive) => return true,
        _ => {}
    }
    match nix::sys::signal::kill(Pid::from_raw(pid as i32), None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        Err(_) => false,
    }
}

/// `kill(-pgid, 0)` and not `kill(leader, 0)`: a dev server's group leader is
/// a `bash -lc` that very often exits while the server it backgrounded keeps
/// running and keeps the port. Asking after the leader alone is how a stop
/// reports success over a process that is still serving.
pub(super) fn group_alive(group: Group) -> bool {
    let pgid = group.as_raw();
    // 0 is "our own group" and 1 is "every process we may signal". Neither
    // is ever a worktree's process group, and both would be catastrophic to
    // pass to kill(2).
    if pgid <= 1 {
        return false;
    }
    match nix::sys::signal::kill(Pid::from_raw(-pgid), None) {
        Ok(()) => true,
        Err(Errno::ESRCH) => false,
        // The group exists but holds something we may not signal.
        Err(Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// SIGTERM to the whole group, then SIGKILL to whatever is left after
/// `grace`.
pub(super) fn stop(target: Group, grace: Duration) -> anyhow::Result<()> {
    let pgid = target.as_raw();
    // Nor the group pando itself runs in. Nothing pando spawns is in it —
    // every spawn leads a session of its own — so a record naming it is a
    // number that has been handed out again, and the shell that ran this
    // pando leads its job's group: signalling it is pando killing itself,
    // and its terminal's job with it, halfway through a mutation.
    if pgid <= 1 || pgid == nix::unistd::getpgrp().as_raw() {
        return Ok(());
    }
    let group = Pid::from_raw(pgid);
    let any_in_group = Pid::from_raw(-pgid);

    match killpg(group, Signal::SIGTERM) {
        Ok(()) => {}
        // Nothing left in the group; there is nothing to wait for.
        Err(Errno::ESRCH) => {
            reap_group(any_in_group);
            return Ok(());
        }
        // macOS returns EPERM for a group whose only member is a zombie
        // leader. Falling through reaps it.
        Err(Errno::EPERM) => {}
        Err(e) => return Err(anyhow::Error::from(e).context("killpg SIGTERM")),
    }

    let deadline = Instant::now() + grace;
    loop {
        // Reap first: a zombie child of ours still counts as a group member
        // to kill(2), so an unreaped leader would look alive forever.
        reap_group(any_in_group);
        if !group_alive(target) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    match killpg(group, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) | Err(Errno::EPERM) => {}
        Err(e) => return Err(anyhow::Error::from(e).context("killpg SIGKILL")),
    }
    // SIGKILL is not instantaneous; the caller is about to reuse this
    // worktree's ports, so it is worth the few milliseconds to see it land.
    let hard_deadline = Instant::now() + KILL_SETTLE;
    loop {
        reap_group(any_in_group);
        if !group_alive(target) || Instant::now() >= hard_deadline {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// How long to wait for SIGKILL to be delivered before giving up and
/// returning anyway.
const KILL_SETTLE: Duration = Duration::from_millis(500);

fn reap_group(any_in_group: Pid) {
    loop {
        match waitpid(Some(any_in_group), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(_) => break,
            Ok(_) => continue,
        }
    }
}

pub(super) fn output_within(mut command: Command, timeout: Duration) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command.spawn()?;
    let pgid = Pid::from_raw(child.id() as i32);
    let reader = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let stdout = reader(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = reader(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let kill_group = || {
        let _ = killpg(pgid, Signal::SIGKILL);
    };

    // The leader is left unreaped until the group has been dealt with. A
    // zombie still holds its pid, and so the group id: once it is reaped
    // and its group is empty, the number can go to an unrelated process
    // group, and a `killpg` after that point is a signal to a stranger.
    // So the exit is *seen* here without being collected, and the child is
    // only reaped below, after the last `killpg`.
    let pid = child.id() as i32;
    let deadline = Instant::now() + timeout;
    let mut reaped: Option<std::process::ExitStatus> = None;
    loop {
        match exited_unreaped(pid) {
            Some(true) => break,
            Some(false) => {}
            // `waitid` not available: the old way, reaping to find out.
            None => {
                if let Some(status) = child.try_wait()? {
                    reaped = Some(status);
                    break;
                }
            }
        }
        if Instant::now() >= deadline {
            kill_group();
            let _ = child.wait();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("no answer in {}s", timeout.as_secs_f32()),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let collect = |rx: &mpsc::Receiver<Vec<u8>>| {
        let left = deadline.saturating_duration_since(Instant::now());
        rx.recv_timeout(left.max(Duration::from_millis(100)))
    };
    let mut out = collect(&stdout);
    let mut err = collect(&stderr);
    if out.is_err() || err.is_err() {
        // Exited, but something it started is still holding a pipe. That
        // group is this call's own, so it goes — which closes the pipe, and
        // what was already written is still the answer. Only while the
        // leader is unreaped, which is what makes the group still ours.
        if reaped.is_none() {
            kill_group();
        }
        let settle = Duration::from_millis(500);
        if out.is_err() {
            out = stdout.recv_timeout(settle);
        }
        if err.is_err() {
            err = stderr.recv_timeout(settle);
        }
    }
    let (out, err) = (out.unwrap_or_default(), err.unwrap_or_default());
    // Reaped last: nothing signals the group after this.
    let status = match reaped {
        Some(status) => status,
        None => child.wait()?,
    };
    Ok(Output {
        status,
        stdout: out,
        stderr: err,
    })
}

/// Whether the child `pid` has exited, *without* reaping it: `Some(true)`
/// once it has, `Some(false)` while it runs, `None` where `waitid` cannot
/// say.
pub(super) fn exited_unreaped(pid: i32) -> Option<bool> {
    // SAFETY: `waitid` writes only into `info`, which is zeroed and owned
    // here; `WNOWAIT` leaves the child waitable for the `wait` that reaps it.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::waitid(
            libc::P_PID,
            pid as libc::id_t,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if rc != 0 {
        return None;
    }
    // With `WNOHANG`, a child that has not changed state leaves `si_pid`
    // zero.
    Some(siginfo_pid(&info) != 0)
}

#[cfg(target_os = "linux")]
fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    // SAFETY: filled by `waitid`, for which `si_pid` is the valid member.
    unsafe { info.si_pid() }
}

#[cfg(not(target_os = "linux"))]
fn siginfo_pid(info: &libc::siginfo_t) -> libc::pid_t {
    info.si_pid
}
