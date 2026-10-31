//! A check that is told to stop — Ctrl-C, a closed terminal, a `kill` —
//! still takes its worktree down before it goes.

/// Makes SIGINT, SIGTERM and SIGHUP end a check through its own teardown
/// rather than where they land: the handler only notes the signal, and the
/// check, which looks between every step and every quarter second while
/// it waits, stops, removes its worktree and records itself interrupted.
///
/// For the CLI, once, before the check — and before `new`, whose
/// copy-on-write checkout is pando's own work rather than one `git
/// worktree add`, which cleans up after itself when told to stop: a `new`
/// told to stop mid-checkout unwinds what it made instead of leaving a
/// half-filled worktree. A second signal of the same kind
/// ends pando at once, as it would have without the handler: somebody
/// pressing Ctrl-C twice means it, and `pando check` sweeps whatever that
/// leaves the next time it runs. No hook starts once it has been told,
/// and one already running is stopped with it: in front of a terminal the
/// hook shares pando's group and gets the Ctrl-C itself; anywhere else it
/// leads a group of its own, which pando stops (`hooks::run`).
pub fn catch_check_interrupts() {
    crate::platform::signals::catch_interrupts();
}

/// Whether the check, or a `new` in its checkout, has been told to stop.
/// Read between the check's steps and on every look it takes while it
/// waits.
pub(in crate::actions) fn interrupted() -> bool {
    crate::platform::signals::interrupted()
}
