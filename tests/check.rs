//! `pando check`, end to end: the binary against generated fixture
//! repositories with an injected `PANDO_HOME`, and the action itself where
//! a test has to shorten a wait. Every check here runs a python server or
//! a `sleep`, never a real toolchain.

use crate::common;

use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use common::{Kind, build, git_raw, python3_available, wait_until};
use pando::paths::{CHECK_HELD_LOGS, CHECK_PROBE_LOGS, CHECK_WORKTREE, PandoPaths};
use pando::setup::{CheckOutcome, CheckRecord, FailureKind, SetupState};
use tempfile::TempDir;

/// A dev server that answers every request with `status`.
///
/// Brace-free: `{` is pando's template syntax. Served by a `TCPServer`
/// that reuses its address, as `HTTPServer` does, rather than by
/// `HTTPServer` itself, whose bind looks the address up in reverse DNS
/// before it listens: on a CI runner, in a session of its own, that lookup
/// never returned, and the port was bound and never listening.
fn answering(status: u16) -> String {
    format!(
        "python3 -u -c \"import http.server as h,os;C=type('C',(h.BaseHTTPRequestHandler,),\
         dict(do_GET=lambda s:(s.send_response({status}),s.end_headers())));\
         type('S',(h.socketserver.TCPServer,),dict(allow_reuse_address=1))(('127.0.0.1',int(os.environ['PORT'])),C).serve_forever()\""
    )
}

/// pando's config for a project whose one process runs `cmd` on the `web`
/// role, with an install step that does nothing, and `extra` after it.
fn config_running(cmd: &str, extra: &str) -> String {
    format!(
        "[project]\ninstall = \"true\"\n\n[dev]\ncmd = '''{cmd}'''\nports = {{ PORT = \"web\" }}\n\n\
         {extra}"
    )
}

struct Env {
    _dir: TempDir,
    home: PathBuf,
    root: PathBuf,
    paths: PandoPaths,
}

fn env(config: &str) -> Env {
    env_of(Kind::Plain, Some(config))
}

fn env_of(kind: Kind, config: Option<&str>) -> Env {
    let dir = TempDir::new().unwrap();
    let parent = std::fs::canonicalize(dir.path()).unwrap();
    let root = build(kind, &parent).root;
    let home = parent.join("pando-home");
    let paths = common::paths_for(&home, &root);
    paths.ensure_home().unwrap();
    // Nothing here needs a runtime initialised in front of it.
    std::fs::write(home.join("config.toml"), "[runtime]\nprelude = \"\"\n").unwrap();
    if let Some(config) = config {
        std::fs::write(paths.config_file(), config).unwrap();
    }
    Env {
        _dir: dir,
        home,
        root: paths.root().to_path_buf(),
        paths,
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        if self.root.exists() {
            let _ = self.pando(&["stop", "--all"]);
        }
    }
}

impl Env {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pando"));
        command
            .env("PANDO_HOME", &self.home)
            .env_remove(pando::actions::CHECK_RAN_BY_ENV)
            .current_dir(&self.root)
            .args(args);
        command
    }

    fn pando(&self, args: &[&str]) -> Output {
        self.command(args).output().expect("run pando")
    }

    fn spawn(&self, args: &[&str]) -> Child {
        self.command(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn pando")
    }

    fn setup_state(&self) -> SetupState {
        let config = pando::config::load(&self.paths).unwrap().config;
        pando::setup::read(&self.paths, &config).state
    }

    fn record(&self) -> Option<CheckRecord> {
        CheckRecord::load(&self.paths)
    }

    fn write_config(&self, config: &str) {
        std::fs::write(self.paths.config_file(), config).unwrap();
    }

    fn check_dir(&self) -> PathBuf {
        let config = pando::config::load(&self.paths).unwrap().config;
        config.check_worktree_path(&self.paths)
    }

    /// Waits until the running check's record has a step starting with
    /// `step`.
    fn wait_for_step(&self, step: &str) {
        assert!(
            wait_until(Duration::from_secs(30), || {
                self.record()
                    .is_some_and(|r| r.progress.iter().any(|line| line.starts_with(step)))
            }),
            "the check never got to {step:?}: {:?}",
            self.record()
        );
    }

    fn git(&self, args: &[&str]) -> String {
        let out = git_raw(&self.root, args);
        assert!(out.status.success(), "git {args:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// No worktree, no branch, no record and no directory of the check.
    fn assert_nothing_left(&self, branches_before: &str) {
        let listed = self.git(&["worktree", "list", "--porcelain"]);
        assert_eq!(
            listed.matches("worktree ").count(),
            1,
            "git lists only the main checkout: {listed}"
        );
        assert_eq!(
            self.git(&["branch", "--list"]),
            branches_before,
            "no branch was made"
        );
        let state = pando::state::load(&self.paths.state_file()).unwrap();
        assert!(
            !state.worktrees.contains_key(CHECK_WORKTREE),
            "the check's state record is gone"
        );
        assert!(!self.check_dir().exists(), "its directory is gone");
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is one JSON object ({e}): {}", stderr(out)))
}

fn skip_without_python() -> bool {
    if !python3_available() {
        eprintln!("skipped: python3 is not available");
        return true;
    }
    false
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn signal(child: &Child, signal: nix::sys::signal::Signal) {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(child.id() as i32), signal).unwrap();
}

// A pass: the page answers 404, which is an app serving pages; the hooks
// after services are skipped and said to be; and nothing is left but the
// logs.
#[test]
fn a_check_passes_skips_the_hooks_after_services_and_leaves_nothing_behind() {
    if skip_without_python() {
        return;
    }
    let dir = TempDir::new().unwrap();
    let migrated = dir.path().join("migrated");
    let seeded = dir.path().join("seeded");
    let e = env(&config_running(
        &answering(404),
        &format!(
            "[[hooks]]\nname = \"migrate\"\nafter = \"services\"\non = \"always\"\ncmd = \"touch {}\"\n\n\
             [[hooks]]\nname = \"seed\"\nafter = \"dev\"\ncmd = \"touch {}\"\n",
            migrated.display(),
            seeded.display()
        ),
    ));
    let branches = e.git(&["branch", "--list"]);
    let out = e.pando(&["check"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "✓ plain is ready: `pando` opens it\n"
    );
    assert!(err.contains("in a throwaway worktree (no branch)"), "{err}");
    assert!(
        err.contains("skipped the hooks that run after services (migrate, seed)"),
        "{err}"
    );
    assert!(err.contains("dev ready :"), "{err}");
    assert!(err.contains("(HTTP 404)"), "{err}");
    assert!(
        err.contains("removed the test worktree; nothing left behind"),
        "{err}"
    );
    assert!(!migrated.exists(), "a hook after services ran in a check");
    assert!(!seeded.exists(), "a hook after dev ran in a check");
    e.assert_nothing_left(&branches);
    let config = pando::config::load(&e.paths).unwrap().config;
    assert!(
        !config.worktrees_dir(&e.paths).exists(),
        "the worktrees directory the check made, empty, went with it"
    );
    assert!(
        e.paths.log_file(CHECK_WORKTREE, "dev").is_file(),
        "the check's logs are kept"
    );

    let record = e.record().unwrap();
    assert_eq!(record.outcome, CheckOutcome::Passed);
    assert_eq!(record.processes.len(), 1);
    assert_eq!(record.processes[0].http_status, Some(404));
    assert!(record.processes[0].ready);
    assert_eq!(record.base_ref.as_deref(), Some("main"));
    assert_eq!(
        record.commit.as_deref(),
        Some(e.git(&["rev-parse", "main"]).trim())
    );
    assert!(!record.changed_while_running());
    assert_eq!(record.ran_by, pando::setup::RanBy::Program);
    assert_eq!(
        e.setup_state(),
        SetupState::Ready,
        "what the dashboard reads"
    );

    // The same, as the published shape, run by the TUI.
    let out = e
        .command(&["check", "--json"])
        .env(pando::actions::CHECK_RAN_BY_ENV, "tui")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["result"], "passed");
    assert_eq!(v["kind"], serde_json::Value::Null);
    assert_eq!(v["ran_by"], "tui");
    assert_eq!(v["processes"][0]["http_status"], 404);
    e.assert_nothing_left(&branches);
}

// origin/HEAD behind the branch the main checkout is on, and a file the
// install needs only on that branch: the failure is the base's, naming
// both refs and the run that tests the right one, and `--base` tests it.
#[test]
fn an_install_that_fails_for_a_file_the_base_lacks_is_the_bases_and_base_tests_another() {
    let e = env(
        "[project]\ninstall = \"cat app.lock\"\n\n[dev]\ncmd = \"echo up; exit 3\"\nports = []\n",
    );
    let old = e.git(&["rev-parse", "HEAD"]);
    e.git(&["update-ref", "refs/remotes/origin/main", old.trim()]);
    e.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ]);
    e.git(&["checkout", "--quiet", "-b", "work"]);
    std::fs::write(e.root.join("app.lock"), "locked\n").unwrap();
    e.git(&["add", "app.lock"]);
    e.git(&["commit", "--quiet", "-m", "lock"]);
    let branches = e.git(&["branch", "--list"]);

    let out = e.pando(&["check", "--json"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    let v = json(&out);
    assert_eq!(v["kind"], "base", "{v}");
    assert_eq!(v["base_ref"], "origin/main");
    assert_eq!(v["failed_process"], "install");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.contains("app.lock is on work, the main checkout's branch, but not at origin/main"),
        "{reason}"
    );
    assert!(reason.contains("`pando check --base work`"), "{reason}");
    assert!(
        err.contains("pando: the check failed on the commit it tested:"),
        "{err}"
    );
    e.assert_nothing_left(&branches);

    // At the branch that has it, the install passes, and what fails next
    // is the settings' own.
    let out = e.pando(&["check", "--json", "--base", "work"]);
    let v = json(&out);
    assert_eq!(v["base_ref"], "work", "{v}");
    assert_eq!(v["kind"], "settings", "{v}");
    assert_eq!(v["failed_process"], "dev");
    assert!(
        v["notes"][0].as_str().unwrap().contains(
            "testing work for this run only, because --base named it, and keeping the last \
             check's result: `pando new` still forks from origin/main; to make work the base, \
             answer `base` with it through `pando init --answers -`"
        ),
        "{v}"
    );
    e.assert_nothing_left(&branches);
    // A probe: the last check at the settings' own base is still the
    // setup's.
    let record = e.record().unwrap();
    assert_eq!(record.base_ref.as_deref(), Some("origin/main"));
    assert!(
        matches!(
            record.outcome,
            CheckOutcome::Failed {
                kind: FailureKind::Base,
                ..
            }
        ),
        "{record:?}"
    );

    // A base already answered is changed only by `--replace`, and the
    // note names the command that would not be refused.
    std::fs::write(
        e.paths.config_file(),
        "[project]\ninstall = \"cat app.lock\"\nbase = \"origin/main\"\n\n\
         [dev]\ncmd = \"echo up; exit 3\"\nports = []\n",
    )
    .unwrap();
    let v = json(&e.pando(&["check", "--json", "--base", "work"]));
    assert!(
        v["notes"][0]
            .as_str()
            .unwrap()
            .contains("answer `base` with it through `pando init --answers - --replace`"),
        "{v}"
    );
    e.assert_nothing_left(&branches);

    // A probe that fails on its base while the project's is answered
    // leaves that base as it was, and says so rather than naming it again
    // as the way past.
    std::fs::write(
        e.paths.config_file(),
        "[project]\ninstall = \"cat app.lock\"\nbase = \"work\"\n\n\
         [dev]\ncmd = \"echo up; exit 3\"\nports = []\n",
    )
    .unwrap();
    let v = json(&e.pando(&["check", "--json", "--base", "origin/main"]));
    assert_eq!(v["kind"], "base", "{v}");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.ends_with(
            "No setting fixes that. The project's base, work, is unaffected by this run, and \
             its last check's result stands"
        ),
        "{reason}"
    );
    e.assert_nothing_left(&branches);

    let out = e.pando(&["check", "--base", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("base \"nope\" does not exist"),
        "{}",
        stderr(&out)
    );
}

// Issue #4's role-play: a check passed at the project's base, then a
// probe of another base failed and replaced it, and the setup read as
// changed when nothing had. A probe prints its whole result and saves
// none of it; a `--base` naming the settings' own is an ordinary check.
#[test]
fn a_probe_of_another_base_never_replaces_the_last_check_at_the_settings_own() {
    if skip_without_python() {
        return;
    }
    let e = env(&config_running(&answering(200), "")
        .replace("install = \"true\"", "install = \"cat app.lock\""));
    e.git(&["branch", "old"]);
    std::fs::write(e.root.join("app.lock"), "locked\n").unwrap();
    e.git(&["add", "app.lock"]);
    e.git(&["commit", "--quiet", "-m", "lock"]);
    let branches = e.git(&["branch", "--list"]);

    let out = e.pando(&["check"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let passed = e.record().unwrap();
    assert_eq!(passed.outcome, CheckOutcome::Passed);
    assert_eq!(passed.settings_base, Some(None));
    assert_eq!(e.setup_state(), SetupState::Ready);

    let out = e.pando(&["check", "--json", "--base", "old"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["result"], "failed", "{v}");
    assert_eq!(v["kind"], "base", "{v}");
    assert_eq!(v["base_ref"], "old");
    assert_eq!(v["failed_process"], "install");
    assert_eq!(v["commit"], e.git(&["rev-parse", "old"]).trim());
    assert!(
        v["notes"][0]
            .as_str()
            .unwrap()
            .starts_with("testing old for this run only"),
        "{v}"
    );
    e.assert_nothing_left(&branches);
    assert_eq!(e.record(), Some(passed.clone()), "the probe saved nothing");
    assert_eq!(e.setup_state(), SetupState::Ready);
    // Nor did it write over the passing check's logs: its own are beside
    // them.
    let read = |dir: &str, source: &str| std::fs::read_to_string(e.paths.log_file(dir, source));
    assert!(read(CHECK_WORKTREE, "install").unwrap().contains("locked"));
    assert!(e.paths.log_file(CHECK_WORKTREE, "dev").is_file());
    assert!(
        read(CHECK_PROBE_LOGS, "install")
            .unwrap()
            .contains("app.lock"),
        "the probe's failure is in its own logs"
    );
    assert!(!e.paths.log_file(CHECK_PROBE_LOGS, "dev").exists());
    assert!(!e.paths.logs_dir(CHECK_HELD_LOGS).exists());
    // And its reason, which says the step failed once, names the log
    // where it is now.
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("the install step failed (exit 1): "),
        "{reason}"
    );
    let log = e.paths.log_file(CHECK_PROBE_LOGS, "install");
    assert!(reason.contains(&log.display().to_string()), "{reason}");

    // A pass at another base says it is not the setup's, and is not.
    e.write_config(&config_running(&answering(200), ""));
    let out = e.pando(&["check", "--base", "old"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "✓ plain passes at old: answer `base` with it to make that the setup's\n"
    );
    assert_eq!(e.record().unwrap().started_at, passed.started_at);
    // Its logs replace the last probe's, and the saved check's stay.
    assert!(e.paths.log_file(CHECK_PROBE_LOGS, "dev").is_file());
    assert!(read(CHECK_WORKTREE, "install").unwrap().contains("locked"));

    // The settings' own base, named: an ordinary check, saved.
    let out = e.pando(&["check", "--base", "main"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "✓ plain is ready: `pando` opens it\n"
    );
    let record = e.record().unwrap();
    assert_ne!(record.started_at, passed.started_at);
    assert!(record.notes.is_empty(), "{:?}", record.notes);
    e.assert_nothing_left(&branches);
}

// A process that reaches outside its worktree — one that opens the app on
// the simulator — has a documented way to know it runs under a check:
// the install, a hook and the process each get `PANDO_CHECK=1`.
#[test]
fn everything_a_check_runs_is_told_so_by_pando_check() {
    let dir = TempDir::new().unwrap();
    let sink = dir.path().join("seen");
    let e = env(&format!(
        "[project]\ninstall = \"echo install=$PANDO_CHECK >> '{sink}'\"\n\n\
         [dev]\ncmd = '''echo dev=$PANDO_CHECK >> '{sink}'; exit 3'''\n\
         ports = {{ PORT = \"web\" }}\n\n\
         [[hooks]]\nname = \"prepare\"\nafter = \"create\"\n\
         cmd = \"echo hook=$PANDO_CHECK >> '{sink}'\"\n",
        sink = sink.display()
    ));
    let out = e.pando(&["check"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let seen = std::fs::read_to_string(&sink).unwrap();
    for line in ["install=1", "hook=1", "dev=1"] {
        assert!(seen.lines().any(|l| l == line), "{line} in {seen}");
    }
}

#[test]
fn a_process_that_exits_at_once_fails_the_check_with_its_lines_redacted() {
    let e = env(&config_running(
        "echo 'connecting with password=hunter2'; echo 'Error: boom'; exit 3",
        "",
    ));
    let branches = e.git(&["branch", "--list"]);
    let out = e.pando(&["check", "--json"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(1), "{err}");
    let v = json(&out);
    assert_eq!(v["result"], "failed");
    assert_eq!(v["kind"], "settings");
    assert_eq!(v["failed_process"], "dev");
    assert_eq!(v["processes"][0]["ready"], false);
    let tail: Vec<&str> = v["failed_tail"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap())
        .collect();
    assert_eq!(tail, ["connecting with password=(hidden)", "Error: boom"]);
    assert!(!err.contains("hunter2"), "{err}");
    assert!(err.contains("the last lines of the dev log:"), "{err}");
    assert!(err.contains("pando: the check failed: dev failed"), "{err}");
    e.assert_nothing_left(&branches);
    assert_eq!(e.setup_state(), SetupState::Failing);
}

#[test]
fn a_page_that_answers_500_fails_the_check() {
    if skip_without_python() {
        return;
    }
    let e = env(&config_running(&answering(500), ""));
    let branches = e.git(&["branch", "--list"]);
    let out = e.pando(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["kind"], "settings");
    assert!(
        v["reason"].as_str().unwrap().contains("HTTP 500"),
        "{}",
        v["reason"]
    );
    assert_eq!(v["processes"][0]["http_status"], 500);
    // It came up; its page is what failed, and `reason` says so.
    assert_eq!(v["processes"][0]["ready"], true);
    assert_eq!(v["failed_process"], "dev");
    e.assert_nothing_left(&branches);
}

// A server that takes the connection and never answers fails once the
// wait is over — shortened here, where it is ninety seconds for real.
#[test]
fn a_page_that_never_comes_fails_when_the_wait_is_over() {
    if skip_without_python() {
        return;
    }
    let e = env(&config_running(&common::listener_on_port_env(), ""));
    let config = pando::config::load(&e.paths).unwrap().config;
    let quiet = |_: &str| {};
    let say = pando::actions::Narration {
        step: &quiet,
        detail: &quiet,
    };
    let checked = pando::ports::with_page_wait(Duration::from_secs(1), || {
        pando::actions::check(&e.paths, &config, pando::setup::RanBy::Program, &say)
    })
    .unwrap();
    match &checked.record.outcome {
        CheckOutcome::Failed { kind, reason } => {
            assert_eq!(*kind, FailureKind::Settings);
            assert!(
                reason.contains("did not answer its first page within 1s"),
                "{reason}"
            );
        }
        other => panic!("{other:?}"),
    }
    // Its port answered, so it is ready; only its page never came.
    let dev = &checked.record.processes[0];
    assert!(dev.ready && dev.http_status.is_none(), "{dev:?}");
    assert!(!e.check_dir().exists());
}

// A shared service nothing answers for is the machine's: said with what
// starts it, and nothing is made — not even the install.
#[test]
fn a_stopped_shared_service_fails_as_the_machines_before_anything_is_made() {
    let dir = TempDir::new().unwrap();
    let installed = dir.path().join("installed");
    let e = env(&format!(
        "[project]\ninstall = \"touch {}\"\n\n[dev]\ncmd = \"sleep 600\"\nports = {{ PORT = \"web\" }}\n\n\
         [[services]]\nkind = \"native\"\nname = \"redis\"\nenv = {{ REDIS_URL = \"redis\" }}\n",
        installed.display()
    ));
    let port = free_port();
    let env_file = e.root.join(".env");
    let mut text = std::fs::read_to_string(&env_file).unwrap();
    text.push_str(&format!("REDIS_URL=redis://localhost:{port}/0\n"));
    std::fs::write(&env_file, text).unwrap();
    let branches = e.git(&["branch", "--list"]);

    let out = e.pando(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["result"], "failed");
    assert_eq!(v["kind"], "machine");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.contains(&format!("nothing answers on localhost:{port}")),
        "{reason}"
    );
    assert!(reason.contains("REDIS_URL"), "{reason}");
    assert!(stderr(&out).contains("This is the machine's to fix"));
    assert!(!installed.exists(), "nothing ran");
    let record = e.record().unwrap();
    assert!(
        !record.progress.iter().any(|l| l == "installing"),
        "{:?}",
        record.progress
    );
    e.assert_nothing_left(&branches);
}

// A service whose port only the env file beside a process's app says is
// still found, and the reason names that file: without it, nothing was
// probed and the check went on to fail on the app instead.
#[test]
fn a_stopped_shared_service_is_found_through_an_app_directorys_env_file() {
    let e = env(
        "[processes.api]\ncmd = \"sleep 600\"\ncwd = \"backend\"\nports = { PORT = \"api\" }\n\n\
         [[services]]\nkind = \"native\"\nname = \"postgres\"\nenv = { POSTGRES_PORT = \"postgres\" }\n",
    );
    let port = free_port();
    std::fs::create_dir_all(e.root.join("backend")).unwrap();
    std::fs::write(
        e.root.join("backend/.env"),
        format!("POSTGRES_SERVER=localhost\nPOSTGRES_PORT={port}\n"),
    )
    .unwrap();
    let branches = e.git(&["branch", "--list"]);

    let out = e.pando(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["kind"], "machine", "{v}");
    let reason = v["reason"].as_str().unwrap();
    assert!(
        reason.contains(&format!(
            "nothing answers on localhost:{port}, where POSTGRES_PORT in the main checkout's \
             backend/.env puts postgres"
        )),
        "{reason}"
    );
    e.assert_nothing_left(&branches);
}

// A question the rules cannot settle alone is exit 3, as everywhere, with
// the result still one object on stdout.
#[test]
fn a_question_still_open_exits_3_and_prints_the_result() {
    let e = env_of(Kind::MonoWebApi, None);
    let branches = e.git(&["branch", "--list"]);
    let out = e.pando(&["check", "--json"]);
    let err = stderr(&out);
    assert_eq!(out.status.code(), Some(3), "{err}");
    let v = json(&out);
    assert_eq!(v["result"], "not_set_up");
    assert_eq!(v["slot"], "processes");
    assert_eq!(v["commit"], serde_json::Value::Null);
    assert!(
        err.contains("starts nothing while a question is open"),
        "{err}"
    );
    assert!(err.contains("Run these as separate processes?"), "{err}");
    // `check` takes no `--yes`: the way out goes through `init`, and back.
    assert!(!err.contains("rerun with --yes"), "{err}");
    assert!(err.contains("`pando init --yes`"), "{err}");
    assert!(err.contains("then run `pando check` again"), "{err}");
    assert!(
        err.contains("`pando init --answers -` with, on stdin,"),
        "{err}"
    );
    assert!(
        err.lines()
            .any(|l| l == "pando: the check starts nothing while a question is open"),
        "{err}"
    );
    assert_eq!(
        e.record().unwrap().outcome,
        CheckOutcome::NotSetUp {
            slot: "processes".to_string()
        }
    );
    e.assert_nothing_left(&branches);
}

#[test]
fn a_second_check_is_refused_while_one_runs() {
    let e = env(&config_running("sleep 600", ""));
    let _held = pando::state::try_lock(&e.paths.check_lock_file())
        .unwrap()
        .expect("the lock is free");
    let out = e.pando(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("a check is already running for this project"),
        "{}",
        stderr(&out)
    );
    assert!(
        out.stdout.is_empty(),
        "nothing on stdout: the running one's result is the one"
    );
    assert!(
        e.record().is_none(),
        "the running check's record is not written over"
    );
}

// `stop --all` stops a check's processes like any other's, and the check,
// finding nothing left to wait on, records itself interrupted and still
// takes its worktree down.
#[test]
fn a_stop_during_a_check_records_it_interrupted() {
    let e = env(&config_running("sleep 600", ""));
    let branches = e.git(&["branch", "--list"]);
    let child = e.spawn(&["check", "--json"]);
    e.wait_for_step("waiting for dev");
    let stopped = e.pando(&["stop", "--all"]);
    assert!(stopped.status.success(), "{}", stderr(&stopped));
    // Named for what it is, as the TUI names it, never by its directory.
    let said = String::from_utf8_lossy(&stopped.stdout);
    assert_eq!(said.trim(), "stopped the running pando check");
    assert!(
        !stderr(&stopped).contains(CHECK_WORKTREE),
        "{}",
        stderr(&stopped)
    );
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(json(&out)["result"], "interrupted");
    assert_eq!(e.record().unwrap().outcome, CheckOutcome::Interrupted);
    e.assert_nothing_left(&branches);
}

#[test]
fn a_signal_ends_a_check_through_its_teardown() {
    let e = env(&config_running("sleep 600", ""));
    let branches = e.git(&["branch", "--list"]);
    let child = e.spawn(&["check"]);
    e.wait_for_step("waiting for dev");
    let pid = pando::state::load(&e.paths.state_file()).unwrap().worktrees[CHECK_WORKTREE]
        .processes["dev"]
        .pid;
    signal(&child, nix::sys::signal::Signal::SIGTERM);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("the check was stopped before it finished"));
    assert_eq!(e.record().unwrap().outcome, CheckOutcome::Interrupted);
    assert!(!pando::process::is_alive(pid), "its process was stopped");
    e.assert_nothing_left(&branches);
}

// A terminal's Ctrl-C reaches the whole foreground group, so the install
// dies of it too: the check was interrupted, not failed by its settings.
#[test]
fn a_ctrl_c_during_the_install_records_the_check_interrupted() {
    use std::os::unix::process::CommandExt;
    let e =
        env(&config_running("sleep 600", "")
            .replace("install = \"true\"", "install = \"sleep 600\""));
    let branches = e.git(&["branch", "--list"]);
    let child = e
        .command(&["check"])
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pando");
    e.wait_for_step("installing");
    // The install is under way, not merely announced.
    assert!(wait_until(Duration::from_secs(30), || {
        e.paths.log_file(CHECK_WORKTREE, "install").exists()
    }));
    nix::sys::signal::killpg(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert_eq!(e.record().unwrap().outcome, CheckOutcome::Interrupted);
    assert_eq!(e.setup_state(), SetupState::Interrupted);
    e.assert_nothing_left(&branches);
}

// What a SIGKILL leaves — a worktree with a server still running in it —
// is in no list, is a problem `doctor` reports, and is swept by the next
// check before it starts anything.
#[test]
fn a_killed_checks_leftover_is_hidden_reported_and_swept() {
    if skip_without_python() {
        return;
    }
    let e = env(&config_running(&common::listener_on_port_env(), ""));
    let branches = e.git(&["branch", "--list"]);
    let mut child = e.spawn(&["check"]);
    e.wait_for_step("asking dev for its first page");
    signal(&child, nix::sys::signal::Signal::SIGKILL);
    child.wait().unwrap();
    let pid = pando::state::load(&e.paths.state_file()).unwrap().worktrees[CHECK_WORKTREE]
        .processes["dev"]
        .pid;
    assert!(
        pando::process::is_alive(pid),
        "a SIGKILL leaves its server up"
    );
    assert!(e.git(&["worktree", "list"]).contains(CHECK_WORKTREE));
    assert_eq!(e.setup_state(), SetupState::Interrupted);

    let ls = e.pando(&["ls", "--json"]);
    assert!(!String::from_utf8_lossy(&ls.stdout).contains(CHECK_WORKTREE));
    let status = e.pando(&["status", "--json"]);
    assert!(!String::from_utf8_lossy(&status.stdout).contains(CHECK_WORKTREE));
    let names = e.pando(&["ls", "--names"]);
    assert!(!String::from_utf8_lossy(&names.stdout).contains(CHECK_WORKTREE));
    let by_name = e.pando(&["stop", CHECK_WORKTREE]);
    assert_eq!(by_name.status.code(), Some(1));
    assert!(
        stderr(&by_name).contains("no worktree named"),
        "{}",
        stderr(&by_name)
    );
    let doctor = e.pando(&["doctor", "--json"]);
    assert_eq!(doctor.status.code(), Some(1), "a leftover is a problem");
    let report = json(&doctor);
    assert!(
        report["findings"].as_array().unwrap().iter().any(|f| {
            f["severity"] == "problem"
                && f["message"]
                    .as_str()
                    .unwrap()
                    .contains("a `pando check` that did not finish")
                && f["fix"]
                    .as_str()
                    .unwrap()
                    .contains("`pando check` sweeps it")
        }),
        "{report}"
    );
    assert!(
        report["worktrees"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["name"] == CHECK_WORKTREE),
        "doctor shows it"
    );

    e.write_config(&config_running(&answering(200), ""));
    let out = e.pando(&["check"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("left its worktree behind — sweeping it first"),
        "{err}"
    );
    assert!(
        wait_until(Duration::from_secs(5), || !pando::process::is_alive(pid)),
        "the leftover's server was stopped"
    );
    e.assert_nothing_left(&branches);
    let doctor = e.pando(&["doctor", "--json"]);
    assert!(
        !String::from_utf8_lossy(&doctor.stdout).contains("did not finish"),
        "nothing left to report"
    );
}

// ---- the schema step, proved in namespaces of the check's own ----------------------

/// A project whose main checkout's MariaDB holds `shop`, on a port the
/// test keeps a listener on so the check finds the server up, with a fake
/// client in pando's own `bin` — no server anywhere. `login` puts the
/// app's login in the main checkout's env files; `hooks` goes after the
/// dev process and the service.
struct WithDb {
    e: Env,
    fake: PathBuf,
    /// Where the schema hook below writes the database it was given.
    marker: PathBuf,
    _server: std::net::TcpListener,
}

fn with_db(login: bool, hooks: &str) -> WithDb {
    with_db_running(&answering(200), login, hooks)
}

fn with_db_running(cmd: &str, login: bool, hooks: &str) -> WithDb {
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    let e = env("");
    let marker = e.home.parent().unwrap().join("schema-ran-on");
    let hooks = hooks.replace("MARKER", &marker.display().to_string());
    e.write_config(&config_running(
        cmd,
        &format!(
            "[[services]]\nkind = \"native\"\nname = \"mariadb\"\n\
             env = {{ DATABASE_PORT = \"mariadb\" }}\n\n{hooks}"
        ),
    ));
    let mut dotenv = format!("DATABASE_HOST=127.0.0.1\nDATABASE_PORT={port}\nDATABASE_NAME=shop\n");
    if login {
        dotenv.push_str("DATABASE_USER=app\nDATABASE_PASSWORD=check-secret-pw\n");
    }
    std::fs::write(e.root.join(".env"), dotenv).unwrap();
    let fake = common::fake_mariadb(&e.home);
    // A refused login, or none, asks for the container that publishes the
    // port: a stand-in `docker` that knows of none, so no test of these
    // reaches the developer's own containers.
    {
        use std::os::unix::fs::PermissionsExt;
        let docker = e.home.join("bin").join("docker");
        std::fs::write(&docker, "#!/bin/sh\n[ \"$1\" = ps ] && exit 0\nexit 1\n").unwrap();
        std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    WithDb {
        e,
        fake,
        marker,
        _server: server,
    }
}

/// A schema step that writes down the database it was pointed at.
const SCHEMA_HOOK: &str = "[[hooks]]\nname = \"schema\"\nafter = \"services\"\n\
                           cmd = '''echo \"$DATABASE_NAME\" >> 'MARKER' '''\n";

impl WithDb {
    fn fake(&self, file: &str) -> String {
        std::fs::read_to_string(self.fake.join(file)).unwrap_or_default()
    }

    /// The databases the fake server holds now.
    fn databases(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.fake.join("dbs"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

// The maintainer's report: a check that passed on the shared services had
// never run the schema step, and the first namespaced start failed on it.
// With a login already there, the check is that namespaced start: the
// schema step runs in a database of the check's own — never main's — and
// the database is dropped with the worktree.
#[test]
fn a_check_with_a_schema_step_and_a_login_runs_it_in_its_own_database_and_drops_it() {
    if skip_without_python() {
        return;
    }
    let db = with_db(true, SCHEMA_HOOK);
    let branches = db.e.git(&["branch", "--list"]);
    let out = db.e.pando(&["check", "--json"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("in a throwaway worktree (no branch), namespaced"),
        "{err}"
    );
    let v = json(&out);
    assert_eq!(v["result"], "passed");
    assert_eq!(v["mode"], "namespaced");
    assert!(
        v["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().starts_with(
                "running the hooks after services (schema) in a database of the check's own"
            )),
        "{v}"
    );
    assert_eq!(
        std::fs::read_to_string(&db.marker).unwrap(),
        "shop__pando_check\n",
        "the schema step ran, on the check's own database"
    );
    assert_eq!(db.fake("created"), "shop__pando_check\n");
    assert_eq!(
        db.fake("dropped"),
        "shop__pando_check\n",
        "exactly what the check made was dropped, and nothing else"
    );
    assert!(db.databases().is_empty(), "{:?}", db.databases());
    assert!(!err.contains("check-secret-pw"), "{err}");
    db.e.assert_nothing_left(&branches);
    let record = db.e.record().unwrap();
    assert_eq!(record.mode, pando::setup::CheckMode::Namespaced);
    assert_eq!(db.e.setup_state(), SetupState::Ready);
    let doctor = db.e.pando(&["doctor", "--json"]);
    assert!(
        !String::from_utf8_lossy(&doctor.stdout).contains("shop__pando_check"),
        "nothing of the check is left to report"
    );
}

// Where the check cannot be that start, it runs shared as it always has,
// and says what that left untested and why — or says nothing when there
// was nothing to prove.
#[test]
fn a_check_that_cannot_namespace_runs_shared_and_says_the_schema_step_was_untested() {
    if skip_without_python() {
        return;
    }
    let note = |v: &serde_json::Value| -> String {
        v["notes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    // No login: namespaced mode would ask for one, and the check asks
    // nothing.
    let db = with_db(false, SCHEMA_HOOK);
    let out = db.e.pando(&["check", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["mode"], "shared");
    let notes = note(&v);
    assert!(
        notes.contains(
            "skipped the hooks that run after services (schema): on the shared services they \
             would run against your own data, so the schema step was not tested — namespaced \
             mode has no login for mariadb yet"
        ),
        "{notes}"
    );
    assert!(!db.marker.exists(), "the schema step ran on shared data");
    assert_eq!(db.fake("created"), "", "nothing was made");
    assert_eq!(db.e.record().unwrap().mode, pando::setup::CheckMode::Shared);

    // A login, and nothing after the services to prove: shared, and
    // nothing to say.
    let db = with_db(true, "");
    let out = db.e.pando(&["check", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["mode"], "shared");
    assert!(!note(&v).contains("not tested"), "{v}");
    assert_eq!(db.fake("argv"), "", "the server was asked nothing");

    // No server pando can make a database in.
    let e = env(&config_running(
        &answering(200),
        "[[hooks]]\nname = \"migrate\"\nafter = \"services\"\ncmd = \"true\"\n",
    ));
    let out = e.pando(&["check", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["mode"], "shared");
    assert!(
        note(&v).contains(
            "so the schema step was not tested — this project has no server pando can make a \
             database in"
        ),
        "{v}"
    );
}

// A schema step that fails in the check's own database is the settings'
// to fix, named with its log's last lines, as a process is — and what the
// check made is dropped all the same.
#[test]
fn a_schema_step_that_fails_in_a_namespaced_check_fails_it_as_settings() {
    if skip_without_python() {
        return;
    }
    let db = with_db(
        true,
        "[[hooks]]\nname = \"schema\"\nafter = \"services\"\n\
         cmd = '''echo \"ERROR 1071: key too long in $DATABASE_NAME\"; exit 1'''\n",
    );
    let branches = db.e.git(&["branch", "--list"]);
    let out = db.e.pando(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["result"], "failed");
    assert_eq!(v["mode"], "namespaced");
    assert_eq!(v["kind"], "settings");
    assert_eq!(v["failed_process"], "schema");
    assert!(
        v["reason"]
            .as_str()
            .unwrap()
            .contains("the schema hook failed"),
        "{v}"
    );
    assert!(
        v["failed_tail"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l == "ERROR 1071: key too long in shop__pando_check"),
        "{v}"
    );
    assert!(
        stderr(&out).contains("the last lines of the schema log:"),
        "{}",
        stderr(&out)
    );
    assert_eq!(db.fake("dropped"), "shop__pando_check\n");
    assert!(db.databases().is_empty());
    db.e.assert_nothing_left(&branches);
}

// A login the server will not let make a database is the machine's: the
// check says the grant that fixes it, and nothing was made.
#[test]
fn a_login_with_no_grant_fails_a_namespaced_check_as_the_machines() {
    if skip_without_python() {
        return;
    }
    let db = with_db(true, SCHEMA_HOOK);
    std::fs::write(db.fake.join("deny"), "").unwrap();
    let branches = db.e.git(&["branch", "--list"]);
    let out = db.e.pando(&["check", "--json"]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!(v["mode"], "namespaced");
    assert_eq!(v["kind"], "machine");
    let reason = v["reason"].as_str().unwrap();
    assert!(reason.contains("GRANT"), "{reason}");
    assert!(!reason.contains("check-secret-pw"), "{reason}");
    assert_eq!(db.fake("created"), "");
    assert_eq!(
        db.fake("dropped"),
        "",
        "nothing was made, so nothing is dropped"
    );
    assert!(!db.marker.exists());
    db.e.assert_nothing_left(&branches);
}

// A namespaced check killed mid-run leaves its database recorded as the
// check's: `doctor` names it, and the next check drops it before it makes
// its own.
#[test]
fn a_killed_namespaced_checks_database_is_reported_and_dropped_by_the_next_check() {
    if skip_without_python() {
        return;
    }
    let db = with_db_running(&common::listener_on_port_env(), true, SCHEMA_HOOK);
    let branches = db.e.git(&["branch", "--list"]);
    let mut child = db.e.spawn(&["check"]);
    db.e.wait_for_step("asking dev for its first page");
    signal(&child, nix::sys::signal::Signal::SIGKILL);
    child.wait().unwrap();
    assert_eq!(db.databases(), vec!["shop__pando_check".to_string()]);

    let doctor = db.e.pando(&["doctor", "--json"]);
    let report = json(&doctor);
    assert!(
        report["findings"].as_array().unwrap().iter().any(|f| {
            f["message"]
                .as_str()
                .unwrap()
                .contains("and the database shop__pando_check pando made for it")
                && f["fix"].as_str().unwrap().contains("drops what it made")
        }),
        "{report}"
    );

    db.e.write_config(
        &std::fs::read_to_string(db.e.paths.config_file())
            .unwrap()
            .replace(&common::listener_on_port_env(), &answering(200)),
    );
    let out = db.e.pando(&["check"]);
    let err = stderr(&out);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains(
            "left its worktree and its database shop__pando_check behind — sweeping them first"
        ),
        "{err}"
    );
    assert_eq!(
        db.fake("dropped"),
        "shop__pando_check\nshop__pando_check\n",
        "the leftover by the sweep, then this check's own"
    );
    assert!(db.databases().is_empty());
    db.e.assert_nothing_left(&branches);
}
