//! What pando does when the OS tells it to stop.
//!
//! Two answers, each installed by the part of pando that wants it. A check
//! (or a `new` mid-checkout) notes the request and unwinds through its own
//! teardown: [`catch_interrupts`] and [`interrupted`]. The TUI, whose hooks
//! run with no terminal of their own, hangs up on them before it goes:
//! [`hang_up_on_exit`], [`hang_up_now`] and the groups each
//! [`HangUpOnExit`] holds. Both handlers do nothing a signal handler may
//! not: atomics, a hang-up, and dying of the signal as pando would have.

use super::process::Group;

/// Makes an interrupt, a termination or a hang-up note itself rather than
/// end pando where it lands: [`interrupted`] says so from then on. A second
/// of the same kind ends pando at once, as it would have without this.
pub fn catch_interrupts() {
    imp::catch_interrupts();
}

/// Whether pando has been told to stop since [`catch_interrupts`].
pub fn interrupted() -> bool {
    imp::interrupted()
}

/// Whether [`catch_interrupts`] has run: pando notes an interrupt rather
/// than ending of it, so what it runs that the signal did not reach is
/// pando's own to stop.
pub fn catching() -> bool {
    imp::catching()
}

/// Before a hang-up or a termination ends pando, hang up on every group a
/// [`HangUpOnExit`] holds: what each would have had from the terminal, had
/// it shared pando's.
pub fn hang_up_on_exit() {
    imp::hang_up_on_exit();
}

/// Hangs up now on every group a [`HangUpOnExit`] holds, and on any held
/// after this.
pub fn hang_up_now() {
    imp::hang_up_now();
}

/// A group to hang up on when pando goes, for as long as this lives. One
/// past the slots there are is not held, and only not hung up on.
pub struct HangUpOnExit(Option<usize>);

impl HangUpOnExit {
    /// Holds `group`; hangs up on it at once when [`hang_up_now`] has run.
    pub fn hold(group: Group) -> HangUpOnExit {
        HangUpOnExit(imp::hold(group))
    }
}

impl Drop for HangUpOnExit {
    fn drop(&mut self) {
        if let Some(slot) = self.0 {
            imp::release(slot);
        }
    }
}

/// The groups held now, for the tests that watch a hook be held and let go.
#[cfg(test)]
pub fn held() -> Vec<Group> {
    imp::held()
}

#[cfg(unix)]
mod imp {
    use super::Group;
    use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, killpg, sigaction};
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Set by [`note_interrupt`].
    static INTERRUPTED: AtomicBool = AtomicBool::new(false);

    /// Set once [`catch_interrupts`] has installed its handler.
    static CATCHING: AtomicBool = AtomicBool::new(false);

    /// How many groups can be held at once. One hook runs at a time per
    /// worktree action, so this is far more than the TUI ever has in
    /// flight.
    const SLOTS: usize = 64;

    /// Every held group's id, 0 in a free slot. Atomics rather than a
    /// locked list, because a signal handler reads them.
    static HELD: [AtomicI32; SLOTS] = [const { AtomicI32::new(0) }; SLOTS];

    /// Set once [`hang_up_now`] has run: a group held after it, as a
    /// fallback hook started when the command before it was hung up on,
    /// is hung up on as soon as it is held.
    static HUNG_UP: AtomicBool = AtomicBool::new(false);

    pub(super) fn catch_interrupts() {
        let action = SigAction::new(
            SigHandler::Handler(note_interrupt),
            SaFlags::SA_RESETHAND,
            SigSet::empty(),
        );
        for signal in [Signal::SIGINT, Signal::SIGTERM, Signal::SIGHUP] {
            // The handler stores one atomic, which is safe in one.
            let _ = unsafe { sigaction(signal, &action) };
        }
        CATCHING.store(true, Ordering::SeqCst);
    }

    pub(super) fn catching() -> bool {
        CATCHING.load(Ordering::SeqCst)
    }

    extern "C" fn note_interrupt(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }

    pub(super) fn interrupted() -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }

    pub(super) fn hang_up_on_exit() {
        let action = SigAction::new(
            SigHandler::Handler(hang_up_and_die),
            SaFlags::SA_RESETHAND,
            SigSet::empty(),
        );
        for signal in [Signal::SIGHUP, Signal::SIGTERM] {
            // The handler only touches atomics and calls `killpg` and
            // `raise`, which are safe in one.
            let _ = unsafe { sigaction(signal, &action) };
        }
    }

    /// Hangs up on the held groups, then dies of the signal as pando would
    /// have with no handler: `SA_RESETHAND` put the default action back on
    /// the way in.
    extern "C" fn hang_up_and_die(signal: libc::c_int) {
        hang_up_now();
        unsafe { libc::raise(signal) };
    }

    pub(super) fn hang_up_now() {
        HUNG_UP.store(true, Ordering::SeqCst);
        for slot in &HELD {
            hang_up(slot.load(Ordering::SeqCst));
        }
    }

    fn hang_up(pgid: i32) {
        if pgid > 0 {
            let _ = killpg(nix::unistd::Pid::from_raw(pgid), Signal::SIGHUP);
        }
    }

    pub(super) fn hold(group: Group) -> Option<usize> {
        let pgid = group.as_raw();
        let slot = HELD.iter().position(|slot| {
            slot.compare_exchange(0, pgid, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        });
        // After the slot is written, so that either this sees the flag or
        // `hang_up_now` sees the slot.
        if HUNG_UP.load(Ordering::SeqCst) {
            hang_up(pgid);
        }
        slot
    }

    pub(super) fn release(slot: usize) {
        HELD[slot].store(0, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(super) fn held() -> Vec<Group> {
        HELD.iter()
            .map(|slot| slot.load(Ordering::SeqCst))
            .filter(|pgid| *pgid != 0)
            .map(Group::from_raw)
            .collect()
    }
}

/// Not installed yet: a console control handler is the way to both. Until
/// it is, Ctrl-C ends pando where it lands, and nothing is held.
#[cfg(windows)]
mod imp {
    use super::Group;

    pub(super) fn catch_interrupts() {}

    pub(super) fn interrupted() -> bool {
        false
    }

    pub(super) fn catching() -> bool {
        false
    }

    pub(super) fn hang_up_on_exit() {}

    pub(super) fn hang_up_now() {}

    pub(super) fn hold(_: Group) -> Option<usize> {
        None
    }

    pub(super) fn release(_: usize) {}

    #[cfg(test)]
    pub(super) fn held() -> Vec<Group> {
        Vec::new()
    }
}
