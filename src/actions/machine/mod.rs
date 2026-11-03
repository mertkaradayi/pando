//! Every project pando knows on this machine, read from its home alone:
//! which projects there are and where each one's repository is
//! (`known.rs`), recovered for a project registered before `project.json`
//! existed (`recover.rs`), and what each one has up (`summary.rs`), read
//! the way every reader of another pando's state reads it.
//!
//! Every function takes the home it reads, so a test reads one it made and
//! never the developer's own.

mod known;
mod recover;
mod summary;
#[cfg(test)]
mod tests;

pub use known::{KnownProject, known};
pub use recover::{checkout_root, recorded_roots, recover_registry, worktrees_in_project_dir};
pub use summary::{LiveProcess, LiveWorktree, ProjectSummary, port_clashes, summaries};
