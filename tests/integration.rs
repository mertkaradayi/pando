//! Every integration test, built as one binary.
//!
//! Each file beside this one was once its own test binary. Cargo links
//! every one of those against the whole library and runs them one after
//! another, so a change paid for fourteen links and the suite waited on
//! the slowest file of each in turn. As modules of one binary they link
//! once and share one pool of test threads. Run one file's tests with its
//! module name as the filter: `cargo test --test integration cli::`.

mod common;

mod agent;
mod check;
mod cli;
mod detect;
mod distribution;
mod docker;
mod engines;
mod git;
mod hooks;
mod invariant;
mod isolation;
mod multi_repo;
mod namespaced;
mod native;
mod services;
mod share;
mod tunnel;
mod update;
