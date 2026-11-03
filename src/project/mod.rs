//! The repository pando is looking at, and the identity it derives from it.
//!
//! Discovery is from `git worktree list --porcelain`, whose first entry is
//! always the main checkout — from any cwd, including inside a linked
//! worktree. `git rev-parse --git-common-dir` is deliberately not used: from
//! a subdirectory it prints a relative path such as `../.git`.
//!
//! A project may also be several repositories: a plain folder whose
//! immediate subdirectories are git repositories (`product/backend`,
//! `product/frontend`) is one project, and a worktree of it is a *set*, a
//! directory holding a linked worktree of every repository under the same
//! name. [`ProjectRef::repos`] says which it is.

//!
//! Each project pando has run in has a directory under the home, named by
//! its id, and in it `project.json` (`registry.rs`) says where the
//! repository is, so a reader of every project need not ask git.

mod discover;
mod registry;
#[cfg(test)]
mod tests;

pub(crate) use discover::canonicalize;
pub use discover::{
    GIT_TIMEOUT, PROJECTS_DIR, ProjectRef, discover, folder_of, git, git_within, linked_gitdir,
    repos_in, set_folder,
};
pub use registry::{PROJECT_FILE, Registered, project_dirs, registered, remember, remember_as};
