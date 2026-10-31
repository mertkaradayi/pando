use super::fingerprint::hashed_text;
use super::*;
use crate::config::{self, Config};
use crate::paths::PandoPaths;
use crate::project::ProjectRef;
use chrono::Utc;
use tempfile::TempDir;

/// A project with a pando home that does not exist yet.
struct Fixture {
    _dir: TempDir,
    paths: PandoPaths,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("acme-shop");
    std::fs::create_dir_all(&root).unwrap();
    let project = ProjectRef::from_root(&root).unwrap();
    let paths = PandoPaths::new(dir.path().join("pando-home"), project);
    Fixture { _dir: dir, paths }
}

const WEB: &str = "[processes.web]\ncmd = \"pnpm dev\"\nports = [\"web\"]\n";

/// Writes the project layer and loads the config the way every command
/// does.
fn configure(f: &Fixture, text: &str) -> Config {
    std::fs::create_dir_all(f.paths.project_dir()).unwrap();
    std::fs::write(f.paths.config_file(), text).unwrap();
    load(f)
}

fn load(f: &Fixture) -> Config {
    config::load(&f.paths).unwrap().config
}

/// A finished check of `fingerprint` that ended as `outcome`.
fn finished(outcome: CheckOutcome, fingerprint: &str) -> CheckRecord {
    let mut record = CheckRecord::begin(fingerprint.to_string(), RanBy::Program);
    record.outcome = outcome;
    record.fingerprint_after = Some(fingerprint.to_string());
    record.finished_at = Some(Utc::now());
    record
}

fn failed() -> CheckOutcome {
    CheckOutcome::Failed {
        kind: FailureKind::Settings,
        reason: "web exited after 0.8s".to_string(),
    }
}

fn state_of(f: &Fixture) -> SetupState {
    read(&f.paths, &load(f)).state
}

// Each state from fixture files.

#[test]
fn a_project_with_nothing_to_run_is_new() {
    let f = fixture();
    assert_eq!(state_of(&f), SetupState::New { skipped: false });
}

#[test]
fn a_process_with_a_blank_command_is_nothing_to_run() {
    let f = fixture();
    configure(&f, "[processes.web]\ncmd = \"  \"\n");
    assert_eq!(state_of(&f), SetupState::New { skipped: false });
}

#[test]
fn a_skipped_project_with_nothing_to_run_stays_new_and_says_it_was_skipped() {
    let f = fixture();
    SetupMemory {
        skipped_at: Some(Utc::now()),
        ..SetupMemory::default()
    }
    .save(&f.paths)
    .unwrap();
    assert_eq!(state_of(&f), SetupState::New { skipped: true });
}

#[test]
fn a_project_configured_before_checks_existed_is_untested_never_new() {
    let f = fixture();
    configure(&f, WEB);
    assert_eq!(state_of(&f), SetupState::Untested);
}

#[test]
fn a_passed_check_of_todays_settings_is_ready() {
    let f = fixture();
    let config = configure(&f, WEB);
    finished(CheckOutcome::Passed, &fingerprint(&config))
        .save(&f.paths)
        .unwrap();
    assert_eq!(state_of(&f), SetupState::Ready);
}

#[test]
fn a_check_by_an_older_pando_is_still_ready() {
    let f = fixture();
    let config = configure(&f, WEB);
    let mut record = finished(CheckOutcome::Passed, &fingerprint(&config));
    record.pando_version = "0.0.1".to_string();
    record.save(&f.paths).unwrap();
    assert_eq!(state_of(&f), SetupState::Ready);
}

#[test]
fn a_failed_check_of_todays_settings_is_failing() {
    let f = fixture();
    let config = configure(&f, WEB);
    for outcome in [
        failed(),
        CheckOutcome::Failed {
            kind: FailureKind::Machine,
            reason: "redis is not running".to_string(),
        },
        CheckOutcome::NotSetUp {
            slot: "services".to_string(),
        },
    ] {
        finished(outcome.clone(), &fingerprint(&config))
            .save(&f.paths)
            .unwrap();
        assert_eq!(state_of(&f), SetupState::Failing, "{outcome:?}");
    }
}

#[test]
fn a_finished_check_of_other_settings_is_stale() {
    let f = fixture();
    configure(&f, WEB);
    for outcome in [CheckOutcome::Passed, failed()] {
        finished(outcome.clone(), "an older fingerprint")
            .save(&f.paths)
            .unwrap();
        assert_eq!(state_of(&f), SetupState::Stale, "{outcome:?}");
    }
}

#[test]
fn a_check_whose_settings_changed_while_it_ran_is_stale_even_matching_today() {
    let f = fixture();
    let config = configure(&f, WEB);
    let mut record = finished(CheckOutcome::Passed, &fingerprint(&config));
    record.fingerprint_before = "the settings it started with".to_string();
    record.save(&f.paths).unwrap();
    assert_eq!(state_of(&f), SetupState::Stale);
}

// A check speaks for the base the settings named when it ran: answered
// or changed since, `new` forks from another commit, and the base is not
// in the fingerprint. A record from before pando kept it is compared with
// nothing, so an upgrade does not undo a test.
#[test]
fn a_check_speaks_for_the_settings_only_while_they_name_the_base_it_ran_at() {
    let f = fixture();
    let config = configure(&f, WEB);
    let mut record = finished(CheckOutcome::Passed, &fingerprint(&config));
    record.settings_base = Some(None);
    record.save(&f.paths).unwrap();
    assert_eq!(state_of(&f), SetupState::Ready, "no base, then and now");

    configure(&f, &format!("[project]\nbase = \"work\"\n{WEB}"));
    assert_eq!(state_of(&f), SetupState::Stale, "a base answered since");

    record.settings_base = Some(Some("work".to_string()));
    record.save(&f.paths).unwrap();
    assert_eq!(state_of(&f), SetupState::Ready, "the same base");
    configure(&f, &format!("[project]\nbase = \"develop\"\n{WEB}"));
    assert_eq!(state_of(&f), SetupState::Stale, "another base");

    // The same for a question the check found open: once `base` is
    // answered, it no longer says the base is open.
    let mut record = finished(
        CheckOutcome::NotSetUp {
            slot: "base".to_string(),
        },
        &fingerprint(&load(&f)),
    );
    record.settings_base = Some(None);
    record.save(&f.paths).unwrap();
    assert_eq!(state_of(&f), SetupState::Stale);

    finished(CheckOutcome::Passed, &fingerprint(&load(&f)))
        .save(&f.paths)
        .unwrap();
    assert_eq!(state_of(&f), SetupState::Ready, "an older record");
}

// `Some(None)` — no base named — survives the file, and a record that
// never had the field reads as one that says nothing about it.
#[test]
fn the_base_a_check_ran_at_round_trips_named_or_not_and_an_older_record_has_none() {
    let f = fixture();
    let mut record = finished(CheckOutcome::Passed, "fp");
    for base in [Some(None), Some(Some("work".to_string()))] {
        record.settings_base = base.clone();
        record.save(&f.paths).unwrap();
        assert_eq!(CheckRecord::load(&f.paths).unwrap().settings_base, base);
    }
    record.settings_base = None;
    record.save(&f.paths).unwrap();
    let text = std::fs::read_to_string(f.paths.check_file()).unwrap();
    assert!(!text.contains("settings_base"), "{text}");
    assert_eq!(CheckRecord::load(&f.paths).unwrap().settings_base, None);
}

#[test]
fn a_check_changed_after_it_ran_is_stale() {
    let f = fixture();
    let config = configure(&f, WEB);
    finished(CheckOutcome::Passed, &fingerprint(&config))
        .save(&f.paths)
        .unwrap();
    configure(&f, &WEB.replace("pnpm dev", "pnpm start"));
    assert_eq!(state_of(&f), SetupState::Stale);
}

#[test]
fn a_running_record_with_the_lock_free_is_interrupted() {
    let f = fixture();
    let config = configure(&f, WEB);
    CheckRecord::begin(fingerprint(&config), RanBy::Tui)
        .save(&f.paths)
        .unwrap();
    // No lock file at all, as after a crash that took it with the process.
    assert_eq!(state_of(&f), SetupState::Interrupted);
    // A lock file nobody holds, as after a SIGKILL.
    drop(crate::state::lock(&f.paths.check_lock_file()).unwrap());
    assert_eq!(state_of(&f), SetupState::Interrupted);
}

#[test]
fn a_record_that_says_interrupted_is_interrupted() {
    let f = fixture();
    let config = configure(&f, WEB);
    finished(CheckOutcome::Interrupted, &fingerprint(&config))
        .save(&f.paths)
        .unwrap();
    assert_eq!(state_of(&f), SetupState::Interrupted);
}

#[test]
fn a_held_lock_is_testing() {
    let f = fixture();
    let config = configure(&f, WEB);
    CheckRecord::begin(fingerprint(&config), RanBy::Program)
        .save(&f.paths)
        .unwrap();
    let _held = crate::state::lock(&f.paths.check_lock_file()).unwrap();
    assert_eq!(state_of(&f), SetupState::Testing);
}

#[test]
fn a_check_record_that_cannot_be_read_counts_as_none() {
    let f = fixture();
    configure(&f, WEB);
    std::fs::write(f.paths.check_file(), "{ half a record").unwrap();
    std::fs::write(f.paths.setup_file(), "not json").unwrap();
    let setup = read(&f.paths, &load(&f));
    assert_eq!(setup.state, SetupState::Untested);
    assert_eq!(setup.last_check, None);
    assert_eq!(setup.memory, SetupMemory::default());
}

// The order the states are decided in.

#[test]
fn a_held_lock_beats_everything() {
    let f = fixture();
    let _held = crate::state::lock(&f.paths.check_lock_file()).unwrap();
    // Nothing to run, and no record: still testing.
    assert_eq!(state_of(&f), SetupState::Testing);
    // A record that is passed and current: still testing.
    let config = configure(&f, WEB);
    finished(CheckOutcome::Passed, &fingerprint(&config))
        .save(&f.paths)
        .unwrap();
    assert_eq!(state_of(&f), SetupState::Testing);
}

#[test]
fn an_unfinished_record_is_interrupted_whatever_the_settings() {
    let config: Config = toml::from_str(WEB).unwrap();
    let memory = SetupMemory::default();
    let running = CheckRecord::begin("an older fingerprint".to_string(), RanBy::Tui);
    // Stale by its fingerprint, but it never finished.
    assert_eq!(
        decide(false, Some(&running), &memory, &config, "today"),
        SetupState::Interrupted
    );
    // Nothing to run any more, but it never finished.
    assert_eq!(
        decide(false, Some(&running), &memory, &Config::default(), "today"),
        SetupState::Interrupted
    );
}

#[test]
fn nothing_to_run_is_new_before_any_record_is_compared() {
    let memory = SetupMemory::default();
    let passed = finished(CheckOutcome::Passed, "today");
    assert_eq!(
        decide(false, Some(&passed), &memory, &Config::default(), "today"),
        SetupState::New { skipped: false }
    );
}

// Reading writes nothing.

#[test]
fn reading_the_state_creates_no_file() {
    let f = fixture();
    let setup = read(&f.paths, &Config::default());
    assert_eq!(setup.state, SetupState::New { skipped: false });
    assert!(!f.paths.home.exists(), "reading made pando's home");

    let config = configure(&f, WEB);
    let before = listing(&f);
    read(&f.paths, &config);
    assert_eq!(listing(&f), before, "reading wrote into the project's dir");
    assert!(!f.paths.check_lock_file().exists());
}

fn listing(f: &Fixture) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(f.paths.project_dir())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// The records.

#[test]
fn a_check_record_round_trips_with_every_field() {
    let f = fixture();
    let record = CheckRecord {
        started_at: Utc::now(),
        finished_at: Some(Utc::now()),
        pando_version: "0.5.0".to_string(),
        fingerprint_before: "a".to_string(),
        fingerprint_after: Some("b".to_string()),
        commit: Some("a1b2c3d4e5f6".to_string()),
        base_ref: Some("main".to_string()),
        settings_base: Some(Some("main".to_string())),
        outcome: failed(),
        processes: vec![
            ProcessResult {
                name: "api".to_string(),
                ready: true,
                port: Some(20280),
                http_status: None,
                secs: 1.5,
            },
            ProcessResult {
                name: "web".to_string(),
                ready: false,
                port: Some(20281),
                http_status: Some(500),
                secs: 0.8,
            },
        ],
        failed_process: Some("web".to_string()),
        failed_tail: vec!["Error: Cannot find module 'dotenv'".to_string()],
        progress: vec!["made a test worktree".to_string(), "installing".to_string()],
        ran_by: RanBy::Terminal,
        mode: CheckMode::Namespaced,
        notes: vec!["hooks after services were skipped".to_string()],
        disk: Some(crate::setup::DiskCost {
            trees: vec![crate::setup::DiskTree {
                path: "target".to_string(),
                bytes: 1_200_000_000,
                main_has: true,
                cloned: false,
            }],
            outside_bytes: Some(400_000_000),
        }),
    };
    record.save(&f.paths).unwrap();
    assert_eq!(CheckRecord::load(&f.paths), Some(record));
    assert!(
        !f.paths.check_file().with_extension("json.tmp").exists(),
        "the temp file must not survive the rename"
    );
}

#[test]
fn every_outcome_round_trips() {
    let f = fixture();
    for outcome in [
        CheckOutcome::Running,
        CheckOutcome::Passed,
        failed(),
        CheckOutcome::NotSetUp {
            slot: "processes".to_string(),
        },
        CheckOutcome::Interrupted,
    ] {
        let record = finished(outcome, "x");
        record.save(&f.paths).unwrap();
        assert_eq!(CheckRecord::load(&f.paths), Some(record));
    }
}

#[test]
fn a_minimal_record_with_a_key_from_a_later_pando_still_reads() {
    let f = fixture();
    std::fs::create_dir_all(f.paths.project_dir()).unwrap();
    std::fs::write(
        f.paths.check_file(),
        r#"{
            "started_at": "2026-09-27T10:00:00Z",
            "pando_version": "0.9.0",
            "fingerprint_before": "abc",
            "outcome": { "result": "failed", "kind": "machine", "reason": "redis is down" },
            "ran_by": "program",
            "something_new": [1, 2]
        }"#,
    )
    .unwrap();
    let record = CheckRecord::load(&f.paths).expect("a record");
    assert_eq!(
        record.outcome,
        CheckOutcome::Failed {
            kind: FailureKind::Machine,
            reason: "redis is down".to_string()
        }
    );
    assert_eq!(record.fingerprint(), "abc");
    assert!(record.processes.is_empty());
    assert_eq!(
        record.mode,
        CheckMode::Shared,
        "a record from before the choice"
    );
}

#[test]
fn setup_memory_round_trips_and_is_empty_when_missing() {
    let f = fixture();
    assert_eq!(SetupMemory::load(&f.paths), SetupMemory::default());
    let memory = SetupMemory {
        skipped_at: Some(Utc::now()),
        tried_by_pando_at: None,
        tip_shown_at: Some(Utc::now()),
    };
    memory.save(&f.paths).unwrap();
    assert_eq!(SetupMemory::load(&f.paths), memory);
}

// The fingerprint.

/// Every run section set, so a change to any of them has something to
/// change.
const RUN: &str = r#"
[project]
install = "pnpm install"
provision = [".env"]

[runtime]
prelude = "source ~/.nvm/nvm.sh"

[processes.web]
cmd = "pnpm dev"
ports = ["web"]

[[services]]
kind = "native"
name = "redis"
preset = "redis"

[[hooks]]
name = "migrate"
after = "services"
cmd = "pnpm migrate"

[[probes]]
name = "node"
cmd = "node -v"
match = "not found"
hint = "install node"
"#;

fn fp(text: &str) -> String {
    let config: Config = toml::from_str(text).unwrap_or_else(|e| panic!("{e}\n{text}"));
    fingerprint(&config)
}

fn replaced(from: &str, to: &str) -> String {
    assert!(RUN.contains(from), "{from:?} is not in the fixture");
    RUN.replacen(from, to, 1)
}

#[test]
fn the_fingerprint_changes_with_every_run_section() {
    let base = fp(RUN);
    let cases = [
        (
            "install",
            replaced("pnpm install", "pnpm install --frozen-lockfile"),
        ),
        (
            "provision",
            replaced(r#"[".env"]"#, r#"[".env", ".env.local"]"#),
        ),
        (
            "provision_from",
            replaced(
                "provision = [\".env\"]",
                "provision = [\".env\"]\nprovision_from = { \".env\" = \".env.example\" }",
            ),
        ),
        (
            "provision_mode",
            replaced(
                "provision = [\".env\"]",
                "provision = [\".env\"]\nprovision_mode = \"copy\"",
            ),
        ),
        ("runtime prelude", replaced("nvm.sh", "nvm.sh && nvm use")),
        (
            "runtime version_files",
            replaced("[runtime]\n", "[runtime]\nversion_files = [\".nvmrc\"]\n"),
        ),
        ("process command", replaced("pnpm dev", "pnpm start")),
        ("process ports", replaced(r#"["web"]"#, r#"["web", "api"]"#)),
        (
            "process env",
            replaced(
                "ports = [\"web\"]",
                "ports = [\"web\"]\nenv = { A = \"1\" }",
            ),
        ),
        (
            "process cwd",
            replaced("ports = [\"web\"]", "ports = [\"web\"]\ncwd = \"apps/web\""),
        ),
        (
            "process readiness",
            replaced(
                "ports = [\"web\"]",
                "ports = [\"web\"]\nready = { role = \"web\", timeout_s = 90 }",
            ),
        ),
        (
            "a second process",
            format!("{RUN}\n[processes.api]\ncmd = \"pnpm api\"\n"),
        ),
        (
            "services",
            replaced("preset = \"redis\"", "preset = \"valkey\""),
        ),
        ("hooks", replaced("pnpm migrate", "pnpm migrate:dev")),
        (
            "a hook's scope",
            replaced(
                "cmd = \"pnpm migrate\"",
                "cmd = \"pnpm migrate\"\non = \"always\"",
            ),
        ),
        ("probes", replaced("install node", "install node 20")),
    ];
    for (section, text) in cases {
        assert_ne!(
            fp(&text),
            base,
            "a change to {section} kept the fingerprint"
        );
    }
}

#[test]
fn the_fingerprint_ignores_what_does_not_affect_a_run() {
    let base = fp(RUN);
    let cases = [
        ("a theme", format!("{RUN}\n[ui]\ntheme = \"gruvbox\"\n")),
        (
            "appearance",
            format!("{RUN}\n[ui]\nappearance = \"dark\"\n"),
        ),
        (
            "share",
            format!("{RUN}\n[share]\nprovider = \"cloudflared\"\nauth_cmd = \"x\"\n"),
        ),
        (
            "branches",
            format!("{RUN}\n[[branches.rules]]\nmatch = \"feat/*\"\nbase = \"dev\"\n"),
        ),
        (
            "namespaced logins",
            format!("{RUN}\n[namespaced.redis]\nuser = \"u\"\npassword = \"secret\"\n"),
        ),
        (
            "isolation",
            format!("{RUN}\n[isolation]\nprefer = \"compose\"\n"),
        ),
        (
            "where worktrees go",
            replaced("[project]\n", "[project]\nworktrees_dir = \"/tmp/wt\"\n"),
        ),
        (
            "the base branch",
            replaced("[project]\n", "[project]\nbase = \"develop\"\n"),
        ),
    ];
    for (what, text) in cases {
        assert_eq!(fp(&text), base, "{what} changed the fingerprint");
    }
}

#[test]
fn the_fingerprint_holds_no_login_and_no_pando_version() {
    let text = format!("{RUN}\n[namespaced.redis]\nuser = \"u\"\npassword = \"s3cr3t-pw\"\n");
    let config: Config = toml::from_str(&text).unwrap();
    let hashed = hashed_text(&config);
    assert!(!hashed.contains("s3cr3t-pw"), "{hashed}");
    assert!(!hashed.contains(env!("CARGO_PKG_VERSION")), "{hashed}");
    assert!(hashed.contains(&format!("\"version\":{FINGERPRINT_VERSION}")));
}

#[test]
fn the_fingerprint_does_not_depend_on_the_order_things_are_written_in() {
    let one = r#"
[processes.web]
cmd = "pnpm dev"
env = { A = "1", B = "2" }

[processes.api]
cmd = "pnpm api"
"#;
    let other = r#"
[processes.api]
cmd = "pnpm api"

[processes.web]
env = { B = "2", A = "1" }
cmd = "pnpm dev"
"#;
    assert_eq!(fp(one), fp(other));
}

#[test]
fn provisioning_nothing_is_one_fingerprint_whether_said_or_not() {
    let unsaid = replaced("provision = [\".env\"]\n", "");
    let said = replaced("provision = [\".env\"]", "provision = []");
    assert_eq!(fp(&unsaid), fp(&said));
}

/// The same settings give the same fingerprint in every process, on every
/// day and in every build. If this value changes, every recorded check
/// goes stale: that is only right with a bump of `FINGERPRINT_VERSION`.
#[test]
fn the_fingerprint_of_fixed_settings_is_pinned() {
    assert_eq!(fp(WEB), "66d7d532acc452209922970af945ea3b");
}

// What a check keeps of a failed process's log.

#[test]
fn a_failed_processs_lines_keep_what_failed_and_lose_every_secret() {
    let hidden = crate::config::HIDDEN;
    let cases = [
        (
            "Error: connect ECONNREFUSED postgres://app:s3cret@localhost:5432/shop",
            format!("Error: connect ECONNREFUSED postgres://app:{hidden}@localhost:5432/shop"),
        ),
        (
            "DATABASE_PASSWORD=hunter2 PORT=3000",
            format!("DATABASE_PASSWORD={hidden} PORT=3000"),
        ),
        (
            r#"config: {"apiToken": "tok_123", "port": 3000}"#,
            format!(r#"config: {{"apiToken": "{hidden}", "port": 3000}}"#),
        ),
        (
            "Authorization: Bearer eyJhbGciOi.abc.def",
            format!("Authorization: Bearer {hidden}"),
        ),
        (
            "authorization: Basic dXNlcjpwYXNz",
            format!("authorization: Basic {hidden}"),
        ),
        (
            "curl -H 'x: Bearer abc123' failed",
            format!("curl -H 'x: Bearer {hidden}' failed"),
        ),
        (
            "stripe_secret_key: sk_live_42",
            format!("stripe_secret_key: {hidden}"),
        ),
        (
            "AWS_ACCESS_KEY_ID=AKIA123 region=eu",
            format!("AWS_ACCESS_KEY_ID={hidden} region=eu"),
        ),
        ("Cookie: session=abc123", format!("Cookie: {hidden}")),
        ("PASSWD=x", format!("PASSWD={hidden}")),
        // Nothing secret, nothing changed.
        (
            "Error: Cannot find module 'dotenv' at http://localhost:3000/x?a=1",
            "Error: Cannot find module 'dotenv' at http://localhost:3000/x?a=1".to_string(),
        ),
        (
            "redis://localhost:6379 and mysql://root@db/x",
            "redis://localhost:6379 and mysql://root@db/x".to_string(),
        ),
    ];
    for (line, want) in cases {
        assert_eq!(redact_line(line), want, "{line}");
    }
}

// ---- the device line ------------------------------------------------

/// An Expo app at the project's root, as its template makes one.
fn expo_app(f: &Fixture) {
    let root = f.paths.root();
    std::fs::write(
        root.join("package.json"),
        r#"{ "scripts": { "start": "expo start" }, "dependencies": { "expo": "~57.0.0" } }"#,
    )
    .unwrap();
    std::fs::write(root.join("app.json"), r#"{ "expo": { "name": "mobile" } }"#).unwrap();
}

fn proposals(f: &Fixture) -> Vec<crate::detect::Proposal> {
    let root = f.paths.root();
    crate::detect::propose(root, &crate::detect::signals(root))
}

// Metro's port variable, in the map form `[dev]` gets or as the template
// a workspace app gets in `env`: either way a device runs the app.
#[test]
fn a_process_on_metros_port_variable_has_the_device_line() {
    let f = fixture();
    for text in [
        "[dev]\ncmd = \"npm run start\"\nports = { RCT_METRO_PORT = \"web\" }\n",
        "[processes.mobile]\ncmd = \"npm run start\"\ncwd = \"apps/mobile\"\n\
         ports = [\"mobile\"]\nenv = { RCT_METRO_PORT = \"{port:mobile}\" }\n",
    ] {
        let note = device_note(&configure(&f, text), &[]).unwrap_or_else(|| panic!("{text}"));
        assert!(note.contains("REACT_NATIVE_PACKAGER_HOSTNAME"), "{note}");
        assert!(note.contains("EXPO_PUBLIC_*"), "{note}");
        let block = memory_block(&f.paths, Some(note));
        assert!(block.contains(note), "{block}");
        assert!(block.contains("never guess it"), "{block}");
    }
}

#[test]
fn a_web_app_has_no_device_line() {
    let f = fixture();
    assert_eq!(device_note(&configure(&f, WEB), &[]), None);
    let block = memory_block(&f.paths, None);
    assert!(!block.contains("127.0.0.1"), "{block}");
}

// Before anything is configured, what the rules propose decides; once
// config runs something, config does, whatever the rules would propose.
#[test]
fn the_device_line_follows_what_runs_proposed_or_configured() {
    let f = fixture();
    expo_app(&f);
    let proposed = proposals(&f);
    assert!(device_note(&Config::default(), &proposed).is_some());
    assert_eq!(device_note(&configure(&f, WEB), &proposed), None);
}
