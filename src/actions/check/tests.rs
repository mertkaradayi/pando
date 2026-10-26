use super::base::{Standing, names, on_the_base};
use super::run::commit_to_test;
use super::*;
use crate::config::Config;
use crate::setup::RanBy;
use crate::testutil::git;
use anyhow::Context as _;
use tempfile::TempDir;

#[test]
fn who_ran_a_check_is_the_tui_when_it_says_so_else_whoever_has_stderr() {
    assert_eq!(ran_by(Some("tui"), false), RanBy::Tui);
    assert_eq!(ran_by(Some("tui"), true), RanBy::Tui);
    assert_eq!(ran_by(None, true), RanBy::Terminal);
    assert_eq!(ran_by(Some("anything"), true), RanBy::Terminal);
    assert_eq!(ran_by(None, false), RanBy::Program);
    assert_eq!(CHECK_RAN_BY_ENV, "PANDO_CHECK_RAN_BY");
}

/// A repository whose default branch `main` is one commit behind the
/// branch it has checked out.
fn repo_ahead_of_main() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    crate::testutil::init_repo(&root);
    git(&root, &["checkout", "--quiet", "-b", "feat/wip"]);
    git(&root, &["commit", "--quiet", "--allow-empty", "-m", "wip"]);
    (dir, root)
}

fn sha(root: &std::path::Path, refname: &str) -> String {
    let out = crate::project::git(root, ["rev-parse", refname]).unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn the_commit_tested_is_the_one_new_would_fork_from_not_head() {
    let (_dir, root) = repo_ahead_of_main();
    let (commit, base) = commit_to_test(&root, None, None).unwrap();
    assert_eq!(base.as_deref(), Some("main"));
    assert_eq!(commit, sha(&root, "main"));
    assert_ne!(commit, sha(&root, "HEAD"));

    // The project's own base wins, as it does for `new`.
    let mut config = Config::default();
    config.project.base = Some("feat/wip".to_string());
    let (commit, base) = commit_to_test(&root, config.project.base.as_deref(), None).unwrap();
    assert_eq!(base.as_deref(), Some("feat/wip"));
    assert_eq!(commit, sha(&root, "HEAD"));

    // And a base given for the run wins over both.
    let (commit, base) =
        commit_to_test(&root, config.project.base.as_deref(), Some("main")).unwrap();
    assert_eq!(base.as_deref(), Some("main"));
    assert_eq!(commit, sha(&root, "main"));
}

// A base the repository does not have is refused before anything is made
// or recorded: tested at origin/HEAD instead, the run would claim a commit
// it never tested.
#[test]
fn a_base_the_repository_does_not_have_is_refused_with_nothing_recorded() {
    let (dir, root) = repo_ahead_of_main();
    let project = crate::project::ProjectRef::from_root(&root).unwrap();
    let paths = crate::paths::PandoPaths::new(dir.path().join("pando-home"), project);
    let quiet = |_: &str| {};
    let say = Narration {
        step: &quiet,
        detail: &quiet,
    };
    let err = check_at(
        &paths,
        &Config::default(),
        Some("nope"),
        RanBy::Program,
        &say,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("base \"nope\" does not exist"),
        "{err:#}"
    );
    assert!(!paths.check_file().exists());
}

#[test]
fn with_no_default_branch_the_commit_is_head_and_with_no_commit_there_is_none() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet", "--initial-branch=trunk"]);
    assert_eq!(commit_to_test(&root, None, None), None);

    git(&root, &["commit", "--quiet", "--allow-empty", "-m", "root"]);
    let (commit, base) = commit_to_test(&root, None, None).unwrap();
    assert_eq!(base, None, "HEAD is said as HEAD");
    assert_eq!(commit, sha(&root, "HEAD"));
}

/// A failure of the settings, as the check records one.
fn settings_failure(reason: &str) -> crate::setup::CheckOutcome {
    crate::setup::CheckOutcome::Failed {
        kind: crate::setup::FailureKind::Settings,
        reason: reason.to_string(),
    }
}

// A file the failure names, on the main checkout's branch and not at the
// commit tested, makes the failure the base's, naming both refs and the
// run that tests the right one.
#[test]
fn a_failure_for_a_file_only_the_main_checkouts_branch_has_is_the_bases() {
    use crate::setup::{CheckOutcome, FailureKind};
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("repo");
    crate::testutil::drifted_repo(&root, 1, 1, Some("backend/uv.lock"));
    let tested = sha(&root, "origin/develop");
    let failed = "the install step failed: `cd backend && uv sync --frozen` exited with 2";
    let tail =
        ["error: Unable to find lockfile at `uv.lock`, but `--frozen` was provided".to_string()];

    let outcome = on_the_base(
        &root,
        settings_failure(failed),
        &tested,
        Some("origin/develop"),
        &tail,
        None,
        None,
    );
    let CheckOutcome::Failed { kind, reason } = outcome else {
        panic!("still a failure");
    };
    assert_eq!(kind, FailureKind::Base);
    assert!(reason.starts_with(failed), "{reason}");
    assert!(
        reason.contains(&format!(
            "backend/uv.lock is on work, the main checkout's branch, but not at origin/develop \
             ({}), the commit this check tested",
            &tested[..7]
        )),
        "{reason}"
    );
    assert!(
        reason.ends_with(
            "No setting fixes that: test work with `pando check --base work`, or answer `base` \
             with it"
        ),
        "{reason}"
    );

    // A probe of another base while the project's is answered says the
    // project's is untouched, not how to test or answer it again.
    for (checked, end) in [
        (
            true,
            "No setting fixes that. The project's base, work, is unaffected by this run, and \
             its last check's result stands",
        ),
        (
            false,
            "No setting fixes that. The project's base, work, is unaffected by this run, and \
             has no check result yet: `pando check` tests it",
        ),
    ] {
        let outcome = on_the_base(
            &root,
            settings_failure(failed),
            &tested,
            Some("origin/develop"),
            &tail,
            None,
            Some(Standing {
                base: "work",
                checked,
            }),
        );
        let CheckOutcome::Failed { kind, reason } = outcome else {
            panic!("still a failure");
        };
        assert_eq!(kind, FailureKind::Base);
        assert!(reason.ends_with(end), "{reason}");
    }

    // A lockfile of a manager the install runs counts unnamed.
    let outcome = on_the_base(
        &root,
        settings_failure(failed),
        &tested,
        Some("origin/develop"),
        &[],
        Some("cd backend && uv sync --frozen"),
        None,
    );
    assert!(matches!(
        outcome,
        CheckOutcome::Failed {
            kind: FailureKind::Base,
            ..
        }
    ));

    // Nothing the base explains is left as it was: another failure, a
    // machine's, or a check of the main checkout's own commit.
    let head = sha(&root, "HEAD");
    let machine = CheckOutcome::Failed {
        kind: FailureKind::Machine,
        reason: "uv.lock".to_string(),
    };
    for (outcome, commit, tail) in [
        (
            settings_failure("dev exited with status 1"),
            tested.as_str(),
            vec!["port in use".to_string()],
        ),
        (machine, tested.as_str(), tail.to_vec()),
        (settings_failure(failed), head.as_str(), tail.to_vec()),
    ] {
        assert_eq!(
            on_the_base(
                &root,
                outcome.clone(),
                commit,
                None,
                &tail,
                Some("npm ci"),
                None
            ),
            outcome
        );
    }
}

#[test]
fn a_file_is_named_whole_or_not_at_all() {
    assert!(names("at `uv.lock`, but", "uv.lock"));
    assert!(names("missing backend/uv.lock.", "uv.lock"));
    assert!(names("uv.lock", "uv.lock"));
    assert!(!names("uv.lockfile is gone", "uv.lock"));
    assert!(!names("myuv.lock", "uv.lock"));
    assert!(!names("uv.lock.bak", "uv.lock"));
}

// A probe writes where the check's processes always write, so it holds
// the last check's logs aside and puts them back after; one killed first
// is put right by the next check.
#[test]
fn a_probe_keeps_its_logs_beside_the_last_checks_and_a_killed_ones_are_put_back() {
    use super::logs::{claim, settle};
    use crate::paths::{CHECK_HELD_LOGS, CHECK_PROBE_LOGS, CHECK_WORKTREE};
    let (dir, root) = repo_ahead_of_main();
    let project = crate::project::ProjectRef::from_root(&root).unwrap();
    let paths = crate::paths::PandoPaths::new(dir.path().join("pando-home"), project);
    let write = |logs: &str, text: &str| {
        std::fs::create_dir_all(paths.logs_dir(logs)).unwrap();
        std::fs::write(paths.log_file(logs, "dev"), text).unwrap();
    };
    let read = |logs: &str| std::fs::read_to_string(paths.log_file(logs, "dev")).ok();
    write(CHECK_WORKTREE, "the check's");

    // A probe, run to its end.
    claim(&paths, true).unwrap();
    assert_eq!(read(CHECK_HELD_LOGS).as_deref(), Some("the check's"));
    write(CHECK_WORKTREE, "the first probe's");
    settle(&paths);
    assert_eq!(read(CHECK_WORKTREE).as_deref(), Some("the check's"));
    assert_eq!(read(CHECK_PROBE_LOGS).as_deref(), Some("the first probe's"));
    assert!(!paths.logs_dir(CHECK_HELD_LOGS).exists());

    // One killed before it settled: the next check's settle does it, and
    // its logs replace the last probe's.
    claim(&paths, true).unwrap();
    write(CHECK_WORKTREE, "the killed probe's");
    settle(&paths);
    assert_eq!(read(CHECK_WORKTREE).as_deref(), Some("the check's"));
    assert_eq!(
        read(CHECK_PROBE_LOGS).as_deref(),
        Some("the killed probe's")
    );

    // With no check's logs to hold, none come back, and a probe that made
    // none leaves no probe's either.
    claim(&paths, false).unwrap();
    assert!(!paths.logs_dir(CHECK_WORKTREE).exists());
    claim(&paths, true).unwrap();
    settle(&paths);
    assert!(!paths.logs_dir(CHECK_WORKTREE).exists());
    assert!(!paths.logs_dir(CHECK_PROBE_LOGS).exists());
    assert!(!paths.logs_dir(CHECK_HELD_LOGS).exists());

    // Nothing held, nothing moved.
    write(CHECK_WORKTREE, "a check's");
    settle(&paths);
    assert_eq!(read(CHECK_WORKTREE).as_deref(), Some("a check's"));
}

// The error an install that fails gives, built by the code that builds it
// for `new`: said by the check once, with its exit status, rather than
// "the install step failed: the install hook failed: exited 2: …".
#[test]
fn an_install_failure_says_the_step_failed_once_with_its_exit_status() {
    use super::run::install_failed;
    use crate::actions::{CREATED_BUT_INSTALL_FAILED, INSTALL_HOOK};
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("install.log");
    let hook = super::super::hooks::failed_words(INSTALL_HOOK);
    let as_new_says = |cmd: &str| {
        let err = crate::hooks::run(&log, cmd, dir.path(), &[])
            .context(hook.clone())
            .context(format!("abc1234 {CREATED_BUT_INSTALL_FAILED}"))
            .unwrap_err();
        format!("{err:#}")
    };
    assert_eq!(
        install_failed(&as_new_says("echo 'error: no lockfile'; exit 2")),
        format!(
            "the install step failed (exit 2): error: no lockfile — the whole log is at {}",
            log.display()
        )
    );
    // With nothing printed, the status alone.
    assert_eq!(
        install_failed(&as_new_says("exit 7")),
        format!(
            "the install step failed (exit 7) — the whole log is at {}",
            log.display()
        )
    );
    // Anything else that failed the step is said as it came.
    assert_eq!(
        install_failed(&format!(
            "abc1234 {CREATED_BUT_INSTALL_FAILED}: {hook}: in the command for hook install"
        )),
        "the install step failed: in the command for hook install"
    );
}
