//! Processes pando starts in groups of their own, and finds again.
//!
//! One file per concern: what a group is, how the OS starts, asks after
//! and stops one (`unix.rs`), and which ports a group's processes listen
//! on (`scan.rs`). The functions here carry the contract
//! every backend keeps; a group is started by one call and handed back
//! with it, because on some systems that is the only moment it can be
//! made.

mod group;
#[cfg(unix)]
mod scan;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as imp;

pub use group::Group;

use std::collections::BTreeMap;
use std::io;
use std::process::{Child, Command, Output};
use std::time::Duration;

/// Spawns `command` as the first process of a group of its own, with no
/// controlling terminal: nothing it runs can prompt on pando's terminal,
/// and no signal the terminal sends reaches it. Everything it starts stays
/// in the group unless it leaves on purpose.
pub fn spawn_group(command: &mut Command) -> io::Result<(Child, Group)> {
    imp::spawn_group(command)
}

/// Spawns `command` as the first process of a group of its own in pando's
/// session. It keeps pando's controlling terminal, as a background group,
/// so no signal the terminal sends reaches it: whoever spawned it passes
/// them on, with [`stop`]. Everything it starts stays in the group unless
/// it leaves on purpose.
pub fn spawn_in_group(command: &mut Command) -> io::Result<(Child, Group)> {
    imp::spawn_in_group(command)
}

/// Whether `pid` is running.
///
/// A child of this pando that has ended is collected here as a side
/// effect: an uncollected one is a zombie, which still counts as a member
/// of its group and keeps [`group_alive`] true. `state::record_alive` asks
/// this first for that reason.
pub fn is_alive(pid: u32) -> bool {
    imp::is_alive(pid)
}

/// Whether anything is left in `group`: the leader, a child it forked, or
/// a grandchild. False for any group that could never be one of pando's.
pub fn group_alive(group: Group) -> bool {
    imp::group_alive(group)
}

/// Asks everything in `group` to end, waits up to `grace`, then ends
/// whatever is left. Returns once the group is empty, or once the end has
/// had a moment to land. Never acts on the group pando itself runs in.
///
/// Unconditional, including for a group pando has already written off as
/// failed: a leader that exited is not a group that exited, and skipping
/// the signal is exactly how a backgrounded child outlives the tool that
/// started it.
pub fn stop(group: Group, grace: Duration) -> anyhow::Result<()> {
    imp::stop(group, grace)
}

/// `Command::output`, with a deadline.
///
/// The child leads its own group, so a timeout ends whatever it started as
/// well; the pipes are read on their own threads, so a child that fills
/// one cannot deadlock against the wait, and the reads are bounded too,
/// because anything the child left behind can hold a pipe open after it
/// exits. It keeps pando's terminal, so git can still prompt from the CLI.
/// A timeout is `ErrorKind::TimedOut`.
///
/// The group is never signalled once the child's exit has been collected:
/// after that its id can belong to someone else.
pub fn output_within(command: Command, timeout: Duration) -> io::Result<Output> {
    imp::output_within(command, timeout)
}

/// The TCP ports each group listens on, from one scan however many groups
/// there are.
///
/// `None` for a group is a scan that could not run, which is not a group
/// listening on nothing: readiness falls back to asking the port itself
/// for the first, and believes the second.
pub fn ports_by_group(groups: &[Group]) -> BTreeMap<Group, Option<Vec<u16>>> {
    imp::ports_by_group(groups)
}

/// Makes a fork safe in this process before any thread could make one
/// unsafe: see the backend.
pub(super) fn settle_before_fork() {
    imp::settle_before_fork();
}

#[cfg(test)]
mod tests;
