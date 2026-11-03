//! Every project on a machine, over homes the tests build.

use std::path::{Path, PathBuf};

use super::*;
use crate::project::{self, ProjectRef, Registered};
use crate::state::{self, WorktreeRecord};
use crate::testutil::{git, init_repo};
use tempfile::tempdir;

/// `project`'s directory under `home`, as pando's first write makes it,
/// with no registry in it: a project registered before there was one.
fn unregistered_dir(home: &Path, project: &ProjectRef) -> PathBuf {
    let dir = home.join(project::PROJECTS_DIR).join(&project.id);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Records `records` in the state file of `project_dir`.
fn record(project_dir: &Path, records: &[(&str, &Path)]) {
    let mut store = state::State::new();
    for (name, path) in records {
        store
            .worktrees
            .insert(name.to_string(), WorktreeRecord::new(*path, true));
    }
    state::save(&project_dir.join("state.json"), &store).unwrap();
}

#[test]
fn a_linked_worktrees_record_recovers_its_repository() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);
    let project = ProjectRef::from_root(&repo).unwrap();
    let project_dir = unregistered_dir(&dir.path().join("home"), &project);
    let linked = dir.path().join("trees").join("feat+x");
    git(
        &repo,
        &["worktree", "add", "-b", "feat/x", linked.to_str().unwrap()],
    );
    record(&project_dir, &[("feat+x", &linked)]);

    let recovered = recover_registry(&project.id, &project_dir).unwrap();
    assert_eq!(recovered, Registered::of(&project));
    assert!(!recovered.moved_from(&project.id));
}

#[test]
fn a_main_checkouts_record_and_a_worktree_inside_recover_it_too() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);
    let project = ProjectRef::from_root(&repo).unwrap();
    let home = dir.path().join("home");
    let project_dir = unregistered_dir(&home, &project);
    record(&project_dir, &[("acme-shop", &project.root)]);
    assert_eq!(
        recover_registry(&project.id, &project_dir),
        Some(Registered::of(&project))
    );

    std::fs::remove_file(project_dir.join("state.json")).unwrap();
    let inside = project_dir.join("worktrees").join("feat+y");
    git(
        &repo,
        &["worktree", "add", "-b", "feat/y", inside.to_str().unwrap()],
    );
    assert_eq!(
        recover_registry(&project.id, &project_dir),
        Some(Registered::of(&project))
    );
}

#[test]
fn a_multi_repo_sets_record_recovers_the_folder_and_its_repositories() {
    let dir = tempdir().unwrap();
    let folder = dir.path().join("product");
    init_repo(&folder.join("backend"));
    init_repo(&folder.join("frontend"));
    let folder = std::fs::canonicalize(folder).unwrap();
    let project =
        ProjectRef::multi_repo(&folder, vec!["backend".into(), "frontend".into()]).unwrap();
    let project_dir = unregistered_dir(&dir.path().join("home"), &project);
    let set = dir.path().join("sets").join("feat+x");
    std::fs::create_dir_all(&set).unwrap();
    for repo in ["backend", "frontend"] {
        let at = set.join(repo);
        git(
            &folder.join(repo),
            &["worktree", "add", "-b", "feat/x", at.to_str().unwrap()],
        );
    }
    record(&project_dir, &[("feat+x", &set)]);
    let recovered = recover_registry(&project.id, &project_dir).unwrap();
    assert_eq!(recovered, Registered::of(&project));

    // The folder's own record says the same.
    record(&project_dir, &[("product", &folder)]);
    assert_eq!(
        recover_registry(&project.id, &project_dir),
        Some(Registered::of(&project))
    );
}

#[test]
fn a_directory_that_records_no_checkout_recovers_nothing() {
    let dir = tempdir().unwrap();
    let project = ProjectRef::from_root(dir.path().join("acme-shop")).unwrap();
    let project_dir = unregistered_dir(&dir.path().join("home"), &project);
    assert_eq!(recover_registry(&project.id, &project_dir), None);
    record(&project_dir, &[]);
    assert_eq!(recover_registry(&project.id, &project_dir), None);
}

#[test]
fn the_root_that_hashes_to_the_directory_wins_over_one_listed_first() {
    let dir = tempdir().unwrap();
    let ours = dir.path().join("one").join("shop");
    let other = dir.path().join("two").join("shop");
    init_repo(&ours);
    init_repo(&other);
    let project = ProjectRef::from_root(&ours).unwrap();
    let project_dir = unregistered_dir(&dir.path().join("home"), &project);
    // A `[project] root` naming the other one comes first in trust order.
    std::fs::write(
        project_dir.join("pando.toml"),
        format!("[project]\nroot = {:?}\n", other.display().to_string()),
    )
    .unwrap();
    record(&project_dir, &[("shop", &project.root)]);
    assert_eq!(
        recover_registry(&project.id, &project_dir).map(|r| r.root),
        Some(project.root)
    );
}

#[test]
fn a_repository_moved_away_is_recovered_where_it_was() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);
    let project = ProjectRef::from_root(&repo).unwrap();
    let project_dir = unregistered_dir(&dir.path().join("home"), &project);
    let linked = project_dir.join("worktrees").join("feat+x");
    git(
        &repo,
        &["worktree", "add", "-b", "feat/x", linked.to_str().unwrap()],
    );
    std::fs::rename(&repo, dir.path().join("moved")).unwrap();

    let recovered = recover_registry(&project.id, &project_dir).unwrap();
    assert_eq!(recovered.root, project.root);
    assert!(recovered.moved_from(&project.id));
}
