use super::*;
use crate::config::Config;
use crate::testutil::git;
use crate::worktree::InProgress;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

/// A bare origin, the main checkout cloned from it, and a second clone
/// that stands for a teammate pushing to origin.
struct Repo {
    dir: TempDir,
    main: PathBuf,
    other: PathBuf,
}

fn repo() -> Repo {
    let dir = TempDir::new().unwrap();
    let origin = dir.path().join("origin.git");
    git(
        dir.path(),
        &[
            "init",
            "--bare",
            "--quiet",
            "--initial-branch=main",
            "origin.git",
        ],
    );
    git(&origin, &["config", "maintenance.auto", "false"]);
    let main = dir.path().join("main");
    crate::testutil::init_repo(&main);
    write(&main, "a.txt", "one\ntwo\nthree\n");
    git(&main, &["add", "."]);
    git(&main, &["commit", "--quiet", "-m", "a"]);
    git(
        &main,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git(&main, &["push", "--quiet", "-u", "origin", "main"]);
    // The moves under test commit with whatever identity git finds, and
    // a test machine may have none: the fixture's own config gives one.
    for (key, value) in [
        ("user.name", "t"),
        ("user.email", "t@t"),
        ("commit.gpgsign", "false"),
    ] {
        git(&main, &["config", key, value]);
    }
    git(&main, &["remote", "set-head", "origin", "main"]);
    git(
        dir.path(),
        &["clone", "--quiet", origin.to_str().unwrap(), "other"],
    );
    let other = dir.path().join("other");
    Repo { dir, main, other }
}

fn write(dir: &Path, file: &str, contents: &str) {
    std::fs::write(dir.join(file), contents).unwrap();
}

fn commit(dir: &Path, file: &str, contents: &str, message: &str) {
    write(dir, file, contents);
    git(dir, &["add", file]);
    git(dir, &["commit", "--quiet", "-m", message]);
}

impl Repo {
    /// A teammate's commit, pushed to origin's main.
    fn advance(&self, file: &str, contents: &str) {
        git(&self.other, &["pull", "--quiet", "--ff-only"]);
        commit(&self.other, file, contents, &format!("teammate: {file}"));
        git(&self.other, &["push", "--quiet", "origin", "main"]);
    }

    /// A worktree on a new branch from origin's main, as `new` makes one.
    fn worktree(&self, branch: &str) -> PathBuf {
        let path = self.dir.path().join(branch.replace('/', "+"));
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "--quiet",
                "--no-track",
                "-b",
                branch,
                path.to_str().unwrap(),
                "origin/main",
            ],
        );
        path
    }
}

fn head(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["-C", dir.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn quiet(_: &str) {}

fn run_on(dir: &Path, main: bool, action: GitAction) -> anyhow::Result<Ran> {
    run(dir, main, Some("origin/main"), action, &quiet)
}

#[test]
fn the_menu_keys_are_one_letter_each_and_every_action_has_a_row() {
    let keys: std::collections::HashSet<char> = ACTIONS.iter().map(|row| row.key).collect();
    assert_eq!(keys.len(), ACTIONS.len(), "two actions share a key");
    // `!` is the menu's way out to a shell, never an action's.
    assert!(!keys.contains(&'!'));
    for action in [
        GitAction::Fetch,
        GitAction::Pull,
        GitAction::Rebase,
        GitAction::Merge,
        GitAction::Abort,
    ] {
        assert_eq!(action.row().action, action);
    }
}

#[test]
fn a_repository_with_no_origin_is_not_fetched() {
    let dir = TempDir::new().unwrap();
    crate::testutil::init_repo(dir.path());
    let read = read(dir.path(), true, None);
    let fetch = &offers(&read)[0];
    assert_eq!(fetch.action, GitAction::Fetch);
    assert_eq!(
        fetch.refused.as_deref(),
        Some("there is no remote called origin")
    );
    let err = run(dir.path(), true, None, GitAction::Fetch, &quiet).unwrap_err();
    assert!(
        format!("{err}").contains("no remote called origin"),
        "{err}"
    );
}

#[test]
fn a_fetch_says_how_many_of_origins_branches_moved() {
    let repo = repo();
    repo.advance("b.txt", "b\n");
    let before = head(&repo.main);
    let ran = run_on(&repo.main, true, GitAction::Fetch).unwrap();
    assert_eq!(
        ran,
        Ran::Unchanged("fetched origin · 1 branch moved".into())
    );
    assert_eq!(head(&repo.main), before, "a fetch moves no branch");
    let read = read(&repo.main, true, Some("origin/main"));
    assert_eq!(read.upstream_drift, Some((0, 1)));
    assert!(read.fetched.is_some());
}

#[test]
fn the_main_checkout_is_fast_forwarded_to_its_upstream() {
    let repo = repo();
    repo.advance("b.txt", "b\n");
    repo.advance("c.txt", "c\n");
    let ran = run_on(&repo.main, true, GitAction::Pull).unwrap();
    assert_eq!(
        ran,
        Ran::Moved("main fast-forwarded 2 commits to origin/main".into())
    );
    assert!(ran.moved());
    assert_eq!(head(&repo.main), head(&repo.other));
    assert!(repo.main.join("c.txt").exists());
}

#[test]
fn a_pull_that_is_not_a_fast_forward_changes_nothing() {
    let repo = repo();
    repo.advance("b.txt", "b\n");
    commit(&repo.main, "mine.txt", "mine\n", "mine");
    let before = head(&repo.main);
    let ran = run_on(&repo.main, true, GitAction::Pull).unwrap();
    assert_eq!(
        ran,
        Ran::Diverged {
            upstream: "origin/main".into(),
            ahead: 1,
            behind: 1
        }
    );
    assert!(!ran.moved());
    assert_eq!(head(&repo.main), before);
}

#[test]
fn a_worktree_rebases_cleanly_onto_its_fetched_base() {
    let repo = repo();
    let wt = repo.worktree("feat/clean");
    commit(&wt, "feature.txt", "f\n", "feature");
    repo.advance("b.txt", "b\n");
    let ran = run_on(&wt, false, GitAction::Rebase).unwrap();
    assert!(ran.moved(), "{ran:?}");
    assert_eq!(
        ran.summary(),
        "rebased feat/clean onto origin/main · 1 commit on top of 1 new"
    );
    // On top of origin's new commit, with its own commit kept.
    let status = Command::new("git")
        .args(["-C", wt.to_str().unwrap()])
        .args(["merge-base", "--is-ancestor", "origin/main", "HEAD"])
        .status()
        .unwrap();
    assert!(status.success());
    assert!(wt.join("b.txt").exists() && wt.join("feature.txt").exists());
}

#[test]
fn a_rebase_that_conflicts_is_aborted_and_leaves_everything_as_it_was() {
    let repo = repo();
    let wt = repo.worktree("feat/conflict");
    commit(&wt, "a.txt", "one\nmine\nthree\n", "mine: a");
    repo.advance("a.txt", "one\ntheirs\nthree\n");
    let before = head(&wt);
    let ran = run_on(&wt, false, GitAction::Rebase).unwrap();
    match &ran {
        Ran::Conflict { op, files, at } => {
            assert_eq!(*op, InProgress::Rebase);
            assert_eq!(files, &vec!["a.txt".to_string()]);
            assert!(
                at.as_deref().is_some_and(|at| at.ends_with("mine: a")),
                "{at:?}"
            );
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
    assert_eq!(
        ran.summary(),
        "conflict in a.txt — rebase aborted, nothing changed"
    );
    assert_eq!(head(&wt), before);
    assert_eq!(crate::worktree::in_progress(&wt), None);
    assert_eq!(
        std::fs::read_to_string(wt.join("a.txt")).unwrap(),
        "one\nmine\nthree\n"
    );
    assert_eq!(read::dirty(&wt), 0);
}

#[test]
fn a_merge_that_conflicts_is_aborted_too() {
    let repo = repo();
    let wt = repo.worktree("feat/merge");
    commit(&wt, "a.txt", "one\nmine\nthree\n", "mine: a");
    repo.advance("a.txt", "one\ntheirs\nthree\n");
    let before = head(&wt);
    let ran = run_on(&wt, false, GitAction::Merge).unwrap();
    assert!(
        matches!(&ran, Ran::Conflict { op: InProgress::Merge, files, at: None } if files == &["a.txt"]),
        "{ran:?}"
    );
    assert_eq!(head(&wt), before);
    assert_eq!(crate::worktree::in_progress(&wt), None);
}

#[test]
fn a_clean_merge_brings_the_base_in_with_one_merge_commit() {
    let repo = repo();
    let wt = repo.worktree("feat/merge");
    commit(&wt, "feature.txt", "f\n", "feature");
    repo.advance("b.txt", "b\n");
    let ran = run_on(&wt, false, GitAction::Merge).unwrap();
    assert_eq!(
        ran,
        Ran::Moved("merged origin/main into feat/merge · brought in 1 commit".into())
    );
    let parents = Command::new("git")
        .args([
            "-C",
            wt.to_str().unwrap(),
            "rev-list",
            "--parents",
            "-1",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&parents.stdout)
            .split_whitespace()
            .count(),
        3,
        "a merge commit has two parents"
    );
}

#[test]
fn uncommitted_changes_refuse_every_move_but_a_fetch() {
    let repo = repo();
    let wt = repo.worktree("fix/dirty");
    repo.advance("b.txt", "b\n");
    write(&wt, "a.txt", "edited\n");
    let read = read(&wt, false, Some("origin/main"));
    assert_eq!(read.dirty, 1);
    for offer in offers(&read) {
        match offer.action {
            GitAction::Fetch => assert_eq!(offer.refused, None),
            GitAction::Rebase | GitAction::Merge => assert_eq!(
                offer.refused.as_deref(),
                Some("✎ 1 uncommitted file — commit or stash first")
            ),
            // No upstream: that is said before the changes are.
            GitAction::Pull => assert_eq!(
                offer.refused.as_deref(),
                Some("fix/dirty tracks no remote branch")
            ),
            GitAction::Abort => panic!("nothing is in progress"),
        }
    }
    let before = head(&wt);
    let err = run_on(&wt, false, GitAction::Rebase).unwrap_err();
    assert!(format!("{err}").contains("uncommitted"), "{err}");
    assert_eq!(head(&wt), before);
    assert_eq!(
        std::fs::read_to_string(wt.join("a.txt")).unwrap(),
        "edited\n"
    );
}

#[test]
fn the_main_checkout_is_only_ever_fast_forwarded() {
    let repo = repo();
    repo.advance("b.txt", "b\n");
    let read = read(&repo.main, true, Some("origin/main"));
    for offer in offers(&read) {
        if matches!(offer.action, GitAction::Rebase | GitAction::Merge) {
            assert_eq!(
                offer.refused.as_deref(),
                Some("not on the main checkout — pando only fast-forwards it")
            );
        }
    }
    assert!(run_on(&repo.main, true, GitAction::Rebase).is_err());
}

#[test]
fn a_rebase_left_half_done_in_a_shell_is_offered_only_its_abort() {
    let repo = repo();
    let wt = repo.worktree("feat/old");
    commit(&wt, "a.txt", "one\nmine\nthree\n", "mine: a");
    repo.advance("a.txt", "one\ntheirs\nthree\n");
    git(&wt, &["fetch", "--quiet", "origin"]);
    let before = head(&wt);
    // As a developer's own shell would leave it: stopped on the conflict.
    let stopped = Command::new("git")
        .args(["-C", wt.to_str().unwrap(), "rebase", "origin/main"])
        .env("GIT_EDITOR", "true")
        .output()
        .unwrap();
    assert!(!stopped.status.success());
    assert_eq!(crate::worktree::in_progress(&wt), Some(InProgress::Rebase));

    let read = read(&wt, false, Some("origin/main"));
    let actions: Vec<GitAction> = offers(&read).iter().map(|o| o.action).collect();
    assert_eq!(actions, vec![GitAction::Fetch, GitAction::Abort]);
    let plan = plan(&read, GitAction::Abort);
    assert_eq!(plan.commands, vec!["git rebase --abort".to_string()]);

    let ran = run_on(&wt, false, GitAction::Abort).unwrap();
    assert_eq!(
        ran,
        Ran::Moved("aborted the rebase · back to where it was".into())
    );
    assert_eq!(crate::worktree::in_progress(&wt), None);
    assert_eq!(head(&wt), before);
}

#[test]
fn the_preview_names_the_commands_and_warns_before_rewriting_pushed_commits() {
    let repo = repo();
    let wt = repo.worktree("feat/pushed");
    commit(&wt, "feature.txt", "f\n", "feature");
    git(&wt, &["push", "--quiet", "-u", "origin", "feat/pushed"]);
    repo.advance("b.txt", "b\n");
    git(&wt, &["fetch", "--quiet", "origin"]);
    let read = read(&wt, false, Some("origin/main"));
    assert_eq!(read.upstream.as_deref(), Some("origin/feat/pushed"));
    assert_eq!(read.pushed, 1);
    let plan = plan(&read, GitAction::Rebase);
    assert_eq!(plan.title, "rebase feat/pushed onto origin/main");
    assert_eq!(
        plan.commands,
        vec![
            "git fetch origin main".to_string(),
            "git rebase origin/main".to_string()
        ]
    );
    assert_eq!(plan.moves.as_deref(), Some("replays 1 commit onto 1 new"));
    assert_eq!(
        plan.warnings,
        vec![
            "origin/feat/pushed has 1 of these commits: the next push needs --force-with-lease"
                .to_string()
        ]
    );
}

#[test]
fn the_base_is_the_configs_first_then_the_repositorys_own() {
    let repo = repo();
    git(&repo.main, &["branch", "develop"]);
    let mut config = Config::default();
    assert_eq!(
        base_for(&repo.main, &config, Some("feat/x")).as_deref(),
        Some("origin/main")
    );
    config.project.base = Some("develop".into());
    // A base origin does not have is taken as the local branch.
    assert_eq!(
        base_for(&repo.main, &config, Some("feat/x")).as_deref(),
        Some("develop")
    );
}

#[test]
fn a_file_list_says_the_first_two_and_how_many_more() {
    let files = |n: usize| (0..n).map(|i| format!("f{i}")).collect::<Vec<_>>();
    assert_eq!(run::file_list(&files(1)), "f0");
    assert_eq!(run::file_list(&files(2)), "f0 and f1");
    assert_eq!(run::file_list(&files(5)), "f0, f1 and 3 more");
}

/// A worktree with a provisioned, ignored `.env`, and a base that has
/// started tracking one: every move onto the base would replace it, and
/// the abort after a conflict would delete it.
fn ignored_env_and_a_base_that_tracks_one(r: &Repo) -> PathBuf {
    let wt = r.worktree("feat/env");
    commit(&wt, ".gitignore", ".env\n", "ignore .env");
    write(&wt, ".env", "SECRET=mine\n");
    git(&r.other, &["pull", "--quiet", "--ff-only"]);
    write(&r.other, ".env", "SECRET=theirs\n");
    git(&r.other, &["add", "-f", ".env"]);
    git(&r.other, &["commit", "--quiet", "-m", "track .env"]);
    git(&r.other, &["push", "--quiet", "origin", "main"]);
    wt
}

#[test]
fn a_move_that_would_overwrite_an_ignored_file_is_refused_before_it_runs() {
    let r = repo();
    let wt = ignored_env_and_a_base_that_tracks_one(&r);
    for action in [GitAction::Rebase, GitAction::Merge] {
        let before = head(&wt);
        let err = run_on(&wt, false, action).unwrap_err().to_string();
        assert!(err.contains(".env"), "{err}");
        assert!(err.contains("nothing changed"), "{err}");
        assert_eq!(head(&wt), before, "{action:?} moved HEAD");
        assert_eq!(
            std::fs::read_to_string(wt.join(".env")).unwrap(),
            "SECRET=mine\n",
            "{action:?} touched the ignored file"
        );
    }
}

#[test]
fn a_fast_forward_that_would_overwrite_an_ignored_file_is_refused() {
    let r = repo();
    write(&r.main, ".env", "SECRET=mine\n");
    git(&r.main, &["config", "core.excludesFile", "/dev/null"]);
    std::fs::write(r.main.join(".git/info/exclude"), ".env\n").unwrap();
    git(&r.other, &["pull", "--quiet", "--ff-only"]);
    write(&r.other, ".env", "SECRET=theirs\n");
    git(&r.other, &["add", "-f", ".env"]);
    git(&r.other, &["commit", "--quiet", "-m", "track .env"]);
    git(&r.other, &["push", "--quiet", "origin", "main"]);
    let before = head(&r.main);
    let err = run_on(&r.main, true, GitAction::Pull)
        .unwrap_err()
        .to_string();
    assert!(err.contains(".env"), "{err}");
    assert_eq!(head(&r.main), before);
    assert_eq!(
        std::fs::read_to_string(r.main.join(".env")).unwrap(),
        "SECRET=mine\n"
    );
}

#[test]
fn an_ignored_directory_counts_and_an_ignored_file_nobody_tracks_does_not() {
    let r = repo();
    let wt = r.worktree("feat/dirs");
    commit(&wt, ".gitignore", "build/\n*.log\n", "ignore");
    std::fs::create_dir(wt.join("build")).unwrap();
    write(&wt, "build/out.js", "x");
    write(&wt, "debug.log", "x");
    git(&r.other, &["pull", "--quiet", "--ff-only"]);
    std::fs::create_dir(r.other.join("build")).unwrap();
    write(&r.other, "build/vendored.js", "y");
    git(&r.other, &["add", "-f", "build/vendored.js"]);
    git(&r.other, &["commit", "--quiet", "-m", "vendor"]);
    git(&r.other, &["push", "--quiet", "origin", "main"]);
    git(&wt, &["fetch", "--quiet", "origin"]);
    assert_eq!(
        run::ignored_in_the_way(&wt, "origin/main"),
        vec!["build/vendored.js".to_string()]
    );
}
