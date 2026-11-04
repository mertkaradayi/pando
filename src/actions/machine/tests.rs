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

// ---- known and summaries --------------------------------------------------

use crate::state::{Phase, ProcessRecord};
use chrono::Utc;

/// A pid nothing has: a child that has exited and been waited for.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

/// A process record for `pid`, in `phase`, its log under `dir`.
fn process(pid: u32, phase: Phase, dir: &Path) -> ProcessRecord {
    ProcessRecord {
        pid,
        pgid: crate::process::Group::from_raw(pid as i32),
        started_at: Utc::now(),
        log_path: dir.join("dev.log"),
        ready_port: None,
        ready_timeout_s: None,
        observed_ports: Vec::new(),
        swept: false,
        phase,
    }
}

fn running() -> Phase {
    Phase::Running { since: Utc::now() }
}

/// A worktree's record: its name, its processes, its ports by role.
type Recorded<'a> = (&'a str, Vec<(&'a str, ProcessRecord)>, Vec<(&'a str, u16)>);

/// A registered project called `name` under `home`, its repository under
/// `code`, with `records` as its state: `(worktree, processes, ports)`.
fn project_with(home: &Path, code: &Path, name: &str, records: Vec<Recorded<'_>>) -> ProjectRef {
    let repo = code.join(name);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let project = ProjectRef::from_root(&repo).unwrap();
    let dir = unregistered_dir(home, &project);
    project::remember(&dir, &project).unwrap();
    let mut store = state::State::new();
    for (worktree, processes, ports) in records {
        let mut record = WorktreeRecord::new(code.join("trees").join(worktree), true);
        for (process, p) in processes {
            record.processes.insert(process.to_string(), p);
        }
        for (role, port) in ports {
            record.ports.insert(role.to_string(), port);
        }
        store.worktrees.insert(worktree.to_string(), record);
    }
    state::save(&dir.join("state.json"), &store).unwrap();
    project
}

/// Every file under `dir`, with its bytes and its modification time.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in std::fs::read_dir(&at).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let meta = std::fs::metadata(&path).unwrap();
                out.push((
                    path.clone(),
                    std::fs::read(&path).unwrap(),
                    meta.modified().unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn summaries_list_every_project_live_first_and_write_nothing() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    let code = dir.path().join("code");
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let live = crate::testutil::spawn_guarded("sleep 30", dir.path(), &logs.join("live.log"));

    // Idle, used last of all: one worktree recorded, nothing running.
    let idle = project_with(&home, &code, "idle", vec![("feat+old", vec![], vec![])]);
    // Up: one worktree with a live process, one stopped.
    let up = project_with(
        &home,
        &code,
        "shop",
        vec![
            (
                "feat+one",
                vec![(
                    "dev",
                    ProcessRecord {
                        pgid: live.pgid,
                        ..process(live.pid, running(), &logs)
                    },
                )],
                vec![("web", 17_001)],
            ),
            ("feat+two", vec![], vec![]),
        ],
    );
    // Failed: recorded running, its process long gone.
    let crashed = project_with(
        &home,
        &code,
        "crashed",
        vec![(
            "feat+x",
            vec![("dev", process(dead_pid(), running(), &logs))],
            vec![],
        )],
    );
    // Moved: its repository is not where it was.
    let moved = project_with(&home, &code, "gone", vec![]);
    std::fs::remove_dir_all(&moved.root).unwrap();
    // Nothing in it at all: not a project to list.
    std::fs::create_dir_all(home.join("projects").join("empty-00000000")).unwrap();

    let before = snapshot(&home);
    let all = summaries(&home).unwrap();
    assert_eq!(snapshot(&home), before, "a summary wrote into the home");

    let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            up.id.as_str(),
            crashed.id.as_str(),
            idle.id.as_str(),
            moved.id.as_str()
        ]
    );
    let shop = &all[0];
    assert_eq!(
        (shop.up, shop.starting, shop.failed, shop.stopped),
        (1, 0, 0, 1)
    );
    assert_eq!(shop.name, "shop");
    assert_eq!(shop.root, up.root);
    assert!(shop.is_live() && !shop.moved);
    assert!(shop.last_used.is_some());
    assert_eq!(shop.worktrees.len(), 1);
    let one = &shop.worktrees[0];
    assert_eq!(one.name, "feat+one");
    assert_eq!(one.ports["web"], 17_001);
    assert!(one.claimed.contains(&17_001));
    assert_eq!(one.processes["dev"].pid, live.pid);
    assert!(matches!(one.phase, state::Aggregate::Running { .. }));

    let crashed = &all[1];
    assert_eq!((crashed.up, crashed.failed), (0, 1));
    assert!(matches!(
        crashed.worktrees[0].phase,
        state::Aggregate::Failed { .. }
    ));
    let idle = &all[2];
    assert_eq!((idle.up, idle.stopped), (0, 1));
    assert!(idle.worktrees.is_empty());
    assert!(all[3].moved);
    // No lock was made in any project to find out.
    for summary in &all {
        let lock = home.join("projects").join(&summary.id).join("state.lock");
        assert!(!lock.exists(), "{}", lock.display());
    }
}

// While another pando holds a project's lock, a process seen dead keeps
// the phase it was recorded in: that pando may be stopping it.
#[test]
fn a_death_under_a_held_lock_keeps_its_recorded_phase() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    let code = dir.path().join("code");
    let shop = project_with(
        &home,
        &code,
        "shop",
        vec![(
            "feat+one",
            vec![("dev", process(dead_pid(), running(), dir.path()))],
            vec![],
        )],
    );
    let lock_file = home.join("projects").join(&shop.id).join("state.lock");
    let held = state::lock(&lock_file).unwrap();
    let before = snapshot(&home);
    let all = summaries(&home).unwrap();
    assert_eq!(snapshot(&home), before);
    assert_eq!((all[0].up, all[0].failed), (1, 0), "{all:?}");

    drop(held);
    let all = summaries(&home).unwrap();
    assert_eq!((all[0].up, all[0].failed), (0, 1), "{all:?}");
}

#[test]
fn known_recovers_an_unregistered_project_and_registers_it_once() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("acme-shop");
    init_repo(&repo);
    let project = ProjectRef::from_root(&repo).unwrap();
    let home = dir.path().join("home");
    let project_dir = unregistered_dir(&home, &project);
    record(&project_dir, &[("acme-shop", &project.root)]);

    let found = known(&home).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, project.id);
    assert_eq!(found[0].registered, Registered::of(&project));
    assert!(!found[0].moved);
    assert_eq!(
        project::registered(&project_dir),
        Some(Registered::of(&project))
    );
    let file = project_dir.join(project::PROJECT_FILE);
    let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
    assert_eq!(known(&home).unwrap(), found);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn a_port_two_projects_claim_is_a_clash_and_one_project_twice_is_not() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    let code = dir.path().join("code");
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let a = crate::testutil::spawn_guarded("sleep 30", dir.path(), &logs.join("a.log"));
    let b = crate::testutil::spawn_guarded("sleep 30", dir.path(), &logs.join("b.log"));
    let c = crate::testutil::spawn_guarded("sleep 30", dir.path(), &logs.join("c.log"));
    let live = |d: &crate::testutil::Detached| ProcessRecord {
        pgid: d.pgid,
        ..process(d.pid, running(), &logs)
    };
    let one = project_with(
        &home,
        &code,
        "one",
        vec![
            // 17_003 twice, in one project: that project's own business.
            (
                "feat+a",
                vec![("dev", live(&a))],
                vec![("web", 17_001), ("api", 17_003)],
            ),
            (
                "feat+b",
                vec![("dev", live(&b))],
                vec![("web", 17_002), ("api", 17_003)],
            ),
        ],
    );
    let two = project_with(
        &home,
        &code,
        "two",
        vec![("feat+c", vec![("dev", live(&c))], vec![("web", 17_001)])],
    );
    // A stopped worktree's ports are no clash: nothing holds them.
    project_with(
        &home,
        &code,
        "three",
        vec![("feat+d", vec![], vec![("web", 17_002)])],
    );

    let clashes = port_clashes(&summaries(&home).unwrap());
    assert_eq!(clashes.len(), 1, "{clashes:?}");
    let mut by = clashes[&17_001].clone();
    by.sort();
    let mut want = vec![
        (one.id.clone(), "feat+a".to_string()),
        (two.id.clone(), "feat+c".to_string()),
    ];
    want.sort();
    assert_eq!(by, want);
}

// Found on the maintainer's machine: a project whose one recorded
// worktree had been removed vanished from `ls --all` and the grove.
#[test]
fn a_project_nothing_locates_is_listed_moved_and_left_unregistered() {
    let dir = tempdir().unwrap();
    let gone = dir.path().join("worktrees").join("feat+old");
    let project = ProjectRef::from_root(dir.path().join("acme-shop")).unwrap();
    let home = dir.path().join("home");
    let project_dir = unregistered_dir(&home, &project);
    record(&project_dir, &[("feat+old", &gone)]);

    let found = known(&home).unwrap();
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].id, project.id);
    assert_eq!(found[0].registered.name, "acme-shop");
    assert_eq!(found[0].registered.root, PathBuf::new());
    assert!(found[0].moved);
    assert_eq!(project::registered(&project_dir), None, "nothing written");
}

// A repository renamed while its server runs: the processes run on in
// worktrees under the home, and still hold their ports.
#[test]
fn a_moved_project_still_running_ranks_live_and_its_ports_clash() {
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    let code = dir.path().join("code");
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let a = crate::testutil::spawn_guarded("sleep 30", dir.path(), &logs.join("a.log"));
    let b = crate::testutil::spawn_guarded("sleep 30", dir.path(), &logs.join("b.log"));
    let live = |d: &crate::testutil::Detached| ProcessRecord {
        pgid: d.pgid,
        ..process(d.pid, running(), &logs)
    };
    let moved = project_with(
        &home,
        &code,
        "renamed",
        vec![("main", vec![("dev", live(&a))], vec![("web", 17_001)])],
    );
    let other = project_with(
        &home,
        &code,
        "other",
        vec![("main", vec![("dev", live(&b))], vec![("web", 17_001)])],
    );
    // An idle one, used more recently than either.
    project_with(&home, &code, "idle", vec![("main", vec![], vec![])]);
    std::fs::rename(code.join("renamed"), code.join("renamed-old")).unwrap();

    let all = summaries(&home).unwrap();
    let at = |id: &str| all.iter().position(|s| s.id == id).unwrap();
    assert!(all[at(&moved.id)].moved);
    assert_eq!(all[at(&moved.id)].rank(), 0, "{:?}", all[at(&moved.id)]);
    assert!(at(&moved.id) < 2, "with the live ones: {all:?}");
    let clashes = port_clashes(&all);
    let mut by: Vec<String> = clashes[&17_001].iter().map(|(id, _)| id.clone()).collect();
    by.sort();
    let mut want = vec![moved.id.clone(), other.id.clone()];
    want.sort();
    assert_eq!(by, want);
}

// A restart that saved and let go between the summary's read and its
// lock: the copy names the replaced process, and the file the new one.
#[test]
fn a_copy_read_before_a_restart_saved_is_read_again_under_the_free_lock() {
    let dir = tempdir().unwrap();
    let logs = dir.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    let new = crate::testutil::spawn_guarded("sleep 30", dir.path(), &logs.join("new.log"));
    let tree = dir.path().join("trees").join("feat+one");
    let with = |p: ProcessRecord| {
        let mut store = state::State::new();
        let mut record = WorktreeRecord::new(tree.clone(), true);
        record.processes.insert("dev".into(), p);
        store.worktrees.insert("feat+one".into(), record);
        store
    };
    let seen = with(process(dead_pid(), running(), &logs));
    let state_file = dir.path().join("state.json");
    let saved = with(ProcessRecord {
        pgid: new.pgid,
        ..process(new.pid, running(), &logs)
    });
    state::save(&state_file, &saved).unwrap();
    let lock_file = dir.path().join("state.lock");
    std::fs::write(&lock_file, "").unwrap();

    let observed = crate::actions::observe(seen, &state_file, &lock_file, &Default::default());
    let record = &observed.state.worktrees["feat+one"];
    assert_eq!(record.processes["dev"].pid, new.pid);
    assert!(
        !matches!(record.processes["dev"].phase, Phase::Failed { .. }),
        "{:?}",
        record.processes["dev"].phase
    );
}

// A project directory pando may not read (made by a run under sudo):
// listed, with why, rather than dropped with the ports it holds.
#[test]
fn a_project_dir_that_cannot_be_read_is_listed_with_why() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempdir().unwrap();
    let home = dir.path().join("home");
    let code = dir.path().join("code");
    let shop = project_with(&home, &code, "shop", vec![("main", vec![], vec![])]);
    let shop_dir = home.join("projects").join(&shop.id);
    std::fs::set_permissions(&shop_dir, std::fs::Permissions::from_mode(0o000)).unwrap();
    // Root reads it all the same: nothing to prove then.
    let readable = std::fs::read_dir(&shop_dir).is_ok();
    let all = summaries(&home);
    std::fs::set_permissions(&shop_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    if readable {
        return;
    }
    let all = all.unwrap();
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(all[0].id, shop.id);
    assert!(all[0].warning.is_some(), "{all:?}");
    // Never acted on, and said to be unreadable rather than moved: it is
    // where it was.
    assert!(all[0].moved && all[0].unreadable, "{all:?}");
    assert_eq!(all[0].hands_off_word(), "unreadable");
    assert_eq!(all[0].not_located(), "cannot be read");
    assert_eq!(all[0].find_again(), ProjectSummary::READ_AGAIN);
}
