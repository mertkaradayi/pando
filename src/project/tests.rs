//! Discovery, identity and the registry.

use super::discover::{first_entry, path_hash8};
use super::*;
use crate::testutil::{git, init_repo};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

/// A pando home that knows no project.
fn no_home() -> &'static Path {
    Path::new("/pando-test-does-not-exist/home")
}

// A project's id names its directory under the pando home: a hash that
// changed with a refactor would orphan every project's state, config
// and logs. Pinned to the bytes of the path as the OS stores them.
#[test]
fn the_path_hash_of_a_project_never_changes() {
    assert_eq!(path_hash8(Path::new("/home/me/code/app")), "1fb718a9");
    assert_eq!(
        path_hash8(Path::new("/Users/me/Code/ünïcode app")),
        "30d92acf"
    );
}

#[test]
fn discovers_the_root_from_the_repository_root() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);

    let project = discover(&repo, no_home()).unwrap();
    assert_eq!(project.root, std::fs::canonicalize(&repo).unwrap());
    assert_eq!(project.display_name, "acme-shop");
    assert!(project.id.starts_with("acme-shop-"));
    assert_eq!(project.id.len(), "acme-shop-".len() + 8);
}

#[test]
fn subdirectory_and_linked_worktree_resolve_to_the_same_project() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);
    let nested = repo.join("apps").join("web");
    std::fs::create_dir_all(&nested).unwrap();
    let linked = dir.path().join("elsewhere").join("feat+x");
    std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
    git(
        &repo,
        &["worktree", "add", "-b", "feat/x", linked.to_str().unwrap()],
    );

    let from_root = discover(&repo, no_home()).unwrap();
    let from_sub = discover(&nested, no_home()).unwrap();
    let from_linked = discover(&linked, no_home()).unwrap();

    assert_eq!(from_root, from_sub);
    assert_eq!(
        from_root, from_linked,
        "a linked worktree must resolve to the main checkout's project"
    );
}

#[test]
fn the_id_is_stable_and_path_dependent() {
    let dir = tempdir().unwrap();
    let a = dir.path().join("one").join("shop");
    let b = dir.path().join("two").join("shop");
    init_repo(&a);
    init_repo(&b);

    let first = discover(&a, no_home()).unwrap();
    assert_eq!(
        first,
        discover(&a, no_home()).unwrap(),
        "id must be deterministic"
    );
    let second = discover(&b, no_home()).unwrap();
    assert_eq!(first.display_name, second.display_name);
    assert_ne!(
        first.id, second.id,
        "same directory name at different paths must not share a project id"
    );
}

#[test]
fn outside_a_repository_is_an_error() {
    let dir = tempdir().unwrap();
    let err = discover(dir.path(), no_home()).unwrap_err();
    assert!(
        format!("{err:#}").contains("not inside a git repository"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn a_bare_repository_is_refused() {
    let dir = tempdir().unwrap();
    let bare = dir.path().join("bare.git");
    git(
        dir.path(),
        &[
            "init",
            "--bare",
            "--quiet",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );

    let err = discover(&bare, no_home()).unwrap_err();
    assert!(
        format!("{err:#}").contains("bare repositories are not supported"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn first_entry_reads_the_main_checkout_only() {
    let text = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                worktree /elsewhere/feat+x\nHEAD def\nbranch refs/heads/feat/x\n\n";
    assert_eq!(
        first_entry(text),
        Some((PathBuf::from("/repo"), false)),
        "only the first entry describes the main checkout"
    );
}

#[test]
fn first_entry_reports_a_bare_main_checkout() {
    assert_eq!(
        first_entry("worktree /repo.git\nbare\n\n"),
        Some((PathBuf::from("/repo.git"), true))
    );
}

#[test]
fn first_entry_ignores_unknown_lines_and_empty_output() {
    assert_eq!(
        first_entry("worktree /repo\nsomething-new-in-git 1\nHEAD abc\n\n"),
        Some((PathBuf::from("/repo"), false))
    );
    assert_eq!(first_entry(""), None);
}

/// `product/` holding the repositories `backend` and `frontend`.
fn folder_of_repos(dir: &Path) -> PathBuf {
    let folder = dir.join("product");
    init_repo(&folder.join("backend"));
    init_repo(&folder.join("frontend"));
    std::fs::create_dir_all(folder.join("docs")).unwrap();
    std::fs::canonicalize(folder).unwrap()
}

/// A pando home that knows `project`, as one `new` has made.
fn home_knowing(dir: &Path, project: &ProjectRef) -> PathBuf {
    let home = dir.join("pando-home");
    std::fs::create_dir_all(home.join("projects").join(&project.id)).unwrap();
    home
}

#[test]
fn a_folder_of_repositories_is_one_project_of_them() {
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());

    let project = discover(&folder, no_home()).unwrap();
    assert_eq!(project.root, folder);
    assert_eq!(project.display_name, "product");
    assert_eq!(project.repos, ["backend", "frontend"]);
    assert!(project.id.starts_with("product-"));
    assert_eq!(
        project.checkouts_in(Path::new("/set")),
        [
            PathBuf::from("/set/backend"),
            PathBuf::from("/set/frontend")
        ]
    );
}

#[test]
fn a_folder_of_one_repository_is_no_project_until_pando_knows_it() {
    let dir = tempdir().unwrap();
    let folder = dir.path().join("product");
    init_repo(&folder.join("backend"));

    let err = discover(&folder, no_home()).unwrap_err();
    assert!(
        format!("{err:#}").contains("not inside a git repository, nor a folder of them"),
        "{err:#}"
    );
    let project = ProjectRef::multi_repo(&folder, vec!["backend".into()]).unwrap();
    let home = home_knowing(dir.path(), &project);
    assert_eq!(discover(&folder, &home).unwrap(), project);
}

#[test]
fn a_plain_folder_below_a_known_one_finds_it_and_an_unknown_one_does_not() {
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let docs = folder.join("docs");
    assert!(discover(&docs, no_home()).is_err());

    let project = discover(&folder, no_home()).unwrap();
    let home = home_knowing(dir.path(), &project);
    assert_eq!(discover(&docs, &home).unwrap(), project);
}

#[test]
fn a_folder_holding_the_pando_home_is_never_a_project_of_its_repositories() {
    // The maintainer's home directory: tool clones beside `.pando`.
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let home = folder.join(".pando");
    let err = discover(&folder, &home).unwrap_err();
    // Said as why, never as "nor a folder of them" over the repositories
    // the next words count.
    let never = err.downcast_ref::<NeverAFolder>().expect("why it is none");
    assert_eq!(never.why, NeverWhy::HoldsHome(home.clone()));
    assert_eq!(never.repos, ["backend", "frontend"]);
    assert_eq!(
        format!("{err:#}"),
        format!(
            "not inside a git repository: {} holds pando's home ({}), never a project of the \
             repositories in it",
            folder.display(),
            home.display()
        )
    );
    // Not when the home does not exist yet, nor when it already has a
    // directory for the folder, nor from a plain folder below it.
    let project =
        ProjectRef::multi_repo(&folder, vec!["backend".into(), "frontend".into()]).unwrap();
    std::fs::create_dir_all(home.join(PROJECTS_DIR).join(&project.id)).unwrap();
    assert!(discover(&folder, &home).is_err());
    assert!(discover(&folder.join("docs"), &home).is_err());
    assert_eq!(
        folder_of(&discover(&folder.join("backend"), &home).unwrap(), &home),
        None
    );
    // Each repository in it is still its own project.
    assert_eq!(
        discover(&folder.join("backend"), &home).unwrap().root,
        folder.join("backend")
    );
}

#[test]
fn the_users_home_directory_is_never_a_project_of_its_repositories() {
    use super::discover::{Places, discover_in};
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let elsewhere = dir.path().join("pando-home");
    let places = Places::with(&elsewhere, Some(folder.clone()));
    for sight in [FirstSight::Allow, FirstSight::Deny] {
        let err = discover_in(&folder, &places, sight).unwrap_err();
        assert!(
            format!("{err:#}").starts_with("not inside a git repository: "),
            "{err:#}"
        );
        assert!(
            format!("{err:#}")
                .ends_with("is your home directory, never a project of the repositories in it"),
            "{err:#}"
        );
        let never = err.downcast_ref::<NeverAFolder>().expect("why it is none");
        assert_eq!(never.why, NeverWhy::UserHome);
        // The repositories it counts are the ones a `cd` is offered into.
        assert_eq!(never.repos, repos_in(&folder));
    }
    // With fewer than two repositories it is none for the plain reason.
    let lone = dir.path().join("lone");
    init_repo(&lone.join("only"));
    let lone_home = Places::with(&elsewhere, Some(lone.clone()));
    let err = discover_in(&lone, &lone_home, FirstSight::Allow).unwrap_err();
    assert!(err.downcast_ref::<NeverAFolder>().is_none(), "{err:#}");
    // Even once a directory for it is there.
    let project =
        ProjectRef::multi_repo(&folder, vec!["backend".into(), "frontend".into()]).unwrap();
    std::fs::create_dir_all(elsewhere.join(PROJECTS_DIR).join(&project.id)).unwrap();
    assert!(discover_in(&folder.join("docs"), &places, FirstSight::Allow).is_err());
    // Another user home leaves it a folder of repositories.
    let other = Places::with(&elsewhere, Some(dir.path().join("someone")));
    assert_eq!(
        discover_in(&folder, &other, FirstSight::Deny).unwrap(),
        project
    );
}

#[test]
fn a_look_never_takes_an_unknown_folder_and_a_command_does() {
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let err = discover_as(&folder, no_home(), FirstSight::Deny).unwrap_err();
    let unknown = err
        .downcast_ref::<UnknownFolder>()
        .expect("an unknown folder");
    assert_eq!(unknown.dir, folder);
    assert_eq!(unknown.repos, ["backend", "frontend"]);
    assert!(
        err.to_string()
            .contains("`pando init` or `pando new <branch>`"),
        "{err}"
    );

    let project = discover_as(&folder, no_home(), FirstSight::Allow).unwrap();
    let home = home_knowing(dir.path(), &project);
    // Known, a look finds it like a command.
    assert_eq!(
        discover_as(&folder, &home, FirstSight::Deny).unwrap(),
        project
    );
    assert_eq!(
        discover_as(&folder.join("docs"), &home, FirstSight::Deny).unwrap(),
        project
    );
}

#[test]
fn an_unknown_folder_names_three_repositories_and_counts_the_rest() {
    let folder = UnknownFolder {
        dir: PathBuf::from("/code"),
        repos: ["a", "b", "c", "d", "e"].map(String::from).to_vec(),
    };
    assert_eq!(folder.names(), "a, b, c, … 2 more");
}

#[test]
fn a_main_checkout_in_the_folder_is_still_its_own_repository() {
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let project = discover(&folder, no_home()).unwrap();
    let home = home_knowing(dir.path(), &project);

    let backend = discover(&folder.join("backend"), &home).unwrap();
    assert_eq!(backend.root, folder.join("backend"));
    assert!(!backend.is_multi_repo());
}

#[test]
fn a_set_and_its_checkouts_are_the_folders_project() {
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let project = discover(&folder, no_home()).unwrap();
    let home = home_knowing(dir.path(), &project);
    let set = dir.path().join("trees").join("feat+x");
    std::fs::create_dir_all(&set).unwrap();
    for repo in ["backend", "frontend"] {
        let at = set.join(repo);
        git(
            &folder.join(repo),
            &["worktree", "add", "-b", "feat/x", at.to_str().unwrap()],
        );
    }
    std::fs::create_dir_all(set.join("backend").join("src")).unwrap();

    assert_eq!(discover(&set, &home).unwrap(), project);
    assert_eq!(discover(&set.join("frontend"), &home).unwrap(), project);
    assert_eq!(
        discover(&set.join("backend").join("src"), &home).unwrap(),
        project
    );
    // Unknown to this home, the set is nobody's, and each checkout is
    // its repository's.
    assert!(discover(&set, no_home()).is_err());
    assert_eq!(
        discover(&set.join("frontend"), no_home()).unwrap().root,
        folder.join("frontend")
    );
}

#[test]
fn a_worktree_named_for_its_repository_is_not_taken_for_a_set() {
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let project = discover(&folder, no_home()).unwrap();
    let home = home_knowing(dir.path(), &project);
    // One repository's worktrees, one of them called `backend`.
    let trees = dir.path().join("trees");
    std::fs::create_dir_all(&trees).unwrap();
    let backend = folder.join("backend");
    for name in ["backend", "feat+y"] {
        let at = trees.join(name);
        git(
            &backend,
            &["worktree", "add", "-b", name, at.to_str().unwrap()],
        );
    }

    let found = discover(&trees.join("backend"), &home).unwrap();
    assert_eq!(found.root, backend);
    assert!(!found.is_multi_repo());
}

#[test]
fn a_path_is_asked_about_in_the_repository_it_starts_with() {
    let project = ProjectRef {
        id: "product-00000000".into(),
        root: PathBuf::from("/product"),
        display_name: "product".into(),
        repos: vec!["backend".into(), "frontend".into()],
        ..Default::default()
    };
    let set = Path::new("/set");
    assert_eq!(
        project.repo_path(set, "backend/.env"),
        Some((PathBuf::from("/set/backend"), ".env".into()))
    );
    assert_eq!(
        project.repo_path(set, "frontend/node_modules/"),
        Some((PathBuf::from("/set/frontend"), "node_modules/".into()))
    );
    assert_eq!(project.repo_path(set, ".env"), None);
    assert_eq!(project.repo_path(set, "backend"), None);
    assert_eq!(project.repo_path(set, "docs/notes.md"), None);
    let one = ProjectRef::from_root("/repo").unwrap();
    assert_eq!(
        one.repo_path(Path::new("/wt"), ".env"),
        Some((PathBuf::from("/wt"), ".env".into()))
    );
}

// ---- the registry ---------------------------------------------------------

/// A project directory for `project`, as `ensure_home` makes one.
fn project_dir_for(home: &Path, project: &ProjectRef) -> PathBuf {
    let dir = home.join(PROJECTS_DIR).join(&project.id);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn remember_writes_the_registry_once_and_again_only_when_it_changes() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);
    let project = discover(&repo, no_home()).unwrap();
    let project_dir = project_dir_for(&dir.path().join("home"), &project);

    assert!(
        remember(&project_dir, &project).unwrap(),
        "missing: written"
    );
    let file = project_dir.join(PROJECT_FILE);
    let written = std::fs::read(&file).unwrap();
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    assert!(!remember(&project_dir, &project).unwrap(), "the same: kept");
    assert_eq!(std::fs::read(&file).unwrap(), written);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
    let read = registered(&project_dir).unwrap();
    assert_eq!(read, Registered::of(&project));
    assert_eq!(read.root, project.root);
    assert_eq!(read.name, "acme-shop");
    assert!(read.repos.is_empty());
    let v: serde_json::Value = serde_json::from_slice(&written).unwrap();
    assert_eq!(v["root"], project.root.display().to_string().as_str());
    assert_eq!(v["name"], "acme-shop");
    assert!(v.get("repos").is_none(), "{v}");

    // Something else in it: written over.
    std::fs::write(&file, "{\"root\": \"/elsewhere\", \"name\": \"x\"}").unwrap();
    assert!(remember(&project_dir, &project).unwrap());
    assert_eq!(registered(&project_dir).unwrap(), Registered::of(&project));
    // Something unreadable: written over too.
    std::fs::write(&file, "not json").unwrap();
    assert!(remember(&project_dir, &project).unwrap());
    assert_eq!(std::fs::read(&file).unwrap(), written);
    // Nothing but the file itself is left beside it.
    let names: Vec<String> = std::fs::read_dir(&project_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, [PROJECT_FILE]);
}

#[test]
fn remember_never_makes_a_project_directory() {
    let dir = tempdir().unwrap();
    let project = ProjectRef::from_root(dir.path()).unwrap();
    let project_dir = dir.path().join("home").join(PROJECTS_DIR).join(&project.id);
    assert!(!remember(&project_dir, &project).unwrap());
    assert!(!project_dir.exists());
}

#[test]
fn a_multi_repo_project_registers_its_repositories() {
    let dir = tempdir().unwrap();
    let folder = folder_of_repos(dir.path());
    let project = discover(&folder, no_home()).unwrap();
    let project_dir = project_dir_for(&dir.path().join("home"), &project);
    remember(&project_dir, &project).unwrap();

    let read = registered(&project_dir).unwrap();
    assert_eq!(read.repos, ["backend", "frontend"]);
    assert_eq!(read.project(), project);
    assert!(!read.moved_from(&project.id));
}

#[test]
fn a_registry_whose_root_is_gone_or_hashes_elsewhere_has_moved() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);
    let project = discover(&repo, no_home()).unwrap();
    let here = Registered::of(&project);
    assert!(!here.moved_from(&project.id));
    // Another id for the same root: the directory is some other path's.
    assert!(here.moved_from("acme-shop-00000000"));

    let gone = Registered {
        root: dir.path().join("gone"),
        name: "gone".into(),
        repos: Vec::new(),
    };
    assert!(gone.moved_from(&gone.project().id));
}

#[test]
fn project_dirs_lists_directories_by_id() {
    let dir = tempdir().unwrap();
    let projects = dir.path().join(PROJECTS_DIR);
    for id in ["b-00000002", "a-00000001"] {
        std::fs::create_dir_all(projects.join(id)).unwrap();
    }
    std::fs::write(projects.join("stray.txt"), "").unwrap();
    let ids: Vec<String> = project_dirs(dir.path())
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ids, ["a-00000001", "b-00000002"]);
    assert!(project_dirs(&dir.path().join("none")).unwrap().is_empty());
}

// A home whose `projects` cannot be read is not a home with none: a
// script told "no projects" would start more on ports that are taken.
#[test]
fn a_projects_dir_that_cannot_be_read_is_an_error_not_none() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(PROJECTS_DIR), "").unwrap();
    let err = project_dirs(dir.path()).unwrap_err();
    assert!(format!("{err:#}").contains("cannot read"), "{err:#}");
    // A `PANDO_HOME` that names a file.
    let file = dir.path().join("home-file");
    std::fs::write(&file, "").unwrap();
    assert!(project_dirs(&file).is_err());
}
