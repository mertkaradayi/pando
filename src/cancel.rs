//! A stop asked of work that runs on one thread, read where that work can
//! stop safely: the TUI's start, which `x` ends halfway.
//!
//! The caller hands the work a [`Cancel`] by running it inside
//! [`Cancel::run`]; whoever holds a clone may [`Cancel::cancel`] it from
//! any thread. The work reads [`check`] at its safe points — between a
//! start's phases, and on every look a readiness wait takes — and fails
//! there, so the undo it runs on any failure is the one that runs.
//! Nothing here interrupts what is already running: a hook runs to its
//! end, and the next safe point after it stops the work.
//!
//! A thread-local, as `new`'s own stop is, so the layers between the
//! caller and the waits — compose, native servers — take no new argument,
//! and work that nobody runs inside a [`Cancel`] is never cancelled.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What cancelled work fails with at its safe point.
pub const CANCELLED: &str = "cancelled before it finished";

/// A stop that one thread's work reads and any thread may ask for.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

thread_local! {
    static CURRENT: RefCell<Option<Cancel>> = const { RefCell::new(None) };
}

impl Cancel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the work to stop at its next safe point.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Runs `work` on this thread with this cancel in force: every
    /// [`check`] inside it fails once [`Cancel::cancel`] has been called,
    /// from whichever thread.
    pub fn run<T>(&self, work: impl FnOnce() -> T) -> T {
        /// Puts back whatever was in force before, even if `work` panics.
        struct Restore(Option<Cancel>);
        impl Drop for Restore {
            fn drop(&mut self) {
                let previous = self.0.take();
                CURRENT.with(|current| *current.borrow_mut() = previous);
            }
        }
        let previous = CURRENT.with(|current| current.replace(Some(self.clone())));
        let _restore = Restore(previous);
        work()
    }
}

/// Whether the work this thread runs has been cancelled.
pub fn requested() -> bool {
    CURRENT.with(|current| current.borrow().as_ref().is_some_and(Cancel::is_cancelled))
}

/// A safe point: an error once the work this thread runs was cancelled.
pub fn check() -> anyhow::Result<()> {
    match requested() {
        true => anyhow::bail!(CANCELLED),
        false => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_is_seen_only_by_the_work_it_was_handed_to() {
        assert!(check().is_ok(), "no work runs inside a cancel");
        let cancel = Cancel::new();
        let seen = cancel.run(|| {
            assert!(check().is_ok());
            let other = cancel.clone();
            std::thread::spawn(move || other.cancel()).join().unwrap();
            // Another thread's work is not this one's.
            let elsewhere = std::thread::spawn(requested).join().unwrap();
            (requested(), elsewhere)
        });
        assert_eq!(seen, (true, false));
        assert!(!requested(), "and it is out of force once the work is done");
    }

    #[test]
    fn a_cancel_in_force_is_put_back_after_a_nested_one() {
        let outer = Cancel::new();
        outer.cancel();
        outer.run(|| {
            Cancel::new().run(|| assert!(!requested()));
            assert!(requested());
        });
    }
}
