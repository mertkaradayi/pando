//! Process groups on Windows: not made yet.
//!
//! A group there is a job object, assigned while the first process is
//! suspended, before it can start anything; stopping it is a console break
//! and then the job's end, and its members and ports are the job's process
//! list and the TCP table. Until that is built, nothing is started, and
//! nothing recorded is taken to be running.

use super::Group;
use std::collections::BTreeMap;
use std::io;
use std::process::{Child, Command, Output};
use std::time::Duration;

fn not_yet() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "a native Windows build cannot start a process group yet",
    )
}

pub(super) fn spawn_group(_: &mut Command) -> io::Result<(Child, Group)> {
    Err(not_yet())
}

pub(super) fn spawn_in_group(_: &mut Command) -> io::Result<(Child, Group)> {
    Err(not_yet())
}

pub(super) fn is_alive(_: u32) -> bool {
    false
}

pub(super) fn group_alive(_: Group) -> bool {
    false
}

pub(super) fn stop(_: Group, _: Duration) -> anyhow::Result<()> {
    Err(not_yet().into())
}

pub(super) fn output_within(_: Command, _: Duration) -> io::Result<Output> {
    Err(not_yet())
}

/// Every group unscanned: a scan that could not run, not one that found
/// nothing.
pub(super) fn ports_by_group(groups: &[Group]) -> BTreeMap<Group, Option<Vec<u16>>> {
    groups.iter().map(|group| (*group, None)).collect()
}

/// Nothing to settle: no fork.
pub(super) fn settle_before_fork() {}
