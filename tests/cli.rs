//! End-to-end behaviour of the `pando` binary: exit codes, and what it
//! prints. Everything runs against a generated fixture repository with an
//! injected `PANDO_HOME`, so no test can reach a real repo or the real home.

use crate::common;

use std::path::Path;
use std::process::{Command, Output};

use common::{Kind, build, build_with_origin, git, status_porcelain};
use tempfile::TempDir;

const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_NEEDS_ANSWER: i32 = 3;

struct Env {
    _dir: TempDir,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
}

fn env() -> Env {
    env_of(Kind::Plain)
}

fn env_of(kind: Kind) -> Env {
    env_built(kind, true)
}

/// The same, cloned rather than worked in: no local `.env`, which is what
/// a fresh clone of a repository really looks like.
fn env_fresh_clone(kind: Kind) -> Env {
    env_built(kind, false)
}

fn env_built(kind: Kind, local_files: bool) -> Env {
    let dir = TempDir::new().unwrap();
    let root = match local_files {
        true => build(kind, dir.path()).root,
        false => common::build_fresh_clone(kind, dir.path()).root,
    };
    let env = Env {
        home: dir.path().join("pando-home"),
        root,
        _dir: dir,
    };
    // The one question that is about this machine rather than the fixture.
    // Several fixtures pin a runtime — `.nvmrc` 22, `.python-version` 3.12
    // — and the commands these tests actually run are `sleep` and a python
    // listener, so nothing has to be initialised in front of them. The
    // answer goes where that kind of answer lives: the user layer.
    //
    // The pnpm fixtures do run their install and scripts, so they get a
    // stand-in pnpm and node in the test's own `bin`, put first on PATH by
    // the prelude the way a developer's `nvm use` would. Before, they ran
    // the real pnpm, and passed only on a machine with nvm and Node 22.
    match matches!(
        kind,
        Kind::NextPnpmCompose | Kind::NextMessy | Kind::MonoWebApi
    ) {
        true => {
            common::fake_pnpm(&env.home);
            common::fake_node(&env.home, "22.11.0");
            env.write_user_config(&format!(
                "[runtime]\nprelude = 'export PATH=\"{}:$PATH\"'\n",
                env.home.join("bin").display()
            ));
        }
        false => env.write_user_config("[runtime]\nprelude = \"\"\n"),
    }
    env
}

/// Every process a test starts is stopped when its environment goes, even
/// when an assertion panicked first. `Drop` on the struct runs before its
/// fields, so the fixture is still on disk when this runs.
impl Drop for Env {
    fn drop(&mut self) {
        // The root too, not only the home: one test moves the repository
        // to somewhere else, and spawning a command in a directory that is
        // not there panics inside a destructor.
        if self.home.exists() && self.root.exists() {
            let _ = self.pando(&["stop"]);
        }
    }
}

impl Env {
    fn pando(&self, args: &[&str]) -> Output {
        self.pando_in(&self.root, args)
    }

    /// Writes pando's own config for this project — the layer detection
    /// would write, and the only one pando ever writes to.
    fn write_config(&self, toml: &str) {
        let path = self.config_file();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, toml).unwrap();
    }

    fn config_file(&self) -> std::path::PathBuf {
        self.project_dir().join("pando.toml")
    }

    /// The machine-wide layer, under every project's own config.
    ///
    /// The home is created 0700 here, which is what
    /// `PandoPaths::ensure_home` does: a harness that left it at whatever
    /// the umask says would be testing against a home pando would never
    /// have made.
    fn write_user_config(&self, toml: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(&self.home).unwrap();
        std::fs::set_permissions(&self.home, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(self.home.join("config.toml"), toml).unwrap();
    }

    /// Takes the machine answer away again, for the tests that are about
    /// pando noticing the machine does not resolve what the project pins.
    fn unanswer_the_runtime(&self) {
        std::fs::remove_file(self.home.join("config.toml")).unwrap();
    }

    fn log_file(&self, name: &str, source: &str) -> std::path::PathBuf {
        self.project_dir()
            .join("logs")
            .join(name)
            .join(format!("{source}.log"))
    }

    fn project_dir(&self) -> std::path::PathBuf {
        let project = pando::project::ProjectRef::from_root(&self.root).unwrap();
        self.home.join("projects").join(&project.id)
    }

    /// Runs pando with something on its stdin, for `--answers -`.
    fn pando_stdin(&self, args: &[&str], input: &str) -> Output {
        use std::io::Write;
        let mut child = Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &self.home)
            .current_dir(&self.root)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run pando");
        child
            .stdin
            .take()
            .expect("a piped stdin")
            .write_all(input.as_bytes())
            .expect("write the answers");
        child.wait_with_output().expect("wait for pando")
    }

    fn pando_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &self.home)
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("run pando")
    }

    /// Runs pando with a `git` in front of the real one that logs each
    /// call's arguments, and returns the calls it saw, one line apiece.
    fn pando_counting_git(&self, args: &[&str]) -> (Output, Vec<String>) {
        use std::os::unix::fs::PermissionsExt;
        let found = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .expect("look for git");
        let real = String::from_utf8_lossy(&found.stdout).trim().to_string();
        let dir = self.home.parent().unwrap();
        let bin = dir.join("counting-git");
        let log = dir.join("git-calls.log");
        std::fs::create_dir_all(&bin).unwrap();
        let _ = std::fs::remove_file(&log);
        let shim = bin.join("git");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{real}' \"$@\"\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out = Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &self.home)
            .env("PATH", path)
            .current_dir(&self.root)
            .args(args)
            .output()
            .expect("run pando");
        let calls = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        (out, calls)
    }
}

/// The calls among `calls` whose arguments include `verb` as a word of
/// its own, such as `status` in `-C <dir> --no-optional-locks status`.
fn git_calls_to<'a>(calls: &'a [String], verb: &str) -> Vec<&'a String> {
    calls
        .iter()
        .filter(|call| call.split_whitespace().any(|arg| arg == verb))
        .collect()
}

/// A fixture with compose services, a fake docker in its home, and a dev
/// process that needs no framework — everything `--isolated` needs.
fn env_isolated() -> Env {
    let e = env_of(Kind::NextPnpmCompose);
    common::docker::install(&e.home);
    e.write_config(&format!(
        "[project]\nprovision = [\".env\"]\ninstall = \"true\"\n\n\
         [dev]\ncmd = '''{}'''\nports = {{ PORT = \"web\" }}\n\n\
         [[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\n\
         include = [\"postgres\", \"redis\"]\n\
         env = {{ DATABASE_URL = \"postgres\", REDIS_URL = \"redis\" }}\n\n\
         [[hooks]]\nname = \"migrate\"\nafter = \"services\"\ncmd = \"true\"\n",
        common::listener_on_port_env()
    ));
    e
}

fn code(out: &Output) -> i32 {
    out.status.code().expect("pando exited via a signal")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn ls_on_a_fresh_fixture_succeeds_with_exit_zero() {
    let e = env();
    let out = e.pando(&["ls"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("no worktrees"), "{}", stdout(&out));
}

#[test]
fn the_full_new_path_rm_cycle_works_from_the_cli() {
    let e = env();

    let out = e.pando(&["new", "feat/one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("feat+one"), "{}", stdout(&out));
    let created = stdout(&out)
        .trim()
        .rsplit_once(" at ")
        .map(|(_, path)| path.to_string())
        .expect("new prints where it created the worktree");

    let out = e.pando(&["ls"]);
    assert_eq!(code(&out), EXIT_OK);
    assert!(stdout(&out).contains("feat/one"));

    let out = e.pando(&["path", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK);
    let printed = stdout(&out).trim().to_string();
    assert_eq!(
        created, printed,
        "new must print the canonical path it recorded, the one `path` gives"
    );
    assert!(Path::new(&printed).is_dir(), "path printed {printed:?}");
    // Canonical on both sides: git prints /private/var where the shell and
    // TempDir say /var.
    let canonical_home = std::fs::canonicalize(&e.home).unwrap();
    assert!(
        Path::new(&printed).starts_with(&canonical_home),
        "{printed} is not under {}",
        canonical_home.display()
    );

    let out = e.pando(&["rm", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(!Path::new(&printed).exists());
    assert_eq!(status_porcelain(&e.root), "", "the fixture must stay clean");
}

#[test]
fn ls_json_parses_and_carries_the_documented_keys() {
    let e = env();
    e.pando(&["new", "feat/one"]);
    let out = e.pando(&["ls", "--json"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("valid json");
    // Pinned as a literal: a version bump must fail here, at the commit
    // that makes it, rather than passing quietly.
    assert_eq!(v["version"], 2);
    assert!(v["project"]["id"].as_str().is_some());
    let w = &v["worktrees"][0];
    for key in [
        "name",
        "path",
        "branch",
        "head",
        "detached",
        "dirty",
        "ahead",
        "behind",
        "created_by_pando",
        "prunable",
        "locked",
        "pr",
    ] {
        assert!(w.get(key).is_some(), "missing key {key:?} in {w}");
    }
}

// Every command lists the worktrees twice before it runs: once to find
// the repository and once to guard the write locations. `ls` needs one
// listing of its own, and enriches from it rather than asking again.
#[test]
fn ls_lists_the_worktrees_once_beyond_what_every_command_does() {
    let e = env();
    for branch in ["feat/one", "feat/two"] {
        let out = e.pando(&["new", branch]);
        assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    }
    for args in [&["ls"][..], &["ls", "--json"]] {
        let (out, calls) = e.pando_counting_git(args);
        assert_eq!(code(&out), EXIT_OK, "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).contains("feat"), "{args:?}: {}", stdout(&out));
        let listings: Vec<&String> = calls
            .iter()
            .filter(|call| call.contains("worktree list --porcelain"))
            .collect();
        assert_eq!(listings.len(), 3, "{args:?}: {listings:#?}");
        // One per worktree, and one for the main checkout's row.
        assert_eq!(git_calls_to(&calls, "status").len(), 3, "{args:?}");
    }
}

// A verb that takes a name finds the worktree and names it for its
// messages from one listing: resolving, naming and `path`'s own lookup
// each listed the worktrees again, for answers the first one had.
#[test]
fn a_verb_that_takes_a_name_lists_the_worktrees_once_to_find_it() {
    let e = env();
    e.write_config(SLEEPER);
    let out = e.pando(&["new", "feat/one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    // `open` of a worktree that is not running is refused, after the name
    // was found: the refusal names it as a person knows it.
    for (args, beyond) in [(&["path", "feat/one"][..], 1), (&["open", "feat/one"], 1)] {
        let (out, calls) = e.pando_counting_git(args);
        match args[0] {
            "path" => assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out)),
            _ => assert!(
                stderr(&out).contains("feat/one is not running"),
                "{}",
                stderr(&out)
            ),
        }
        let listings = calls
            .iter()
            .filter(|call| call.contains("worktree list --porcelain"))
            .count();
        assert_eq!(listings, 2 + beyond, "{args:?}: {calls:#?}");
    }
}

// `status` prints no git field in either shape, so it pays for none: no
// `git status` in any worktree, and none of the repository-wide reads
// that fill `ls`'s commit and ahead/behind columns.
#[test]
fn status_reads_the_listing_and_asks_git_for_nothing_it_does_not_print() {
    let e = env();
    for branch in ["feat/one", "feat/two"] {
        let out = e.pando(&["new", branch]);
        assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    }
    for args in [
        &["status"][..],
        &["status", "--json"],
        &["status", "feat/one"],
        &["status", "feat/one", "--json"],
    ] {
        let (out, calls) = e.pando_counting_git(args);
        assert_eq!(code(&out), EXIT_OK, "{args:?}: {}", stderr(&out));
        assert!(stdout(&out).contains("feat"), "{args:?}: {}", stdout(&out));
        assert!(!calls.is_empty(), "{args:?}: the stand-in git saw nothing");
        for verb in ["status", "log", "for-each-ref", "rev-list"] {
            let asked = git_calls_to(&calls, verb);
            assert!(asked.is_empty(), "{args:?} ran git {verb}: {asked:?}");
        }
    }
}

// Every published shape says which shape it is, and `agent/json.md` is the
// contract that promises so. A program pins `version` and refuses one it
// was not written for; a shape that does not carry it cannot be pinned at
// all.
#[test]
fn every_machine_readable_shape_carries_its_version() {
    let e = env();
    e.pando(&["new", "feat/one"]);
    // A log with a line in it, without starting anything: `logs --json` is
    // about the shape here, not about a process.
    let log = e.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "ready in 412ms\n").unwrap();

    for args in [
        vec!["ls", "--json"],
        vec!["status", "--json"],
        vec!["signals"],
        // Whose exit code is a verdict about the machine, not about the
        // shape: it prints the same object either way.
        vec!["doctor", "--json"],
    ] {
        let out = e.pando(&args);
        let printed = stdout(&out);
        let v: serde_json::Value = serde_json::from_str(&printed)
            .unwrap_or_else(|e| panic!("{args:?} did not print one JSON object: {e}\n{printed}"));
        assert_eq!(v["version"], pando::cli::JSON_VERSION, "{args:?}");
    }

    // And the one that is a stream rather than a document. It carries the
    // version on every line because a follower reading a pipe has no other
    // object to learn the shape from.
    let out = e.pando(&["logs", "feat+one", "--json"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let printed = stdout(&out);
    let line: serde_json::Value =
        serde_json::from_str(printed.lines().next().expect("one line")).expect("one object");
    assert_eq!(line["version"], pando::cli::JSON_VERSION);
    assert_eq!(line["level"], "info");
    assert_eq!(line["ts"], serde_json::Value::Null);
    assert_eq!(line["line"], "ready in 412ms");
}

// Discovery is from porcelain, whose first entry is the main checkout from
// any cwd — so a subdirectory sees exactly what the root sees.
#[test]
fn commands_work_from_a_subdirectory_of_the_repository() {
    let e = env();
    e.pando(&["new", "feat/one"]);
    let nested = e.root.join("apps").join("web");
    std::fs::create_dir_all(&nested).unwrap();

    let from_root = stdout(&e.pando(&["ls", "--json"]));
    let from_sub = stdout(&e.pando_in(&nested, &["ls", "--json"]));
    assert_eq!(from_root, from_sub);
}

#[test]
fn running_outside_a_git_repository_fails_with_exit_one_and_one_line() {
    let e = env();
    let outside = e.root.parent().unwrap().join("not-a-repo");
    std::fs::create_dir_all(&outside).unwrap();

    let out = e.pando_in(&outside, &["ls"]);
    assert_eq!(code(&out), EXIT_ERROR);
    let err = stderr(&out);
    assert_eq!(err.trim().lines().count(), 1, "expected one line: {err}");
    assert!(err.contains("not inside a git repository"), "{err}");
}

#[test]
fn a_bare_repository_fails_the_same_way() {
    let e = env();
    let bare = e.root.parent().unwrap().join("bare.git");
    git(
        e.root.parent().unwrap(),
        &[
            "init",
            "--bare",
            "--quiet",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );

    let out = e.pando_in(&bare, &["ls"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(
        stderr(&out).contains("bare repositories are not supported"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    let e = env();
    assert_eq!(code(&e.pando(&["definitely-not-a-command"])), EXIT_USAGE);
    assert_eq!(code(&e.pando(&["rm"])), EXIT_USAGE, "missing argument");
}

// Bare `pando` from a script or an agent: the TUI panicked with 101 when
// there was no terminal to take, and drew into the pipe waiting for keys
// when there was one.
#[test]
fn bare_pando_with_no_terminal_is_a_usage_error_that_names_the_commands() {
    let e = env();
    let out = e.pando(&[]);
    assert_eq!(code(&out), EXIT_USAGE, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "", "nothing is drawn into the pipe");
    assert!(stderr(&out).contains("`pando --help`"), "{}", stderr(&out));
}

#[test]
fn help_and_version_work_outside_a_repository() {
    let e = env();
    let outside = e.root.parent().unwrap().join("nowhere");
    std::fs::create_dir_all(&outside).unwrap();
    assert_eq!(code(&e.pando_in(&outside, &["--help"])), EXIT_OK);
    assert_eq!(code(&e.pando_in(&outside, &["--version"])), EXIT_OK);
}

#[test]
fn removing_a_worktree_pando_did_not_create_needs_yes() {
    let e = env();
    let adopted = e.root.parent().unwrap().join("adopted");
    git(
        &e.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "adopted",
            adopted.to_str().unwrap(),
        ],
    );

    let out = e.pando(&["rm", "adopted"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(stderr(&out).contains("--yes"), "{}", stderr(&out));
    assert!(adopted.exists());

    let out = e.pando(&["rm", "adopted", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(!adopted.exists());
}

// A fixture with a real origin is what makes the tracking rules testable:
// a forked branch must not end up with the base as its upstream.
#[test]
fn a_new_branch_created_through_the_cli_does_not_track_its_base() {
    let dir = TempDir::new().unwrap();
    let fixture = build_with_origin(Kind::Plain, dir.path());
    let home = dir.path().join("pando-home");
    assert!(fixture.remote.is_some());

    let out = Command::new(env!("CARGO_BIN_EXE_pando"))
        .env("PANDO_HOME", &home)
        .current_dir(&fixture.root)
        .args(["new", "feat/one"])
        .output()
        .expect("run pando");
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let upstream = common::git_raw(
        &fixture.root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "feat/one@{upstream}",
        ],
    );
    assert!(
        !upstream.status.success(),
        "a forked branch must have no upstream, got {:?}",
        String::from_utf8_lossy(&upstream.stdout)
    );
    assert_eq!(status_porcelain(&fixture.root), "");
}

#[test]
fn every_fixture_kind_builds_a_clean_repository() {
    let dir = TempDir::new().unwrap();
    for kind in Kind::ALL {
        let fixture = build(kind, &dir.path().join(kind.dir_name()));
        assert!(
            fixture.remote.is_none(),
            "{kind:?} was built without an origin"
        );
        assert!(
            fixture.root.join(".gitignore").is_file(),
            "{:?} has no .gitignore",
            kind
        );
        assert_eq!(
            status_porcelain(&fixture.root),
            "",
            "{:?} must be clean after building — its ignored files are ignored",
            kind
        );
    }
}

// `PANDO_HOME` decides where state, caches, logs, and every worktree pando
// creates live. A home inside the repository puts all of it in the working
// tree, which Invariant 1 forbids — so it is refused before anything is
// written, in the absolute and the relative form alike.
#[test]
fn a_pando_home_inside_the_repository_is_refused() {
    let e = env();
    let absolute = e.root.join(".pando-home");
    for home in [absolute.to_str().unwrap(), ".pando"] {
        for args in [["new", "feat/inside"], ["ls", "--json"]] {
            let out = Command::new(env!("CARGO_BIN_EXE_pando"))
                .env("PANDO_HOME", home)
                .current_dir(&e.root)
                .args(args)
                .output()
                .expect("run pando");
            assert_eq!(
                code(&out),
                EXIT_ERROR,
                "PANDO_HOME={home} {args:?} should be refused; stdout: {}",
                stdout(&out)
            );
            assert!(
                stderr(&out).contains("inside the repository"),
                "PANDO_HOME={home}: {}",
                stderr(&out)
            );
        }
    }
    assert!(!absolute.exists(), "the refused home must not be created");
    assert!(!e.root.join(".pando").exists());
    assert_eq!(status_porcelain(&e.root), "", "the fixture must stay clean");
}

// `adopted` in the listing and `rm`'s confirmation rule read the same state
// file. When it cannot be read, the listing has to say so rather than call
// every worktree adopted with exit 0 while `rm` fails hard on the same file.
// A config that configures a native service. It warned for as long as
// nothing could run it, because saying nothing left the developer reading
// "no services configured" over a file that configures one. Something can
// run it now, so the warning would be the false statement — and a command
// that prints one on every run trains people to ignore stderr.
#[test]
fn a_native_service_block_is_run_by_this_build_and_is_not_warned_about() {
    let e = env();
    e.write_config(
        "[project]\nprovision = [\".env\"]\n\n\
         [[services]]\nkind = \"native\"\nname = \"postgres\"\npreset = \"postgres\"\n",
    );
    let out = e.pando(&["ls"]);
    assert_eq!(code(&out), EXIT_OK, "a legal block is not an error");
    assert!(
        !stderr(&out).contains("no runner"),
        "the block is run by this build now: {}",
        stderr(&out)
    );

    let out = e.pando(&["ls", "--json"]);
    assert_eq!(code(&out), EXIT_OK);
    serde_json::from_str::<serde_json::Value>(&stdout(&out))
        .expect("anything printed must go to stderr, leaving stdout parseable");

    // And `doctor` is where it is explained: which recipe, from where,
    // and where its data would go.
    let out = e.pando(&["doctor"]);
    let said = stdout(&out);
    assert!(said.contains("postgres"), "{said}");
    assert!(said.contains("built-in"), "{said}");
    assert!(said.contains("data/<worktree>/postgres"), "{said}");
}

#[test]
fn ls_warns_when_the_state_file_cannot_be_used() {
    let e = env();
    e.pando(&["new", "feat/one"]);
    let listed: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["ls", "--json"]))).expect("valid json");
    let id = listed["project"]["id"].as_str().expect("a project id");
    let state = e.home.join("projects").join(id).join("state.json");
    std::fs::write(&state, r#"{"version":3,"worktrees":{}}"#).unwrap();

    let out = e.pando(&["ls"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("adopted"), "{}", stdout(&out));
    assert!(stderr(&out).contains("version 3"), "{}", stderr(&out));

    let out = e.pando(&["ls", "--json"]);
    assert_eq!(code(&out), EXIT_OK);
    assert!(stderr(&out).contains("version 3"), "{}", stderr(&out));
    serde_json::from_str::<serde_json::Value>(&stdout(&out))
        .expect("the warning must go to stderr, leaving stdout parseable");

    // The same one line `rm` refuses with, so the two never disagree.
    let out = e.pando(&["rm", "feat+one", "--yes"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(stderr(&out).contains("version 3"), "{}", stderr(&out));

    // `open` said "not running" about a worktree it could not read.
    let out = e.pando(&["open", "feat/one"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(stderr(&out).contains("version 3"), "{}", stderr(&out));
    assert!(!stderr(&out).contains("not running"), "{}", stderr(&out));
}

// A refresh that forgets a service which exited saves it as gone, so the
// command that ran it is the only one that can say so. `ls --json` and
// `open` said nothing, and the fact was lost.
#[test]
fn ls_json_and_open_say_what_their_refresh_forgot() {
    let e = env();
    e.pando(&["new", "feat/one"]);
    let state = e.project_dir().join("state.json");
    let mut exited = Command::new("true").spawn().unwrap();
    exited.wait().unwrap();
    let with_a_dead_service = || {
        let mut store: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
        store["worktrees"]["feat+one"]["services"] = serde_json::json!([{
            "name": "mariadb",
            "kind": "native",
            "port": 17_004,
            "pid": exited.id(),
        }]);
        std::fs::write(&state, store.to_string()).unwrap();
    };

    with_a_dead_service();
    let out = e.pando(&["ls", "--json"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("its mariadb exited"),
        "{}",
        stderr(&out)
    );
    serde_json::from_str::<serde_json::Value>(&stdout(&out)).expect("stdout stays parseable");

    with_a_dead_service();
    let out = e.pando(&["open", "feat/one"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(
        stderr(&out).contains("its mariadb exited"),
        "{}",
        stderr(&out)
    );
}

// The committed file is the one a teammate can change under you, so the
// worst it may do is drop out with a warning. Every read-only command has
// to keep working.
#[test]
fn a_committed_pando_toml_pando_cannot_use_does_not_stop_ls() {
    let e = env();
    for bad in [
        "[dev]\ncmd = \"x\"\n\n[processes.api]\ncmd = \"y\"\n",
        "[project]\nprovision = [\"../shared/.env\"]\n",
        "[project]\nbase = \"main\"\nnope = 1\n",
        "[project\nbase =\n",
    ] {
        std::fs::write(e.root.join("pando.toml"), bad).unwrap();
        let out = e.pando(&["ls"]);
        assert_eq!(code(&out), EXIT_OK, "{bad:?} stderr: {}", stderr(&out));
        assert!(
            stderr(&out).contains("ignoring"),
            "{bad:?} should warn: {}",
            stderr(&out)
        );
        assert!(stdout(&out).contains("no worktrees"), "{}", stdout(&out));
    }

    // pando's own file is a different matter: a command that would act on
    // it fails hard, and says which key.
    std::fs::remove_file(e.root.join("pando.toml")).unwrap();
    e.write_config("[project]\nnope = 1\n");
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));
}

// You need `stop` most when the config is broken, and `ls` to see what is
// there at all. Only the commands that would act on the config refuse.
#[test]
fn a_home_pando_toml_pando_cannot_use_still_lets_you_look_and_stop() {
    for (what, bad) in [
        ("a syntax error", "[dev\ncmd = \"x\"\n"),
        ("a type error", "[dev]\ncmd = 3\n"),
        (
            "a validation error",
            "[project]\nprovision = [\"../escape\"]\n",
        ),
    ] {
        let e = env_of(Kind::NextPnpmCompose);
        e.write_config(bad);

        let main_name = Kind::NextPnpmCompose.dir_name();
        for args in [
            vec!["ls"],
            vec!["status"],
            vec!["stop"],
            vec!["path", main_name],
        ] {
            let out = e.pando(&args);
            assert_eq!(
                code(&out),
                EXIT_OK,
                "{what}: {args:?} should still run: {}",
                stderr(&out)
            );
            assert!(
                stderr(&out).contains("pando.toml"),
                "{what}: {args:?} should say what it ignored: {}",
                stderr(&out)
            );
        }
        // `rm` gets as far as its own refusal rather than the config's.
        let out = e.pando(&["rm", "feat+nope"]);
        assert_eq!(code(&out), EXIT_ERROR);
        assert!(
            stderr(&out).contains("no worktree named"),
            "{what}: {}",
            stderr(&out)
        );

        for args in [
            vec!["start", "feat+one"],
            vec!["restart", "feat+one"],
            vec!["new", "feat/one"],
        ] {
            let out = e.pando(&args);
            assert_eq!(
                code(&out),
                EXIT_ERROR,
                "{what}: {args:?} must refuse: {}",
                stdout(&out)
            );
            assert!(
                stderr(&out).contains("pando.toml"),
                "{what}: {args:?} must say the config is why: {}",
                stderr(&out)
            );
        }
    }
}

// The review's repro: detection used to append `[dev]` beside a configured
// process, writing a file pando's own loader refuses — after which every
// command failed, `stop` included.
#[test]
fn a_configured_process_is_never_given_a_dev_table_beside_it() {
    let e = env_of(Kind::NextPnpmCompose);
    e.write_config("[project]\ninstall = \"true\"\n\n[processes.web]\ncmd = \"sleep 300\"\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let before = std::fs::read_to_string(e.config_file()).unwrap();

    let out = e.pando(&["start", "feat+one", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let after = std::fs::read_to_string(e.config_file()).unwrap();
    // Detection may still fill slots this file says nothing about — the
    // services and the schema step — but never a process, and never by
    // rewriting what was already there.
    assert!(
        after.starts_with(&before),
        "a project that declares its processes is not detected at\n\
         before:\n{before}\nafter:\n{after}"
    );
    assert!(!after.contains("[dev]"), "{after}");
    assert!(after.contains("[processes.web]"), "{after}");

    for args in [vec!["ls"], vec!["status"], vec!["stop"]] {
        let out = e.pando(&args);
        assert_eq!(
            code(&out),
            EXIT_OK,
            "{args:?} after the start: {}",
            stderr(&out)
        );
    }
}

// The shape a developer writes when they want pando to fill the command in:
// every other command still works, and only `start` has something to say.
#[test]
fn a_dev_table_with_no_command_is_refused_only_by_start() {
    // A repository with nothing to serve, so detection has no command to
    // fill in and `cmd` really is missing when `start` asks for it.
    let e = env_of(Kind::RustLib);
    // The install step is only here so there is a log to read below.
    e.write_config("[project]\ninstall = \"true\"\n\n[dev]\ncwd = \".\"\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    for args in [
        vec!["ls"],
        vec!["status"],
        vec!["stop"],
        vec!["path", "feat+one"],
        vec!["logs", "feat+one", "--source", "install"],
    ] {
        let out = e.pando(&args);
        assert_eq!(code(&out), EXIT_OK, "{args:?}: {}", stderr(&out));
    }

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("cmd"),
        "it says what is missing: {}",
        stderr(&out)
    );
}

// The other half of the same shape: a `[dev]` a developer left half
// written is an invitation, not an error. Detection fills the command and
// the ports, and keeps every key they did write.
#[test]
fn a_dev_table_with_no_command_is_filled_in_by_detection() {
    let e = env_of(Kind::NextPnpmCompose);
    e.write_config("[dev]\ncwd = \".\"\nenv = { GREETING = \"hello\" }\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let text = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        text.contains("cmd = \"pnpm dev\""),
        "the command is filled in: {text}"
    );
    assert!(
        text.contains("# detected:"),
        "and says where it came from: {text}"
    );
    assert!(
        text.contains("ports = { PORT = \"web\" }"),
        "and so is the port, which was unset: {text}"
    );
    assert!(
        text.contains("cwd = \".\"") && text.contains("GREETING"),
        "what the developer wrote is untouched: {text}"
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
}

// A command run from a directory that no longer exists fails before git is
// ever consulted, and a bare errno names nothing at all.
#[test]
fn running_from_a_deleted_directory_says_which_directory() {
    let e = env();
    let gone = e.root.parent().unwrap().join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    // Only a child can delete its own cwd out from under itself, so the
    // shell sets that up and then becomes pando.
    let script = format!(
        "cd '{dir}' && rmdir '{dir}' && exec '{bin}' ls",
        dir = gone.display(),
        bin = env!("CARGO_BIN_EXE_pando"),
    );
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .env("PANDO_HOME", &e.home)
        .output()
        .expect("run sh");

    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains("current directory"), "{err}");
    assert_eq!(err.trim().lines().count(), 1, "expected one line: {err}");
}

// ---- start, stop, status, logs -------------------------------------------

/// A dev "server": prints a line and stays up. No real server is needed to
/// prove the lifecycle, and `sleep` is available everywhere.
const SLEEPER: &str = "[dev]\ncmd = \"echo started-ok && sleep 30\"\nports = { PORT = \"web\" }\n";

/// Polls until `check` passes, or gives up.
///
/// A spawned process writes when it gets round to it, which is not when
/// `start` returns — and a busy CI runner gets round to it a lot later
/// than an idle laptop does. The budget is therefore generous rather than
/// tight, and costs nothing when the machine is quick: the loop ends on
/// the first pass either way.
fn poll_until(mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// Polls a log file until it holds `needle`.
fn log_contains(path: &Path, needle: &str) -> bool {
    poll_until(|| {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains(needle)
    })
}

// Committed < user < project, end to end. The machine-wide file answers
// what nothing else has, and pando's own file for this project beats it.
#[test]
fn the_user_layer_is_read_and_the_project_layer_beats_it() {
    let e = env();
    std::fs::create_dir_all(&e.home).unwrap();
    let user = e.home.join("config.toml");
    std::fs::write(
        &user,
        "[dev]\ncmd = \"echo from-the-user-layer && sleep 30\"\nports = []\n",
    )
    .unwrap();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        log_contains(&e.log_file("feat+one", "dev"), "from-the-user-layer"),
        "the user layer's dev command is the one that ran: {}",
        std::fs::read_to_string(e.log_file("feat+one", "dev")).unwrap_or_default()
    );

    // And the project layer, which is the one pando writes, wins over it.
    e.write_config("[dev]\ncmd = \"echo from-the-project-layer && sleep 30\"\nports = []\n");
    let out = e.pando(&["restart", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        log_contains(&e.log_file("feat+one", "dev"), "from-the-project-layer"),
        "{}",
        std::fs::read_to_string(e.log_file("feat+one", "dev")).unwrap_or_default()
    );

    // A machine-wide file pando cannot use is dropped with a warning, not
    // a failure: it applies to every project on the laptop.
    std::fs::write(&user, "[project\nbase =\n").unwrap();
    let out = e.pando(&["ls"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("ignoring") && stderr(&out).contains("config.toml"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn start_status_logs_and_stop_work_from_the_cli() {
    let e = env();
    e.write_config(SLEEPER);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("started feat+one"),
        "{}",
        stdout(&out)
    );
    assert!(
        stdout(&out).contains("http://localhost:"),
        "start prints the URL: {}",
        stdout(&out)
    );

    // Status, as a machine sees it.
    let out = e.pando(&["status", "--json"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).expect("status --json parses");
    // Pinned as a literal: a version bump must fail here, at the commit
    // that makes it, rather than passing quietly.
    assert_eq!(v["version"], 2);
    let wt = &v["worktrees"][0];
    assert_eq!(wt["name"], "feat+one");
    let port = wt["ports"]["web"].as_u64().expect("a web port");
    assert!(
        (17_000..=56_998).contains(&port),
        "port {port} out of range"
    );
    let phase = wt["processes"]["dev"]["phase"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(phase == "running" || phase == "starting", "{phase}");

    // The listing carries them too.
    let out = e.pando(&["ls"]);
    assert!(stdout(&out).contains(&port.to_string()), "{}", stdout(&out));

    // Logs.
    let found = poll_until(|| {
        let out = e.pando(&["logs", "feat+one", "--tail", "5"]);
        assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
        stdout(&out).contains("started-ok")
    });
    assert!(found, "the dev process's output never reached its log");

    let out = e.pando(&["logs", "feat+one", "--json"]);
    let line: serde_json::Value = serde_json::from_str(stdout(&out).lines().next().unwrap())
        .expect("logs --json emits one object per line");
    assert!(line["line"].is_string());
    assert!(line["level"].is_string());

    // Starting twice says so rather than starting twice.
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK);
    assert!(stdout(&out).contains("already running"), "{}", stdout(&out));

    let out = e.pando(&["stop", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("stopped feat+one"),
        "{}",
        stdout(&out)
    );

    let out = e.pando(&["status", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert!(
        v["worktrees"][0]["processes"]
            .as_object()
            .unwrap()
            .is_empty(),
        "a stopped worktree has no processes"
    );
    assert_eq!(
        v["worktrees"][0]["ports"]["web"].as_u64(),
        Some(port),
        "but it still owns its ports"
    );
    assert_eq!(status_porcelain(&e.root), "");
}

/// Two processes, each with its own role, in the long `[processes.<name>]`
/// form the workspace shape uses.
const PAIR: &str = "[processes.web]\ncmd = \"echo web-ok && sleep 30\"\nports = { PORT = \"web\" }\n\
                    \n[processes.api]\ncmd = \"echo api-ok && sleep 30\"\nports = { API_PORT = \"api\" }\n";

fn status_of(e: &Env) -> serde_json::Value {
    serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).expect("status --json parses")
}

#[test]
fn only_starts_and_stops_one_process_of_a_pair() {
    let e = env();
    e.write_config(PAIR);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat+one", "--only", "api"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let v = status_of(&e);
    let wt = &v["worktrees"][0];
    assert_eq!(
        wt["processes"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["api"],
        "only the process it named was started"
    );
    let ports = wt["ports"].clone();
    assert!(
        ports["web"].is_number() && ports["api"].is_number(),
        "every role is reserved, whatever was started: {ports}"
    );

    // The text form: one line for the worktree, one for each process.
    let out = e.pando(&["status"]);
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(lines[1].trim_start().starts_with("api"), "{text}");

    let out = e.pando(&["start", "feat+one", "--only", "web"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let v = status_of(&e);
    let wt = &v["worktrees"][0];
    assert_eq!(
        wt["processes"].as_object().unwrap().len(),
        2,
        "and now both are running"
    );
    assert_eq!(wt["ports"], ports, "starting the second never moved a port");

    // Stopping one leaves the other exactly as it was.
    let out = e.pando(&["stop", "feat+one", "--only", "web"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let v = status_of(&e);
    assert_eq!(
        v["worktrees"][0]["processes"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["api"],
        "the api never stopped"
    );

    // And a restart of the one that is left keeps its port.
    let before = status_of(&e)["worktrees"][0]["processes"]["api"]["pid"].clone();
    let out = e.pando(&["restart", "feat+one", "--only", "api"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let v = status_of(&e);
    assert_ne!(v["worktrees"][0]["processes"]["api"]["pid"], before);
    assert_eq!(v["worktrees"][0]["ports"], ports);

    assert_eq!(code(&e.pando(&["stop", "feat+one"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

// The URL is the web process's port. With web stopped on its own and the
// api still up, `open` opened it on a refused connection and exited 0,
// `ls` and `status` showed it as live, and `start --only api` printed it,
// while `share` refused the same state.
#[test]
fn a_url_whose_own_process_is_stopped_is_not_handed_out_while_a_sibling_runs() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not installed");
        return;
    }
    let e = env();
    let listener = common::listener_on_port_env();
    e.write_config(&format!(
        "[processes.web]\ncmd = '''{listener}'''\nports = {{ PORT = \"web\" }}\n\n\
         [processes.api]\ncmd = '''{listener}'''\nports = {{ PORT = \"api\" }}\n"
    ));
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one", "--wait"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("http://localhost:"),
        "{}",
        stdout(&out)
    );

    let out = e.pando(&["stop", "feat+one", "--only", "web"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let text = stdout(&e.pando(&["status"]));
    assert!(text.contains("running"), "{text}");
    assert!(!text.contains("http://"), "{text}");
    let text = stdout(&e.pando(&["ls"]));
    assert!(!text.contains("http://"), "{text}");

    // A browser that records what it was given, so nothing real opens.
    let opened = e.home.join("opened");
    let browser = e.home.join("browser.sh");
    std::fs::write(
        &browser,
        format!("#!/bin/sh\necho \"$1\" > '{}'\n", opened.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pando"))
        .env("PANDO_HOME", &e.home)
        .env("BROWSER", &browser)
        .current_dir(&e.root)
        .args(["open", "feat+one"])
        .output()
        .unwrap();
    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("is not running web, the process its URL points at")
            && stderr(&out).contains("pando start feat+one --only web"),
        "{}",
        stderr(&out)
    );
    assert!(!opened.exists(), "no browser was opened");

    assert_eq!(code(&e.pando(&["stop", "feat+one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one", "--only", "api"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "started feat+one");
    assert_eq!(code(&e.pando(&["stop", "feat+one"])), EXIT_OK);
}

#[test]
fn an_only_nothing_answers_to_names_the_processes_there_are() {
    let e = env();
    e.write_config(PAIR);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat+one", "--only", "worker"]);
    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("api, web"), "{}", stderr(&out));

    // And `--only` without a worktree to apply it to is a usage error, not
    // a silent stop of everything.
    let out = e.pando(&["stop", "--only", "api"]);
    assert_eq!(code(&out), EXIT_USAGE, "stdout: {}", stdout(&out));
}

#[test]
fn restart_keeps_the_port_and_replaces_the_process() {
    let e = env();
    e.write_config(SLEEPER);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(code(&e.pando(&["start", "feat+one"])), EXIT_OK);

    let first: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).unwrap();
    let port = first["worktrees"][0]["ports"]["web"].as_u64().unwrap();
    let pid = first["worktrees"][0]["processes"]["dev"]["pid"]
        .as_u64()
        .unwrap();

    let out = e.pando(&["restart", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let second: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).unwrap();
    assert_eq!(
        second["worktrees"][0]["ports"]["web"].as_u64(),
        Some(port),
        "a restart keeps the URL"
    );
    assert_ne!(
        second["worktrees"][0]["processes"]["dev"]["pid"].as_u64(),
        Some(pid)
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
}

#[test]
fn a_project_with_no_dev_process_fails_with_exit_one_and_a_hint() {
    let e = env();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert_eq!(
        stderr(&out).trim(),
        format!(
            "pando: nothing to run: this project configures no process — add a dev command to \
             {}:\n[dev]\ncmd = \"…\"   # the command that runs it, e.g. \"cargo run\"",
            e.config_file().display()
        )
    );
    // And `open` does not send it to a `start` that cannot work.
    let out = e.pando(&["open", "feat/one"]);
    assert_eq!(code(&out), EXIT_ERROR);
    let said = stderr(&out);
    assert!(
        said.starts_with("pando: feat/one is not running, and this project has nothing to run"),
        "{said}"
    );
    assert!(!said.contains("pando start"), "{said}");
}

#[test]
fn stopping_nothing_is_not_an_error() {
    let e = env();
    let out = e.pando(&["stop"]);
    assert_eq!(code(&out), EXIT_OK);
    assert!(
        stdout(&out).contains("nothing was running"),
        "{}",
        stdout(&out)
    );

    // A worktree that exists and is not running: still not an error.
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["stop", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK);
    assert!(stdout(&out).contains("was not running"), "{}", stdout(&out));
}

// It used to say "nope was not running" and exit 0, which is true of a
// typo and tells nobody it was one.
#[test]
fn a_name_that_matches_nothing_fails_and_says_what_was_meant() {
    let e = env();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    for verb in [
        "stop", "start", "restart", "logs", "path", "rm", "status", "open",
    ] {
        let out = e.pando(&[verb, "feat+on"]);
        assert_eq!(code(&out), EXIT_ERROR, "{verb}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("no worktree named \"feat+on\"")
                && stderr(&out).contains("did you mean feat/one?"),
            "{verb}: {}",
            stderr(&out)
        );
    }
    let out = e.pando(&["stop", "zzz"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(
        stderr(&out).contains("this project has feat/one"),
        "nothing close: every name, since there are few: {}",
        stderr(&out)
    );
}

// The `+` is how a branch becomes a directory name; a person types the
// branch.
#[test]
fn every_verb_takes_the_branch_name_as_well_as_the_directory_name() {
    let e = env();
    e.write_config(SLEEPER);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let by_dir = e.pando(&["path", "feat+one"]);
    let by_branch = e.pando(&["path", "feat/one"]);
    assert_eq!(code(&by_branch), EXIT_OK, "{}", stderr(&by_branch));
    assert_eq!(stdout(&by_dir), stdout(&by_branch));

    let out = e.pando(&["start", "feat/one"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("started feat+one"),
        "{}",
        stdout(&out)
    );
    let out = e.pando(&["stop", "feat/one"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert!(
        stdout(&out).contains("stopped feat+one"),
        "{}",
        stdout(&out)
    );
}

// Inside a worktree, `stop` means that worktree — and says so, since it
// used to mean every one — while `--all` still means every one.
#[test]
fn stop_with_no_name_inside_a_worktree_stops_only_that_one() {
    let e = env();
    e.write_config(SLEEPER);
    for branch in ["feat/one", "feat/two"] {
        assert_eq!(code(&e.pando(&["new", branch])), EXIT_OK);
        assert_eq!(code(&e.pando(&["start", branch])), EXIT_OK);
    }
    let inside = std::path::PathBuf::from(stdout(&e.pando(&["path", "feat/one"])).trim());

    let out = e.pando_in(&inside, &["stop"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "stopped feat+one");
    assert!(
        stderr(&out).contains("the worktree you are in") && stderr(&out).contains("--all"),
        "{}",
        stderr(&out)
    );
    let status: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "feat/two", "--json"]))).unwrap();
    assert_eq!(
        status["worktrees"][0]["processes"]["dev"]["phase"]
            .as_str()
            .map(|p| p != "failed"),
        Some(true),
        "the other worktree is untouched: {status}"
    );

    // The same verbs with no name act on the worktree the shell is in.
    let out = e.pando_in(&inside, &["logs", "--tail", "1"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));

    let out = e.pando_in(&inside, &["stop", "--all"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert!(stdout(&out).contains("feat+two"), "{}", stdout(&out));
}

// Inside the main checkout, a verb that takes a name means it — but
// `stop`, which there stops every one, as it always has, and whose
// `--only` so needs a worktree named.
#[test]
fn in_the_main_checkout_a_verb_with_no_name_means_it_but_stop_means_every_one() {
    let e = env();
    e.write_config(SLEEPER);
    let main = e.root.file_name().unwrap().to_string_lossy().to_string();
    let out = e.pando(&["start", "--no-wait"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert!(
        stdout(&out).starts_with(&format!("started {main}")),
        "{}",
        stdout(&out)
    );
    assert!(
        poll_until(|| stdout(&e.pando(&["logs", "--tail", "5"])).contains("started-ok")),
        "logs with no name reads the main checkout's"
    );
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(code(&e.pando(&["start", "feat/one", "--no-wait"])), EXIT_OK);

    let out = e.pando(&["stop", "--only", "dev"]);
    assert_eq!(code(&out), EXIT_USAGE, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("run it from inside one"),
        "{}",
        stderr(&out)
    );

    let out = e.pando(&["stop"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains(&main) && said.contains("feat+one"), "{said}");
}

// The main checkout runs by its branch or its directory's name: its
// processes on an allocated port, and nothing else — not the install,
// not a hook — so it stays exactly as the developer left it. The modes
// that give a worktree data apart from main's, and `rm`, refuse it.
#[test]
fn the_main_checkout_runs_by_branch_or_name_and_runs_nothing_else() {
    let e = env();
    e.write_config(&format!(
        "[project]\ninstall = \"touch install-ran\"\n\n{SLEEPER}\n\
         [[hooks]]\nname = \"migrate\"\nafter = \"services\"\non = \"always\"\n\
         cmd = \"touch migrate-ran\"\n"
    ));
    let main = e.root.file_name().unwrap().to_string_lossy().to_string();

    let out = e.pando(&["start", "main", "--no-wait"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim().split(" — ").next(),
        Some(&*format!("started {main}"))
    );
    assert!(
        stderr(&out).contains("no install, no hooks"),
        "{}",
        stderr(&out)
    );
    let status: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).unwrap();
    let first = &status["worktrees"][0];
    assert_eq!(first["name"], main.as_str(), "{status}");
    assert_eq!(first["main"], true, "{status}");
    let port = first["ports"]["web"].as_u64().expect("an allocated port");
    let listed: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["ls", "--json"]))).unwrap();
    assert_eq!(listed["worktrees"][0]["main"], true, "{listed}");
    assert_eq!(
        stdout(&e.pando(&["ls", "--names"])).lines().next(),
        Some(main.as_str())
    );
    assert!(poll_until(
        || stdout(&e.pando(&["logs", "main"])).contains("started-ok")
    ));

    for flag in ["--isolated", "--namespaced"] {
        let out = e.pando(&["start", &main, flag]);
        assert_eq!(code(&out), EXIT_ERROR, "{flag}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("is the main checkout") && stderr(&out).contains(flag),
            "{flag}: {}",
            stderr(&out)
        );
    }
    let out = e.pando(&["rm", "main"]);
    assert_eq!(code(&out), EXIT_ERROR, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("never removes it"),
        "{}",
        stderr(&out)
    );

    let out = e.pando(&["restart", &main, "--no-wait"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "main", "--json"]))).unwrap();
    assert_eq!(status["worktrees"][0]["ports"]["web"].as_u64(), Some(port));

    let out = e.pando(&["stop", "main"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), format!("stopped {main}"));

    for mark in ["install-ran", "migrate-ran"] {
        assert!(
            !e.root.join(mark).exists(),
            "{mark} ran in the main checkout"
        );
    }
    assert_eq!(
        status_porcelain(&e.root),
        "",
        "the main checkout is untouched"
    );
}

#[test]
fn start_wait_returns_once_ready_and_open_hands_the_url_to_the_browser() {
    let e = env();
    e.write_config(&format!(
        "[dev]\ncmd = '''{}'''\nports = {{ PORT = \"web\" }}\n",
        common::listener_on_port_env()
    ));
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat/one", "--wait"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert!(stderr(&out).contains("dev is ready"), "{}", stderr(&out));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).unwrap();
    let url = status["worktrees"][0]["url"].as_str().unwrap().to_string();
    assert_eq!(
        status["worktrees"][0]["processes"]["dev"]["phase"],
        "running"
    );

    // `$BROWSER` is the opener when set; here it records what it was given.
    let opened = e.home.join("opened");
    let browser = e.home.join("browser.sh");
    std::fs::write(
        &browser,
        format!("#!/bin/sh\necho \"$1\" > '{}'\n", opened.display()),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_pando"))
        .env("PANDO_HOME", &e.home)
        .env("BROWSER", &browser)
        .current_dir(&e.root)
        .args(["open", "feat/one"])
        .output()
        .unwrap();
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), url);
    assert_eq!(std::fs::read_to_string(&opened).unwrap().trim(), url);

    let out = e.pando(&["open", "feat/one", "--public"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(stderr(&out).contains("is not shared"), "{}", stderr(&out));

    e.pando(&["stop", "--all"]);
    let out = e.pando(&["open", "feat/one"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(stderr(&out).contains("pando start"), "{}", stderr(&out));
}

// A worktree that runs Expo's Metro alone serves no page: `open` opens
// its app on the booted simulator, running the very command `status`
// prints, and with none booted, on the connected Android emulator. Every
// program it runs is a stand-in in the test's own bin.
#[test]
fn open_opens_a_device_app_on_the_simulator_or_the_emulator() {
    if !common::python3_available() {
        return;
    }
    let e = env();
    let metro = common::listener_on_port_env().replace("['PORT']", "['RCT_METRO_PORT']");
    e.write_config(&format!(
        "[processes.mobile]\ncmd = '''{metro}'''\nports = {{ RCT_METRO_PORT = \"metro\" }}\n"
    ));
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat/one", "--wait"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    let status: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).unwrap();
    let worktree = &status["worktrees"][0];
    assert_eq!(worktree["url"], serde_json::Value::Null, "no page");
    let app = &worktree["processes"]["mobile"]["app"];
    let url = app["url"].as_str().unwrap();
    let port = worktree["ports"]["metro"].as_u64().unwrap();

    common::fake_devices(&e.home, true, true);
    let out = e.pando(&["open", "feat/one"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "mobile: opened its app in Expo Go on the booted iOS simulator"
    );
    assert!(
        stderr(&out).contains("opening mobile's app in Expo Go"),
        "{}",
        stderr(&out)
    );
    // The command status prints, as sh ran it.
    assert_eq!(
        app["simulator"].as_str().unwrap(),
        format!("xcrun simctl openurl booted '{url}'")
    );
    // The installed builds first, read as `status` reads them; then the
    // booted simulator, and the open.
    assert_eq!(
        common::device_calls(&e.home, "xcrun"),
        [
            "simctl list -j devices booted".to_string(),
            "simctl list devices booted".to_string(),
            format!("simctl openurl booted {url}")
        ]
    );
    assert!(common::device_calls(&e.home, "adb").is_empty());

    std::fs::remove_dir_all(e.home.join("bin")).unwrap();
    common::fake_devices(&e.home, false, true);
    let out = e.pando(&["open", "feat/one", "--app"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert_eq!(
        stdout(&out).trim(),
        "mobile: opened its app in Expo Go on the connected Android device or emulator"
    );
    assert_eq!(
        common::device_calls(&e.home, "adb"),
        [
            "devices".to_string(),
            format!("reverse tcp:{port} tcp:{port}"),
            format!("shell am start -a android.intent.action.VIEW -d {url}"),
        ]
    );
    // Nothing started a simulator.
    assert!(common::device_calls(&e.home, "open").is_empty());
    e.pando(&["stop", "--all"]);
}

#[test]
fn start_wait_fails_with_the_reason_when_the_process_dies() {
    let e = env();
    e.write_config("[dev]\ncmd = \"echo boom; exit 3\"\nports = { PORT = \"web\" }\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat/one", "--wait"]);
    assert_eq!(code(&out), EXIT_ERROR, "{}", stderr(&out));
    assert!(
        stderr(&out).contains("dev failed") && stderr(&out).contains("pando logs"),
        "{}",
        stderr(&out)
    );
}

// A process with no port is "running" as soon as it is alive, so `--wait`
// returned 0 for a dev command that exited a second later, and `status`
// then said it had failed. It is watched for a few seconds now; and the
// closing lines of its log come with the reason.
#[test]
fn start_wait_fails_when_a_portless_process_exits_at_once() {
    let e = env();
    e.write_config("[dev]\ncmd = \"echo compiled; sleep 1\"\nports = []\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat/one", "--wait"]);
    assert_eq!(code(&out), EXIT_ERROR, "{}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("dev failed"), "{err}");
    assert!(
        err.contains("status 0"),
        "a clean exit is named as the likely cause: {err}"
    );
    assert!(err.contains("compiled"), "the log's last lines: {err}");
    assert!(
        err.contains("pando logs feat/one"),
        "the branch, not the directory: {err}"
    );
}

#[test]
fn logs_with_no_source_reads_the_only_process_when_there_is_no_dev() {
    let e = env();
    e.write_config(
        "[processes.web]\ncmd = \"echo from-web && sleep 30\"\nports = [\"web\"]\n\
         env = { PORT = \"{port:web}\" }\n",
    );
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(code(&e.pando(&["start", "feat/one"])), EXIT_OK);
    let found = poll_until(|| stdout(&e.pando(&["logs", "feat/one"])).contains("from-web"));
    e.pando(&["stop", "--all"]);
    assert!(found, "`logs` with no --source read the only process's log");
}

// Completions are generated from a dotfiles setup, which is in no
// repository, so they work outside one.
#[test]
fn completions_print_a_script_outside_any_repository() {
    let dir = TempDir::new().unwrap();
    for shell in ["bash", "zsh", "fish"] {
        let out = Command::new(env!("CARGO_BIN_EXE_pando"))
            .current_dir(dir.path())
            .args(["completions", shell])
            .output()
            .unwrap();
        assert_eq!(code(&out), EXIT_OK, "{shell}: {}", stderr(&out));
        assert!(stdout(&out).contains("pando"), "{shell}");
        assert!(stdout(&out).contains("unshare"), "{shell}: every verb");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_pando"))
        .current_dir(dir.path())
        .args(["completions", "tcsh"])
        .output()
        .unwrap();
    assert_eq!(code(&out), EXIT_USAGE);
}

// The worktree-name completion is spliced into clap's script by text, so
// a change on either side can leave a script the shell refuses to load.
// Each shell that is installed parses its own; one that is not is skipped.
#[test]
fn completion_scripts_parse_in_their_own_shells() {
    let dir = TempDir::new().unwrap();
    for (shell, check) in [
        ("zsh", &["-f", "-n"][..]),
        ("bash", &["--norc", "-n"][..]),
        ("fish", &["--no-config", "-n"][..]),
    ] {
        let installed = Command::new(shell)
            .args(["-c", "exit 0"])
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
        if !installed {
            continue;
        }
        let out = Command::new(env!("CARGO_BIN_EXE_pando"))
            .current_dir(dir.path())
            .args(["completions", shell])
            .output()
            .unwrap();
        assert_eq!(code(&out), EXIT_OK, "{shell}: {}", stderr(&out));
        let script = dir.path().join(format!("pando.{shell}"));
        std::fs::write(&script, &out.stdout).unwrap();
        let parsed = Command::new(shell)
            .args(check)
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            parsed.status.success(),
            "{shell} rejects its completion script: {}",
            stderr(&parsed)
        );
    }
}

#[test]
fn logs_for_a_worktree_that_has_never_run_says_where_they_would_be() {
    let e = env();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["logs", "feat+one"]);
    assert_eq!(code(&out), EXIT_ERROR);
    assert!(
        stderr(&out).contains("no logs for feat/one"),
        "{}",
        stderr(&out)
    );
}

// A dev server that prints a binary blob leaves invalid UTF-8 in its log;
// `from_utf8_lossy` turns it into U+FFFD, and a short date-ish token a few
// bytes later used to put a timestamp match inside that character. Slicing
// there panicked the process — `pando logs` exited 101, and the TUI died
// with it. Reading a log must never be able to crash pando.
#[test]
fn logs_reads_a_file_with_invalid_utf8_before_a_short_timestamp() {
    let e = env();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let log = e.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(
        &log,
        b"starting up fine\n\xff 21-09-26T10:00:00 hello\nafter\n",
    )
    .unwrap();

    let out = e.pando(&["logs", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("21-09-26T10:00:00"),
        "the line is printed, not swallowed: {}",
        stdout(&out)
    );
    assert!(stdout(&out).contains("after"), "{}", stdout(&out));
}

// No CLI verb prints a colour, yet the first line `logs` read asked the
// system whether it was in dark mode: a `defaults` run on macOS, for
// colours nobody saw.
#[test]
fn logs_asks_the_system_nothing_about_colours_it_never_prints() {
    use std::os::unix::fs::PermissionsExt;
    let e = env();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let log = e.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "ready in 412ms\nError: boom\n").unwrap();

    let dir = e.home.parent().unwrap();
    let bin = dir.join("counting-defaults");
    let calls = dir.join("defaults-calls.log");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("defaults");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            calls.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new(env!("CARGO_BIN_EXE_pando"))
        .env("PANDO_HOME", &e.home)
        .env("PATH", path)
        // What would answer before the system is asked.
        .env_remove("PANDO_APPEARANCE")
        .current_dir(&e.root)
        .args(["logs", "feat+one"])
        .output()
        .expect("run pando");
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(stdout(&out).contains("Error: boom"), "{}", stdout(&out));
    assert_eq!(
        std::fs::read_to_string(&calls).unwrap_or_default(),
        "",
        "nothing asked the system"
    );
}

#[test]
fn a_crashed_process_stays_visible_as_failed() {
    let e = env();
    e.write_config("[dev]\ncmd = \"echo 'Error: Cannot find module next' && exit 1\"\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(code(&e.pando(&["start", "feat+one"])), EXIT_OK);

    let mut reason = String::new();
    poll_until(|| {
        let v: serde_json::Value =
            serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).unwrap();
        let process = &v["worktrees"][0]["processes"]["dev"];
        if process["phase"] == "failed" {
            reason = process["reason"].as_str().unwrap_or_default().to_string();
            return true;
        }
        false
    });
    assert!(
        reason.starts_with("process exited"),
        "reason was {reason:?}"
    );
    assert!(
        reason.contains("dependencies are missing"),
        "the log's own words become a hint: {reason}"
    );
    // Still there on the next read: a failure is sticky until acted on.
    let v: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "--json"]))).unwrap();
    assert_eq!(v["worktrees"][0]["processes"]["dev"]["phase"], "failed");
}

// ---- questions ------------------------------------------------------------

// An agent never hangs: a question it cannot answer is exit code 3 with the
// question printed, not a process blocked on a prompt nobody will read.
#[test]
fn a_question_with_no_terminal_to_ask_on_exits_three() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), 3, "stdout: {}", stdout(&out));
    let printed = stderr(&out);
    assert!(
        printed.contains("Which command starts the local development server?"),
        "{printed}"
    );
    assert!(
        printed.contains("pnpm dev"),
        "the options are printed: {printed}"
    );
    assert!(printed.contains("--yes"), "and the way out: {printed}");
    assert!(
        stdout(&out).is_empty(),
        "stdout stays clean: {}",
        stdout(&out)
    );
}

#[test]
fn yes_accepts_the_recommendation_and_never_asks_again() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    let out = e.pando(&["start", "feat+one", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    // Written down, with the reason, in pando's own config.
    let project = pando::project::ProjectRef::from_root(&e.root).unwrap();
    let config = e.home.join("projects").join(&project.id).join("pando.toml");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("cmd = \"pnpm dev\""), "{text}");
    assert!(text.contains("ports = { PORT = \"web\" }"), "{text}");
    assert!(text.contains("# detected:"), "{text}");
    assert!(
        text.contains("install = \"pnpm install --frozen-lockfile\""),
        "new answered the install slot on its own: {text}"
    );

    assert_eq!(code(&e.pando(&["stop", "feat+one"])), EXIT_OK);
    // Without --yes and without a terminal: nothing left to ask, so it runs.
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(
        code(&out),
        EXIT_OK,
        "an answered question is never asked again: {}",
        stderr(&out)
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

// Level zero: a library has no dev server, so there is nothing to ask about
// and the failure is the honest one.
#[test]
fn a_library_is_never_asked_about_a_dev_server() {
    let e = env_of(Kind::RustLib);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_ERROR, "stderr: {}", stderr(&out));
    let said = stderr(&out);
    assert!(said.starts_with("pando: nothing to run: "), "{said}");
    assert!(
        said.contains(&format!("{}:\n[dev]\ncmd = \"", e.config_file().display())),
        "{said}"
    );
}

// The common case: detection resolves every slot, so the first start needs
// no answers at all.
#[test]
fn a_common_project_starts_with_no_questions() {
    let e = env_of(Kind::NextPnpmCompose);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("pnpm dev"), "{}", stderr(&out));
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

// `logs -f` went permanently silent once the log reached `--tail` lines:
// the follow loop compared against the length of a bounded ring buffer,
// which stops growing, so every later line was skipped as already printed.
#[test]
fn follow_keeps_printing_once_the_log_is_longer_than_the_tail() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    let e = env();
    let log = e.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "l1\nl2\nl3\nl4\nl5\n").unwrap();

    // Under a guard: an assertion that panics part way through must not
    // leave a follower running for the rest of the day.
    let mut child = Follower(
        Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &e.home)
            .current_dir(&e.root)
            .args(["logs", "feat+one", "--tail", "3", "-f"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("run pando logs -f"),
    );

    let (tx, rx) = mpsc::channel::<String>();
    let out = child.0.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    // Whatever happens, the follower does not outlive this test.
    let printed = |what: &str| -> String {
        rx.recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("nothing was printed for {what}"))
    };

    for expected in ["l3", "l4", "l5"] {
        assert_eq!(printed("the initial tail"), expected);
    }

    let append = |text: &str| {
        let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        f.write_all(text.as_bytes()).unwrap();
    };
    // Well past the ring's capacity, in batches, the way a dev server logs.
    for i in 6..=20 {
        append(&format!("l{i}\n"));
        std::thread::sleep(Duration::from_millis(20));
    }
    for i in 6..=20 {
        let line = printed("a line after the window filled");
        assert_eq!(line, format!("l{i}"), "a line went missing after l{i}");
    }

    // And across the truncation a restart does: the file shrinks, and the
    // lines written after it are still new.
    std::fs::write(&log, "").unwrap();
    std::thread::sleep(Duration::from_millis(400));
    append("fresh-1\nfresh-2\n");
    assert_eq!(printed("the first line after a truncation"), "fresh-1");
    assert_eq!(printed("the second line after a truncation"), "fresh-2");

    drop(child);
    drop(rx);
    let _ = reader.join();
}

/// A `logs -f` child that is killed when it goes out of scope, however the
/// test ended.
struct Follower(std::process::Child);

impl Drop for Follower {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

// `pando logs | head` printed "pando: Broken pipe (os error 32)" and exited
// 1 once head had its lines, which a script reads as pando failing.
#[test]
fn a_reader_that_stops_early_ends_logs_quietly_with_success() {
    use std::io::{BufRead, BufReader};

    let e = env();
    let log = e.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    // Far more than a pipe holds, so pando is still writing when the
    // reader goes.
    let lines: String = (0..5000).map(|i| format!("{i:0>100}\n")).collect();
    std::fs::write(&log, lines).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_pando"))
        .env("PANDO_HOME", &e.home)
        .current_dir(&e.root)
        .args(["logs", "feat+one", "-n", "5000"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run pando logs");
    let mut first = String::new();
    BufReader::new(child.stdout.take().expect("piped stdout"))
        .read_line(&mut first)
        .unwrap();
    assert!(first.ends_with("0\n"), "{first:?}");

    let out = child.wait_with_output().unwrap();
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
}

// `pando start x 2>&1 | grep -q starting`: every notice after the reader
// left panicked, and one between two spawns lost the record of the first,
// so `stop` could not reach a process that went on holding its port. The
// error paths exited 101 the same way instead of their documented codes.
#[test]
fn a_stderr_nobody_reads_loses_no_process_and_changes_no_exit_code() {
    fn unread(e: &Env, args: &[&str]) -> i32 {
        let (reader, writer) = std::io::pipe().expect("a pipe");
        drop(reader);
        let out = Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &e.home)
            .current_dir(&e.root)
            .args(args)
            .stderr(writer)
            .output()
            .expect("run pando");
        code(&out)
    }
    let e = env();
    e.write_config(PAIR);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    assert_eq!(unread(&e, &["start", "feat+one"]), EXIT_OK);
    let v = status_of(&e);
    assert_eq!(
        v["worktrees"][0]["processes"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["api", "web"],
        "both processes are recorded, so stop reaches them"
    );

    assert_eq!(unread(&e, &["stop", "nothing-like-it"]), EXIT_ERROR);
    assert_eq!(unread(&e, &["status", "--env"]), EXIT_USAGE);
    assert_eq!(unread(&e, &["stop", "feat+one"]), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");

    let messy = env_of(Kind::NextMessy);
    assert_eq!(unread(&messy, &["init"]), EXIT_NEEDS_ANSWER);
}

// `logs -f` read everything written since its last poll at once and kept
// the newest 4096 lines of it, so a burst of more — or the backlog after
// a pager stopped reading — lost its middle with no marker.
#[test]
fn follow_prints_every_line_of_a_burst_longer_than_its_buffer() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    let e = env();
    let log = e.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "ready\n").unwrap();
    let mut child = Follower(
        Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &e.home)
            .current_dir(&e.root)
            .args(["logs", "feat+one", "-f"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("run pando logs -f"),
    );
    let (tx, rx) = mpsc::channel::<String>();
    let out = child.0.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(out).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                return;
            }
        }
    });
    let printed = |what: &str| -> String {
        rx.recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("nothing was printed for {what}"))
    };
    assert_eq!(printed("the initial tail"), "ready");

    // One write, so it all lands between two polls.
    let burst: String = (0..10_000).map(|i| format!("burst-{i}\n")).collect();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(burst.as_bytes())
        .unwrap();
    for i in 0..10_000 {
        assert_eq!(printed("the burst"), format!("burst-{i}"));
    }

    drop(child);
    drop(rx);
    let _ = reader.join();
}

// `--yes` takes the first option of a question nothing decided, so the line
// it writes may not claim the rules detected it.
#[test]
fn yes_records_that_it_took_the_first_option_rather_than_detecting_one() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let text = std::fs::read_to_string(e.config_file()).unwrap();
    let cmd = text
        .lines()
        .find(|l| l.starts_with("cmd = "))
        .unwrap_or_else(|| panic!("no dev command was written: {text}"));
    assert!(
        cmd.contains("--yes took the first of 4 options"),
        "the comment has to say a flag chose it: {cmd}"
    );
    assert!(
        !cmd.contains("# detected:"),
        "the rules did not decide this one: {cmd}"
    );
    // And the same for the port question, which had three.
    let ports = text
        .lines()
        .find(|l| l.starts_with("ports = "))
        .unwrap_or_else(|| panic!("no ports were written: {text}"));
    assert!(
        ports.contains("--yes took the first of 3 options"),
        "{ports}"
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
}

// A process the developer deliberately gave no ports — a worker, a watcher,
// a queue consumer — used to have one injected into its own `[dev]` table,
// and was then reported failed for never binding a port it was never told
// about.
#[test]
fn a_process_with_no_ports_of_its_own_is_running_once_it_is_alive() {
    // A project full of port signals, so there is every temptation.
    let e = env_of(Kind::NextPnpmCompose);
    e.write_config("[dev]\ncmd = \"sleep 300\"\n\n[dev.ready]\ntimeout_s = 2\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let text = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        !text.contains("ports ="),
        "nothing may give a portless process a port: {text}"
    );

    // Well past its readiness window, which it has no port to satisfy.
    std::thread::sleep(std::time::Duration::from_secs(4));
    let out = e.pando(&["status", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("running"),
        "a process with no ports is running once alive: {}",
        stdout(&out)
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
}

// Phase 2b review, finding 5, second half. `Command::Restart` passed the
// raw config while `Command::Start` resolved it, so `restart` on a project
// whose process question has never been answered refused instead of asking
// — the same input, two different answers, depending on which verb was
// typed.
#[test]
fn restart_asks_the_question_start_would_have_asked() {
    let e = env_of(Kind::MonoWebApi);
    assert_eq!(code(&e.pando(&["new", "feat/one", "--yes"])), EXIT_OK);

    let restart = e.pando(&["restart", "feat+one"]);
    let start = e.pando(&["start", "feat+one"]);
    assert_eq!(
        code(&restart),
        EXIT_NEEDS_ANSWER,
        "stdout: {} stderr: {}",
        stdout(&restart),
        stderr(&restart)
    );
    assert_eq!(
        code(&start),
        EXIT_NEEDS_ANSWER,
        "and this is the answer it should match: {}",
        stderr(&start)
    );
    assert_eq!(
        stderr(&restart),
        stderr(&start),
        "the same question, asked the same way"
    );
    assert_eq!(status_porcelain(&e.root), "");
}

// Phase 2b review, finding 7. Stopping one process reconciled the whole
// state file, and a sibling's `Failed` record — the one phase whose entire
// purpose is to outlive its process — went with it, so `status` stopped
// mentioning that anything had crashed.
#[test]
fn stopping_one_process_leaves_the_others_crash_visible() {
    let e = env();
    e.write_config(
        "[project]\ninstall = \"true\"\n\n\
         [processes.web]\ncmd = \"sleep 300\"\nports = []\n\n\
         [processes.api]\ncmd = \"sleep 1\"\nports = []\n",
    );
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    // The api exits on its own, and a read is what notices — but *when* it
    // exits is a fact about how quickly the machine got round to spawning
    // it, not about pando. A fixed sleep asserted that fact and failed on a
    // loaded runner; this waits for the state the test is really about.
    assert!(
        poll_until(|| stdout(&e.pando(&["status", "feat+one"])).contains("failed")),
        "the api should have failed by now: {}",
        stdout(&e.pando(&["status", "feat+one"]))
    );

    let out = e.pando(&["stop", "feat+one", "--only", "web"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let text = e.pando(&["status", "feat+one"]);
    assert!(
        stdout(&text).contains("api") && stdout(&text).contains("failed"),
        "stopping web must not erase the api's crash: {}",
        stdout(&text)
    );
    let json = e.pando(&["status", "feat+one", "--json"]);
    assert!(
        stdout(&json).contains("\"api\""),
        "and --json says so too: {}",
        stdout(&json)
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

// ---- isolation from the command line --------------------------------------

#[test]
fn start_isolated_brings_up_services_and_status_shows_them() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not installed");
        return;
    }
    let e = env_isolated();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one", "--isolated"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let text = e.pando(&["status", "feat+one"]);
    let shown = stdout(&text);
    assert!(shown.contains("postgres"), "{shown}");
    assert!(shown.contains("redis"), "{shown}");
    assert!(shown.contains("service on"), "with its port: {shown}");
    assert!(shown.contains("up"), "and whether it is answering: {shown}");

    let json = stdout(&e.pando(&["status", "feat+one", "--json"]));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let worktree = &value["worktrees"][0];
    assert_eq!(worktree["isolated"], serde_json::json!(true), "{json}");
    let postgres = &worktree["services"]["postgres"];
    assert_eq!(postgres["kind"], serde_json::json!("compose"));
    assert_eq!(postgres["up"], serde_json::json!(true), "{json}");
    assert_eq!(
        postgres["port"],
        serde_json::json!(worktree["ports"]["postgres"].as_u64().unwrap())
    );
    assert!(
        postgres["project"]
            .as_str()
            .unwrap()
            .starts_with("pando-next-pnpm-compose-"),
        "{json}"
    );

    // The service's container log is a source like any other.
    let logs = e.pando(&["logs", "feat+one", "--source", "postgres"]);
    assert_eq!(code(&logs), EXIT_OK, "stderr: {}", stderr(&logs));
    assert!(
        stdout(&logs).contains("fake docker log for postgres"),
        "{}",
        stdout(&logs)
    );

    assert_eq!(code(&e.pando(&["rm", "feat+one", "--force"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

#[test]
fn status_env_prints_lines_a_shell_can_eval() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not installed");
        return;
    }
    let e = env_isolated();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(
        code(&e.pando(&["start", "feat+one", "--isolated"])),
        EXIT_OK
    );
    let json = stdout(&e.pando(&["status", "feat+one", "--json"]));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let ports = &value["worktrees"][0]["ports"];
    let postgres = ports["postgres"].as_u64().unwrap();
    let web = ports["web"].as_u64().unwrap();

    let out = e.pando(&["status", "feat+one", "--env"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(
        text.contains(&format!(
            "export DATABASE_URL='postgres://acme:acme@localhost:{postgres}/acme'"
        )),
        "{text}"
    );
    assert!(text.contains(&format!("export PORT='{web}'")), "{text}");
    assert!(text.contains("export PANDO_NAME='feat+one'"), "{text}");
    // Every line is an export, so `eval` on the whole thing is safe.
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        assert!(line.starts_with("export "), "{line:?}");
    }

    // And a shell really does read it back.
    let shell = Command::new("bash")
        .arg("-c")
        .arg("eval \"$(cat)\" && echo \"$DATABASE_URL|$PANDO_NAME\"")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("stdin")
                .write_all(text.as_bytes())?;
            child.wait_with_output()
        })
        .expect("run bash");
    assert_eq!(
        stdout(&shell).trim(),
        format!("postgres://acme:acme@localhost:{postgres}/acme|feat+one")
    );

    assert_eq!(code(&e.pando(&["rm", "feat+one", "--force"])), EXIT_OK);
}

#[test]
fn status_env_on_a_worktree_that_has_never_started_says_so() {
    let e = env_isolated();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["status", "feat+one", "--env"]);
    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("start it once"), "{}", stderr(&out));
    assert!(stdout(&out).is_empty(), "nothing to eval: {}", stdout(&out));
}

#[test]
fn status_env_without_a_name_is_a_usage_error() {
    let e = env();
    assert_eq!(code(&e.pando(&["status", "--env"])), EXIT_USAGE);
    assert_eq!(
        code(&e.pando(&["status", "feat+one", "--env", "--json"])),
        EXIT_USAGE,
        "--env and --json are two different shapes"
    );
}

// Every guess is visible, and so is every non-guess: a flag taking the
// services must not be written down as if a human had chosen them.
#[test]
fn yes_says_in_the_file_that_a_flag_took_the_services() {
    let e = env_of(Kind::NextMessy);
    common::docker::install(&e.home);
    // The process slots are already answered, so `--isolated --yes` has
    // exactly one question left to take.
    e.write_config("[project]\ninstall = \"true\"\n\n[dev]\ncmd = \"sleep 30\"\nports = []\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one", "--isolated", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let text = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(text.contains("[[services]]"), "{text}");
    assert!(
        text.contains("include = [\"db\", \"mail\"]"),
        "--yes takes the ones a rule resolved and leaves cache and queue: {text}"
    );
    assert!(
        text.contains("--yes took the 2 of 4 the rules resolved"),
        "the file has to say a flag decided this: {text}"
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
}

// The plan's own words: an isolated start on a project with no services
// runs shared and says so, rather than refusing.
#[test]
fn start_isolated_on_a_project_with_no_services_runs_shared() {
    let e = env_of(Kind::GoService);
    common::docker::install(&e.home);
    e.write_config("[project]\ninstall = \"true\"\n\n[dev]\ncmd = \"sleep 30\"\nports = []\n");
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let out = e.pando(&["start", "feat+one", "--isolated"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("shared mode"), "{}", stderr(&out));
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
}

// ---- the runtime the project asks for -------------------------------------

/// The fixture, plus a committed pin no machine resolves, and the machine
/// answer taken away again.
fn env_pinning_an_impossible_runtime() -> Env {
    let e = env();
    e.unanswer_the_runtime();
    std::fs::write(e.root.join(".nvmrc"), "99\n").unwrap();
    git(&e.root, &["add", ".nvmrc"]);
    git(&e.root, &["commit", "--quiet", "-m", "pin node 99"]);
    e.write_config(SLEEPER);
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    e
}

// A dev server started under a runtime the project rejects dies of it. The
// whole point of asking first is that nothing is started.
#[test]
fn a_runtime_this_machine_does_not_resolve_is_a_question_not_a_dead_process() {
    let e = env_pinning_an_impossible_runtime();

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_NEEDS_ANSWER, "stdout: {}", stdout(&out));
    let printed = stderr(&out);
    assert!(printed.contains("node 99 (.nvmrc)"), "{printed}");
    assert!(
        printed.contains("bash -lc"),
        "the shell pando actually uses is named: {printed}"
    );
    assert!(
        printed.contains(&e.home.join("config.toml").display().to_string()),
        "and the file the answer belongs in, absolutely, which is not the project's own: \
         {printed}"
    );
    assert!(
        !e.log_file("feat+one", "dev").exists(),
        "nothing may be spawned before the runtime is settled"
    );
    assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    assert_eq!(status_porcelain(&e.root), "");
}

// The case that is invisible without a probe: a prelude is set, and it is
// not doing anything.
#[test]
fn a_prelude_that_does_not_work_stops_the_start_and_names_the_file() {
    let e = env_pinning_an_impossible_runtime();
    e.write_user_config("[runtime]\nprelude = \"true\"\n");

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    let printed = stderr(&out);
    assert!(printed.contains("is not working"), "{printed}");
    assert!(printed.contains("prelude: true"), "{printed}");
    assert!(printed.contains("config.toml"), "{printed}");
    assert!(
        !e.log_file("feat+one", "dev").exists(),
        "nothing may be spawned"
    );
}

// And the answer that makes it go away: a line this machine really can
// run, written to the machine-wide layer and never asked about again.
#[test]
fn a_prelude_that_works_is_written_to_the_user_layer_and_the_start_proceeds() {
    let e = env_pinning_an_impossible_runtime();
    let bin = e.home.join("fake-bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("node"), "#!/bin/sh\necho v99.0.0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(bin.join("node"), std::fs::Permissions::from_mode(0o755)).unwrap();
    e.write_user_config(&format!(
        "[runtime]\nprelude = 'export PATH=\"{}:$PATH\"'\n",
        bin.display()
    ));

    let out = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("started feat+one"),
        "{}",
        stdout(&out)
    );
    assert_eq!(code(&e.pando(&["stop"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

// A prelude a program supplied, which this machine then proves does not
// work, is a bad value in the file the program wrote: exit 2, the code
// every other refused answer gets, and nothing written.
#[test]
fn a_program_supplied_prelude_that_fails_its_probe_exits_as_a_usage_error() {
    let e = env_pinning_an_impossible_runtime();
    let out = e.pando_stdin(&["init", "--answers", "-"], r#"{"prelude": "true"}"#);
    assert_eq!(code(&out), EXIT_USAGE, "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("does not work"), "{}", stderr(&out));
    assert!(
        !e.home.join("config.toml").exists()
            || !std::fs::read_to_string(e.home.join("config.toml"))
                .unwrap()
                .contains("prelude = \"true\""),
        "a line that does not work is not written down"
    );
}

// A pin only an app directory states is read once `init` has answered the
// version files, in the same pass: `init --yes` used to check the runtime
// before that, find nothing pinned, and exit 0, and only the first `check`
// raised the prelude. No machine has node 99, so no line fixes it here and
// `--yes` has nothing to take.
#[test]
fn init_yes_raises_the_prelude_for_a_pin_in_an_app_directory() {
    let e = env();
    e.unanswer_the_runtime();
    std::fs::create_dir_all(e.root.join("backend")).unwrap();
    std::fs::write(
        e.root.join("backend/package.json"),
        r#"{ "scripts": { "dev": "node server.js" } }"#,
    )
    .unwrap();
    std::fs::write(e.root.join("backend/.nvmrc"), "99\n").unwrap();

    let out = e.pando(&["init", "--yes"]);
    assert_eq!(code(&out), EXIT_NEEDS_ANSWER, "stderr: {}", stderr(&out));
    let printed = stderr(&out);
    assert!(printed.contains("node 99 (backend/.nvmrc)"), "{printed}");
    assert!(
        printed.contains("every project on this machine"),
        "the question says the answer is the machine's: {printed}"
    );
    assert!(
        !e.home.join("config.toml").exists(),
        "nothing is written to the machine-wide file"
    );
}

// ---- init -----------------------------------------------------------------

// The batch form of every question, in one pass, through the same paths
// `new` and `start` use. It prints where the answers went.
#[test]
fn init_answers_every_slot_and_says_which_file_it_wrote() {
    let e = env_of(Kind::NextPnpmCompose);

    let out = e.pando(&["init", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let printed = stdout(&out);
    assert!(
        printed.contains(&format!("wrote {}", e.config_file().display())),
        "the file to go and read is the first thing it says: {printed}"
    );
    assert!(
        printed.contains("pnpm install --frozen-lockfile"),
        "{printed}"
    );
    assert!(printed.contains("pnpm dev"), "{printed}");
    assert!(printed.contains("postgres, redis"), "{printed}");

    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(written.contains(r#"cmd = "pnpm dev""#), "{written}");
    assert!(written.contains("[[services]]"), "{written}");
    // Nothing was started, and nothing was written into the repository.
    assert_eq!(
        stdout(&e.pando(&["ls"])),
        "no worktrees — `pando new <branch>` creates one\n"
    );
    assert_eq!(status_porcelain(&e.root), "");
}

#[test]
fn a_second_init_has_nothing_left_to_answer() {
    let e = env_of(Kind::NextPnpmCompose);
    assert_eq!(code(&e.pando(&["init", "--yes"])), EXIT_OK);
    let first = std::fs::read_to_string(e.config_file()).unwrap();

    // No `--yes` this time: a run that asks nothing needs no flag, which
    // is the whole point of having written the answers down.
    let out = e.pando(&["init"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("nothing left to answer"),
        "{}",
        stdout(&out)
    );
    assert_eq!(std::fs::read_to_string(e.config_file()).unwrap(), first);
}

// Agents never hang: with nothing to read an answer from, the first
// undecided slot is exit 3 and the question is on stderr.
#[test]
fn init_with_nobody_to_ask_exits_three_with_the_question() {
    let e = env_of(Kind::NextMessy);

    let out = e.pando(&["init"]);
    assert_eq!(code(&out), EXIT_NEEDS_ANSWER, "stdout: {}", stdout(&out));
    let printed = stderr(&out);
    assert!(
        printed.contains("Which command starts the local development server?"),
        "{printed}"
    );
    assert!(
        printed.contains("pnpm dev"),
        "the options are printed: {printed}"
    );
    assert!(printed.contains("--yes"), "and the way out: {printed}");
    // What the rules settled on the way is written down — the pass got
    // that far — and the slot it stopped on is not: a question is not an
    // answer.
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(written.contains("install = "), "{written}");
    assert!(!written.contains("[dev]"), "{written}");
    assert_eq!(status_porcelain(&e.root), "");
}

// `--yes` answers the same questions and says in the file that a flag did.
#[test]
fn init_with_yes_records_that_a_flag_chose() {
    let e = env_of(Kind::NextMessy);
    let out = e.pando(&["init", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        written.contains("# answered: --yes took the first of"),
        "a config that claims a rule decided what a flag decided is one nobody can review: \
         {written}"
    );
}

// ---- init --agent ---------------------------------------------------------

/// Every path under `dir`, with each file's bytes: what "writes nothing"
/// is checked against, contents included.
fn contents(dir: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Option<Vec<u8>>> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match path.is_dir() {
                true => {
                    out.insert(path.clone(), None);
                    stack.push(path);
                }
                false => {
                    out.insert(path.clone(), std::fs::read(&path).ok());
                }
            }
        }
    }
    out
}

// What `contents` takes in includes `.git`, which holds still between two
// looks only while no git goes on working there after returning. git's
// automatic maintenance does just that, so it is off: for every git this
// binary starts, at the command scope, which no config file overrides; and
// in a fixture origin's own config, since the receive-pack a push starts
// there is given nothing from the environment.
#[test]
fn no_git_started_here_runs_automatic_maintenance() {
    let dir = TempDir::new().unwrap();
    let fixture = build_with_origin(Kind::Plain, dir.path());
    let setting = |repo: &Path, args: &[&str]| {
        let out = common::git_raw(repo, args);
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(
        setting(
            &fixture.root,
            &["config", "--show-scope", "--get", "maintenance.auto"]
        ),
        "command\tfalse\n"
    );
    assert_eq!(
        setting(
            fixture.remote.as_deref().unwrap(),
            &["config", "--local", "--get", "maintenance.auto"]
        ),
        "false\n"
    );
}

/// What an agent's shell is trusted to show whole. Claude Code's cuts
/// output at about 30,000 characters, and the job is meant to be read
/// in one go beside everything else the agent has.
const JOB_LIMIT: usize = 10 * 1024;

// The head start: a workspace whose process list is a question gets the
// question with pando's options and the one it would take, and the
// guesses it would take without asking, each with its reason.
#[test]
fn init_agent_names_the_open_questions_and_pandos_guesses() {
    let e = env_of(Kind::MonoWebApi);
    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let job = stdout(&out);
    assert!(
        job.starts_with("# Set up pando for mono-web-api (pando "),
        "{job}"
    );
    assert!(job.contains("on branch main"), "{job}");
    // Open, with its options, pando's own marked.
    assert!(job.contains("- processes: open, below"), "{job}");
    assert!(
        job.contains("- processes: Run these as separate processes?"),
        "{job}"
    );
    assert!(
        job.contains("`api: pnpm dev in apps/api; web: pnpm dev --port {port:web} in apps/web`"),
        "{job}"
    );
    assert!(job.contains("← pando's choice"), "{job}");
    // Taken, not asked: a first run puts no question to the developer.
    let flat = job.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("take it, and ask the developer nothing"),
        "{job}"
    );
    // Guessed, with the evidence.
    assert!(
        job.contains("- install: pando's guess: `pnpm install --frozen-lockfile` (pnpm-lock.yaml)"),
        "{job}"
    );
    // The main checkout's database, by its port and never its value.
    assert!(job.contains("postgres :5432"), "{job}");
    assert!(
        !job.contains("postgres://"),
        "a value is never printed: {job}"
    );
    assert!(job.contains("## First run"), "{job}");
    assert!(
        job.contains("`pando init --agent --reference json`"),
        "{job}"
    );
}

// Answered slots read as set, and the open-question list goes quiet once
// nothing is left for anybody to answer.
#[test]
fn init_agent_after_init_says_what_is_set_and_that_nothing_is_open() {
    let e = env_of(Kind::NextPnpmCompose);
    assert_eq!(code(&e.pando(&["init", "--yes"])), EXIT_OK);
    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let job = stdout(&out);
    assert!(
        job.contains("- install: set: `pnpm install --frozen-lockfile`"),
        "{job}"
    );
    assert!(job.contains("- runs: dev: `pnpm dev`"), "{job}");
    assert!(job.contains("(written)"), "{job}");
    assert!(job.contains("Open questions: none."), "{job}");
}

// The limit, on every fixture — the monorepo, the services, the messy
// ones — before and after their questions are answered.
#[test]
fn init_agent_stays_under_ten_kilobytes_on_every_fixture() {
    for kind in Kind::ALL {
        let e = env_of(kind);
        for pass in ["fresh", "after init --yes"] {
            let out = e.pando(&["init", "--agent"]);
            assert_eq!(code(&out), EXIT_OK, "{kind:?} {pass}: {}", stderr(&out));
            let size = out.stdout.len();
            assert!(
                size < JOB_LIMIT,
                "{kind:?} {pass}: the job is {size} bytes, over {JOB_LIMIT}"
            );
            let _ = e.pando(&["init", "--yes"]);
        }
    }
}

// Read-only, like `signals`: not the repository, not pando's home, not
// even the home's directory when there is none yet.
#[test]
fn init_agent_writes_nothing_not_even_pandos_home() {
    let e = env_of(Kind::MonoWebApi);
    let home = contents(&e.home);
    let repo = contents(&e.root);
    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert_eq!(contents(&e.home), home, "pando's home changed");
    assert_eq!(contents(&e.root), repo, "the repository changed");
    assert_eq!(status_porcelain(&e.root), "");

    std::fs::remove_dir_all(&e.home).unwrap();
    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(!e.home.exists(), "init --agent made pando's home");
}

// `--reference memory` prints the block the job ends with, and only it:
// read-only like the job, and with nothing written it is the same text.
#[test]
fn init_agent_reference_memory_prints_the_block_the_job_ends_with() {
    let e = env();
    let home = contents(&e.home);
    let repo = contents(&e.root);
    let out = e.pando(&["init", "--agent", "--reference", "memory"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let block = stdout(&out);
    assert!(block.starts_with("## pando runs "), "{block}");
    let job = stdout(&e.pando(&["init", "--agent"]));
    assert!(
        job.ends_with(&format!("```markdown\n{block}```\n")),
        "the job does not end with the block, fenced:\n{job}"
    );
    assert_eq!(contents(&e.home), home, "pando's home changed");
    assert_eq!(contents(&e.root), repo, "the repository changed");
}

// Every worktree pando makes gets the block above it, under the names
// Claude Code and Codex read, in pando's own home and nowhere else — not
// the repository, not the worktree. Written again by the next `new`,
// whole: a copy somebody edited is replaced, never appended to.
#[test]
fn new_writes_the_memory_files_above_its_worktrees_and_only_there() {
    let e = env();
    let block = stdout(&e.pando(&["init", "--agent", "--reference", "memory"]));
    let files = ["CLAUDE.md", "AGENTS.md"].map(|name| e.project_dir().join(name));
    let written = |file: &Path| std::fs::read_to_string(file).unwrap();

    let out = e.pando(&["new", "feat/one"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let worktree = e.project_dir().join("worktrees").join("feat+one");
    assert!(worktree.is_dir());
    for file in &files {
        let text = written(file);
        assert!(
            text.starts_with("<!-- pando wrote this file and rewrites it"),
            "{text}"
        );
        assert!(text.ends_with(&block), "{}: {text}", file.display());
        assert!(worktree.starts_with(file.parent().unwrap()));
    }
    for place in [&e.root, &worktree] {
        for name in ["CLAUDE.md", "AGENTS.md"] {
            assert!(
                !place.join(name).exists(),
                "{name} was written into {}",
                place.display()
            );
        }
    }
    assert_eq!(status_porcelain(&e.root), "", "the repository changed");
    assert_eq!(status_porcelain(&worktree), "", "the worktree changed");

    for file in &files {
        std::fs::write(file, "an edit of somebody's\n").unwrap();
    }
    let out = e.pando(&["new", "feat/two"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    for file in &files {
        let text = written(file);
        assert!(!text.contains("an edit of somebody's"), "{text}");
        assert_eq!(text.matches("## pando runs ").count(), 1, "{text}");
        assert!(text.ends_with(&block), "{text}");
    }
}

// A config pando cannot read is the first thing the job says, and the
// command still runs and exits 0 — where plain `init` is stopped by it.
#[test]
fn init_agent_reports_a_config_it_cannot_read_rather_than_refusing() {
    let e = env();
    e.write_config("[project\ninstall = \n");
    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let job = stdout(&out);
    assert!(
        job.contains("**pando cannot read its settings for this project:**"),
        "{job}"
    );
    assert!(job.contains("`pando doctor`"), "{job}");
    // Before the facts: it is the thing to tell the developer first.
    assert!(
        job.find("cannot read its settings") < job.find("## This project"),
        "{job}"
    );
    assert_eq!(code(&e.pando(&["init"])), EXIT_ERROR);
}

// After a failed check the job leads with it: the reason and the failed
// process's last lines, ahead of the facts. The lines are redacted again
// on the way out, so a secret a record still holds never reaches the
// agent.
#[test]
fn init_agent_after_a_failed_check_starts_with_the_failure() {
    let e = env();
    e.write_config(SLEEPER);
    let paths = common::paths_for(&e.home, &e.root);
    let config = pando::config::load(&paths).unwrap().config;
    let mut record = pando::setup::CheckRecord::begin(
        pando::setup::fingerprint(&config),
        pando::setup::RanBy::Program,
    );
    record.outcome = pando::setup::CheckOutcome::Failed {
        kind: pando::setup::FailureKind::Settings,
        reason: "dev exited after 0.8s".to_string(),
    };
    record.failed_tail = vec![
        "DATABASE_URL=postgres://u:hunter2@localhost/db".to_string(),
        "Error: Cannot find module 'dotenv'".to_string(),
    ];
    record.save(&paths).unwrap();
    let before = contents(&e.home);

    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let job = stdout(&out);
    let failed = job.find("## The last test failed").expect(&job);
    assert!(failed < job.find("## This project").unwrap(), "{job}");
    assert!(job.contains("dev exited after 0.8s"), "{job}");
    assert!(
        !job.contains("hunter2"),
        "a password reached the job: {job}"
    );
    assert!(
        job.contains("    Error: Cannot find module 'dotenv'"),
        "{job}"
    );
    assert!(
        job.contains("localhost/db"),
        "the line itself is kept: {job}"
    );
    assert_eq!(
        contents(&e.home),
        before,
        "reading the record wrote something"
    );
}

// The whole documents, as this pando was built with them.
#[test]
fn init_agent_reference_prints_the_brief_and_the_contract_whole() {
    let e = env();
    for (doc, file) in [("brief", "agent/brief.md"), ("json", "agent/json.md")] {
        let out = e.pando(&["init", "--agent", "--reference", doc]);
        assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
        let expected =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap();
        assert_eq!(stdout(&out), expected, "--reference {doc}");
    }
    // And never without `--agent`, or beside a flag that answers.
    assert_eq!(
        code(&e.pando(&["init", "--reference", "brief"])),
        EXIT_USAGE
    );
    assert_eq!(code(&e.pando(&["init", "--agent", "--yes"])), EXIT_USAGE);
}

// ---- an answers file ------------------------------------------------------

/// The answers a program would send for the deliberately ambiguous
/// fixture: the two questions its rules cannot settle, plus the services.
const ANSWERS: &str = r#"{
  "dev_cmd": "pnpm dev:web",
  "port_env": "WEB_PORT",
  "services": ["db", "cache"]
}"#;

#[test]
fn an_answers_file_fills_the_config_and_says_a_program_answered() {
    let e = env_of(Kind::NextMessy);
    let path = e.home.join("answers.json");
    std::fs::write(&path, ANSWERS).unwrap();

    let out = e.pando(&["init", "--answers", path.to_str().unwrap()]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        written.contains(r#"cmd = "pnpm dev:web""#),
        "the option was named by its own text: {written}"
    );
    assert!(
        written.contains(r#"ports = { WEB_PORT = "web" }"#),
        "{written}"
    );
    assert!(
        written.contains(r#"include = ["db", "cache"]"#),
        "{written}"
    );
    assert!(
        written.matches("# answered: a program,").count() >= 3,
        "every key a program answered says so, and none of the detected ones do: {written}"
    );
    assert!(
        written.contains(r#"install = "pnpm install --frozen-lockfile"  # detected:"#),
        "a rule's own answer still says a rule decided it: {written}"
    );
    assert_eq!(status_porcelain(&e.root), "");
}

#[test]
fn an_answers_file_can_come_from_stdin() {
    let e = env_of(Kind::NextMessy);
    let out = e.pando_stdin(&["init", "--answers", "-"], ANSWERS);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        std::fs::read_to_string(e.config_file())
            .unwrap()
            .contains(r#"cmd = "pnpm dev:web""#)
    );
}

// Never a silent skip: a program that named a question pando does not ask
// has to hear about it, with the names it could have used.
#[test]
fn a_question_pando_does_not_ask_is_a_usage_error() {
    let e = env_of(Kind::NextMessy);
    let out = e.pando_stdin(
        &["init", "--answers", "-"],
        r#"{"dev_command": "pnpm dev"}"#,
    );
    assert_eq!(code(&out), EXIT_USAGE, "stdout: {}", stdout(&out));
    let printed = stderr(&out);
    assert!(printed.contains("dev_command"), "{printed}");
    assert!(printed.contains("dev_cmd"), "{printed}");
    assert!(
        !e.config_file().exists(),
        "the file is checked before a single key is written"
    );
}

#[test]
fn an_answer_that_is_not_on_offer_names_what_is() {
    let e = env_of(Kind::NextMessy);
    let out = e.pando_stdin(
        &["init", "--answers", "-"],
        r#"{"services": ["postgres"], "dev_cmd": "pnpm dev", "port_env": "PORT"}"#,
    );
    assert_eq!(code(&out), EXIT_USAGE, "stdout: {}", stdout(&out));
    let printed = stderr(&out);
    assert!(printed.contains("postgres"), "{printed}");
    assert!(
        printed.contains("cache, db, mail, queue"),
        "a service the compose file does not declare is not one pando can run: {printed}"
    );
}

// A project of several apps in sibling directories, answered as it is
// written: one table per process, through the one write path.
#[test]
fn process_tables_answer_a_project_of_several_apps() {
    let e = env();
    for dir in ["backend", "frontend"] {
        std::fs::create_dir_all(e.root.join(dir)).unwrap();
    }
    let out = e.pando_stdin(
        &["init", "--answers", "-"],
        r#"{"processes": {
            "api": {"cmd": "uv run uvicorn app:app --port {port}", "cwd": "backend", "ports": ["api"]},
            "web": {"cmd": "npm run dev", "cwd": "frontend", "ports": {"PORT": "web"},
                    "env": {"API_URL": "http://127.0.0.1:{port:api}"}}
        }}"#,
    );
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        written.contains("[processes.api]  # answered: a program,"),
        "{written}"
    );
    assert!(written.contains(r#"cwd = "frontend""#), "{written}");
}

// What is no way to say it is exit 2, naming what is.
#[test]
fn answers_that_cannot_describe_the_processes_are_usage_errors() {
    for (answers, says) in [
        (
            r#"{"processes": "api: uvicorn app:app in backend; web: npm run dev in frontend"}"#,
            "object of process tables",
        ),
        (
            r#"{"processes": ["uvicorn app:app", "npm run dev"]}"#,
            "an object of process tables",
        ),
        (
            r#"{"processes": {"api": {"command": "x"}}}"#,
            "unknown field",
        ),
        (
            r#"{"dev_cmd": "./serve.sh", "port_env": "PORT, API-PORT"}"#,
            "not an environment variable name",
        ),
    ] {
        let e = env();
        let out = e.pando_stdin(&["init", "--answers", "-"], answers);
        assert_eq!(code(&out), EXIT_USAGE, "{answers}: {}", stderr(&out));
        assert!(stderr(&out).contains(says), "{answers}: {}", stderr(&out));
        let written = std::fs::read_to_string(e.config_file()).unwrap_or_default();
        assert!(
            !written.contains("ports") && !written.contains("[processes"),
            "{written}"
        );
    }
}

// A project that would run nothing got "Open questions: none" from the
// job and exit 0 from `init --yes`, and the gap surfaced as a failed
// check. The dev command is an open question there, and `--yes` has
// nothing it may take for it.
#[test]
fn a_project_that_would_run_nothing_has_the_dev_command_open() {
    let e = env();
    let job = stdout(&e.pando(&["init", "--agent"]));
    assert!(
        job.contains("- dev_cmd: open, below: nothing would run"),
        "{job}"
    );
    assert!(!job.contains("Open questions: none"), "{job}");

    let out = e.pando(&["init", "--yes"]);
    assert_eq!(code(&out), EXIT_NEEDS_ANSWER, "stderr: {}", stderr(&out));
    let printed = stderr(&out);
    assert!(
        printed.contains("Which command starts the local development server?"),
        "{printed}"
    );
    assert!(
        printed.contains(r#"{"dev_cmd": "<your own>"}"#),
        "{printed}"
    );
    assert!(printed.contains(r#"{"processes": {"#), "{printed}");

    // Answered, it is not open any more.
    let out = e.pando_stdin(&["init", "--answers", "-"], r#"{"dev_cmd": "./serve.sh"}"#);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert_eq!(code(&e.pando(&["init", "--yes"])), EXIT_OK);
}

// A rule's guess never stands in for an answer the file gives: on a first
// run the gitignored files were taken as decided and the answer for them
// reported as asked about by nothing.
#[test]
fn an_answer_beats_the_guess_a_rule_decided() {
    let e = env();
    std::fs::write(e.root.join(".env"), "A=1\n").unwrap();
    std::fs::write(e.root.join(".env.local"), "B=2\n").unwrap();
    let out = e.pando_stdin(
        &["init", "--answers", "-"],
        r#"{"dev_cmd": "./serve.sh", "provision": [".env"]}"#,
    );
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(!stderr(&out).contains("nothing asked"), "{}", stderr(&out));
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        written.contains(r#"provision = [".env"]  # answered: a program"#),
        "{written}"
    );
}

// An answer written before the next open question stopped the run printed
// nothing, and exit 3 read as the answer refused. It says what it wrote
// as a finished run does, before the question.
#[test]
fn answers_written_before_an_open_question_are_said_as_written() {
    let e = env();
    let out = e.pando_stdin(&["init", "--answers", "-"], r#"{"install": "true"}"#);
    assert_eq!(code(&out), EXIT_NEEDS_ANSWER, "stderr: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("wrote {}\n", e.config_file().display())
    );
    assert!(
        std::fs::read_to_string(e.config_file())
            .unwrap()
            .contains("install = \"true\""),
    );
    assert!(
        stderr(&out).contains("Which command starts the local development server?"),
        "{}",
        stderr(&out)
    );

    // Nothing new written, nothing said.
    let out = e.pando_stdin(&["init", "--answers", "-"], "{}");
    assert_eq!(code(&out), EXIT_NEEDS_ANSWER, "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "");

    // The contract says so.
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"))
        .unwrap();
    let flat = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("stdout says `wrote <file>` for each file it changed"),
        "agent/json.md"
    );
}

// A Python api with no script beside a frontend with one: the job marked
// the frontend alone as pando's choice, `init --yes` took it, and the
// check passed with the api never run. The process list is open, says
// which directory nothing starts, and `--yes` has nothing to take.
#[test]
fn an_app_directory_nothing_starts_holds_the_process_list_open() {
    let e = env();
    for (rel, contents) in [
        ("backend/pyproject.toml", "[project]\nname = \"api\"\n"),
        ("backend/uv.lock", "version = 1\n"),
        (
            "frontend/package.json",
            r#"{ "scripts": { "dev": "nuxt dev" } }"#,
        ),
        ("frontend/package-lock.json", "{}"),
    ] {
        let path = e.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    git(&e.root, &["add", "."]);
    git(&e.root, &["commit", "--quiet", "-m", "apps"]);

    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let job = stdout(&out);
    assert!(out.stdout.len() < JOB_LIMIT, "{} bytes", out.stdout.len());
    assert!(job.contains("- processes: open, below"), "{job}");
    assert!(
        job.contains("backend has uv.lock but no dev script: nothing here starts it"),
        "{job}"
    );
    assert!(!job.contains("← pando's choice"), "{job}");
    let flat = job.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains("only the project's docs or the developer know how each directory"),
        "{job}"
    );

    let out = e.pando(&["init", "--yes"]);
    assert_eq!(code(&out), EXIT_NEEDS_ANSWER, "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("Run these as separate processes?"),
        "{}",
        stderr(&out)
    );
}

// The job said "postgres: no port in the main checkout's env files" of a
// backend whose own `.env` has it. The app directory's files are read,
// and the job names the one the port came from.
#[test]
fn init_agent_reads_a_services_port_beside_the_app() {
    let e = env();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    for (rel, contents) in [
        ("backend/pyproject.toml", "[project]\nname = \"api\"\n"),
        ("backend/uv.lock", "version = 1\n"),
        (
            "backend/.env.example",
            "POSTGRES_SERVER=localhost\nPOSTGRES_PORT=5432\n",
        ),
    ] {
        let path = e.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    git(&e.root, &["add", "."]);
    git(&e.root, &["commit", "--quiet", "-m", "api"]);
    std::fs::write(
        e.root.join("backend/.env"),
        format!("POSTGRES_SERVER=localhost\nPOSTGRES_PORT={port}\n"),
    )
    .unwrap();

    let out = e.pando(&["init", "--agent"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let job = stdout(&out);
    assert!(
        job.contains(&format!("- postgres :{port} in backend/.env (not running)")),
        "{job}"
    );
}

// An answer for a slot that already has one is refused, whole file and
// preview alike: dropped with a note and exit 0, a program took the run
// for its answer being written.
#[test]
fn answers_for_questions_already_answered_are_refused_and_nothing_is_written() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(
        code(&e.pando_stdin(&["init", "--answers", "-"], ANSWERS)),
        EXIT_OK
    );
    let first = std::fs::read_to_string(e.config_file()).unwrap();

    // An unanswered slot beside them is not written either: nothing of a
    // refused file lands, so the corrected file is the only one to send.
    let mixed = r#"{"dev_cmd": "pnpm dev", "install": "make deps"}"#;
    for args in [
        &["init", "--answers", "-"][..],
        &["init", "--answers", "-", "--dry-run"][..],
    ] {
        let out = e.pando_stdin(args, mixed);
        assert_eq!(code(&out), EXIT_USAGE, "{args:?} stderr: {}", stderr(&out));
        let printed = stderr(&out);
        assert!(
            printed.contains(
                "dev_cmd is already answered, so your answer was not applied — add --replace to \
                 replace it"
            ),
            "{printed}"
        );
        assert!(printed.contains("nothing was written"), "{printed}");
        assert!(!printed.contains("answers file"), "{printed}");
        assert!(stdout(&out).is_empty(), "{}", stdout(&out));
    }
    assert_eq!(
        std::fs::read_to_string(e.config_file()).unwrap(),
        first,
        "a refused file changes nothing"
    );

    // Every slot it names is named back, not only the first.
    let out = e.pando_stdin(&["init", "--answers", "-"], ANSWERS);
    assert_eq!(code(&out), EXIT_USAGE, "stderr: {}", stderr(&out));
    for name in ["dev_cmd", "port_env", "services"] {
        assert!(
            stderr(&out).contains(&format!("{name} is already answered")),
            "{}",
            stderr(&out)
        );
    }

    // And the way it names is the way through.
    let out = e.pando_stdin(&["init", "--answers", "-", "--replace"], mixed);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
}

// The preview is the real renderer against a copy, so what it prints is
// what would land — and nothing is kept.
#[test]
fn dry_run_prints_the_config_it_would_write_and_writes_nothing() {
    let e = env_of(Kind::NextMessy);
    let out = e.pando_stdin(&["init", "--answers", "-", "--dry-run"], ANSWERS);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let printed = stdout(&out);
    assert!(
        printed.contains(&format!("# {}", e.config_file().display())),
        "the file it would write is named: {printed}"
    );
    assert!(printed.contains(r#"cmd = "pnpm dev:web""#), "{printed}");
    assert!(printed.contains("# answered: a program,"), "{printed}");
    assert!(
        stderr(&out).contains("would write"),
        "the summary says it did not: {}",
        stderr(&out)
    );
    assert!(
        !e.config_file().exists(),
        "a preview that writes the file is not a preview"
    );
    assert!(
        !e.home.join("preview").exists(),
        "and the scratch copy it made is gone"
    );
    assert!(
        !e.project_dir().join("decisions.jsonl").exists(),
        "a preview that records the decisions it did not make is not a preview"
    );
    assert_eq!(status_porcelain(&e.root), "");

    // And the real run then writes exactly what the preview showed.
    assert_eq!(
        code(&e.pando_stdin(&["init", "--answers", "-"], ANSWERS)),
        EXIT_OK
    );
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        printed.contains(&written),
        "the preview was the real thing: {printed}"
    );
}

// ---- correcting an answer: --replace --------------------------------------

/// The slots the decisions log records for a project, with the kind of
/// line each was.
fn decision_lines(e: &Env) -> Vec<serde_json::Value> {
    std::fs::read_to_string(e.project_dir().join("decisions.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

// The way a setup found wrong is corrected without anybody editing the
// file: previewed, then applied, through the same checks, and written
// down as a program's — so the next command does not take it for a
// person overriding the program's first answer.
#[test]
fn replace_corrects_answered_slots_and_says_a_program_did() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(
        code(&e.pando_stdin(&["init", "--answers", "-"], ANSWERS)),
        EXIT_OK
    );
    let first = std::fs::read_to_string(e.config_file()).unwrap();
    const CORRECTED: &str = r#"{"dev_cmd": "pnpm dev:all", "services": ["db"]}"#;

    let out = e.pando_stdin(
        &["init", "--answers", "-", "--dry-run", "--replace"],
        CORRECTED,
    );
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let preview = stdout(&out);
    assert!(preview.contains(r#"cmd = "pnpm dev:all""#), "{preview}");
    assert_eq!(
        std::fs::read_to_string(e.config_file()).unwrap(),
        first,
        "a preview writes nothing"
    );
    let logged = decision_lines(&e).len();

    let out = e.pando_stdin(&["init", "--answers", "-", "--replace"], CORRECTED);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!("wrote {}", e.config_file().display())),
        "{}",
        stdout(&out)
    );
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        written.contains(r#"cmd = "pnpm dev:all"  # answered: a program,"#),
        "{written}"
    );
    assert!(!written.contains("pnpm dev:web"), "{written}");
    assert_eq!(
        written.matches("[[services]]").count(),
        1,
        "a set is replaced, not appended to: {written}"
    );
    assert!(written.contains(r#"include = ["db"]"#), "{written}");
    assert!(
        written.contains(r#"ports = { WEB_PORT = "web" }"#),
        "a slot the file did not name is left as it was: {written}"
    );
    let lines = decision_lines(&e);
    let new: Vec<(&str, &str)> = lines[logged..]
        .iter()
        .map(|l| (l["slot"].as_str().unwrap(), l["kind"].as_str().unwrap()))
        .collect();
    assert_eq!(new, vec![("dev_cmd", "answer"), ("services", "answer")]);

    // A later command reads both as the program's, not as a person's.
    assert_eq!(code(&e.pando(&["init"])), EXIT_OK);
    assert!(
        !decision_lines(&e).iter().any(|l| l["kind"] == "override"),
        "{:?}",
        decision_lines(&e)
    );
    assert_eq!(status_porcelain(&e.root), "");
}

// The prelude is about the machine, and only a person changes it: an
// answer for it under `--replace` is a refused answer, and nothing in the
// pass is written.
#[test]
fn replace_refuses_an_answered_prelude_and_writes_nothing() {
    let e = env_of(Kind::NextMessy);
    let machine = "[runtime]\nprelude = \"\"\n";
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::write(e.home.join("config.toml"), machine).unwrap();

    let out = e.pando_stdin(
        &["init", "--answers", "-", "--replace"],
        r#"{"prelude": "true", "dev_cmd": "pnpm dev:web"}"#,
    );
    assert_eq!(code(&out), EXIT_USAGE, "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("--replace never changes it"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        std::fs::read_to_string(e.home.join("config.toml")).unwrap(),
        machine
    );
    assert!(!e.config_file().exists());
}

#[test]
fn replace_needs_an_answers_file() {
    let e = env_of(Kind::NextMessy);
    let out = e.pando(&["init", "--replace"]);
    assert_eq!(code(&out), EXIT_USAGE, "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("--answers"), "{}", stderr(&out));
}

// ---- a slot the rules are silent about ------------------------------------
//
// "Every question has a custom answer" has to hold where pando had no
// guess at all, or it quietly becomes "except the ones we had nothing to
// offer for" — and those are exactly the projects that need the caller's
// help most.

#[test]
fn an_answers_file_fills_a_slot_no_rule_proposed_anything_for() {
    // No lockfile, so pando will not propose an install — it never
    // proposes a non-frozen one — and the slot has no proposal at all.
    let e = env_of(Kind::WorkspaceNoLock);
    assert_eq!(
        signals_of(&e)["slots"][0]["proposal"],
        serde_json::Value::Null,
        "this shape is only interesting while the rules stay silent here"
    );

    // The per-app form answers the one question this shape really has,
    // and settles the ports with it; `install` and `schema_hook` are the
    // two the rules never offered anything for. The install command is a
    // step of the project's own, which is the honest shape here: there is
    // no lockfile, so there is no frozen install for a rule to have found.
    let out = e.pando_stdin(
        &["init", "--answers", "-"],
        r#"{"install": "make deps",
            "processes": "api: npm run dev in apps/api; web: npm run dev in apps/web",
            "schema_hook": "npm run db:migrate"}"#,
    );
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("nothing was proposed here"),
        "a guess pando takes is a guess it says out loud: {}",
        stderr(&out)
    );

    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        written.contains(r#"install = "make deps"  # answered: a program,"#),
        "{written}"
    );
    // And the schema answer is a whole [[hooks]] entry, which is the part
    // that used to be accepted and then silently dropped.
    assert!(written.contains("[[hooks]]"), "{written}");
    assert!(
        written.contains(r#"cmd = "npm run db:migrate""#),
        "{written}"
    );

    // Nothing was reported unused, because everything was used.
    assert!(!stderr(&out).contains("was not used"), "{}", stderr(&out));
    assert_eq!(
        code(&e.pando(&["doctor"])),
        EXIT_OK,
        "stderr: {}",
        stderr(&out)
    );
    assert_eq!(status_porcelain(&e.root), "");

    // And afterwards `signals` says the true thing about the slot, which
    // is the half a program reads on its next pass: the rules are still
    // silent here — no rule was written by this — and the question is
    // nevertheless closed. A program that re-read only `proposal` would
    // answer it again on every run.
    let after = signals_of(&e);
    assert_eq!(after["slots"][0]["slot"], "install");
    assert_eq!(after["slots"][0]["proposal"], serde_json::Value::Null);
    assert_eq!(
        after["slots"][0]["answered"],
        serde_json::Value::Bool(true),
        "a slot a program filled is a slot config answers for: {:#?}",
        after["slots"][0]
    );
}

// And it is recorded like every other answer a program gave — with an
// empty option list, which is the corpus saying in so many words that the
// rules had nothing to offer here.
#[test]
fn a_slot_the_rules_were_silent_about_is_recorded_with_no_options() {
    let e = env_of(Kind::WorkspaceNoLock);
    let out = e.pando_stdin(
        &["init", "--answers", "-"],
        r#"{"install": "make deps",
            "processes": "api: npm run dev in apps/api; web: npm run dev in apps/web"}"#,
    );
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let log = decisions_of(&e);
    let install = log
        .iter()
        .find(|d| d["slot"] == "install")
        .unwrap_or_else(|| panic!("{log:#?}"));
    assert_eq!(install["kind"], "answer");
    assert_eq!(install["shape"], "custom");
    assert_eq!(install["answer"], "make deps");
    assert_eq!(install["wrote"], "make deps");
    assert_eq!(
        install["evidence"]["options"].as_array().map(Vec::len),
        Some(0),
        "the rules offered nothing, and the record says so"
    );
    assert_eq!(install["evidence"]["preferred"], serde_json::Value::Null);
}

// Only a program volunteers. A person at a terminal is never asked to
// invent a command out of nothing, and `--yes` has nothing to take.
#[test]
fn a_slot_the_rules_are_silent_about_stays_silent_without_an_answers_file() {
    let e = env_of(Kind::WorkspaceNoLock);
    let out = e.pando(&["init", "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        !written.contains("install ="),
        "no lockfile, no install, and no question about it: {written}"
    );
    assert!(!written.contains("[[hooks]]"), "{written}");
}

// The two that are deliberately out. Both stay exactly as they were: an
// answer nothing used, reported rather than applied.
#[test]
fn the_set_question_and_the_machine_question_are_not_volunteered_for() {
    let e = env_of(Kind::WorkspaceNoLock);
    // With the machine answer taken away, and a shape that pins no
    // runtime at all, the prelude is unanswered *and* unproposed on every
    // host — which is the state a volunteered answer would fill.
    e.unanswer_the_runtime();
    let out = e.pando_stdin(
        &["init", "--answers", "-"],
        r#"{"services": ["postgres"], "prelude": "export FOO=1",
            "processes": "api: npm run dev in apps/api; web: npm run dev in apps/web"}"#,
    );
    assert_eq!(code(&out), EXIT_OK, "stdout: {}", stdout(&out));
    let printed = stderr(&out);
    for name in ["services", "prelude"] {
        assert!(
            printed.contains(&format!(
                "nothing asked about {name} in this run — your answer for it was not used"
            )),
            "{printed}"
        );
    }
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(!written.contains("[[services]]"), "{written}");
    // The machine-wide file is the one a prelude would land in, and this
    // run must not have touched it: a prelude nothing verified can break
    // every command pando spawns.
    let user = std::fs::read_to_string(e.home.join("config.toml")).unwrap_or_default();
    assert!(!user.contains("export FOO=1"), "{user}");
}

// ---- the decisions log ----------------------------------------------------
//
// Every answer a program supplies that the rules could not decide, with
// the evidence it had, and — later — whether a person replaced it. Without
// it the skill that answers these questions is a crutch that hides rule
// weakness from everybody who does not have one.

fn decisions_of(e: &Env) -> Vec<serde_json::Value> {
    let path = e.project_dir().join("decisions.jsonl");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("one JSON object per line"))
        .collect()
}

#[test]
fn an_answers_file_records_what_a_program_decided_and_the_evidence_for_it() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(
        code(&e.pando_stdin(&["init", "--answers", "-"], ANSWERS)),
        EXIT_OK
    );

    let log = decisions_of(&e);
    let slots: Vec<&str> = log.iter().map(|d| d["slot"].as_str().unwrap()).collect();
    assert_eq!(
        slots,
        vec!["dev_cmd", "port_env", "services"],
        "the three the rules could not settle, and nothing a rule decided: {log:#?}"
    );
    assert!(log.iter().all(|d| d["kind"] == "answer"));
    assert!(log.iter().all(|d| d["version"] == 1));

    // The answer is recorded in the shape the answers file sent, so the
    // log can be turned back into one.
    assert_eq!(log[0]["answer"], "pnpm dev:web");
    assert_eq!(log[0]["shape"], "choice");
    assert_eq!(log[2]["answer"], serde_json::json!(["db", "cache"]));
    assert_eq!(log[2]["shape"], "set");

    // And the evidence is what `signals` published for the slot, not a
    // summary written afterwards: a corpus whose evidence column is a
    // paraphrase cannot be used to test a rule against the case it came
    // from.
    let evidence = &log[0]["evidence"];
    assert!(
        evidence["prompt"]
            .as_str()
            .unwrap()
            .contains("development server")
    );
    let offered: Vec<&str> = evidence["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["value"].as_str().unwrap())
        .collect();
    assert_eq!(
        offered,
        vec![
            "pnpm dev",
            "pnpm dev:all",
            "pnpm dev:web",
            "pnpm dev:worker"
        ]
    );
    assert_eq!(evidence["options"][0]["why"], "package.json scripts.dev");
    assert_eq!(
        evidence["preferred"], 0,
        "and what the rules would have taken"
    );

    // `wrote` is what config says about the slot afterwards, read back
    // from the file. It is what the next run compares against.
    assert_eq!(log[0]["wrote"], "pnpm dev:web");
}

// The trap under the override check: `wrote` is read back through
// `config::load`, and if it were taken from the config in hand instead,
// every later run would report a person changing an answer nobody
// touched.
#[test]
fn a_second_run_that_changes_nothing_records_no_override() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(
        code(&e.pando_stdin(&["init", "--answers", "-"], ANSWERS)),
        EXIT_OK
    );
    let after_first = decisions_of(&e);

    for _ in 0..2 {
        let out = e.pando(&["init"]);
        assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    }
    // And a command that resolves on its way to doing something else.
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);

    assert_eq!(
        decisions_of(&e),
        after_first,
        "nothing changed, so there is nothing to record"
    );
}

// The label the whole file exists for: an answer nobody corrected is weak
// evidence that it was right, and one somebody replaced is strong
// evidence that it was wrong.
#[test]
fn an_answer_a_person_replaced_is_recorded_once_as_an_override() {
    let e = env_of(Kind::NextMessy);
    assert_eq!(
        code(&e.pando_stdin(&["init", "--answers", "-"], ANSWERS)),
        EXIT_OK
    );
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    std::fs::write(
        e.config_file(),
        written.replace(r#"cmd = "pnpm dev:web""#, r#"cmd = "pnpm dev:all""#),
    )
    .unwrap();

    let out = e.pando(&["init"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("not what a program answered any more"),
        "a guess pando records is a guess it says out loud: {}",
        stderr(&out)
    );

    let log = decisions_of(&e);
    let overrides: Vec<&serde_json::Value> =
        log.iter().filter(|d| d["kind"] == "override").collect();
    assert_eq!(overrides.len(), 1, "{log:#?}");
    assert_eq!(overrides[0]["slot"], "dev_cmd");
    assert_eq!(overrides[0]["was"], "pnpm dev:web");
    assert_eq!(overrides[0]["now"], "pnpm dev:all");

    // Once, not on every run afterwards: the override is itself the last
    // word about the slot.
    assert_eq!(code(&e.pando(&["init"])), EXIT_OK);
    assert_eq!(decisions_of(&e).len(), log.len());
}

// The comparison is on the slot's *answer*, not on the file.
//
// `processes` is answered with a shape — a process per app, or the root
// script — so its value is the process names. Switching between the two
// forms is the decision being overturned and is recorded; tuning a
// command inside the form that was chosen is not, and a corpus about
// which shape a program picked is better off without it.
#[test]
fn an_edit_inside_the_shape_a_program_chose_is_not_an_override() {
    let e = env_of(Kind::WorkspaceNoLock);
    let answers = r#"{"processes": "api: npm run dev in apps/api; web: npm run dev in apps/web"}"#;
    assert_eq!(
        code(&e.pando_stdin(&["init", "--answers", "-"], answers)),
        EXIT_OK
    );
    assert_eq!(decisions_of(&e).len(), 1);

    // A command tuned inside the shape that was chosen.
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    std::fs::write(
        e.config_file(),
        written.replace(r#"cmd = "npm run dev""#, r#"cmd = "npm run dev --silent""#),
    )
    .unwrap();
    assert_eq!(code(&e.pando(&["init"])), EXIT_OK);
    assert_eq!(
        decisions_of(&e).len(),
        1,
        "the shape is still the one the program chose"
    );

    // And now the shape itself: the per-app table for the root script.
    std::fs::write(
        e.config_file(),
        "[processes.dev]\ncmd = \"npm run dev\"\n\n[project]\nprovision = [\".env\"]\n",
    )
    .unwrap();
    let out = e.pando(&["init"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let log = decisions_of(&e);
    assert_eq!(log.len(), 2, "{log:#?}");
    assert_eq!(log[1]["kind"], "override");
    assert_eq!(log[1]["was"], "api, web");
    assert_eq!(log[1]["now"], "dev");
}

// Only what a program decided. A rule that settled a slot is not a gap in
// the rules, and `--yes` taking what the rules preferred is not one
// either — recording those would bury the lines that mean something.
#[test]
fn nothing_is_recorded_for_a_project_the_rules_understand() {
    let e = env_of(Kind::NextPnpmCompose);
    assert_eq!(code(&e.pando(&["init", "--yes"])), EXIT_OK);
    assert!(
        !e.project_dir().join("decisions.jsonl").exists(),
        "a project the rules understand leaves no decisions to record"
    );

    // And a flag answering for a developer is not a program deciding.
    let e = env_of(Kind::NextMessy);
    assert_eq!(code(&e.pando(&["init", "--yes"])), EXIT_OK);
    assert!(decisions_of(&e).is_empty());
}

// ---- signals --------------------------------------------------------------

fn signals_of(e: &Env) -> serde_json::Value {
    let out = e.pando(&["signals"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    serde_json::from_str(&stdout(&out)).expect("signals parses as JSON")
}

#[test]
fn signals_is_json_identical_on_two_runs_and_writes_nothing() {
    let e = env_of(Kind::NextPnpmCompose);
    let first = e.pando(&["signals"]);
    let second = e.pando(&["signals"]);
    assert_eq!(code(&first), EXIT_OK, "stderr: {}", stderr(&first));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "detection is read-only, so two runs say the same thing"
    );
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&first)).unwrap();
    // Pinned as a literal: a version bump must fail here, at the commit
    // that makes it, rather than passing quietly.
    assert_eq!(parsed["version"], 2, "versioned like the other shapes");
    assert_eq!(parsed["project"]["name"], "next-pnpm-compose");
    assert!(
        !e.config_file().exists(),
        "signals reads; it never answers anything"
    );
    assert_eq!(status_porcelain(&e.root), "");
}

// Completeness over brevity: what the rules considered, why, and what they
// would take — including the two things the last two work items added.
#[test]
fn signals_publishes_every_proposal_with_its_reasons() {
    let e = env_of(Kind::NextPnpmCompose);
    let signals = signals_of(&e);

    let slot = |name: &str| {
        signals["slots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["slot"] == name)
            .unwrap_or_else(|| panic!("no slot named {name}"))
            .clone()
    };

    let install = slot("install");
    assert_eq!(
        install["prompt"],
        "Which command installs this project's dependencies?"
    );
    assert_eq!(install["answered"], false);
    assert_eq!(install["proposal"]["decided"], true);
    assert_eq!(install["proposal"]["preferred"], 0);
    assert_eq!(
        install["proposal"]["candidates"][0]["value"],
        "pnpm install --frozen-lockfile"
    );
    assert_eq!(
        install["proposal"]["candidates"][0]["why"],
        "pnpm-lock.yaml"
    );

    // The set question publishes what starts ticked and that it is a set,
    // which is exactly what an answers file needs to answer it.
    let services = slot("services");
    assert_eq!(services["proposal"]["multi"], true);
    assert_eq!(services["proposal"]["allow_custom"], false);
    assert_eq!(services["proposal"]["allow_none"], true);
    assert_eq!(services["proposal"]["file"], "docker-compose.yml");
    assert_eq!(
        services["proposal"]["checked"].as_array().unwrap().len(),
        2,
        "postgres and redis are resolved; the mail catcher is not"
    );
    let postgres = services["proposal"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["value"] == "postgres")
        .unwrap()
        .clone();
    assert_eq!(postgres["service"]["env_key"], "DATABASE_URL");
    assert_eq!(postgres["service"]["file"], "docker-compose.yml");

    // The whole `[[hooks]]` entry, not only its command.
    let hook = slot("schema_hook");
    assert_eq!(hook["proposal"]["candidates"][0]["hook"]["name"], "migrate");
    assert_eq!(
        hook["proposal"]["candidates"][0]["hook"]["after"],
        "services"
    );

    // A slot no rule had anything to say about is present and empty,
    // which is not the same as a rule deciding the answer is "none".
    assert_eq!(slot("processes")["proposal"], serde_json::Value::Null);

    // The runtime requirement is a project fact and is published; the
    // machine's answer to it is not, because asking costs a spawn.
    assert_eq!(
        signals["signals"]["runtime_requirements"][0]["language"],
        "node"
    );
    assert_eq!(
        signals["signals"]["runtime_requirements"][0]["source"],
        ".nvmrc"
    );

    // And what this build's own compose reader saw.
    assert_eq!(signals["compose"][0]["file"], "docker-compose.yml");
    assert_eq!(signals["compose"][0]["include"], false);
    assert!(signals["compose"][0]["error"].is_null());
}

// A clone with no local files is the case the provision seeds exist for.
#[test]
fn signals_carries_the_provision_seeds_of_a_fresh_clone() {
    let e = env_fresh_clone(Kind::NextPnpmCompose);
    let signals = signals_of(&e);
    let seeds = signals["signals"]["provision_seeds"].as_array().unwrap();
    assert!(
        seeds
            .iter()
            .any(|pair| pair[0] == ".env" && pair[1] == ".env.example"),
        "{seeds:?}"
    );
}

// The keys pando does not follow are published, so an agent can tell a
// missing services proposal from a project with no services.
#[test]
fn signals_says_what_its_compose_reader_could_not_follow() {
    let e = env_of(Kind::Plain);
    std::fs::write(
        e.root.join("docker-compose.yml"),
        "include:\n  - other.yml\nservices:\n  db:\n    image: postgres:16\n",
    )
    .unwrap();
    git(&e.root, &["add", "."]);
    git(&e.root, &["commit", "--quiet", "-m", "compose"]);

    let signals = signals_of(&e);
    assert_eq!(signals["compose"][0]["include"], true);
    assert_eq!(signals["compose"][0]["services"][0], "db");
}

// A config that declares its processes has answered the dev command and
// its ports, and the resolver never asks either. Published as open, a
// program that watches `answered` sent them again on every run and was
// told each time that nothing asked about them.
#[test]
fn signals_publishes_the_dev_command_answered_when_config_declares_its_processes() {
    let e = env_of(Kind::NextMessy);
    e.write_config(
        "[processes.web]\ncmd = \"pnpm dev:web\"\nports = [\"web\"]\n\n\
         [processes.api]\ncmd = \"pnpm dev:api\"\nports = [\"api\"]\n",
    );
    let signals = signals_of(&e);
    for name in ["dev_cmd", "port_env"] {
        let slot = signals["slots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["slot"] == name)
            .unwrap_or_else(|| panic!("no slot named {name}"));
        assert_eq!(slot["answered"], true, "{slot:#}");
    }

    // A preview, so the questions this fixture still has stay open. The
    // refusal points at the answer that covers the slot: `--replace` on
    // `dev_cmd` alone would have nothing to replace here.
    let out = e.pando_stdin(
        &["init", "--answers", "-", "--dry-run"],
        r#"{"dev_cmd": "pnpm dev"}"#,
    );
    assert_eq!(code(&out), EXIT_USAGE, "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains(
            "dev_cmd is already answered, so your answer was not applied — the processes \
             answer covers it"
        ),
        "{}",
        stderr(&out)
    );
}

// The names `signals` publishes are the names an answers file uses,
// because both are the slot's own. Proven by answering what it reports.
#[test]
fn the_questions_signals_names_are_the_ones_an_answers_file_answers() {
    let e = env_of(Kind::NextMessy);
    let signals = signals_of(&e);

    let mut answers = serde_json::Map::new();
    for slot in signals["slots"].as_array().unwrap() {
        let proposal = &slot["proposal"];
        if proposal.is_null() || proposal["decided"] == true || slot["answered"] == true {
            continue;
        }
        let candidates = proposal["candidates"].as_array().unwrap();
        let name = slot["slot"].as_str().unwrap().to_string();
        if proposal["multi"] == true {
            let checked: Vec<serde_json::Value> = proposal["checked"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| candidates[i.as_u64().unwrap() as usize]["value"].clone())
                .collect();
            answers.insert(name, serde_json::Value::Array(checked));
            continue;
        }
        let preferred = proposal["preferred"].as_u64().expect("an option to take") as usize;
        answers.insert(name, candidates[preferred]["value"].clone());
    }
    assert!(!answers.is_empty(), "this fixture has questions to answer");

    let sent = serde_json::Value::Object(answers).to_string();
    let out = e.pando_stdin(&["init", "--answers", "-"], &sent);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        !stderr(&out).contains("not used"),
        "every answer it published a question for was used: {}",
        stderr(&out)
    );
    let written = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(written.contains("# answered: a program,"), "{written}");
}

// ---- the way back, and a pump that died -----------------------------------

#[test]
fn isolated_and_shared_together_is_a_usage_error() {
    let e = env();
    let out = e.pando(&["start", "feat+one", "--isolated", "--shared"]);
    assert_eq!(code(&out), EXIT_USAGE, "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("--isolated"),
        "the pair is named: {}",
        stderr(&out)
    );
}

#[test]
fn start_shared_puts_a_worktree_back_on_the_projects_own_services() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not installed");
        return;
    }
    let e = env_isolated();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(
        code(&e.pando(&["start", "feat+one", "--isolated"])),
        EXIT_OK
    );
    let before: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "feat+one", "--json"]))).unwrap();
    assert_eq!(before["worktrees"][0]["isolated"], serde_json::json!(true));

    let out = e.pando(&["start", "feat+one", "--shared"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));

    let after: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "feat+one", "--json"]))).unwrap();
    let worktree = &after["worktrees"][0];
    assert_eq!(worktree["isolated"], serde_json::json!(false));
    assert_eq!(
        worktree["services"]["postgres"]["up"],
        serde_json::json!(false),
        "the private services are down: {after}"
    );
    assert_eq!(
        worktree["ports"]["web"], before["worktrees"][0]["ports"]["web"],
        "and the application keeps the port it was bookmarked on"
    );

    // `stop` never changes the mode: only a start says which one it is.
    assert_eq!(code(&e.pando(&["stop", "feat+one"])), EXIT_OK);
    let stopped: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "feat+one", "--json"]))).unwrap();
    assert_eq!(
        stopped["worktrees"][0]["isolated"],
        serde_json::json!(false)
    );

    assert_eq!(code(&e.pando(&["rm", "feat+one", "--force"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

// A container answering while nothing fills its log tab is the one case
// worth a word, and `status` is where it gets one.
#[test]
fn status_says_when_a_service_is_up_with_no_log_pump() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not installed");
        return;
    }
    let e = env_isolated();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(
        code(&e.pando(&["start", "feat+one", "--isolated"])),
        EXIT_OK
    );
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "feat+one", "--json"]))).unwrap();
    assert_eq!(
        json["worktrees"][0]["services"]["postgres"]["logging"],
        serde_json::json!(true),
        "{json}"
    );
    assert!(
        !stdout(&e.pando(&["status", "feat+one"])).contains("no log pump"),
        "nothing to report while it is running"
    );

    // Kill the pump the way a crash would, and read again.
    let store = pando::state::load(&e.project_dir().join("state.json")).unwrap();
    let pump = store.worktrees["feat+one"]
        .services
        .iter()
        .find(|s| s.name == "postgres")
        .expect("a postgres record")
        .clone();
    pando::process::stop(pump.pgid.unwrap(), std::time::Duration::from_secs(2)).unwrap();

    let json: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "feat+one", "--json"]))).unwrap();
    let postgres = &json["worktrees"][0]["services"]["postgres"];
    assert_eq!(postgres["up"], serde_json::json!(true), "{json}");
    assert_eq!(
        postgres["logging"],
        serde_json::json!(false),
        "the log tab has stopped filling: {json}"
    );
    assert!(
        stdout(&e.pando(&["status", "feat+one"])).contains("no log pump"),
        "and the text says so: {}",
        stdout(&e.pando(&["status", "feat+one"]))
    );

    // `start` is what puts it back — and a read path never did.
    let again = e.pando(&["start", "feat+one"]);
    assert_eq!(code(&again), EXIT_OK, "stderr: {}", stderr(&again));
    assert_eq!(code(&again), EXIT_OK, "stderr: {}", stderr(&again));
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["status", "feat+one", "--json"]))).unwrap();
    assert_eq!(
        json["worktrees"][0]["services"]["postgres"]["logging"],
        serde_json::json!(true),
        "{json}"
    );

    assert_eq!(code(&e.pando(&["rm", "feat+one", "--force"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

// ---- doctor ---------------------------------------------------------------

#[test]
fn doctor_on_a_project_with_nothing_wrong_exits_zero_and_names_every_layer() {
    let e = env();
    // Something to run: a project with nothing is a note, not a clean bill.
    e.write_config("[dev]\ncmd = \"true\"\n");
    let out = e.pando(&["doctor"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    for line in ["project", "config"] {
        assert!(
            text.lines().any(|l| l == line),
            "{line} is missing:\n{text}"
        );
    }
    assert!(text.contains("committed"), "{text}");
    assert!(text.contains("user"), "{text}");
    assert!(
        text.contains(&e.config_file().display().to_string()),
        "the project layer is named:\n{text}"
    );
    // Said once: at the top, not again at the bottom.
    assert_eq!(text.matches("nothing to report").count(), 1, "{text}");
}

// A library: nothing detected, nothing configured. doctor says so, with
// the file and the lines that would fix it, and `start` says the same.
#[test]
fn doctor_and_start_say_a_project_has_nothing_to_run() {
    let e = env_of(Kind::RustLib);
    let out = e.pando(&["doctor"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("nothing to run"), "{text}");
    assert!(
        text.contains(&e.config_file().display().to_string()),
        "{text}"
    );
    assert!(!text.contains("nothing to report"), "{text}");
}

#[test]
fn doctor_exits_one_for_a_problem_it_printed_and_says_nothing_else() {
    let e = env();
    e.write_config("[project]\ninstall = \"pnpm install\"\n\n[dev]\ncmd = \"true\"\n");
    // A pnpm on the PATH, or a machine without one has two problems: the
    // one this test is about, and the missing tool.
    common::fake_pnpm(&e.home);
    e.write_user_config(&format!(
        "[runtime]\nprelude = 'export PATH=\"{}:$PATH\"'\n",
        e.home.join("bin").display()
    ));
    let out = e.pando(&["doctor"]);
    assert_eq!(code(&out), EXIT_ERROR, "stdout: {}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("non-frozen install"), "{text}");
    assert!(
        text.contains("pnpm install --frozen-lockfile"),
        "the fix is printed: {text}"
    );
    assert!(text.contains("1 problem, 0 notes"), "{text}");
    // Exit 1 is the whole message. A `pando: …` line under the report
    // would be a reason the report did not give.
    assert!(!stderr(&out).contains("pando:"), "stderr: {}", stderr(&out));
}

#[test]
fn doctor_still_runs_when_the_project_layer_cannot_be_loaded() {
    let e = env();
    e.write_config("[project]\na_key_pando_has_never_heard_of = 1\n");
    let out = e.pando(&["doctor"]);
    assert_eq!(code(&out), EXIT_ERROR);
    let text = stdout(&out);
    assert!(text.contains("the config does not load"), "{text}");
    assert!(
        text.contains("a_key_pando_has_never_heard_of"),
        "and the file is still shown, key by key:\n{text}"
    );
}

/// The gap a first contact left: an improved rule that could not reach
/// the developer who needed it, because the bad value it replaced was
/// already in their config and "ask once" meant nobody asked again.
///
/// The shape is the honest one and nothing more: a Makefile target with
/// a prerequisite, whose first recipe line is a guard that exits before
/// anything starts, and a config holding that line as the dev command.
#[test]
fn doctor_names_a_detected_value_the_rules_would_not_write_now() {
    let e = env();
    std::fs::write(
        e.root.join("Makefile"),
        concat!(
            "build:\n\t./scripts/build.sh\n\n",
            "dev: build\n",
            "\tcommand -v watcher >/dev/null || { echo \"install watcher first\"; exit 1; }\n",
            "\t./scripts/serve.sh --reload\n",
        ),
    )
    .unwrap();
    e.write_config(
        "[dev]\ncmd = 'command -v watcher >/dev/null || { echo \"install watcher first\"; \
         exit 1; }'  # detected: the dev target\n",
    );

    let out = e.pando(&["doctor"]);
    // A note: a stale detection is suspicious, not broken, so it does not
    // fail the shell on its own.
    assert_eq!(code(&out), EXIT_OK, "stdout: {}", stdout(&out));
    let text = stdout(&out);
    assert!(text.contains("dev.cmd"), "{text}");
    assert!(text.contains("pando detected itself"), "{text}");
    assert!(
        text.contains("command -v watcher"),
        "what it holds now:\n{text}"
    );
    assert!(text.contains("make dev"), "what it would detect:\n{text}");
    assert!(
        text.contains("delete that line"),
        "and the fix that reopens the question:\n{text}"
    );
    assert!(text.contains("0 problems, 1 note"), "{text}");

    // And in the shape an agent reads, as a finding like any other.
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["doctor", "--json"]))).expect("one object");
    assert_eq!(json["ok"], true);
    let finding = json["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| {
            f["message"]
                .as_str()
                .is_some_and(|m| m.contains("pando detected itself"))
        })
        .unwrap_or_else(|| panic!("{}", json["findings"]));
    assert_eq!(finding["section"], "config");
    assert_eq!(finding["severity"], "note");
    assert!(
        finding["fix"]
            .as_str()
            .is_some_and(|f| f.contains("delete")),
        "{finding}"
    );

    // Answering it by hand closes it: pando does not second-guess a
    // decision, whatever its rules would say.
    e.write_config(
        "[dev]\ncmd = 'command -v watcher >/dev/null || { echo \"install watcher first\"; \
         exit 1; }'  # answered: 2026-09-22\n",
    );
    let after = stdout(&e.pando(&["doctor"]));
    assert!(!after.contains("pando detected itself"), "{after}");
}

/// The reporter's run, end to end: an Expo app whose `dev.ports` an older
/// pando gave the browser's role. Deleting the line, as doctor used to
/// say, asked nothing and left Metro on 8081 in every worktree; the fix
/// doctor prints now, run as printed, leaves nothing to report and the
/// process with its port.
#[test]
fn the_command_doctor_prints_for_a_stale_port_fixes_it() {
    let e = env();
    write_expo_app(&e);
    e.write_config(
        "[dev]\ncmd = \"npm run start\"  # detected: package.json scripts.start\n\
         ports = { RCT_METRO_PORT = \"web\" }  # detected: the Expo rule\n",
    );
    run_printed(&e, &doctor_fix(&e, "dev.ports = "));
    assert_metro_has_its_port(&e);
}

/// And with the line already deleted: doctor says so, as a problem, and
/// its command fixes that too.
#[test]
fn the_command_doctor_prints_for_a_server_with_no_port_fixes_it() {
    let e = env();
    write_expo_app(&e);
    e.write_config("[dev]\ncmd = \"npm run start\"  # detected: package.json scripts.start\n");
    assert_eq!(code(&e.pando(&["doctor"])), EXIT_ERROR);
    run_printed(&e, &doctor_fix(&e, "process \"dev\" runs Expo"));
    assert_metro_has_its_port(&e);
}

/// An Expo app at the root of `e`, as its template makes one.
fn write_expo_app(e: &Env) {
    std::fs::write(
        e.root.join("package.json"),
        r#"{"name":"shop","dependencies":{"expo":"57.0.0"},"scripts":{"start":"expo start"}}"#,
    )
    .unwrap();
    std::fs::write(e.root.join("app.json"), r#"{"expo":{"slug":"shop"}}"#).unwrap();
}

/// The fix of the one finding whose message starts with `lead`, as
/// `doctor --json` gives it.
fn doctor_fix(e: &Env, lead: &str) -> String {
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["doctor", "--json"]))).expect("one object");
    json["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .find(|f| f["message"].as_str().is_some_and(|m| m.starts_with(lead)))
        .and_then(|f| f["fix"].as_str())
        .unwrap_or_else(|| panic!("{}", json["findings"]))
        .to_string()
}

/// Metro has its port in pando's config, and doctor has nothing to say.
fn assert_metro_has_its_port(e: &Env) {
    let config = std::fs::read_to_string(e.config_file()).unwrap();
    assert!(
        config.contains(r#"ports = { RCT_METRO_PORT = "metro" }"#),
        "{config}"
    );
    let out = e.pando(&["doctor"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stdout(&out));
    assert!(
        stdout(&out).contains("nothing to report"),
        "{}",
        stdout(&out)
    );
}

/// Runs the command a fix gives between its first backquotes, as printed:
/// through a shell, with this test's pando first on PATH.
fn run_printed(e: &Env, fix: &str) {
    let command = fix.split('`').nth(1).unwrap_or_else(|| panic!("{fix}"));
    assert!(
        command.ends_with("| pando init --answers - --replace"),
        "{command}"
    );
    let bin = std::path::Path::new(env!("CARGO_BIN_EXE_pando"))
        .parent()
        .unwrap()
        .to_path_buf();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = Command::new("sh")
        .args(["-c", command])
        .current_dir(&e.root)
        .env("PANDO_HOME", &e.home)
        .env("PATH", path)
        .output()
        .unwrap();
    assert_eq!(code(&out), EXIT_OK, "{}{}", stdout(&out), stderr(&out));
}

/// `agent/json.md` describes the provenance comment on a config key, and
/// it is one of the few strings in the JSON an agent is told to branch
/// on. It documented the spelling without the `#` the note is actually
/// copied out of the file with, and nothing held the document to the
/// binary.
#[test]
fn the_documented_provenance_note_is_the_one_doctor_prints() {
    let e = env();
    e.write_config("[project]\ninstall = \"true\"  # detected: a rule that once existed\n");
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&e.pando(&["doctor", "--json"]))).expect("one object");
    let note = json["config"]["layers"]
        .as_array()
        .expect("layers")
        .iter()
        .flat_map(|layer| layer["keys"].as_array().cloned().unwrap_or_default())
        .find(|key| key["key"] == "project.install")
        .and_then(|key| key["note"].as_str().map(str::to_string))
        .expect("the note on project.install");
    assert_eq!(note, "# detected: a rule that once existed");

    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"))
        .expect("read agent/json.md");
    assert!(
        doc.contains("`# detected: <evidence>`"),
        "agent/json.md documents a spelling the binary does not print"
    );
    assert!(
        !doc.contains("\"note\": \"detected:"),
        "the example in agent/json.md drops the comment's own `#`"
    );
}

/// The check that a config pando *just wrote* is never one it complains
/// about. It closes the loop the whole finding rests on: the same
/// candidate list writes the value and is later asked whether the value
/// is still among them, so any disagreement between `detect::edits` and
/// what doctor reads back is a contradiction, and this is where it
/// surfaces.
///
/// Every fixture shape, because the shapes differ in exactly the way
/// that would break it: a framework command that carries its own port
/// role writes two keys under one note, a workspace writes whole tables,
/// and a project with nothing to run writes no dev command at all.
#[test]
fn a_config_pando_just_wrote_never_reads_as_a_stale_detection() {
    // A run that wrote no `# detected:` line at all would pass this
    // without testing anything, so the loop counts what it compared.
    let mut compared = 0usize;
    for kind in common::Kind::ALL {
        let e = env_of(kind);
        // Whatever `--yes` can settle without a person. It may stop at a
        // question it is not allowed to answer; what it wrote before
        // stopping is the config under test either way.
        e.pando(&["init", "--yes"]);
        let json: serde_json::Value =
            serde_json::from_str(&stdout(&e.pando(&["doctor", "--json"]))).expect("one object");
        let stale: Vec<&str> = json["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .filter_map(|f| f["message"].as_str())
            .filter(|m| m.contains("pando detected itself"))
            .collect();
        assert!(
            stale.is_empty(),
            "{}: pando reported a value it had just written: {stale:?}",
            kind.dir_name()
        );
        compared += json["config"]["layers"]
            .as_array()
            .expect("layers")
            .iter()
            .flat_map(|layer| layer["keys"].as_array().cloned().unwrap_or_default())
            .filter(|key| {
                key["value"].is_string()
                    && key["note"]
                        .as_str()
                        .is_some_and(|note| note.starts_with("# detected:"))
            })
            .count();
    }
    assert!(
        compared > 0,
        "no fixture wrote a detected value, so nothing here was compared"
    );
}

#[test]
fn doctor_writes_nothing_and_does_not_create_a_home() {
    let dir = TempDir::new().unwrap();
    let root = build(Kind::Plain, dir.path()).root;
    let home = dir.path().join("pando-home");
    let out = Command::new(env!("CARGO_BIN_EXE_pando"))
        .env("PANDO_HOME", &home)
        .current_dir(&root)
        .arg("doctor")
        .output()
        .expect("run pando");
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        !home.exists(),
        "doctor created {} — it reads, it does not write",
        home.display()
    );
    assert_eq!(status_porcelain(&root), "");
}

#[test]
fn doctor_reports_the_runtime_this_project_pins_and_where_it_says_so() {
    let e = env_of(Kind::NextPnpmCompose);
    let out = e.pando(&["doctor"]);
    let text = stdout(&out);
    assert!(text.lines().any(|l| l == "runtime"), "{text}");
    // Read out of the repository, so this says the same thing on every
    // machine — unlike what the shell resolves, which is the point of the
    // section and is never asserted on here.
    assert!(text.contains("wants 22 (.nvmrc)"), "{text}");
    assert!(
        text.contains("`bash -lc`"),
        "and it names the shell pando really uses:\n{text}"
    );
}

#[test]
fn doctor_reports_the_docker_it_would_use_and_the_context_it_is_on() {
    let e = env_of(Kind::NextPnpmCompose);
    common::docker::install(&e.home);
    let out = e.pando(&["doctor"]);
    let text = stdout(&out);
    assert!(text.lines().any(|l| l == "tools"), "{text}");
    assert!(text.contains("Docker version 27.0.0-fake"), "{text}");
    assert!(text.contains("context: fake-context"), "{text}");
    assert!(text.contains("2.29.0-fake"), "compose too:\n{text}");
    assert!(
        text.contains(&e.home.join("bin").join("docker").display().to_string()),
        "and the path it resolved from, which is the shim:\n{text}"
    );
}

#[test]
fn doctor_flags_a_dead_log_pump_beside_a_service_that_is_up() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not installed");
        return;
    }
    let e = env_isolated();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    assert_eq!(
        code(&e.pando(&["start", "feat+one", "--isolated"])),
        EXIT_OK
    );
    assert!(
        !stdout(&e.pando(&["doctor"])).contains("log pump died"),
        "nothing to report while it is running"
    );

    let store = pando::state::load(&e.project_dir().join("state.json")).unwrap();
    let pump = store.worktrees["feat+one"]
        .services
        .iter()
        .find(|s| s.name == "postgres")
        .expect("a postgres record")
        .clone();
    pando::process::stop(pump.pgid.unwrap(), std::time::Duration::from_secs(2)).unwrap();

    let out = e.pando(&["doctor"]);
    let text = stdout(&out);
    assert!(text.lines().any(|l| l == "worktrees"), "{text}");
    assert!(text.contains("its log pump died"), "{text}");
    assert!(
        text.contains("`pando start feat+one` puts it back"),
        "and what to do about it:\n{text}"
    );
    // A note, not a problem: the containers are up and a start fixes it.
    //
    // The *exit code* is not asserted here on purpose. This fixture pins
    // `.nvmrc` 22, and whether this machine's `bash -lc` resolves that is
    // a fact about the machine — so doctor's exit code on it is one too.
    assert!(
        text.lines()
            .any(|l| l.starts_with("  - ") && l.contains("its log pump died")),
        "a dash, not a bang:\n{text}"
    );
    // And the services section says how readiness is decided.
    assert!(text.lines().any(|l| l == "services"), "{text}");
    assert!(text.contains("ready by connect"), "{text}");

    assert_eq!(code(&e.pando(&["rm", "feat+one", "--force"])), EXIT_OK);
    assert_eq!(status_porcelain(&e.root), "");
}

#[test]
fn doctor_reports_a_hook_and_the_worktrees_it_has_run_in() {
    let e = env_of(Kind::NextPnpmCompose);
    e.write_config(
        "[project]\nprovision = [\".env\"]\ninstall = \"true\"\n\n\
         [dev]\ncmd = \"sleep 30\"\nports = []\n\n\
         [[hooks]]\nname = \"migrate\"\nafter = \"install\"\n\
         fingerprint = [\"prisma/migrations/**\"]\ncmd = \"true\"\n",
    );
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let text = stdout(&e.pando(&["doctor"]));
    assert!(text.lines().any(|l| l == "hooks"), "{text}");
    assert!(text.contains("after install: true"), "{text}");
    assert!(
        text.contains("prisma/migrations/**"),
        "the globs it is keyed on:\n{text}"
    );
    assert_eq!(code(&e.pando(&["rm", "feat+one", "--force"])), EXIT_OK);
}

#[test]
fn doctor_adopts_the_project_folder_of_a_repository_that_moved() {
    let e = env();
    assert_eq!(code(&e.pando(&["new", "feat/one"])), EXIT_OK);
    let old_id = pando::project::ProjectRef::from_root(&e.root).unwrap().id;
    assert!(e.home.join("projects").join(&old_id).is_dir());

    // Move the repository, keeping its directory name — the id is that
    // name plus a hash of the path, so this is exactly what a move looks
    // like from pando's side.
    let elsewhere = e.root.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let moved = elsewhere.join(e.root.file_name().unwrap());
    std::fs::rename(&e.root, &moved).unwrap();
    let new_id = pando::project::ProjectRef::from_root(&moved).unwrap().id;
    assert_ne!(new_id, old_id);

    // doctor offers it, naming the folder and the repository it came from.
    let out = e.pando_in(&moved, &["doctor"]);
    let text = stdout(&out);
    assert!(text.lines().any(|l| l == "adoption"), "{text}");
    assert!(text.contains(&old_id), "{text}");
    assert!(text.contains("feat+one"), "{text}");

    // Without --yes and with no terminal to ask, nothing moves.
    let refused = e.pando_in(&moved, &["doctor", "--adopt", &old_id]);
    assert_eq!(code(&refused), EXIT_ERROR);
    assert!(
        stderr(&refused).contains("nothing was moved"),
        "{}",
        stderr(&refused)
    );
    assert!(e.home.join("projects").join(&old_id).is_dir());

    let out = e.pando_in(&moved, &["doctor", "--adopt", &old_id, "--yes"]);
    assert_eq!(code(&out), EXIT_OK, "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains(&format!("adopted {old_id} as {new_id}")),
        "{}",
        stdout(&out)
    );
    assert!(!e.home.join("projects").join(&old_id).exists());

    // Config, state and worktrees all came with it.
    let project = e.home.join("projects").join(&new_id);
    assert!(project.join("pando.toml").is_file(), "the config");
    assert!(project.join("worktrees/feat+one").is_dir(), "the worktree");
    let store = pando::state::load(&project.join("state.json")).unwrap();
    assert_eq!(
        store.worktrees["feat+one"].path,
        std::fs::canonicalize(project.join("worktrees/feat+one")).unwrap(),
        "the recorded path moved with the folder"
    );

    // And pando finds it again from the repository's new home.
    let ls = e.pando_in(&moved, &["ls"]);
    assert_eq!(code(&ls), EXIT_OK, "stderr: {}", stderr(&ls));
    assert!(stdout(&ls).contains("feat/one"), "{}", stdout(&ls));
    let path = e.pando_in(&moved, &["path", "feat+one"]);
    assert_eq!(code(&path), EXIT_OK, "stderr: {}", stderr(&path));
    assert!(stdout(&path).contains(&new_id), "{}", stdout(&path));

    // git agrees, which is what `git worktree repair` was for.
    let listed = common::git_raw(&moved, &["worktree", "list", "--porcelain"]);
    let listed = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(listed.contains(&new_id), "{listed}");
    assert!(!listed.contains("prunable"), "{listed}");

    // Nothing left to adopt, and nothing to report about the move.
    let after = stdout(&e.pando_in(&moved, &["doctor"]));
    assert!(after.contains("nothing to adopt"), "{after}");

    assert_eq!(
        code(&e.pando_in(&moved, &["rm", "feat+one", "--force"])),
        EXIT_OK
    );
    assert_eq!(status_porcelain(&moved), "");
}

#[test]
fn doctor_json_is_versioned_and_carries_every_section() {
    let e = env();
    let out = e.pando(&["doctor", "--json"]);
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&out)).expect("doctor --json parses as JSON");
    // Pinned as a literal: a version bump must fail here, at the commit
    // that makes it, rather than passing quietly.
    assert_eq!(json["version"], serde_json::json!(2));
    assert_eq!(
        json["ok"],
        serde_json::json!(code(&out) == EXIT_OK),
        "`ok` is the exit code as a value: {json}"
    );
    for section in [
        "project",
        "config",
        "runtime",
        "tools",
        "worktrees",
        "services",
        "hooks",
        "adoption",
        "findings",
    ] {
        assert!(!json[section].is_null(), "{section} is missing from {json}");
    }
    assert_eq!(
        json["project"]["id"],
        e.project_dir().file_name().unwrap().to_str().unwrap()
    );
    assert!(
        json["config"]["layers"].as_array().unwrap().len() == 3,
        "{json}"
    );
    assert_eq!(json["config"]["layers"][1]["layer"], "user");
    // The one key the harness wrote, with the file it is in.
    assert_eq!(
        json["config"]["layers"][1]["keys"][0]["key"],
        "runtime.prelude"
    );
}

#[test]
fn doctor_json_says_the_same_thing_the_text_does() {
    let e = env();
    // An npm on the PATH, or a machine without one has two problems, as
    // with pnpm in `doctor_exits_one_for_a_problem_it_printed_and_says_nothing_else`.
    common::fake_npm(&e.home);
    e.write_user_config(&format!(
        "[runtime]\nprelude = 'export PATH=\"{}:$PATH\"'\n",
        e.home.join("bin").display()
    ));
    e.write_config("[project]\ninstall = \"npm install\"\n");
    let text = e.pando(&["doctor"]);
    let json = e.pando(&["doctor", "--json"]);
    assert_eq!(code(&text), EXIT_ERROR);
    assert_eq!(code(&json), EXIT_ERROR, "the same exit code either way");
    let parsed: serde_json::Value = serde_json::from_str(&stdout(&json)).unwrap();
    assert_eq!(parsed["ok"], serde_json::json!(false));
    let problems: Vec<&serde_json::Value> = parsed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["severity"] == "problem")
        .collect();
    assert_eq!(problems.len(), 1, "{parsed}");
    assert_eq!(problems[0]["section"], "config");
    assert!(
        stdout(&text).contains(problems[0]["message"].as_str().unwrap()),
        "the text prints what the JSON reports"
    );
    assert_eq!(problems[0]["fix"], serde_json::json!("use `npm ci`"));
}

// The whole of namespaced mode through the binary: `start --namespaced`
// makes the worktree's own database, `status` says so, `restart` keeps
// the mode, the flags refuse each other, and `rm` drops it — with the
// password in no line of any of it.
#[test]
fn a_namespaced_worktree_through_the_cli_from_start_to_rm() {
    let e = env();
    let fake = common::fake_mariadb(&e.home);
    std::fs::write(
        e.root.join(".env"),
        "DATABASE_HOST=localhost\nDATABASE_PORT=3306\nDATABASE_NAME=shop\n\
         DATABASE_USER=app\nDATABASE_PASSWORD=cli-secret-pw\n",
    )
    .unwrap();
    e.write_config(&format!(
        "[dev]\ncmd = '''{}'''\nports = {{ PORT = \"web\" }}\n\n\
         [[services]]\nkind = \"native\"\nname = \"mariadb\"\n\
         env = {{ DATABASE_PORT = \"mariadb\" }}\n",
        common::listener_on_port_env()
    ));
    let mut printed = String::new();
    let mut run = |args: &[&str], expect: i32| {
        let out = e.pando(args);
        printed.push_str(&stdout(&out));
        printed.push_str(&stderr(&out));
        assert_eq!(code(&out), expect, "{args:?}: {}", stderr(&out));
        out
    };
    run(&["new", "feat/one"], 0);
    let out = run(&["start", "feat/one", "--namespaced", "--no-wait"], 0);
    assert!(
        stderr(&out).contains("mariadb: own database shop__feat_one, made just now"),
        "{}",
        stderr(&out)
    );
    let status =
        |out: &Output| -> serde_json::Value { serde_json::from_str(&stdout(out)).unwrap() };
    let v = status(&run(&["status", "--json"], 0));
    assert_eq!(v["worktrees"][0]["mode"], "namespaced");
    assert_eq!(
        v["worktrees"][0]["namespaces"][0]["database"],
        "shop__feat_one"
    );
    let text = stdout(&run(&["status", "feat/one"], 0));
    assert!(
        text.contains("database shop__feat_one on localhost:3306"),
        "{text}"
    );

    run(&["restart", "feat/one", "--no-wait"], 0);
    let v = status(&run(&["status", "--json"], 0));
    assert_eq!(
        v["worktrees"][0]["mode"], "namespaced",
        "restart keeps the mode"
    );
    assert_eq!(
        std::fs::read_to_string(fake.join("created")).unwrap(),
        "shop__feat_one\n",
        "and its database"
    );

    run(&["start", "feat/one", "--namespaced", "--isolated"], 2);
    run(&["start", "feat/one", "--namespaced", "--shared"], 2);

    let out = run(&["rm", "feat/one"], 0);
    assert!(
        stderr(&out).contains("mariadb: dropped database shop__feat_one"),
        "{}",
        stderr(&out)
    );
    assert_eq!(
        std::fs::read_to_string(fake.join("dropped")).unwrap(),
        "shop__feat_one\n"
    );
    assert!(!printed.contains("cli-secret-pw"), "{printed}");
}

// `--only` through a switch onto namespaces is refused before anything
// else: no login is asked for, and no database is made, for a start that
// will not happen.
#[test]
fn start_only_onto_namespaces_is_refused_before_any_question() {
    let e = env();
    let fake = common::fake_mariadb(&e.home);
    std::fs::write(
        e.root.join(".env"),
        "DATABASE_HOST=localhost\nDATABASE_PORT=3306\nDATABASE_NAME=shop\n",
    )
    .unwrap();
    e.write_config(
        "[dev]\ncmd = \"sleep 30\"\nports = []\n\n\
         [[services]]\nkind = \"native\"\nname = \"mariadb\"\n\
         env = { DATABASE_PORT = \"mariadb\" }\n",
    );
    let out = e.pando(&["new", "feat/one"]);
    assert_eq!(code(&out), EXIT_OK, "{}", stderr(&out));
    for verb in ["start", "restart"] {
        let out = e.pando(&[
            verb,
            "feat/one",
            "--only",
            "dev",
            "--namespaced",
            "--no-wait",
        ]);
        assert_eq!(code(&out), EXIT_ERROR, "{verb}: {}", stderr(&out));
        assert!(
            stderr(&out).contains("--only dev"),
            "{verb}: {}",
            stderr(&out)
        );
        assert!(
            !stderr(&out).contains("[namespaced.mariadb]"),
            "{verb} asked for a login first: {}",
            stderr(&out)
        );
    }
    assert!(!fake.join("created").exists(), "nothing was made");
}

// Decision 7: visible from the start, and labelled experimental in help.
#[test]
fn namespaced_is_in_start_help_and_says_it_is_experimental() {
    let e = env();
    for command in ["start", "restart"] {
        let help = stdout(&e.pando(&[command, "--help"]));
        assert!(help.contains("--namespaced"), "{command}: {help}");
        assert!(help.contains("Experimental"), "{command}: {help}");
    }
}
