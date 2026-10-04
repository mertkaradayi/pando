//! A check that is told to stop — Ctrl-C, a closed terminal, a `kill` —
//! still takes its worktree down before it goes.

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the handler [`catch_check_interrupts`] installs. Read between
/// the check's steps and on every look it takes while it waits.
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

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
/// leaves the next time it runs. A hook the check runs in pando's own
/// process group gets a terminal's Ctrl-C itself and ends; one that a
/// `kill` of pando alone does not reach runs to its end first.
pub fn catch_check_interrupts() {
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
    let action = SigAction::new(
        SigHandler::Handler(note_interrupt),
        SaFlags::SA_RESETHAND,
        SigSet::empty(),
    );
    for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
        // The handler stores one atomic, which is safe in one.
        let _ = unsafe { sigaction(signal, &action) };
    }
}

extern "C" fn note_interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Whether the check, or a `new` in its checkout, has been told to stop.
pub(in crate::actions) fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}
