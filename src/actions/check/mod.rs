//! `pando check`: the test that says a project's setup works.
//!
//! It makes a throwaway worktree of the commit a new one would fork from,
//! with no branch, and runs it the way a first shared start would: the
//! install, every process, the page the browser would get. Then it takes
//! all of it down and records the result in `check.json`, which the setup
//! state and the TUI read. One file per concern: the run, the machine it
//! runs on, where its logs are kept, the teardown and the sweep of what a
//! killed check left, and the signals that end one early.

mod base;
mod interrupt;
mod logs;
mod machine;
mod run;
mod teardown;

pub use interrupt::catch_check_interrupts;
pub(super) use interrupt::interrupted;
pub use run::{CHECK_RAN_BY_ENV, Checked, Narration, check, check_at, ran_by};
pub use teardown::{LeftoverCheck, leftover_check};

#[cfg(test)]
mod tests;
