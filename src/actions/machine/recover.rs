//! Where a project's repository was, worked out from what its directory
//! records: for a project registered before `project.json` existed, and
//! for `doctor`'s adoption of a folder a moved repository left behind.

use std::path::{Path, PathBuf};

use crate::project::{self, Registered};
use crate::state;

/// Every repository a project directory's contents name, in the order
/// they are trusted: its registry, the `[project] root` its config was
/// written with, then the checkouts it knows — each worktree still inside
/// it, and each one its state file records, which covers a worktree kept
/// in a configured `worktrees_dir` and a main checkout that was started.
///
/// Spawns nothing: a checkout's own `.git` names its repository
/// ([`checkout_root`]), where `git rev-parse --git-common-dir` would need
/// the checkout to still be a working one.
pub fn recorded_roots(project_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = project::registered(project_dir)
        .map(|registered| registered.root)
        .into_iter()
        .collect();
    out.extend(
        std::fs::read_to_string(project_dir.join("pando.toml"))
            .ok()
            .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
            .and_then(|doc| {
                Some(PathBuf::from(
                    doc.get("project")?.get("root")?.as_str()?.to_string(),
                ))
            }),
    );
    let mut checkouts: Vec<PathBuf> = worktrees_in_project_dir(project_dir)
        .iter()
        .map(|name| project_dir.join("worktrees").join(name))
        .collect();
    // A state file this build cannot read says nothing here.
    if let Ok(store) = state::load(&project_dir.join("state.json")) {
        checkouts.extend(
            store
                .worktrees
                .into_iter()
                .filter(|(name, _)| name != crate::paths::CHECK_WORKTREE)
                .map(|(_, record)| record.path),
        );
    }
    out.extend(checkouts.iter().filter_map(|c| checkout_root(c)));
    out
}

/// The repository a checkout belongs to, read from its `.git`: itself for
/// a main checkout, whose `.git` is a directory; the repository a linked
/// worktree's `.git` file names; and for a multi-repo project, the folder
/// a set is of, or the folder itself.
pub fn checkout_root(checkout: &Path) -> Option<PathBuf> {
    let marker = checkout.join(".git");
    if marker.is_dir() {
        return checkout.is_absolute().then(|| checkout.to_path_buf());
    }
    if !marker.exists() {
        if let Some(folder) = project::set_folder(checkout) {
            return Some(folder);
        }
        let folder = !project::repos_in(checkout).is_empty();
        return (folder && checkout.is_absolute()).then(|| checkout.to_path_buf());
    }
    let gitdir = project::linked_gitdir(checkout)?;
    // `<root>/.git/worktrees/<name>` — three components back to the
    // checkout it was linked from. A root whose own `.git` is a file is a
    // linked checkout and not a repository: a relative
    // `.git/worktrees/<name>` climbs back to the checkout itself, which
    // git never writes, and that is no repository rather than one that is
    // still there.
    let root = gitdir.ancestors().nth(3)?;
    (root.is_absolute() && !root.join(".git").is_file()).then(|| root.to_path_buf())
}

/// The worktree directories inside a project directory, by name, sorted.
pub fn worktrees_in_project_dir(project_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(project_dir.join("worktrees")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    out.sort();
    out
}

/// The registry a project directory with none would have: the recorded
/// root that hashes to the directory's own id, else the first that is
/// still there, else the first. `None` when it records no checkout.
pub fn recover_registry(id: &str, project_dir: &Path) -> Option<Registered> {
    let roots = recorded_roots(project_dir);
    let hashes_here = |root: &&PathBuf| {
        crate::project::ProjectRef::from_root(root).is_ok_and(|project| project.id == id)
    };
    let root = roots
        .iter()
        .find(hashes_here)
        .or_else(|| roots.iter().find(|root| root.exists()))
        .or_else(|| roots.first())?
        .clone();
    let name = root.file_name()?.to_string_lossy().into_owned();
    let repos = match root.join(".git").exists() {
        true => Vec::new(),
        false => project::repos_in(&root),
    };
    Some(Registered { root, name, repos })
}
