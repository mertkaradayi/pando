//! Every project pando knows on this machine, read from its home alone:
//! where each one's repository is, recovered for a project registered
//! before `project.json` existed.

mod recover;
#[cfg(test)]
mod tests;

pub use recover::{checkout_root, recorded_roots, recover_registry, worktrees_in_project_dir};
