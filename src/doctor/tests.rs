//! Tests for `doctor`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

use crate::actions::Machine;
use crate::paths::PandoPaths;
use crate::process::Group;
use crate::project::ProjectRef;
use crate::testutil::{WSL_MOUNTS, WSL_RELEASE, git, init_repo, wsl_system};
use crate::{ports, state};

use super::*;
use super::{services::*, tools::*};

struct Fx {
    _dir: TempDir,
    paths: PandoPaths,
    root: std::path::PathBuf,
    home: std::path::PathBuf,
    /// The *developer's* home, where version managers live — never
    /// the real one, so no test reads what this laptop has installed.
    machine_home: std::path::PathBuf,
}

fn fixture() -> Fx {
    fixture_of(init_repo)
}

/// [`fixture`], with the repository made by `make`.
fn fixture_of(make: impl Fn(&Path)) -> Fx {
    let dir = TempDir::new().expect("temp dir");
    let root = dir.path().join("repo");
    make(&root);
    // Canonical, because `ProjectRef` canonicalises and macOS prints
    // `/var` where git prints `/private/var`: a test comparing the two
    // spellings is comparing the platform, not the report.
    let root = std::fs::canonicalize(&root).expect("canonical root");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).expect("home");
    // 0700, the way `PandoPaths::ensure_home` creates it. A fixture
    // that leaves it at whatever the umask says would make every test
    // here read a note about the test's own directory.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700))
            .expect("chmod home");
    }
    let home = std::fs::canonicalize(&home).expect("canonical home");
    let paths = PandoPaths::new(&home, ProjectRef::from_root(&root).expect("project"));
    let machine_home = dir.path().join("developer-home");
    std::fs::create_dir_all(&machine_home).expect("machine home");
    Fx {
        _dir: dir,
        paths,
        root,
        home,
        machine_home,
    }
}

fn write_compose(fx: &Fx, body: &str) {
    std::fs::write(fx.root.join("docker-compose.yml"), body).expect("write compose");
}

fn write_project_config(fx: &Fx, body: &str) {
    let path = fx.paths.config_file();
    std::fs::create_dir_all(path.parent().expect("project dir")).expect("mkdir");
    std::fs::write(&path, body).expect("write config");
}

/// A shell that finds every tool the script asks about and reports
/// nothing for the runtime.
///
/// Injected rather than spawned: a unit test that ran a real login
/// shell would be slow, would read whatever this laptop has installed,
/// and would fail on a runner with a different PATH.
fn every_tool(script: &str) -> Option<String> {
    if !script.contains(TOOL_DONE_MARK) {
        return Some("pando-runtime-ok\n".to_string());
    }
    let mut out = String::new();
    for index in 0.. {
        if !script.contains(&format!("{TOOL_PATH_MARK}{index} ")) {
            break;
        }
        let _ = writeln!(out, "{TOOL_PATH_MARK}{index} /usr/bin/thing{index}");
        let _ = writeln!(out, "{TOOL_VERSION_MARK}{index} 1.2.3");
    }
    let _ = writeln!(out, "{TOOL_DONE_MARK}");
    Some(out)
}

/// A shell that finds nothing at all, for the tests that are about a
/// tool pando cannot find.
fn no_tools(script: &str) -> Option<String> {
    if !script.contains(TOOL_DONE_MARK) {
        return Some("pando-runtime-ok\n".to_string());
    }
    Some(format!("{TOOL_DONE_MARK}\n"))
}

fn report_of(fx: &Fx, shell: &dyn Fn(&str) -> Option<String>) -> Report {
    run_on(&fx.paths, &Machine::at(shell, fx.machine_home.clone()))
}

fn report(fx: &Fx) -> Report {
    report_of(fx, &every_tool)
}

fn messages(report: &Report) -> Vec<String> {
    report.findings.iter().map(|f| f.message.clone()).collect()
}

fn mentions(report: &Report, needle: &str) -> bool {
    messages(report).iter().any(|m| m.contains(needle))
}

#[test]
fn a_project_with_no_config_at_all_is_healthy_and_says_where_everything_is() {
    let fx = fixture();
    let report = report(&fx);
    assert!(report.healthy(), "{:?}", report.findings);
    assert_eq!(report.project.root, fx.root.display().to_string());
    assert_eq!(report.project.home, fx.home.display().to_string());
    assert_eq!(report.project.id, fx.paths.project_id());
    let text = report.render();
    // An empty repository has nothing to run, and that is the one thing
    // worth saying about it.
    assert!(text.starts_with("0 problems, 1 note"), "{text}");
    assert!(text.contains("nothing to run"), "{text}");
    assert!(text.contains("not there"), "every layer is named: {text}");
}

// origin/HEAD far behind the branch work happens on is a note with both
// ways out, until the project names its own base.
#[test]
fn origin_head_far_behind_the_main_checkout_is_a_note_until_a_base_is_named() {
    let fx = fixture_of(|root| {
        crate::testutil::drifted_repo(
            root,
            crate::worktree::FAR_AHEAD,
            crate::worktree::STALE_DAYS,
            None,
        )
    });
    let report = report(&fx);
    let finding = report
        .findings
        .iter()
        .find(|f| f.message.starts_with("origin/HEAD is origin/develop"))
        .unwrap_or_else(|| panic!("no base note: {:?}", report.findings));
    assert_eq!(finding.section, Section::Project);
    assert_eq!(finding.severity, Severity::Note);
    assert!(
        finding.message.contains(&format!(
            "last committed {} days before work",
            crate::worktree::STALE_DAYS
        )),
        "{}",
        finding.message
    );
    let fix = finding.fix.as_deref().unwrap();
    assert!(fix.contains("answer `base` with it"), "{fix}");
    assert!(fix.contains("git remote set-head origin work"), "{fix}");

    write_project_config(&fx, "[project]\nbase = \"work\"\n");
    assert!(!mentions(&report_of(&fx, &every_tool), "origin/HEAD is"));
}

#[test]
fn every_layer_is_named_with_its_path_whether_or_not_it_is_there() {
    let fx = fixture();
    let report = report(&fx);
    let layers: Vec<&str> = report.config.layers.iter().map(|l| l.layer).collect();
    assert_eq!(layers, vec!["committed", "user", "project"]);
    assert_eq!(
        report.config.layers[0].path,
        fx.root.join("pando.toml").display().to_string()
    );
    assert_eq!(
        report.config.layers[1].path,
        fx.paths.user_config_file().display().to_string()
    );
    assert_eq!(
        report.config.layers[2].path,
        fx.paths.config_file().display().to_string()
    );
    assert!(report.config.layers.iter().all(|l| !l.present));
}

#[test]
fn every_key_is_reported_with_the_comment_the_file_carries() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[project]\ninstall = \"pnpm install --frozen-lockfile\"  # detected: pnpm-lock.yaml\n\
             \n[dev]\ncmd = \"pnpm dev\"  # answered: 2026-09-21\nports = { PORT = \"web\" }\n",
    );
    let report = report(&fx);
    let project = report
        .config
        .layers
        .iter()
        .find(|l| l.layer == "project")
        .expect("the project layer");
    assert!(project.present);
    let install = project
        .keys
        .iter()
        .find(|k| k.key == "project.install")
        .expect("project.install");
    assert_eq!(
        install.value.as_deref(),
        Some("\"pnpm install --frozen-lockfile\"")
    );
    assert_eq!(install.note.as_deref(), Some("# detected: pnpm-lock.yaml"));
    let cmd = project
        .keys
        .iter()
        .find(|k| k.key == "dev.cmd")
        .expect("dev.cmd");
    assert_eq!(cmd.note.as_deref(), Some("# answered: 2026-09-21"));
    let ports = project
        .keys
        .iter()
        .find(|k| k.key == "dev.ports")
        .expect("dev.ports");
    assert_eq!(ports.note, None, "a key with no comment has no note");
}

#[test]
fn an_array_of_tables_reports_its_entry_note_and_every_key_under_it() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]  # detected: docker-compose.yml names postgres\n\
             kind = \"compose\"\nfile = \"docker-compose.yml\"\ninclude = [\"postgres\"]\n",
    );
    let report = report(&fx);
    let project = &report.config.layers[2];
    let header = project
        .keys
        .iter()
        .find(|k| k.key == "services[0]")
        .expect("the entry header");
    assert_eq!(header.value, None);
    assert_eq!(
        header.note.as_deref(),
        Some("# detected: docker-compose.yml names postgres")
    );
    assert!(
        project.keys.iter().any(|k| k.key == "services[0].include"),
        "{:?}",
        project.keys
    );
}

#[test]
fn a_key_a_committed_layer_may_not_set_is_marked_ignored_and_warned_about() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("pando.toml"),
        "[project]\nroot = \"/somewhere/else\"\ninstall = \"make setup\"\n",
    )
    .expect("write committed config");
    let report = report(&fx);
    let committed = &report.config.layers[0];
    let root = committed
        .keys
        .iter()
        .find(|k| k.key == "project.root")
        .expect("project.root");
    assert!(root.ignored, "a committed project.root is stripped");
    let install = committed
        .keys
        .iter()
        .find(|k| k.key == "project.install")
        .expect("project.install");
    assert!(!install.ignored, "an ordinary key is not");
    assert!(
        mentions(&report, "ignoring project.root"),
        "{:?}",
        messages(&report)
    );
    // With the findings, at the top, tagged with the section it is about.
    let text = report.render();
    let layer_line = text
        .lines()
        .position(|l| l.trim_start().starts_with("committed"))
        .expect("the committed line");
    let warning_line = text
        .lines()
        .position(|l| l.contains("ignoring project.root"))
        .expect("the warning");
    assert!(warning_line < layer_line, "{text}");
    assert!(
        text.lines().nth(warning_line).unwrap().contains("[config]"),
        "{text}"
    );
    assert_eq!(
        text.matches("ignoring project.root").count(),
        1,
        "said once, not twice:\n{text}"
    );
    assert!(report.healthy(), "a stripped key is a note, not a problem");
}

// ---- a detection that has gone stale ---------------------------------

/// A `Makefile` whose `dev` target is the shape a first run on a
/// repository pando had not generated actually met: a prerequisite,
/// and a recipe whose first line is a guard that exits before
/// anything is started.
///
/// Generic on purpose. Target names, a `command -v` guard and a
/// script under `scripts/` are makefile convention; nothing here is
/// anybody's project.
const GUARDED_MAKEFILE: &str = concat!(
    "build:\n\t./scripts/build.sh\n\n",
    "dev: build\n",
    "\tcommand -v watcher >/dev/null || { echo \"install watcher first\"; exit 1; }\n",
    "\t./scripts/serve.sh --reload\n\n",
    "run:\n\t./scripts/run.sh\n",
);

/// What an older pando wrote for that target: the guard line, lifted
/// out of the recipe on its own. Current rules propose `make dev`.
const LIFTED_GUARD: &str =
    "command -v watcher >/dev/null || { echo \"install watcher first\"; exit 1; }";

fn write_makefile(fx: &Fx, body: &str) {
    std::fs::write(fx.root.join("Makefile"), body).expect("write Makefile");
}

fn stale(report: &Report) -> Option<&Finding> {
    report
        .findings
        .iter()
        .find(|f| f.message.contains("pando detected itself"))
}

#[test]
fn a_detected_value_the_rules_would_not_write_now_is_named_with_what_they_offer() {
    let fx = fixture();
    write_makefile(&fx, GUARDED_MAKEFILE);
    write_project_config(
        &fx,
        &format!("[dev]\ncmd = '{LIFTED_GUARD}'  # detected: the dev target\n"),
    );
    let report = report(&fx);
    let finding = stale(&report).unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert_eq!(finding.section, Section::Config);
    // Suspicious, not broken: the developer may have kept it on
    // purpose, and a process that fails is reported separately. The
    // two together tell the story.
    assert_eq!(finding.severity, Severity::Note);
    assert!(report.healthy(), "{:?}", messages(&report));
    // The key, the value, the evidence that wrote it, and what the
    // rules offer in its place.
    assert!(finding.message.contains("dev.cmd"), "{}", finding.message);
    assert!(
        finding.message.contains("command -v watcher"),
        "{}",
        finding.message
    );
    assert!(
        finding.message.contains("(the dev target)"),
        "{}",
        finding.message
    );
    assert!(
        finding.message.contains("\"make dev\""),
        "{}",
        finding.message
    );
    // And the fix is the edit that reopens the question, in the file
    // that holds the line.
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(&fx.paths.config_file().display().to_string()),
        "{fix}"
    );
    assert!(fix.contains("delete that line"), "{fix}");
}

#[test]
fn a_detected_value_that_is_still_among_the_candidates_says_nothing() {
    let fx = fixture();
    write_makefile(&fx, GUARDED_MAKEFILE);
    write_project_config(
        &fx,
        "[dev]\ncmd = \"make dev\"  # detected: the dev target\n",
    );
    let report = report(&fx);
    assert!(stale(&report).is_none(), "{:?}", messages(&report));
}

// The rule is "not among the candidates", not "not the first one".
// `make dev` leads here and `./scripts/run.sh` is the second option —
// still an option, and demoting a candidate is not rejecting it.
#[test]
fn a_detected_value_the_rules_demoted_but_still_offer_says_nothing() {
    let fx = fixture();
    write_makefile(&fx, GUARDED_MAKEFILE);
    write_project_config(
        &fx,
        "[dev]\ncmd = \"./scripts/run.sh\"  # detected: the run target\n",
    );
    let report = report(&fx);
    assert!(stale(&report).is_none(), "{:?}", messages(&report));
}

// A value somebody decided is not pando's to have a second opinion
// about — in any of the four spellings a decision is written in, and
// for a comment a developer wrote themselves.
#[test]
fn a_value_that_was_answered_rather_than_detected_is_never_second_guessed() {
    for note in [
        "# answered: 2026-09-21",
        "# answered: a program, 2026-09-21",
        "# answered: --yes took the first of 2 options",
        "# the guard is deliberate, leave it",
    ] {
        let fx = fixture();
        write_makefile(&fx, GUARDED_MAKEFILE);
        write_project_config(&fx, &format!("[dev]\ncmd = '{LIFTED_GUARD}'  {note}\n"));
        let report = report(&fx);
        assert!(stale(&report).is_none(), "{note}: {:?}", messages(&report));
    }
}

// Nothing to compare against is not a divergence. A repository with
// no script, no target and no framework marker gets no dev-command
// proposal at all, and a rule that was withdrawn has nothing to say
// about the value it left behind.
#[test]
fn a_key_the_rules_have_no_opinion_about_says_nothing() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[dev]\ncmd = \"./serve\"  # detected: a rule that no longer exists\n",
    );
    let report = report(&fx);
    assert!(stale(&report).is_none(), "{:?}", messages(&report));
}

// Formatting is not a divergence: the file writes a literal string
// where pando writes a basic one, and they are the same value.
#[test]
fn quote_style_is_not_a_divergence() {
    let fx = fixture();
    write_makefile(&fx, GUARDED_MAKEFILE);
    write_project_config(&fx, "[dev]\ncmd = 'make dev'  # detected: the dev target\n");
    let report = report(&fx);
    assert!(stale(&report).is_none(), "{:?}", messages(&report));
}

// The one place a key does not belong to the slot named after it.
// Django's rule proposes a command carrying `{port:web}`, so
// answering the *dev command* question writes `dev.ports` too, under
// the dev command's own `# detected:` note. The port slot's own
// candidates are a different shape — `{ PORT = "web" }` — and
// measuring the one against the other would report every project
// built this way.
#[test]
fn a_key_one_candidate_writes_beside_its_own_is_not_measured_against_another_slot() {
    let fx = fixture();
    std::fs::write(fx.root.join("manage.py"), "#!/usr/bin/env python\n").expect("manage.py");
    std::fs::write(fx.root.join(".env.example"), "PORT=8000\n").expect("env example");
    write_project_config(
        &fx,
        "[dev]\ncmd = \"python manage.py runserver 127.0.0.1:{port:web}\"  \
             # detected: the Django rule\nports = [\"web\"]  # detected: the Django rule\n",
    );
    let report = report(&fx);
    assert!(stale(&report).is_none(), "{:?}", messages(&report));
}

// A higher layer wins, so a stale line a higher layer has already
// replaced is not what pando reads and not what doctor reports.
#[test]
fn a_stale_line_a_higher_layer_answers_over_is_not_reported() {
    let fx = fixture();
    write_makefile(&fx, GUARDED_MAKEFILE);
    std::fs::write(
        fx.root.join("pando.toml"),
        format!("[dev]\ncmd = '{LIFTED_GUARD}'  # detected: the dev target\n"),
    )
    .expect("write committed config");
    write_project_config(&fx, "[dev]\ncmd = \"make dev\"  # answered: 2026-09-22\n");
    let report = report(&fx);
    assert!(stale(&report).is_none(), "{:?}", messages(&report));
}

/// An Expo app, as its template makes one: the dev server is the `start`
/// script.
fn write_expo_app(dir: &Path) {
    std::fs::write(
        dir.join("package.json"),
        r#"{"name":"shop","dependencies":{"expo":"57.0.0"},"scripts":{"start":"expo start"}}"#,
    )
    .expect("package.json");
    std::fs::write(dir.join("app.json"), r#"{"expo":{"slug":"shop"}}"#).expect("app.json");
}

// The reporter's case: the role an older pando gave Metro's port. The
// fix said to delete the line and let the next command ask — but a
// `[dev]` whose command is still there has answered its ports, so
// nothing asked, and Metro ran on 8081 in every worktree. The fix is the
// command that writes what the rules offer now.
#[test]
fn a_stale_port_beside_its_command_is_fixed_by_the_answer_not_by_deleting_it() {
    let fx = fixture();
    write_expo_app(&fx.root);
    write_project_config(
        &fx,
        "[dev]\ncmd = \"npm run start\"  # detected: package.json scripts.start\n\
         ports = { RCT_METRO_PORT = \"web\" }  # detected: the Expo rule\n",
    );
    let report = report(&fx);
    let finding = stale(&report).unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert!(
        finding.message.starts_with("dev.ports = "),
        "{}",
        finding.message
    );
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(
            r#"`echo '{"port_env":"RCT_METRO_PORT"}' | pando init --answers - --replace`"#
        ),
        "{fix}"
    );
    assert!(fix.contains(r#"{ RCT_METRO_PORT = "metro" }"#), "{fix}");
    assert!(
        !fix.contains("delete that line"),
        "deleting it asks nothing: {fix}"
    );
}

// ---- a server with no port of its own -----------------------------

fn portless(report: &Report) -> Vec<&Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.message.contains("and has no `ports`"))
        .collect()
}

// The other half of the reporter's case: with the line gone, nothing
// asked and nothing said, and Metro listened on 8081 in every worktree.
#[test]
fn a_dev_process_running_a_framework_with_no_ports_is_a_problem_with_the_answer() {
    let fx = fixture();
    write_expo_app(&fx.root);
    write_project_config(&fx, "[dev]\ncmd = \"npm run start\"\n");
    let report = report(&fx);
    let found = portless(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    assert_eq!(finding.severity, Severity::Problem);
    assert!(!report.healthy());
    assert!(
        finding
            .message
            .contains("process \"dev\" runs Expo through package.json's \"start\""),
        "{}",
        finding.message
    );
    assert!(finding.message.contains("8081"), "{}", finding.message);
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(
            r#"`echo '{"port_env":"RCT_METRO_PORT"}' | pando init --answers - --replace`"#
        ),
        "{fix}"
    );
}

// A named process table is not a question's to fill: the fix is the
// line, in the table, with the role the catalog gives the variable.
#[test]
fn a_named_process_with_no_ports_is_given_the_line_that_fixes_it() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("apps/mobile")).expect("app dir");
    write_expo_app(&fx.root.join("apps/mobile"));
    write_project_config(
        &fx,
        "[processes.api]\ncmd = \"./serve\"\nports = [\"web\"]\n\n\
         [processes.mobile]\ncmd = \"pnpm start\"\ncwd = \"apps/mobile\"\n",
    );
    let report = report(&fx);
    let found = portless(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(r#"`ports = { RCT_METRO_PORT = "metro" }`"#),
        "{fix}"
    );
    assert!(fix.contains("[processes.mobile]"), "{fix}");
}

// A framework told its port by a flag gets the flag, on the script it
// runs, through the runner's own separator.
#[test]
fn a_process_whose_framework_takes_a_flag_is_given_the_command_with_it() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("package.json"),
        r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"6"}}"#,
    )
    .expect("package.json");
    write_project_config(&fx, "[dev]\ncmd = \"npm run dev\"\n");
    let report = report(&fx);
    let found = portless(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(
            r#"`echo '{"dev_cmd":"npm run dev -- --port {port:web}"}' | pando init --answers - --replace`"#
        ),
        "{fix}"
    );

    // A command that hands its script arguments already has npm's `--`.
    write_project_config(&fx, "[dev]\ncmd = \"npm run dev -- --host\"\n");
    let again = self::report(&fx);
    let found = portless(&again);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&again));
    };
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(r#"{"dev_cmd":"npm run dev -- --host --port {port:web}"}"#),
        "{fix}"
    );
}

// Said, not guessed at: `ports = []` is a process that has none, a
// command that fixes its own port is not moved by one pando hands it,
// and a plain `node` script is as often a worker as a server.
#[test]
fn a_process_that_has_said_or_may_have_no_port_is_left_alone() {
    for config in [
        "[dev]\ncmd = \"npm run start\"\nports = []\n",
        "[dev]\ncmd = \"npx expo start --port 8082\"\n",
        "[processes.worker]\ncmd = \"node worker.js\"\n",
    ] {
        let fx = fixture();
        write_expo_app(&fx.root);
        write_project_config(&fx, config);
        let report = report(&fx);
        assert!(
            portless(&report).is_empty(),
            "{config}: {:?}",
            messages(&report)
        );
    }
}

// Where taking the line out does reopen the question, both ways are
// given.
#[test]
fn a_stale_command_can_be_answered_again_or_deleted() {
    let fx = fixture();
    write_makefile(&fx, GUARDED_MAKEFILE);
    write_project_config(
        &fx,
        &format!("[dev]\ncmd = '{LIFTED_GUARD}'  # detected: the dev target\n"),
    );
    let report = report(&fx);
    let finding = stale(&report).unwrap_or_else(|| panic!("{:?}", messages(&report)));
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(r#"`echo '{"dev_cmd":"make dev"}' | pando init --answers - --replace`"#),
        "{fix}"
    );
    assert!(fix.contains("delete that line"), "{fix}");
}

// The process list's answer is every process table at once, and
// `--replace` takes them all away before it writes: a process added by
// hand beside the stale one goes too. The fix says so rather than
// promising one line.
#[test]
fn a_stale_process_key_says_its_command_replaces_every_process_table() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("apps/mobile")).expect("app dir");
    write_expo_app(&fx.root.join("apps/mobile"));
    std::fs::create_dir_all(fx.root.join("backend")).expect("backend dir");
    std::fs::write(
        fx.root.join("backend/package.json"),
        r#"{"name":"backend","scripts":{"dev":"tsx watch src/index.ts"},"dependencies":{"express":"5"}}"#,
    )
    .expect("package.json");
    write_project_config(
        &fx,
        "[processes.mobile]\ncmd = \"npm run start --old\"  # detected: package.json scripts.start\n\
         cwd = \"apps/mobile\"\nports = [\"mobile\"]\n\n\
         [processes.worker]\ncmd = \"sleep 100\"\nports = []\n",
    );
    let report = report(&fx);
    let finding = stale(&report).unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert!(
        finding.message.starts_with("processes.mobile.cmd = "),
        "{}",
        finding.message
    );
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(fix.contains("| pando init --answers - --replace`"), "{fix}");
    assert!(
        fix.contains("replaces every process table in") && fix.contains("added to them by hand"),
        "{fix}"
    );
    assert!(!fix.contains("in its place"), "{fix}");
}

#[test]
fn a_project_layer_that_does_not_load_is_the_headline_problem() {
    let fx = fixture();
    write_project_config(&fx, "[project]\nnonsense_key = 1\n");
    let report = report(&fx);
    assert!(!report.healthy());
    assert!(report.config.error.is_some());
    assert!(
        mentions(&report, "the config does not load"),
        "{:?}",
        messages(&report)
    );
    // And the file is still shown, key by key: the point is to see
    // what is in it.
    assert!(
        report.config.layers[2]
            .keys
            .iter()
            .any(|k| k.key == "project.nonsense_key")
    );
}

#[test]
fn a_layer_that_is_not_valid_toml_says_so_instead_of_pretending_it_is_empty() {
    let fx = fixture();
    write_project_config(&fx, "[project\n");
    let report = report(&fx);
    let project = &report.config.layers[2];
    assert!(project.present);
    assert!(
        project
            .error
            .as_deref()
            .is_some_and(|e| e.contains("not valid TOML")),
        "{:?}",
        project.error
    );
}

#[test]
fn a_provision_path_the_repository_does_not_ignore_is_a_problem() {
    let fx = fixture();
    std::fs::write(fx.root.join(".gitignore"), ".env\n").expect("gitignore");
    git(&fx.root, &["add", "."]);
    git(&fx.root, &["commit", "--quiet", "-m", "ignore"]);
    write_project_config(&fx, "[project]\nprovision = [\".env\", \"secrets.json\"]\n");
    let report = report(&fx);
    assert!(!report.healthy());
    assert!(
        mentions(&report, "\"secrets.json\""),
        "{:?}",
        messages(&report)
    );
    assert!(
        !mentions(&report, "\".env\""),
        "an ignored path is fine: {:?}",
        messages(&report)
    );
}

#[test]
fn a_provision_from_entry_for_a_path_nothing_provisions_is_a_note() {
    let fx = fixture();
    std::fs::write(fx.root.join(".gitignore"), ".env\n").expect("gitignore");
    git(&fx.root, &["add", "."]);
    git(&fx.root, &["commit", "--quiet", "-m", "ignore"]);
    write_project_config(
        &fx,
        "[project]\nprovision = [\".env\"]\n\n[project.provision_from]\n\
             \".env.local\" = \".env.example\"\n",
    );
    let report = report(&fx);
    assert!(report.healthy(), "{:?}", report.findings);
    assert!(
        mentions(&report, "\".env.local\""),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn an_install_command_that_can_rewrite_a_lockfile_is_a_problem() {
    for (install, expected) in [
        ("pnpm install", "pnpm install --frozen-lockfile"),
        ("npm install", "npm ci"),
        ("yarn install", "yarn install --frozen-lockfile"),
        ("uv sync", "uv sync --frozen"),
        ("bundle install", "BUNDLE_FROZEN=true bundle install"),
    ] {
        let fx = fixture();
        write_project_config(&fx, &format!("[project]\ninstall = {install:?}\n"));
        let report = report(&fx);
        assert!(!report.healthy(), "{install} should be a problem");
        let fix = report
            .findings
            .iter()
            .find(|f| f.message.contains("non-frozen install"))
            .and_then(|f| f.fix.clone())
            .unwrap_or_default();
        assert!(fix.contains(expected), "{install}: {fix}");
    }
}

// The install pando proposes for a project that gitignores its lockfile
// is not a problem there — and still is where the lockfile is tracked.
#[test]
fn the_plain_install_is_fine_where_the_lockfile_is_gitignored() {
    let fx = fixture();
    std::fs::write(fx.root.join(".gitignore"), "package-lock.json\n").unwrap();
    write_project_config(&fx, "[project]\ninstall = \"npm install\"\n");
    let report = report(&fx);
    assert!(
        !report
            .findings
            .iter()
            .any(|f| f.message.contains("non-frozen")),
        "{:?}",
        report.findings
    );
}

// A `bun.lockb` in the gitignore says nothing about the `bun.lock` the
// project tracks, which is what `bun install` would rewrite.
#[test]
fn the_plain_install_is_a_problem_where_only_another_lockfile_name_is_ignored() {
    let fx = fixture();
    std::fs::write(fx.root.join(".gitignore"), "bun.lockb\n").unwrap();
    std::fs::write(fx.root.join("bun.lock"), "{}\n").unwrap();
    write_project_config(&fx, "[project]\ninstall = \"bun install\"\n");
    let report = report(&fx);
    assert!(
        mentions(&report, "non-frozen install"),
        "{:?}",
        report.findings
    );
}

// The old bun habit: only the binary lockfile is gitignored, and it is the
// one present. A new worktree has none, and there `bun install` from bun
// 1.2 on writes a `bun.lock` git does not ignore: the install that writes
// no lockfile is the one pando proposes, and the one doctor suggests.
#[test]
fn the_install_that_writes_no_lockfile_is_fine_where_the_lockfile_present_is_the_one_ignored() {
    let fx = fixture();
    std::fs::write(fx.root.join(".gitignore"), "bun.lockb\n").unwrap();
    std::fs::write(fx.root.join("bun.lockb"), "\0").unwrap();
    write_project_config(&fx, "[project]\ninstall = \"bun install --no-save\"\n");
    let unsaved = report(&fx);
    assert!(
        !mentions(&unsaved, "non-frozen install"),
        "{:?}",
        unsaved.findings
    );

    write_project_config(&fx, "[project]\ninstall = \"bun install\"\n");
    let plain = report(&fx);
    let fix = plain
        .findings
        .iter()
        .find(|f| f.message.contains("non-frozen install"))
        .and_then(|f| f.fix.clone())
        .unwrap_or_default();
    assert_eq!(fix, "use `bun install --no-save`", "{:?}", plain.findings);
}

#[test]
fn a_frozen_install_and_a_command_pando_has_no_opinion_about_are_both_fine() {
    for install in [
        "pnpm install --frozen-lockfile",
        "npm ci",
        "BUNDLE_FROZEN=true bundle install",
        "make setup",
        "cd apps/web && pnpm install --frozen-lockfile",
    ] {
        let fx = fixture();
        write_project_config(&fx, &format!("[project]\ninstall = {install:?}\n"));
        let report = report(&fx);
        assert!(report.healthy(), "{install}: {:?}", report.findings);
    }
}

#[test]
fn a_chained_install_whose_second_step_is_not_frozen_is_still_caught() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[project]\ninstall = \"corepack enable && pnpm install\"\n",
    );
    let report = report(&fx);
    assert!(!report.healthy(), "{:?}", report.findings);
}

#[test]
fn a_port_placeholder_naming_a_role_nothing_owns_is_a_problem() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[processes.web]\ncmd = \"serve --port {port:web}\"\nports = [\"web\"]\n\
             env = { API = \"http://localhost:{port:api}\" }\n",
    );
    let report = report(&fx);
    assert!(!report.healthy(), "{:?}", report.findings);
    assert!(mentions(&report, "env.API"), "{:?}", messages(&report));
    assert!(mentions(&report, "{port:api}"), "{:?}", messages(&report));
}

#[test]
fn a_hooks_command_is_checked_too_and_a_hook_owns_no_role_of_its_own() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[processes.dev]\ncmd = \"serve\"\nports = [\"web\"]\n\n\
             [[hooks]]\nname = \"migrate\"\nafter = \"services\"\n\
             cmd = \"migrate --at {port:db}\"\n",
    );
    let report = report(&fx);
    assert!(!report.healthy(), "{:?}", report.findings);
    assert!(
        mentions(&report, "hook \"migrate\": cmd cannot be resolved"),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn a_process_cwd_that_cannot_be_resolved_is_caught_with_its_command() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[processes.dev]\ncmd = \"serve\"\nports = [\"web\"]\ncwd = \"apps/{nope}\"\n",
    );
    let report = report(&fx);
    assert!(!report.healthy(), "{:?}", report.findings);
    assert!(
        mentions(&report, "cwd cannot be resolved"),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn a_placeholder_naming_a_service_resolves_because_a_service_is_a_role_too() {
    let fx = fixture();
    write_compose(
        &fx,
        "services:\n  postgres:\n    image: postgres:16\n    healthcheck:\n      \
             test: [\"CMD\", \"true\"]\n",
    );
    write_project_config(
        &fx,
        "[processes.web]\ncmd = \"serve\"\nports = [\"web\"]\n\
             env = { DB = \"postgres://localhost:{port:postgres}\" }\n\n\
             [[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\n\
             include = [\"postgres\"]\n",
    );
    let report = report(&fx);
    assert!(report.healthy(), "{:?}", report.findings);
}

// An isolated start gives a native service's name a port as it does a
// compose one's; doctor's roles had only the compose names, so this was a
// problem a start never hit.
#[test]
fn a_placeholder_naming_a_native_service_resolves_as_well() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[processes.web]\ncmd = \"serve\"\nports = [\"web\"]\n\
             env = { DB = \"postgres://localhost:{port:postgres}\" }\n\n\
             [[services]]\nkind = \"native\"\nname = \"postgres\"\n\n\
             [[hooks]]\nname = \"migrate\"\nafter = \"services\"\n\
             cmd = \"migrate --port {port:postgres}\"\n",
    );
    let report = report(&fx);
    assert!(
        !mentions(&report, "cannot be resolved"),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn the_project_section_reports_the_home_mode_and_the_port_window() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    std::fs::set_permissions(&fx.home, std::fs::Permissions::from_mode(0o755)).expect("chmod home");
    let report = report(&fx);
    assert_eq!(report.project.home_mode.as_deref(), Some("755"));
    assert!(mentions(&report, "mode 755"), "{:?}", messages(&report));
    assert!(report.healthy(), "a loose home is a note, not a problem");
    assert_eq!(report.project.port_min, ports::PORT_MIN);
    assert_eq!(report.project.port_max, ports::PORT_MAX);
    assert_eq!(report.project.windows_held, 0);
}

// ---- tools ------------------------------------------------------

fn tool(report: &Report, name: &str) -> ToolReport {
    report
        .tools
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no {name} in {:?}", report.tools))
        .clone()
}

#[test]
fn a_missing_tool_is_a_line_and_only_a_problem_when_this_project_needs_it() {
    let fx = fixture();
    let report = report_of(&fx, &no_tools);
    // git is not optional: worktrees are git's.
    assert!(!report.healthy(), "{:?}", report.findings);
    assert!(
        mentions(&report, "git is not on the PATH"),
        "{:?}",
        messages(&report)
    );
    // cloudflared is, and `share` says so itself.
    assert!(!tool(&report, "cloudflared").found);
    assert!(
        !mentions(&report, "cloudflared is not on the PATH"),
        "{:?}",
        messages(&report)
    );
    // Every tool still gets a line, whether or not it is there.
    let text = report.render();
    assert!(text.contains("cloudflared"), "{text}");
    assert!(text.contains("`pando share`"), "{text}");
}

#[test]
fn a_missing_tool_says_how_to_get_it_and_a_found_one_does_not() {
    let fx = fixture();
    let missing = report_of(&fx, &no_tools);
    let text = missing.render();
    for name in ["git", "docker", "cloudflared", "gh"] {
        let get = crate::catalog::tools::how_to_get(name).expect("a catalog row");
        assert_eq!(tool(&missing, name).install.as_deref(), Some(get), "{name}");
        let line = format!("install it with: {get} — pando never will");
        assert!(text.contains(&line), "{text}");
    }
    // A finding's fix names the same command as the line does.
    let git = missing
        .findings
        .iter()
        .find(|f| f.message.starts_with("git is not on"))
        .expect("git's finding");
    assert!(
        git.fix
            .as_deref()
            .is_some_and(|fix| fix.contains("xcode-select")),
        "{git:?}"
    );
    // `docker compose` is docker's own news, told once.
    assert_eq!(tool(&missing, "docker compose").install, None);

    let found = report(&fx);
    for tool in &found.tools {
        assert_eq!(tool.install, None, "{}", tool.name);
    }
    assert!(!found.render().contains("install it with:"));
}

// Every key of a `tools[]` entry is one an agent reads, so the contract
// names each of them.
#[test]
fn agent_json_documents_every_tools_key() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    let section = doc
        .split("## `pando doctor --json`")
        .nth(1)
        .expect("the doctor section")
        .split("\n## ")
        .next()
        .unwrap();
    let fx = fixture();
    let published = serde_json::to_value(tool(&report_of(&fx, &no_tools), "git")).unwrap();
    for key in published.as_object().unwrap().keys() {
        assert!(
            section.contains(&format!("\"{key}\"")),
            "agent/json.md never documents doctor's tools[].{key}"
        );
    }
}

#[test]
fn docker_missing_is_a_note_only_when_the_project_declares_compose_services() {
    let fx = fixture();
    let plain = report_of(&fx, &no_tools);
    assert!(
        !mentions(&plain, "docker is not on the PATH"),
        "{:?}",
        messages(&plain)
    );
    write_project_config(
        &fx,
        "[[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\n\
             include = [\"postgres\"]\n",
    );
    let with_services = report_of(&fx, &no_tools);
    let docker = with_services
        .findings
        .iter()
        .find(|f| f.message.starts_with("docker is not on the PATH"))
        .unwrap_or_else(|| panic!("{:?}", messages(&with_services)));
    // A note, not a problem: a plain `start` still works, and only
    // `--isolated` does not.
    assert_eq!(docker.severity, Severity::Note);
    assert!(
        docker.message.contains("a plain `start` still can"),
        "{}",
        docker.message
    );
    // pando runs docker itself, so a prelude is not the way to reach it.
    let fix = docker.fix.as_deref().unwrap_or_default();
    assert!(!fix.contains("prelude"), "{fix}");
    assert!(
        fix.contains(&fx.home.join("bin").join("docker").display().to_string()),
        "{fix}"
    );
}

#[test]
fn a_tool_reports_the_path_it_resolved_from_and_anything_else_worth_a_line() {
    let fx = fixture();
    let shell = |script: &str| -> Option<String> {
        if !script.contains(TOOL_DONE_MARK) {
            return Some("pando-runtime-ok\n".to_string());
        }
        Some(format!(
            "{TOOL_PATH_MARK}1 /usr/local/bin/docker\n\
                 {TOOL_VERSION_MARK}1 Docker version 27.0.3, build 1234\n\
                 {TOOL_DETAIL_MARK}1 desktop-linux\n\
                 {TOOL_DONE_MARK}\n"
        ))
    };
    let report = report_of(&fx, &shell);
    let docker = tool(&report, "docker");
    assert!(docker.found);
    assert_eq!(docker.path.as_deref(), Some("/usr/local/bin/docker"));
    assert_eq!(
        docker.version.as_deref(),
        Some("Docker version 27.0.3, build 1234")
    );
    assert_eq!(docker.detail.as_deref(), Some("context: desktop-linux"));
    let text = report.render();
    assert!(text.contains("/usr/local/bin/docker"), "{text}");
    assert!(text.contains("context: desktop-linux"), "{text}");
}

#[test]
fn the_program_a_config_tells_pando_to_run_is_a_problem_when_it_is_missing() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[project]\ninstall = \"pnpm install --frozen-lockfile\"\n",
    );
    let report = report_of(&fx, &no_tools);
    assert!(!report.healthy());
    assert!(
        mentions(&report, "pnpm is not on the PATH"),
        "{:?}",
        messages(&report)
    );
    assert!(
        mentions(&report, "the install step"),
        "and why pando wanted it: {:?}",
        messages(&report)
    );
}

/// A shell that finds every tool but one.
fn every_tool_but(missing: &'static str) -> impl Fn(&str) -> Option<String> {
    move |script: &str| {
        let answer = every_tool(script)?;
        // Each probe is one `if … fi` line of the script, naming its index.
        let asked = format!("command -v '{missing}' ");
        let Some(index) = script
            .lines()
            .find(|l| l.contains(&asked))
            .and_then(|l| l.split(TOOL_PATH_MARK).nth(1))
            .and_then(|rest| rest.split(' ').next())
        else {
            return Some(answer);
        };
        let found = [
            format!("{TOOL_PATH_MARK}{index} "),
            format!("{TOOL_VERSION_MARK}{index} "),
        ];
        Some(
            answer
                .lines()
                .filter(|l| !found.iter().any(|mark| l.starts_with(mark.as_str())))
                .map(|l| format!("{l}\n"))
                .collect(),
        )
    }
}

#[test]
fn a_hook_switched_off_needs_nothing_on_the_path() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[hooks]]\nname = \"schema\"\nafter = \"services\"\n\
         cmd = \"alembic upgrade head\"\non = \"never\"\n",
    );
    let report = report_of(&fx, &every_tool_but("alembic"));
    assert!(report.healthy(), "{:?}", report.findings);
    assert!(
        !mentions(&report, "alembic is not on the PATH"),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn a_hook_only_a_start_with_its_own_data_runs_is_a_note_when_its_program_is_missing() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("docker-compose.yml"),
        "services:\n  postgres:\n    image: postgres:16\n",
    )
    .expect("compose file");
    let services = "[[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\n\
                    include = [\"postgres\"]\n";
    let isolated = "[[hooks]]\nname = \"schema\"\nafter = \"services\"\n\
                    cmd = \"alembic upgrade head\"\n";
    write_project_config(&fx, &format!("{services}{isolated}"));
    let report = report_of(&fx, &every_tool_but("alembic"));
    assert!(report.healthy(), "{:?}", report.findings);
    let alembic = report
        .findings
        .iter()
        .find(|f| f.message.starts_with("alembic is not on the PATH"))
        .unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert_eq!(alembic.severity, Severity::Note);
    assert!(
        alembic.message.contains("`--namespaced`"),
        "{}",
        alembic.message
    );

    // A hook every start runs, on the same program, is the one it names.
    write_project_config(
        &fx,
        &format!(
            "{services}{isolated}[[hooks]]\nname = \"check\"\nafter = \"dev\"\n\
             cmd = \"alembic check\"\n"
        ),
    );
    let report = report_of(&fx, &every_tool_but("alembic"));
    assert!(!report.healthy());
    assert!(
        mentions(
            &report,
            "alembic is not on the PATH `bash -lc` has — the hook \"check\""
        ),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn a_probe_that_never_finished_claims_nothing_about_the_machine() {
    let fx = fixture();
    // No done mark: the shell died, or the prelude in front of it did.
    let shell = |_: &str| Some("bash: line 1: nvm: command not found\n".to_string());
    let report = report_of(&fx, &shell);
    assert!(
        report.healthy(),
        "a shell that did not answer is not evidence that git is missing: {:?}",
        report.findings
    );
    assert!(
        mentions(&report, "could not ask this shell what it has"),
        "{:?}",
        messages(&report)
    );
    assert!(
        mentions(&report, "command not found"),
        "{:?}",
        messages(&report)
    );
    // Nor do its rows: a tool nobody could ask about is not "not found".
    let text = report.render();
    assert!(!text.contains("not found — "), "{text}");
    assert!(text.contains("no answer from the shell"), "{text}");
}

// Every process, hook and share runs behind the prelude, so one that
// fails is every spawn failing. It was a note, and in a repository that
// pins no runtime nothing else said it, so doctor exited 0.
#[test]
fn a_prelude_that_fails_in_front_of_the_probe_is_a_problem_naming_its_file() {
    let fx = fixture();
    write_project_config(&fx, "[runtime]\nprelude = \"source ~/.nvm/nvm.sh\"\n");
    let shell =
        |_: &str| Some("bash: /Users/someone/.nvm/nvm.sh: No such file or directory\n".to_string());
    let report = report_of(&fx, &shell);
    assert!(!report.healthy(), "{:?}", messages(&report));
    let problem = report
        .findings
        .iter()
        .find(|f| f.section == Section::Tools && f.severity == Severity::Problem)
        .unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert!(
        problem.message.contains("\"source ~/.nvm/nvm.sh\""),
        "{}",
        problem.message
    );
    assert!(
        problem.message.contains("No such file or directory"),
        "{}",
        problem.message
    );
    assert!(
        problem
            .message
            .contains(&fx.paths.config_file().display().to_string()),
        "{}",
        problem.message
    );
    assert!(
        !mentions(&report, "is not on the PATH"),
        "a prelude that failed says nothing about what the PATH has: {:?}",
        messages(&report)
    );
}

// docker and cloudflared are asked before the prelude, so they answer
// whether it works or not: their lines are no sign it got through.
#[test]
fn a_prelude_that_fails_after_the_tools_pando_runs_itself_answered_is_a_problem() {
    let fx = fixture();
    let prelude = "source ~/.nvm/nvm.sh";
    write_project_config(&fx, &format!("[runtime]\nprelude = \"{prelude}\"\n"));
    let error = "bash: /Users/someone/.nvm/nvm.sh: No such file or directory\n";
    let shell = |script: &str| {
        let Some((direct, _)) = script.split_once(&format!("{prelude} && {{")) else {
            return Some(error.to_string());
        };
        let mut out = String::new();
        for index in direct
            .lines()
            .filter_map(|l| l.split(TOOL_PATH_MARK).nth(1))
            .filter_map(|rest| rest.split(' ').next())
        {
            let _ = writeln!(out, "{TOOL_PATH_MARK}{index} /usr/local/bin/thing{index}");
            let _ = writeln!(out, "{TOOL_VERSION_MARK}{index} 1.2.3");
        }
        assert!(
            !out.is_empty(),
            "no tool is asked before the prelude: {script}"
        );
        Some(out + error)
    };
    let report = report_of(&fx, &shell);
    assert!(!report.healthy(), "{:?}", messages(&report));
    assert!(
        report.findings.iter().any(|f| f.section == Section::Tools
            && f.severity == Severity::Problem
            && f.message.contains("No such file or directory")),
        "{:?}",
        messages(&report)
    );
    assert!(
        !mentions(&report, "could not ask this shell what it has"),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn the_tool_script_runs_behind_the_prelude_a_real_spawn_would_use() {
    let probes = vec![ToolProbe {
        name: "git".to_string(),
        program: "git".to_string(),
        args: "--version",
        detail_args: None,
        detail_label: "",
        needed_for: "worktrees".to_string(),
        missing: None,
        direct: false,
    }];
    let script = tool_script(&probes, "nvm use 22");
    assert!(script.starts_with("nvm use 22 && {"), "{script}");
    assert!(script.contains("command -v 'git'"), "{script}");
    assert!(tool_script(&probes, "").starts_with("if "));
}

// pando runs docker itself, on the PATH it was started with: a prelude
// that finds it helps no isolated start, so it must not help the probe.
#[test]
fn a_tool_pando_runs_itself_is_looked_for_without_the_prelude() {
    let dir = TempDir::new().unwrap();
    let only_prelude = dir.path().join("only-on-the-prelude");
    std::fs::create_dir_all(&only_prelude).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let fake = only_prelude.join("pando-fake-docker");
        std::fs::write(&fake, "#!/bin/sh\necho 'Docker version 27'\n").unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let probe = |direct: bool| ToolProbe {
        name: "docker".to_string(),
        program: "pando-fake-docker".to_string(),
        args: "--version",
        detail_args: None,
        detail_label: "",
        needed_for: "isolated starts".to_string(),
        missing: None,
        direct,
    };
    let probes = vec![probe(true), probe(false)];
    let prelude = format!(
        "export PATH={}:\"$PATH\"",
        crate::process::shell_quote(&only_prelude.display().to_string())
    );
    let script = tool_script(&probes, &prelude);
    let out = std::process::Command::new("bash")
        .arg("-c")
        .arg(&script)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(TOOL_DONE_MARK), "{script}\n{text}");
    assert!(
        !text.contains(&format!("{TOOL_PATH_MARK}0 ")),
        "the prelude found it for the direct probe: {text}"
    );
    assert!(
        text.contains(&format!("{TOOL_PATH_MARK}1 ")),
        "and the same program behind the prelude is found: {text}"
    );
}

#[test]
fn a_program_name_comes_from_the_last_step_of_a_chained_command() {
    assert_eq!(command_program("pnpm dev").as_deref(), Some("pnpm"));
    assert_eq!(
        command_program("corepack enable && pnpm install").as_deref(),
        Some("pnpm")
    );
    assert_eq!(
        command_program("BUNDLE_FROZEN=true bundle install").as_deref(),
        Some("bundle")
    );
    // A path or a template is not a name worth asking about, and is
    // exactly the shape that would put something odd on a command line.
    assert_eq!(command_program("./scripts/setup.sh"), None);
    assert_eq!(command_program("{worktree}/run"), None);
}

// ---- runtime ----------------------------------------------------

/// A shell that resolves node, for the tests about what this machine
/// answers. The marks are `runtime`'s own: a fake that stopped
/// matching them would read as "could not parse" and fail loudly.
fn shell_resolving_node<'a>(
    version: &'a str,
    path: &'a str,
) -> impl Fn(&str) -> Option<String> + 'a {
    move |script: &str| {
        // Tools are not what these tests are about, so they are all
        // there: a missing git would be a problem in every one of them.
        if script.contains(TOOL_DONE_MARK) {
            return every_tool(script);
        }
        Some(format!(
            "pando-runtime-path:{path}\npando-runtime-version:v{version}\n\
                 pando-runtime-ok\n"
        ))
    }
}

/// A shell that resolves node `without`, and `with` once a prelude
/// carrying `needle` is in front of it — a version manager's line, or a
/// directory put first on PATH.
fn shell_resolving_node_behind<'a>(
    without: &'a str,
    needle: &'a str,
    with: &'a str,
) -> impl Fn(&str) -> Option<String> + 'a {
    move |script: &str| {
        if script.contains(TOOL_DONE_MARK) {
            return every_tool(script);
        }
        let version = match script.contains(needle) {
            true => with,
            false => without,
        };
        Some(format!(
            "pando-runtime-path:/n/{version}/bin/node\npando-runtime-version:v{version}\n\
             pando-runtime-status:0\npando-runtime-ok\n"
        ))
    }
}

/// The runtime finding's fix, whole.
fn runtime_fix(report: &Report) -> String {
    report
        .findings
        .iter()
        .find(|f| f.section == Section::Runtime)
        .and_then(|f| f.fix.clone())
        .unwrap_or_default()
}

fn pin_node(fx: &Fx, version: &str) {
    std::fs::write(fx.root.join(".nvmrc"), format!("{version}\n")).expect("write .nvmrc");
}

#[test]
fn a_runtime_the_shell_resolves_says_nothing_and_still_shows_its_working() {
    let fx = fixture();
    pin_node(&fx, "22");
    let report = report_of(&fx, &shell_resolving_node("22.14.0", "/n/bin/node"));
    assert!(report.healthy(), "{:?}", report.findings);
    let node = &report.runtime.languages[0];
    assert_eq!(node.verdict, "satisfied");
    assert_eq!(node.spec, "22");
    assert_eq!(node.source, ".nvmrc");
    assert_eq!(node.resolved.as_deref(), Some("22.14.0"));
    assert_eq!(node.resolved_from.as_deref(), Some("/n/bin/node"));
    let text = report.render();
    assert!(text.contains("wants 22 (.nvmrc)"), "{text}");
    assert!(
        text.contains("/n/bin/node"),
        "the path it resolved from is the diagnosis:\n{text}"
    );
}

#[test]
fn a_mismatch_nobody_has_been_asked_about_is_a_note_with_the_question_coming() {
    let fx = fixture();
    pin_node(&fx, "22");
    let report = report_of(&fx, &shell_resolving_node("24.21.0", "/n/bin/node"));
    assert!(
        report.healthy(),
        "the next start asks; that is designed, not broken: {:?}",
        report.findings
    );
    assert!(
        mentions(&report, "nobody has answered the prelude question"),
        "{:?}",
        messages(&report)
    );
    assert!(
        mentions(&report, "24.21.0, from /n/bin/node"),
        "{:?}",
        messages(&report)
    );
    assert_eq!(report.runtime.languages[0].verdict, "mismatch");
}

// A shim whose pinned version is not installed names that version in its
// error. doctor called it satisfied; it is a mismatch, and says what the
// shim said.
#[test]
fn a_shim_whose_version_is_not_installed_is_a_mismatch_in_its_own_words() {
    let fx = fixture();
    pin_node(&fx, "22");
    let error = "nodenv: version `22' is not installed (set by .nvmrc)";
    let shell = |script: &str| {
        if script.contains(TOOL_DONE_MARK) {
            return every_tool(script);
        }
        Some(crate::runtime::probe_failure("/n/shims/node", error, 1))
    };
    let report = report_of(&fx, &shell);
    let node = &report.runtime.languages[0];
    assert_eq!(node.verdict, "mismatch");
    assert_eq!(node.resolved, None);
    assert_eq!(node.failure.as_deref(), Some(error));
    let said = format!("finds /n/shims/node, and it fails: {error}");
    assert!(mentions(&report, &said), "{:?}", messages(&report));
    let text = report.render();
    assert!(text.contains(&said), "{text}");
}

#[test]
fn a_mismatch_with_a_prelude_that_says_nothing_is_needed_is_a_problem() {
    let fx = fixture();
    pin_node(&fx, "22");
    write_project_config(&fx, "[runtime]\nprelude = \"\"\n");
    let report = report_of(&fx, &shell_resolving_node("24.21.0", "/n/bin/node"));
    assert!(!report.healthy(), "{:?}", report.findings);
    assert!(
        mentions(&report, "which says this machine needs nothing"),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn a_mismatch_with_a_prelude_set_names_the_prelude_and_the_file_it_is_in() {
    let fx = fixture();
    pin_node(&fx, "22");
    write_project_config(&fx, "[runtime]\nprelude = \"nvm use 18\"\n");
    let report = report_of(&fx, &shell_resolving_node("24.21.0", "/n/bin/node"));
    assert!(!report.healthy(), "{:?}", report.findings);
    assert!(
        mentions(&report, "\"nvm use 18\""),
        "{:?}",
        messages(&report)
    );
    assert!(
        mentions(&report, &fx.paths.config_file().display().to_string()),
        "{:?}",
        messages(&report)
    );
    assert!(
        mentions(&report, "is not working"),
        "{:?}",
        messages(&report)
    );
    assert_eq!(
        report.runtime.prelude_from.as_deref(),
        Some(fx.paths.config_file().display().to_string().as_str())
    );
}

// A start skips the runtime check for a language whose every command goes
// through `uv run`, which finds the pinned Python itself. doctor checked
// it anyway, so a project that starts without a question read as one
// whose prelude was broken, and doctor failed.
#[test]
fn a_mismatch_uv_run_resolves_for_every_command_is_not_reported() {
    let fx = fixture();
    std::fs::write(fx.root.join(".python-version"), "3.12\n").expect("write .python-version");
    std::fs::write(fx.root.join("uv.lock"), "version = 1\n").expect("write uv.lock");
    let shell = |has_uv: bool| {
        move |script: &str| -> Option<String> {
            if script.contains(TOOL_DONE_MARK) {
                return every_tool(script);
            }
            if script.contains("command -v uv") {
                return Some(if has_uv { "pando-runner-ok\n" } else { "" }.to_string());
            }
            Some(crate::runtime::probe_reply("/usr/bin/python3", "3.9.6"))
        }
    };
    let runtime_findings = |report: &Report| -> Vec<String> {
        report
            .findings
            .iter()
            .filter(|f| f.section == Section::Runtime)
            .map(|f| f.message.clone())
            .collect()
    };
    for prelude in ["", "[runtime]\nprelude = \"source ~/.profile-extra\"\n\n"] {
        write_project_config(
            &fx,
            &format!("{prelude}[dev]\ncmd = \"uv run python manage.py runserver\"\n"),
        );
        let report = report_of(&fx, &shell(true));
        assert!(
            runtime_findings(&report).is_empty(),
            "{prelude:?}: {:?}",
            messages(&report)
        );
        assert!(report.healthy(), "{prelude:?}: {:?}", messages(&report));

        // A shell with no uv gets the interpreter it resolves, so the
        // mismatch is real there.
        let report = report_of(&fx, &shell(false));
        assert!(
            !runtime_findings(&report).is_empty(),
            "{prelude:?}: {:?}",
            messages(&report)
        );
    }
}

#[test]
fn a_mismatch_offers_the_prelude_of_a_manager_this_machine_really_has() {
    let fx = fixture();
    pin_node(&fx, "22");
    // Under the *injected* machine home, so what this laptop has
    // installed never decides the assertion.
    std::fs::create_dir_all(fx.machine_home.join(".nvm")).expect("nvm dir");
    std::fs::write(fx.machine_home.join(".nvm/nvm.sh"), "# fake\n").expect("nvm.sh");
    let report = report_of(
        &fx,
        &shell_resolving_node_behind("24.21.0", "nvm.sh", "22.14.0"),
    );
    let node = &report.runtime.languages[0];
    assert!(node.managers.contains(&"nvm"), "{:?}", node.managers);
    let from_home = node
        .fixes
        .iter()
        .find(|fix| fix.contains(&fx.machine_home.display().to_string()))
        .expect("a fix built from the injected home");
    assert!(from_home.contains("nvm use"), "{from_home}");
    let fix = runtime_fix(&report);
    let user = fx.paths.user_config_file();
    assert!(
        fix.contains(&format!(
            "set [runtime].prelude in {} to one of:",
            user.display()
        )),
        "{fix}"
    );
    assert!(fix.contains("nvm"), "{fix}");
    assert!(
        fix.ends_with(
            "`pando init --answers -` with {\"prelude\": \"<the line>\"} writes it there, and \
             `prelude = \"\"` there accepts the mismatch"
        ),
        "nobody has answered, so the question's own answer is the way in: {fix}"
    );
    let message = &report
        .findings
        .iter()
        .find(|f| f.section == Section::Runtime)
        .unwrap()
        .message;
    assert!(
        message.ends_with("so the next `init`, `check` or `start` asks"),
        "{message}"
    );
}

// A prelude that is set and not working is a person's to change: the fix
// names the file it is in, and no `init --answers`, which would refuse it.
#[test]
fn a_set_prelude_that_fails_is_changed_in_its_own_file() {
    let fx = fixture();
    pin_node(&fx, "22");
    let user = fx.paths.user_config_file();
    std::fs::create_dir_all(user.parent().unwrap()).unwrap();
    std::fs::write(&user, "[runtime]\nprelude = \"true\"\n").unwrap();
    let report = report_of(&fx, &shell_resolving_node("24.21.0", "/n/bin/node"));
    let fix = runtime_fix(&report);
    assert!(
        fix.contains(&format!("set [runtime].prelude in {}", user.display())),
        "{fix}"
    );
    assert!(!fix.contains("--answers"), "{fix}");
    assert!(
        fix.ends_with("`prelude = \"\"` there accepts the mismatch"),
        "{fix}"
    );
}

// A manager that is here and gives `bash -lc` nothing the project accepts
// is not "no manager": the fix names it and the command that would put
// the version under it, and never says no manager is installed while the
// same entry lists one.
#[test]
fn a_manager_without_the_version_is_named_with_its_install_command() {
    let fx = fixture();
    pin_node(&fx, "22");
    std::fs::create_dir_all(fx.machine_home.join(".nvm")).expect("nvm dir");
    std::fs::write(fx.machine_home.join(".nvm/nvm.sh"), "# fake\n").expect("nvm.sh");
    let report = report_of(&fx, &shell_resolving_node("24.21.0", "/n/bin/node"));
    let fix = runtime_fix(&report);
    assert!(
        fix.contains(&format!(
            "nvm is installed here, and gives `bash -lc` no node 22 — `nvm install 22` puts one \
             under nvm (pando never installs one), or set [runtime].prelude in {} to a line \
             that puts a node 22 first on PATH",
            fx.paths.user_config_file().display()
        )),
        "{fix}"
    );
    assert!(!fix.contains("no version manager"), "{fix}");
    assert!(
        !fix.contains("to one of:"),
        "nvm's line does not work yet, so it is not what to set: {fix}"
    );
    assert!(
        fix.contains("then [runtime].prelude can be one of:") && fix.contains("nvm use"),
        "and it is what to set once the version is there: {fix}"
    );
}

// The same when nvm is here but its line cannot read the requirement —
// a `.nvmrc` in an app directory — so there is no line of its own.
#[test]
fn a_manager_with_no_line_for_the_requirement_is_still_named() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("backend")).expect("backend");
    std::fs::write(fx.root.join("backend/.nvmrc"), "25\n").expect("write .nvmrc");
    std::fs::write(
        fx.root.join("pando.toml"),
        "[runtime]\nversion_files = [\"backend/.nvmrc\"]\n",
    )
    .expect("write pando.toml");
    std::fs::create_dir_all(fx.machine_home.join(".nvm")).expect("nvm dir");
    std::fs::write(fx.machine_home.join(".nvm/nvm.sh"), "# fake\n").expect("nvm.sh");
    let report = report_of(&fx, &shell_resolving_node("24.21.0", "/n/bin/node"));
    let node = &report.runtime.languages[0];
    assert_eq!(node.managers, vec!["nvm"]);
    let fix = runtime_fix(&report);
    assert!(
        fix.contains("nvm is installed here, and gives `bash -lc` no node 25 — `nvm install 25`"),
        "{fix}"
    );
    assert!(!fix.contains("no version manager"), "{fix}");
}

#[test]
fn no_manager_at_all_says_so() {
    let fx = fixture();
    pin_node(&fx, "22");
    let report = report_of(&fx, &shell_resolving_node("24.21.0", "/n/bin/node"));
    let fix = runtime_fix(&report);
    assert!(
        fix.contains(&format!(
            "no version manager pando knows about is installed for node — install one, or set \
             [runtime].prelude in {} to a line that puts a node 22 first on PATH",
            fx.paths.user_config_file().display()
        )),
        "{fix}"
    );
}

// A node the project accepts, sitting where Homebrew puts one, is a line
// doctor prints: the directory first on PATH, with the version it gives
// and the fact that it runs in every project on this machine. Read under
// the injected machine, so this laptop's own /opt/homebrew decides
// nothing.
#[test]
fn a_node_in_a_well_known_place_is_offered_as_a_path_line() {
    let fx = fixture();
    pin_node(&fx, "25");
    let brew = fx.machine_home.join("opt/homebrew/bin");
    std::fs::create_dir_all(&brew).expect("brew bin");
    std::fs::write(brew.join("node"), "#!/bin/sh\n").expect("node");
    let report = report_of(
        &fx,
        &shell_resolving_node_behind("24.21.0", "opt/homebrew/bin", "25.8.2"),
    );
    let fix = runtime_fix(&report);
    assert!(fix.contains("to one of:"), "{fix}");
    assert!(
        fix.contains(&format!("export PATH=\"{}:$PATH\"", brew.display())),
        "{fix}"
    );
    assert!(fix.contains("node 25.8.2 is in"), "{fix}");
    assert!(fix.contains("every project on this machine"), "{fix}");
    assert!(
        !fx.paths.runtime_cache_file().exists(),
        "doctor tries the line and remembers nothing"
    );
}

#[test]
fn a_requirement_with_no_language_behind_it_is_still_reported() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("package.json"),
        "{\"engines\": {\"pnpm\": \">=9\"}}\n",
    )
    .expect("write package.json");
    let report = report(&fx);
    assert!(
        report
            .runtime
            .requirements
            .iter()
            .any(|r| r.language == "pnpm" && r.spec == ">=9"),
        "{:?}",
        report.runtime.requirements
    );
    let text = report.render();
    assert!(text.contains("nothing here probes it"), "{text}");
    assert!(report.healthy(), "{:?}", report.findings);
}

// `version_files = ["backend/.nvmrc"]` was written and never read: doctor
// said the repository pinned nothing. The file config names counts, and
// it is asked about in the directory its processes run in.
#[test]
fn a_version_file_config_names_in_an_app_directory_is_checked_there() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("backend")).expect("mkdir backend");
    std::fs::write(fx.root.join("backend/.nvmrc"), "22\n").expect("write .nvmrc");
    let asked: std::cell::RefCell<Vec<String>> = Default::default();
    let shell = |script: &str| -> Option<String> {
        if script.contains(TOOL_DONE_MARK) {
            return every_tool(script);
        }
        asked.borrow_mut().push(script.to_string());
        Some(crate::runtime::probe_reply("/usr/bin/node", "v20.1.0"))
    };

    // Not named: a file below the root is config's to name, not doctor's
    // to find.
    let report = report_of(&fx, &shell);
    assert!(report.runtime.requirements.is_empty());
    assert!(report.render().contains("states no runtime"));

    write_project_config(
        &fx,
        "[runtime]\nversion_files = [\"backend/.nvmrc\"]\n\n[dev]\ncmd = \"npm run dev\"\n",
    );
    let report = report_of(&fx, &shell);
    let [node] = report.runtime.languages.as_slice() else {
        panic!("one language: {:?}", report.runtime.languages);
    };
    assert_eq!(node.source, "backend/.nvmrc");
    assert_eq!(node.verdict, "mismatch");
    assert!(
        asked
            .borrow()
            .iter()
            .any(|script| script.starts_with("cd 'backend' && exec bash -lc ")),
        "{:?}",
        asked.borrow()
    );
    assert!(
        !report.render().contains("states no runtime"),
        "{}",
        report.render()
    );
}

#[test]
fn a_repository_that_pins_nothing_says_so_and_probes_no_language() {
    let fx = fixture();
    let report = report(&fx);
    assert!(report.runtime.languages.is_empty());
    assert!(
        report.render().contains("states no runtime"),
        "{}",
        report.render()
    );
}

// ---- services ---------------------------------------------------

fn services_config(include: &str) -> String {
    format!(
        "[[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\ninclude = [{include}]\n"
    )
}

#[test]
fn a_service_without_a_healthcheck_is_flagged_as_connect_probed() {
    let fx = fixture();
    write_compose(
        &fx,
        "services:\n  postgres:\n    image: postgres:16\n  redis:\n    image: redis:7\n    \
             healthcheck:\n      test: [\"CMD\", \"redis-cli\", \"ping\"]\n",
    );
    write_project_config(&fx, &services_config("\"postgres\", \"redis\""));
    let report = report(&fx);
    let entry = &report.services.compose[0];
    let postgres = entry
        .services
        .iter()
        .find(|s| s.name == "postgres")
        .unwrap();
    let redis = entry.services.iter().find(|s| s.name == "redis").unwrap();
    assert_eq!(postgres.ready, "connect");
    assert_eq!(redis.ready, "healthcheck");
    assert!(
        mentions(&report, "\"postgres\" declares no healthcheck"),
        "{:?}",
        messages(&report)
    );
    assert!(
        !mentions(&report, "\"redis\" declares no healthcheck"),
        "{:?}",
        messages(&report)
    );
    // A weaker probe is a note: it works, it is just less sure.
    assert!(report.healthy(), "{:?}", report.findings);
    assert!(
        report.render().contains("ready by connect"),
        "{}",
        report.render()
    );
}

#[test]
fn a_compose_file_pando_could_not_follow_whole_says_which_key_stopped_it() {
    let fx = fixture();
    write_compose(
        &fx,
        "include:\n  - ./other.yml\nservices:\n  db:\n    extends:\n      file: base.yml\n      \
             service: db\n",
    );
    write_project_config(&fx, &services_config("\"db\""));
    let report = report(&fx);
    let entry = &report.services.compose[0];
    assert_eq!(entry.extends, vec!["db".to_string()]);
    assert!(entry.include);
    assert!(mentions(&report, "`extends:`"), "{:?}", messages(&report));
    assert!(
        mentions(&report, "top-level `include:`"),
        "{:?}",
        messages(&report)
    );
    assert!(
        mentions(&report, "does not follow"),
        "{:?}",
        messages(&report)
    );
    // Reported, not fixed.
    assert!(report.healthy(), "{:?}", report.findings);
}

#[test]
fn a_compose_file_that_is_not_there_is_a_problem() {
    let fx = fixture();
    write_project_config(&fx, &services_config("\"postgres\""));
    let report = report(&fx);
    assert!(!report.healthy());
    assert!(
        mentions(&report, "is not in this repository"),
        "{:?}",
        messages(&report)
    );
    assert!(!report.services.compose[0].file_exists);
}

#[test]
fn a_service_the_compose_file_does_not_declare_is_a_problem() {
    let fx = fixture();
    write_compose(&fx, "services:\n  postgres:\n    image: postgres:16\n");
    write_project_config(&fx, &services_config("\"postgres\", \"mysql\""));
    let report = report(&fx);
    assert!(!report.healthy());
    assert!(
        mentions(&report, "`include` names the service \"mysql\""),
        "{:?}",
        messages(&report)
    );
}

/// A shell that finds every engine binary a native recipe asks about
/// and lets the tools probe through unchanged.
///
/// Injected for the same reason the tools shell is: this laptop
/// really does have PostgreSQL installed, and a test that asked the
/// real PATH would pass here and fail on a runner without it.
fn every_tool_and_engine(script: &str) -> Option<String> {
    engine_answer(script, true).or_else(|| every_tool(script))
}

/// The same shell on a machine with no engine at all.
fn no_engine(script: &str) -> Option<String> {
    engine_answer(script, false).or_else(|| every_tool(script))
}

fn engine_answer(script: &str, found: bool) -> Option<String> {
    if !script.contains(NATIVE_BIN_MARK) {
        return None;
    }
    let mut out = String::new();
    if found {
        for line in script.lines() {
            let Some(rest) = line.split("command -v ").nth(1) else {
                continue;
            };
            let Some(name) = rest.split('\'').nth(1) else {
                continue;
            };
            let _ = writeln!(out, "{NATIVE_BIN_MARK}{name} /usr/local/bin/{name}");
        }
        let _ = writeln!(
            out,
            "{NATIVE_VERSION_MARK}postgres (PostgreSQL) 16.10 (Homebrew)"
        );
    }
    Some(out)
}

fn native_of(report: &Report, name: &str) -> NativeServiceReport {
    report
        .services
        .native
        .iter()
        .find(|n| n.name == name)
        .unwrap_or_else(|| panic!("no native report for {name}"))
        .clone()
}

/// The machine-evidence probe's answer, for a machine that has the
/// named things and nothing else. Injected like every other probe:
/// this laptop really has Docker and Postgres, and a test that asked
/// the real PATH would pass here and fail on a runner without them.
fn evidence_answer(script: &str, has: &[&str]) -> Option<String> {
    if !script.contains("command -v docker") {
        return None;
    }
    let mut out = String::new();
    for name in has {
        let _ = writeln!(out, "{name}");
    }
    Some(out)
}

fn shell_with(has: &'static [&'static str]) -> impl Fn(&str) -> Option<String> {
    move |script: &str| {
        evidence_answer(script, has)
            .or_else(|| engine_answer(script, true))
            .or_else(|| every_tool(script))
    }
}

#[test]
fn doctor_says_which_mechanism_would_run_the_services_and_why() {
    let fx = fixture();
    // A project that needs a database and says nothing about how to
    // run one: the shape the native path exists for.
    std::fs::write(
        fx.root.join(".env.example"),
        "DATABASE_URL=postgres://acme:acme@localhost:5432/acme\n",
    )
    .unwrap();
    let report = report_of(&fx, &shell_with(&["docker", "postgres"]));
    let isolation = &report.services.isolation;
    assert_eq!(isolation.mechanism.as_deref(), Some("native"));
    assert_eq!(isolation.prefer, None);
    assert!(!isolation.answered, "nothing is configured yet");
    // Never a bare verdict.
    let said = isolation.evidence.join(" | ");
    assert!(said.contains("no compose file"), "{said}");
    assert!(said.contains("postgres"), "{said}");
    let text = report.render();
    assert!(
        text.contains("native, if this worktree is started isolated"),
        "{text}"
    );
    assert!(text.contains("no compose file"), "{text}");
}

#[test]
fn doctor_names_the_preference_and_the_engine_that_overruled_it() {
    let fx = fixture();
    std::fs::write(
        fx.root.join(".env.example"),
        "DATABASE_URL=postgres://acme:acme@localhost:5432/acme\n",
    )
    .unwrap();
    std::fs::write(
        fx.root.join("docker-compose.yml"),
        "services:\n  postgres:\n    image: postgres:16\n",
    )
    .unwrap();
    std::fs::write(
        fx.paths.user_config_file(),
        "[isolation]\nprefer = \"native\"\n",
    )
    .unwrap();

    // With the engine there, the preference is honoured.
    let report = report_of(&fx, &shell_with(&["docker", "postgres"]));
    assert_eq!(
        report.services.isolation.mechanism.as_deref(),
        Some("native")
    );
    assert_eq!(
        report.services.isolation.prefer.as_deref(),
        Some("native"),
        "the preference has to be reported whether or not it won"
    );

    // Without it, the other mechanism is used and the report says
    // which engine was missing rather than silently disagreeing.
    let report = report_of(&fx, &shell_with(&["docker"]));
    assert_eq!(
        report.services.isolation.mechanism.as_deref(),
        Some("compose")
    );
    let said = report.services.isolation.evidence.join(" | ");
    assert!(said.contains("prefers native"), "{said}");
    assert!(said.contains("postgres is not installed here"), "{said}");
}

#[test]
fn an_answered_project_is_reported_as_its_config_says_and_not_as_detection_would_choose() {
    let fx = fixture();
    std::fs::write(
        fx.root.join(".env.example"),
        "DATABASE_URL=postgres://acme:acme@localhost:5432/acme\n",
    )
    .unwrap();
    std::fs::write(
        fx.root.join("docker-compose.yml"),
        "services:\n  postgres:\n    image: postgres:16\n",
    )
    .unwrap();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\n\
         include = [\"postgres\"]\n",
    );
    // No docker and the engine installed: detection alone would pick
    // native, and a start still runs the compose entry config names.
    let report = report_of(&fx, &shell_with(&["postgres"]));
    let isolation = &report.services.isolation;
    assert!(isolation.answered);
    assert_eq!(isolation.mechanism.as_deref(), Some("compose"));
    let said = isolation.evidence.join(" | ");
    assert!(
        said.contains("`[[services]]` names the compose file docker-compose.yml, for postgres"),
        "{said}"
    );
    assert!(
        said.contains("detection alone would choose native now"),
        "{said}"
    );
    let text = report.render();
    assert!(
        text.contains("compose, as this project's config already says"),
        "{text}"
    );

    // `[isolation] none` is an answer too: nothing, whatever the compose
    // file offers.
    write_project_config(&fx, "[isolation]\nnone = true\n");
    let report = report_of(&fx, &shell_with(&["docker", "postgres"]));
    let isolation = &report.services.isolation;
    assert!(isolation.answered);
    assert_eq!(isolation.mechanism, None);
    let said = isolation.evidence.join(" | ");
    assert!(said.contains("`[isolation] none` says"), "{said}");
    assert!(
        said.contains("detection alone would choose compose now"),
        "{said}"
    );
}

// `include = []` is the written-down "none of them", and a start brings
// nothing up for it. doctor named the mechanism after the entry's kind,
// so a compose file that only packages the app — the brief's own scenario
// — read as "compose, as this project's config already says".
#[test]
fn a_compose_entry_that_includes_nothing_is_reported_as_nothing_to_isolate() {
    let fx = fixture();
    write_compose(&fx, "services:\n  app:\n    build: .\n");
    let none_of_them = "[[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\n\
                        include = []\n";
    write_project_config(&fx, none_of_them);
    let report = report_of(&fx, &shell_with(&["docker"]));
    let isolation = &report.services.isolation;
    assert!(isolation.answered);
    assert_eq!(isolation.mechanism, None);
    let said = isolation.evidence.join(" | ");
    assert!(
        said.contains(
            "`[[services]]` names the compose file docker-compose.yml, for none of its services"
        ),
        "{said}"
    );
    let text = report.render();
    assert!(
        text.contains("nothing here to run a private copy of"),
        "{text}"
    );
    // The brief quotes this render as what an agent reports for it.
    let brief =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/brief.md"))
            .expect("read agent/brief.md");
    let quoted = brief
        .split("## C. ")
        .nth(1)
        .and_then(|scenario| scenario.split("Afterwards, `doctor`").nth(1))
        .and_then(|rest| rest.split("```").nth(1))
        .expect("scenario C's doctor block");
    for line in quoted.lines().map(str::trim).filter(|l| !l.is_empty()) {
        assert!(
            text.lines().any(|printed| printed.trim() == line),
            "agent/brief.md quotes {line:?}, which doctor does not print:\n{text}"
        );
    }

    // What detection would choose is still said where it differs, as it is
    // for `[isolation] none`.
    write_compose(&fx, "services:\n  postgres:\n    image: postgres:16\n");
    let report = report_of(&fx, &shell_with(&["docker"]));
    let said = report.services.isolation.evidence.join(" | ");
    assert!(
        said.contains("detection alone would choose compose now"),
        "{said}"
    );

    // Before a native entry, it is not the entry a start runs.
    write_project_config(
        &fx,
        &format!("{none_of_them}\n[[services]]\nkind = \"native\"\nname = \"postgres\"\n"),
    );
    let report = report_of(&fx, &shell_with(&["docker", "postgres"]));
    assert_eq!(
        report.services.isolation.mechanism.as_deref(),
        Some("native")
    );
}

#[test]
fn a_project_with_nothing_to_isolate_says_that_rather_than_guessing() {
    let fx = fixture();
    let report = report_of(&fx, &shell_with(&["docker", "postgres"]));
    assert_eq!(report.services.isolation.mechanism, None);
    assert!(
        report
            .render()
            .contains("nothing here to run a private copy of"),
        "{}",
        report.render()
    );
    assert!(report.healthy(), "{:?}", report.findings);
}

#[test]
fn a_native_service_says_which_recipe_it_resolved_to_and_from_where() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n\
             env = { DATABASE_URL = \"postgres\" }\n",
    );
    let report = report_of(&fx, &every_tool_and_engine);
    let native = native_of(&report, "postgres");
    assert_eq!(native.preset, "postgres", "a preset defaults to the name");
    assert_eq!(native.source.as_deref(), Some("built-in"));
    assert!(native.overrides.is_empty());
    assert_eq!(native.env_key.as_deref(), Some("DATABASE_URL"));
    assert!(native.error.is_none());
    // Where its data goes, with the worktree left as a placeholder,
    // and under pando's home rather than in the repository.
    assert!(
        native.datadir.ends_with("data/<worktree>/postgres"),
        "{native:?}"
    );
    assert!(native.datadir.starts_with(&fx.home.display().to_string()));
    // And where the sockets go, which is the one thing that is not
    // under the home — because the home's own path is what overflows
    // `sun_path`.
    assert!(
        !native
            .socket_root
            .starts_with(&fx.home.display().to_string())
    );

    assert_eq!(
        native
            .engine
            .iter()
            .map(|b| b.name.as_str())
            .collect::<Vec<_>>(),
        vec!["postgres", "initdb", "pg_isready", "psql", "createdb"]
    );
    assert!(native.engine.iter().all(|b| b.path.is_some()));
    assert_eq!(
        native.version.as_deref(),
        Some("postgres (PostgreSQL) 16.10 (Homebrew)")
    );

    let text = report.render();
    assert!(text.contains("the \"postgres\" recipe, built-in"), "{text}");
    assert!(
        text.contains("postgres at /usr/local/bin/postgres"),
        "{text}"
    );
    assert!(text.contains("addressed by DATABASE_URL"), "{text}");
    // The authentication choice, said out loud rather than buried in
    // a recipe nobody reads.
    assert!(text.contains("trust authentication on 127.0.0.1"), "{text}");
    assert!(report.healthy(), "{:?}", report.findings);
}

#[test]
fn an_engine_this_machine_does_not_have_is_a_line_and_not_a_crash() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n",
    );
    let report = report_of(&fx, &no_engine);
    let native = native_of(&report, "postgres");
    assert!(native.engine.iter().all(|b| b.path.is_none()));
    assert_eq!(
        native.version, None,
        "a version from a binary that is not there"
    );
    let finding = report
        .findings
        .iter()
        .find(|f| f.message.contains("not on PATH"))
        .unwrap_or_else(|| panic!("{:?}", messages(&report)));
    // A note, not a problem, exactly as a missing Docker is: only
    // `--isolated` needs the engine, so a developer running against a
    // shared database is not broken by not having a private one.
    assert_eq!(finding.severity, Severity::Note);
    assert!(
        finding.message.contains("a plain `start` still can"),
        "{finding:?}"
    );
    assert!(report.healthy(), "{:?}", report.findings);
    let text = report.render();
    assert!(text.contains("postgres is not on PATH"), "{text}");
    assert!(text.contains("brew install postgresql"), "{text}");
    assert!(text.contains("pando never will"), "{text}");
}

#[test]
fn an_engine_probe_the_shell_never_answered_claims_nothing_about_the_engine() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n",
    );
    // Every other question is answered; the engine's is not, the way a
    // login shell that overran its deadline answers.
    let shell = |script: &str| match script.contains(NATIVE_BIN_MARK) {
        true => None,
        false => every_tool(script),
    };
    let report = report_of(&fx, &shell);
    let native = native_of(&report, "postgres");
    assert!(!native.engine.is_empty(), "its binaries are still listed");
    assert!(native.engine.iter().all(|b| b.path.is_none()));
    assert!(
        !mentions(&report, "not on PATH"),
        "a shell that did not answer is not evidence the engine is missing: {:?}",
        messages(&report)
    );
    assert!(
        mentions(
            &report,
            "could not ask the shell where the \"postgres\" recipe's engine is"
        ),
        "{:?}",
        messages(&report)
    );
    let text = report.render();
    assert!(!text.contains("is not on PATH"), "{text}");
    assert!(!text.contains("install it with"), "{text}");
    assert!(text.contains("no answer from the shell"), "{text}");
}

#[test]
fn a_users_own_recipe_is_named_by_its_path_and_a_broken_one_is_a_problem() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n",
    );
    let recipes = fx.paths.recipes_dir();
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::write(
        recipes.join("postgres.toml"),
        "kind = \"service\"\nname = \"postgres\"\nbinaries = [\"postgres\"]\n\n\
             [service]\ncmd = \"mine -p {port}\"\n",
    )
    .unwrap();
    let report = report_of(&fx, &every_tool_and_engine);
    let native = native_of(&report, "postgres");
    assert_eq!(
        native.source.as_deref(),
        Some(recipes.join("postgres.toml").display().to_string().as_str()),
        "a replaced built-in has to say which file replaced it"
    );

    // And a file that does not parse is a problem, not a silent
    // fallback to the built-in it shadows.
    std::fs::write(recipes.join("postgres.toml"), "kind = \"service\"\nname =").unwrap();
    let report = report_of(&fx, &every_tool_and_engine);
    assert!(
        mentions(&report, "does not load"),
        "{:?}",
        messages(&report)
    );
    assert!(
        messages(&report)
            .iter()
            .any(|m| m.contains("postgres.toml")),
        "{:?}",
        messages(&report)
    );
    assert!(native_of(&report, "postgres").error.is_some());
    assert!(!report.healthy());
}

#[test]
fn a_preset_no_recipe_answers_to_is_a_problem_naming_the_ones_there_are() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"native\"\nname = \"db\"\npreset = \"cassandra\"\n",
    );
    let report = report_of(&fx, &every_tool_and_engine);
    let native = native_of(&report, "db");
    assert!(
        native.error.as_deref().unwrap().contains("cassandra"),
        "{native:?}"
    );
    assert!(
        mentions(&report, "no recipe named \"cassandra\""),
        "{:?}",
        messages(&report)
    );
    assert!(mentions(&report, "postgres"), "{:?}", messages(&report));
    assert!(!report.healthy());
}

#[test]
fn an_entry_that_overrides_the_recipe_says_which_fields_it_overrode() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n\
             ready = \"pg_isready -p {port}\"\nready_timeout_s = 120\n",
    );
    let report = report_of(&fx, &every_tool_and_engine);
    let native = native_of(&report, "postgres");
    assert_eq!(native.overrides, vec!["ready", "ready_timeout_s"]);
    assert!(
        report
            .render()
            .contains("(ready, ready_timeout_s from this entry)"),
        "{}",
        report.render()
    );
}

#[test]
fn a_worktree_that_already_has_data_is_listed_with_where_it_is() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n",
    );
    let datadir = fx.paths.service_data_dir("feat+one", "postgres");
    std::fs::create_dir_all(&datadir).unwrap();
    // No marker yet: a directory pando did not initialise is one it
    // would adopt, and saying so is the point.
    let report = report_of(&fx, &every_tool_and_engine);
    let native = native_of(&report, "postgres");
    assert_eq!(native.instances.len(), 1);
    assert_eq!(native.instances[0].worktree, "feat+one");
    assert_eq!(native.instances[0].initialised, None);
    assert!(
        report
            .render()
            .contains("the next start adopts it as it is")
    );

    std::fs::write(
        crate::native::marker_file(&datadir),
        "recipe = \"postgres\"\nat = \"2026-09-21T10:00:00Z\"\nadopted = false\n",
    )
    .unwrap();
    let report = report_of(&fx, &every_tool_and_engine);
    let native = native_of(&report, "postgres");
    assert_eq!(
        native.instances[0].initialised.as_deref(),
        Some("initialised 2026-09-21")
    );
    assert_eq!(
        native.instances[0].socket_dir,
        fx.paths
            .service_socket_dir("feat+one", "postgres")
            .display()
            .to_string()
    );
}

#[test]
fn a_compose_service_that_shares_a_name_with_a_role_is_reported_before_the_question() {
    let fx = fixture();
    write_compose(
        &fx,
        "services:\n  api:\n    image: kong:3\n  postgres:\n    image: postgres:16\n",
    );
    write_project_config(
        &fx,
        "[processes.dev]\ncmd = \"serve\"\nports = { WEB_PORT = \"web\", API_PORT = \"api\" }\n",
    );
    let report = report(&fx);
    assert!(
        mentions(&report, "declares a service called \"api\""),
        "{:?}",
        messages(&report)
    );
    assert!(
        mentions(&report, "already owns the role \"api\""),
        "{:?}",
        messages(&report)
    );
    assert!(
        !mentions(&report, "\"postgres\""),
        "only the one that collides: {:?}",
        messages(&report)
    );
    // A note: nothing is broken until somebody answers the question,
    // and answering it is now refused.
    assert!(report.healthy(), "{:?}", report.findings);
    // The fix is the role, pando's side: never an edit to the
    // repository's compose file.
    let fix = report
        .findings
        .iter()
        .find(|f| f.message.contains("already owns the role"))
        .and_then(|f| f.fix.clone())
        .unwrap_or_default();
    assert!(
        fix.starts_with("give that process's role \"api\" another name in its `ports`"),
        "{fix}"
    );
    assert!(!fix.contains("docker-compose.yml"), "{fix}");
}

// A compose file below the root is never where services are proposed
// from, so a name in it collides with nothing — unless a `[[services]]`
// table names that file, and its other services can be offered.
#[test]
fn a_collision_is_reported_only_in_a_compose_file_pando_takes_services_from() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("docker")).unwrap();
    std::fs::write(
        fx.root.join("docker/compose.yml"),
        "services:\n  api:\n    image: kong:3\n  postgres:\n    image: postgres:16\n",
    )
    .unwrap();
    let processes =
        "[processes.dev]\ncmd = \"serve\"\nports = { WEB_PORT = \"web\", API_PORT = \"api\" }\n";
    write_project_config(&fx, processes);
    let below = report(&fx);
    assert!(
        !mentions(&below, "already owns the role"),
        "{:?}",
        messages(&below)
    );

    write_project_config(
        &fx,
        &format!(
            "{processes}\n[[services]]\nkind = \"compose\"\nfile = \"docker/compose.yml\"\n\
             include = [\"postgres\"]\n"
        ),
    );
    let named = report(&fx);
    assert!(
        mentions(
            &named,
            "docker/compose.yml declares a service called \"api\""
        ),
        "{:?}",
        messages(&named)
    );
}

#[test]
fn no_collision_is_reported_for_a_service_the_config_already_includes() {
    let fx = fixture();
    write_compose(&fx, "services:\n  postgres:\n    image: postgres:16\n");
    write_project_config(
        &fx,
        "[processes.dev]\ncmd = \"serve\"\nports = [\"web\"]\n\n\
             [[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\n\
             include = [\"postgres\"]\n",
    );
    let report = report(&fx);
    assert!(
        !mentions(&report, "already owns the role"),
        "{:?}",
        messages(&report)
    );
}

// ---- hooks ------------------------------------------------------

#[test]
fn a_hook_whose_globs_match_nothing_is_flagged_as_running_every_start() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("prisma/migrations")).expect("migrations dir");
    write_project_config(
        &fx,
        "[[hooks]]\nname = \"migrate\"\nafter = \"services\"\n\
             fingerprint = [\"prisma/migrations\"]\ncmd = \"true\"\n",
    );
    let report = report(&fx);
    assert_eq!(report.hooks[0].matches, Some(0));
    assert!(
        mentions(&report, "matches nothing in this worktree"),
        "{:?}",
        messages(&report)
    );
    // The shape of the mistake, named: a glob matches files.
    assert!(
        mentions(&report, "prisma/migrations/**"),
        "{:?}",
        messages(&report)
    );
    assert!(report.healthy(), "{:?}", report.findings);
}

#[test]
fn a_hook_keyed_on_files_that_are_there_counts_them_and_says_nothing() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("prisma/migrations")).expect("migrations dir");
    std::fs::write(fx.root.join("prisma/migrations/001.sql"), "select 1;\n").expect("sql");
    write_project_config(
        &fx,
        "[[hooks]]\nname = \"migrate\"\nafter = \"services\"\n\
             fingerprint = [\"prisma/migrations/**\"]\ncmd = \"true\"\n",
    );
    let report = report(&fx);
    assert_eq!(report.hooks[0].matches, Some(1));
    assert_eq!(report.hooks[0].after, "services");
    assert!(report.healthy(), "{:?}", report.findings);
    assert!(
        report.render().contains("matching 1 file"),
        "{}",
        report.render()
    );
}

#[test]
fn a_hook_with_no_fingerprint_at_all_is_a_fact_and_not_a_finding() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[[hooks]]\nname = \"seed\"\nafter = \"dev\"\ncmd = \"true\"\n",
    );
    let report = report(&fx);
    assert_eq!(report.hooks[0].matches, None);
    assert!(report.healthy(), "{:?}", report.findings);
    assert!(
        report
            .render()
            .contains("keyed on nothing, so it runs on every start"),
        "{}",
        report.render()
    );
}

#[test]
fn the_schema_question_with_no_way_to_say_no_is_reported_once() {
    let fx = fixture();
    // Two candidates and nothing to choose between them is what makes
    // the slot undecided, and it has no empty form to record.
    std::fs::write(
        fx.root.join("package.json"),
        "{\"scripts\": {\"migrate\": \"x\", \"db:migrate\": \"y\"}}\n",
    )
    .expect("manifest");
    std::fs::create_dir_all(fx.root.join("prisma")).expect("prisma");
    std::fs::write(fx.root.join("prisma/schema.prisma"), "// schema\n").expect("schema");
    let report = report(&fx);
    let hits = messages(&report)
        .iter()
        .filter(|m| m.contains("no way to record"))
        .count();
    assert!(hits <= 1, "said at most once: {:?}", messages(&report));
}

// A namespaced start runs a hook scoped to data of its own only where no
// service its steps could reach stays on the main checkout's. doctor went
// by the database the worktree recorded, so one beside a postgres pando
// knows no namespace for read as a hook the next start runs again.
#[test]
fn a_hook_a_namespaced_start_skips_beside_a_shared_database_is_not_one_it_runs_again() {
    let fx = fixture();
    // No login in it, so nothing asks a server; and a client in pando's
    // bin, which every namespace command finds first, answers nothing.
    std::fs::write(
        fx.root.join(".env"),
        "DATABASE_PORT=3306\nDATABASE_NAME=shop\nPOSTGRES_PORT=5432\n",
    )
    .expect("env");
    {
        use std::os::unix::fs::PermissionsExt;
        let bin = fx.home.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        std::fs::write(bin.join("mariadb"), "#!/bin/sh\nexit 1\n").expect("fake client");
        std::fs::set_permissions(bin.join("mariadb"), std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
    }
    let mut record = state::WorktreeRecord::new(&fx.root, false);
    record.mode = Some(state::ServiceMode::Namespaced);
    record.namespaces.push(state::NamespaceRecord {
        service: "mariadb".to_string(),
        recipe: "mariadb".to_string(),
        kind: state::NamespaceKind::Database,
        host: "127.0.0.1".to_string(),
        port: 3306,
        name: "shop__feat_one".to_string(),
        main: "shop".to_string(),
        mains: Vec::new(),
        keys: vec!["DATABASE_PORT".to_string()],
        used_at: chrono::Utc::now(),
    });
    record.hooks.insert(
        "schema".to_string(),
        state::HookRecord {
            fingerprint: Some("before this change".to_string()),
            ran_at: chrono::Utc::now(),
        },
    );
    write_state(&fx, &one_worktree("feat+one", record));
    let mariadb = "[[services]]\nkind = \"native\"\nname = \"mariadb\"\n\
                   env = { DATABASE_PORT = \"mariadb\" }\n\n";
    let postgres = "[[services]]\nkind = \"native\"\nname = \"postgres\"\n\
                    env = { POSTGRES_PORT = \"postgres\" }\n\n";
    let will_run_again = |services: &str| {
        write_project_config(
            &fx,
            &format!(
                "{services}[[hooks]]\nname = \"schema\"\nafter = \"services\"\ncmd = \"true\"\n"
            ),
        );
        let report = report(&fx);
        report.hooks[0].runs[0].will_run_again
    };
    assert!(
        will_run_again(mariadb),
        "on a database of its own, with its inputs changed, the next start runs it"
    );
    assert!(
        !will_run_again(&format!("{mariadb}{postgres}")),
        "beside a postgres that stays on the main checkout's data, the next start skips it"
    );
}

/// The JSON type a documented example gives `key`: the first character of
/// the first value written after `"key": ` in it.
fn documented_type(example: &str, key: &str) -> &'static str {
    let needle = format!("\"{key}\": ");
    let at = example
        .find(&needle)
        .unwrap_or_else(|| panic!("agent/json.md's hooks example has no `{key}`"));
    match example[at + needle.len()..].chars().next() {
        Some('[') => "array",
        Some('{') => "object",
        Some('"') => "string",
        Some('t' | 'f') => "bool",
        Some(c) if c.is_ascii_digit() => "number",
        other => panic!("`{key}` is documented as {other:?}"),
    }
}

fn printed_type(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::Null => "null",
    }
}

// agent/json.md gave a hook's `matches` as a list and its `runs` as a
// boolean, and doctor has always printed a count and a list of runs, each
// with its own `will_run_again`. Every key doctor prints for a hook and for
// a run of one is in the document's example, as a value of the same type.
#[test]
fn every_key_doctor_prints_for_a_hook_is_documented_with_its_type() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("prisma/migrations")).expect("migrations dir");
    std::fs::write(fx.root.join("prisma/migrations/001.sql"), "select 1;\n").expect("sql");
    write_project_config(
        &fx,
        "[[hooks]]\nname = \"migrate\"\nafter = \"services\"\n\
             fingerprint = [\"prisma/migrations/**\"]\ncmd = \"true\"\n",
    );
    let mut record = state::WorktreeRecord::new(&fx.root, false);
    record.hooks.insert(
        "migrate".to_string(),
        state::HookRecord {
            fingerprint: Some("before this change".to_string()),
            ran_at: chrono::Utc::now(),
        },
    );
    write_state(&fx, &one_worktree("feat+one", record));
    let hook = serde_json::to_value(&report(&fx).hooks[0]).expect("serialises");
    let run = &hook["runs"][0];
    assert!(run.is_object(), "a run is reported: {hook}");

    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"))
        .expect("read agent/json.md");
    let example = doc
        .split("## `pando doctor --json`")
        .nth(1)
        .and_then(|section| section.split("\"hooks\":").nth(1))
        .and_then(|rest| rest.split("\"adoption\":").next())
        .expect("the doctor example's hooks");
    for printed in [&hook, run] {
        for (key, value) in printed.as_object().expect("an object") {
            assert_eq!(
                documented_type(example, key),
                printed_type(value),
                "agent/json.md documents hooks' `{key}` as another type than doctor prints"
            );
        }
    }
}

// ---- worktrees --------------------------------------------------

/// A state file, written through the real types: doctor reads state
/// and never writes it, so a fixture may — and building it from
/// `state::State` means a field that moves breaks here loudly instead
/// of parsing into nothing.
fn write_state(fx: &Fx, store: &state::State) {
    let path = fx.paths.state_file();
    std::fs::create_dir_all(path.parent().expect("project dir")).expect("mkdir");
    state::save(&path, store).expect("write state");
}

fn one_worktree(name: &str, record: state::WorktreeRecord) -> state::State {
    let mut store = state::State::new();
    store.worktrees.insert(name.to_string(), record);
    store
}

#[test]
fn a_worktree_pando_has_a_record_for_that_git_has_forgotten_is_reported() {
    let fx = fixture();
    write_state(
        &fx,
        &one_worktree(
            "feat+one",
            state::WorktreeRecord::new(fx.root.join("gone"), true),
        ),
    );
    let report = report(&fx);
    assert_eq!(report.worktrees.len(), 1);
    assert_eq!(report.worktrees[0].phase, "stopped");
    assert!(report.worktrees[0].created_by_pando);
    assert!(!report.worktrees[0].known_to_git);
    assert!(
        mentions(&report, "git does not list it"),
        "{:?}",
        messages(&report)
    );
    assert!(report.healthy(), "{:?}", report.findings);
}

// The main checkout's record is one git lists, under its directory's
// name: reported as the main checkout, never as forgotten or adopted.
#[test]
fn the_main_checkouts_record_is_reported_as_the_main_checkout() {
    let fx = fixture();
    let main = fx.root.file_name().unwrap().to_string_lossy().to_string();
    write_state(
        &fx,
        &one_worktree(&main, state::WorktreeRecord::new(&fx.root, false)),
    );
    let report = report(&fx);
    assert_eq!(report.worktrees.len(), 1);
    assert!(report.worktrees[0].main);
    assert!(report.worktrees[0].known_to_git);
    assert!(
        !mentions(&report, "git does not list it"),
        "{:?}",
        messages(&report)
    );
}

#[test]
fn a_failed_process_is_a_problem_carrying_its_reason_and_a_hint_from_the_log() {
    let fx = fixture();
    let log = fx.paths.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().expect("logs dir")).expect("mkdir");
    std::fs::write(&log, "listen EADDRINUSE: address already in use :::17342\n")
        .expect("write log");
    let mut record = state::WorktreeRecord::new(&fx.root, true);
    record.processes.insert(
        "dev".to_string(),
        state::ProcessRecord {
            pid: 999_999,
            pgid: Group::from_raw(999_999),
            started_at: chrono::Utc::now(),
            log_path: log.clone(),
            ready_port: None,
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: state::Phase::Failed {
                at: chrono::Utc::now(),
                reason: "process exited".to_string(),
            },
        },
    );
    write_state(&fx, &one_worktree("feat+one", record));
    let report = report(&fx);
    assert!(!report.healthy(), "{:?}", report.findings);
    assert_eq!(report.worktrees[0].phase, "failed");
    assert!(
        mentions(&report, "the process \"dev\" failed — process exited"),
        "{:?}",
        messages(&report)
    );
    let fix = report
        .findings
        .iter()
        .find(|f| f.section == Section::Worktrees && f.severity == Severity::Problem)
        .and_then(|f| f.fix.clone())
        .unwrap_or_default();
    assert!(fix.contains("17342"), "the classifier's hint: {fix}");
    assert!(fix.contains("pando logs feat+one --source dev"), "{fix}");
}

/// The end of the dead end: when the process printed nothing, the fix
/// line does not send the developer to an empty file. The record knows
/// the difference, and after the exit status landed it knows why.
#[test]
fn a_failure_with_an_empty_log_is_not_sent_to_read_it() {
    let fx = fixture();
    let log = fx.paths.log_file("feat+one", "dev");
    std::fs::create_dir_all(log.parent().expect("logs dir")).expect("mkdir");
    std::fs::write(&log, "").expect("write log");
    let mut record = state::WorktreeRecord::new(&fx.root, true);
    record.processes.insert(
        "dev".to_string(),
        state::ProcessRecord {
            pid: 999_999,
            pgid: Group::from_raw(999_999),
            started_at: chrono::Utc::now(),
            log_path: log.clone(),
            ready_port: None,
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: state::Phase::Failed {
                at: chrono::Utc::now(),
                reason: "process exited with status 0 — it printed nothing at all".to_string(),
            },
        },
    );
    write_state(&fx, &one_worktree("feat+one", record));
    let report = report(&fx);
    let fix = report
        .findings
        .iter()
        .find(|f| f.section == Section::Worktrees && f.severity == Severity::Problem)
        .and_then(|f| f.fix.clone())
        .unwrap_or_default();
    assert!(
        !fix.contains("pando logs"),
        "there is nothing to read there: {fix}"
    );
    assert!(fix.contains("empty"), "and it says so: {fix}");
    assert!(
        fix.contains("pando start feat+one"),
        "and still says what to do: {fix}"
    );
    assert!(
        mentions(&report, "status 0"),
        "the reason is unchanged: {:?}",
        messages(&report)
    );
}

#[test]
fn a_service_record_the_config_no_longer_includes_is_reported_with_its_volume() {
    let fx = fixture();
    write_compose(&fx, "services:\n  postgres:\n    image: postgres:16\n");
    write_project_config(&fx, &services_config("\"postgres\""));
    let mut record = state::WorktreeRecord::new(&fx.root, true);
    record.mode = Some(crate::state::ServiceMode::Isolated);
    for (name, port) in [("postgres", 17_001u16), ("mailpit", 17_002)] {
        record.services.push(state::ServiceRecord {
            name: name.to_string(),
            kind: state::ServiceKind::Compose,
            port: Some(port),
            pid: None,
            pgid: None,
            compose_project: None,
        });
    }
    write_state(&fx, &one_worktree("feat+one", record));
    let report = report(&fx);
    assert_eq!(report.worktrees[0].mode, state::ServiceMode::Isolated);
    assert!(report.worktrees[0].isolated, "the old flag says so too");
    let mailpit = report.worktrees[0]
        .services
        .iter()
        .find(|s| s.name == "mailpit")
        .expect("the dropped service");
    assert!(!mailpit.declared);
    assert!(
        mentions(&report, "still has a record for the service \"mailpit\""),
        "{:?}",
        messages(&report)
    );
    assert!(
        !mentions(&report, "record for the service \"postgres\""),
        "the one config still includes is not news: {:?}",
        messages(&report)
    );
    assert!(report.healthy(), "{:?}", report.findings);
}

// ---- provisioned files an adopted worktree lacks ----------------

/// A repository that ignores `.env` and has one in its main checkout, as
/// a project that provisions it does, with `config` as pando's layer.
fn provisioned_fixture(config: &str) -> Fx {
    let fx = fixture();
    std::fs::write(fx.root.join(".gitignore"), ".env\n").expect("gitignore");
    git(&fx.root, &["add", "."]);
    git(&fx.root, &["commit", "--quiet", "-m", "ignore .env"]);
    std::fs::write(fx.root.join(".env"), "SECRET=1\n").expect("env");
    write_project_config(&fx, config);
    fx
}

/// A worktree made by git and never by pando, beside the main checkout,
/// on a new branch — or on `branch` as it is, when it exists.
fn adopted_worktree(fx: &Fx, branch: &str) -> PathBuf {
    let dir = fx.root.parent().expect("parent").join(branch);
    let dir_arg = dir.to_str().expect("utf-8").to_string();
    let exists = git_ok(&fx.root, &["rev-parse", "--verify", "--quiet", branch]);
    match exists {
        true => git(&fx.root, &["worktree", "add", "--quiet", &dir_arg, branch]),
        false => git(
            &fx.root,
            &["worktree", "add", "--quiet", "-b", branch, &dir_arg],
        ),
    }
    std::fs::canonicalize(dir).expect("canonical worktree")
}

fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .is_ok_and(|out| out.status.success())
}

fn lacking(report: &Report) -> Vec<&Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.section == Section::Worktrees && f.message.contains("did not create"))
        .collect()
}

// The reporter had fourteen of seventeen adopted worktrees without the
// `.env` `provision` names, and doctor said nothing: `provision` only
// reaches the worktrees `new` makes.
#[test]
fn an_adopted_worktree_without_a_provisioned_file_is_named_with_the_command_that_gives_it() {
    let fx = provisioned_fixture("[project]\nprovision = [\".env\"]\n");
    let dir = adopted_worktree(&fx, "feat-a");
    let report = report(&fx);
    let found = lacking(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    assert_eq!(finding.severity, Severity::Note);
    assert!(
        finding.message.starts_with("feat-a: "),
        "{}",
        finding.message
    );
    assert!(finding.message.contains(".env"), "{}", finding.message);
    // Linked, as `new` would have: `provision_mode` is `link` unless said.
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(&format!(
            "`ln -s {} {}`",
            fx.root.join(".env").display(),
            dir.join(".env").display()
        )),
        "{fix}"
    );
    assert!(report.healthy(), "{:?}", report.findings);
    assert!(!dir.join(".env").exists(), "doctor writes nothing");
}

#[test]
fn many_adopted_worktrees_lacking_the_same_file_are_one_note_with_one_command() {
    let fx = provisioned_fixture("[project]\nprovision = [\".env\"]\nprovision_mode = \"copy\"\n");
    let a = adopted_worktree(&fx, "feat-a");
    let b = adopted_worktree(&fx, "feat-b");
    let has = adopted_worktree(&fx, "feat-c");
    std::fs::write(has.join(".env"), "MINE=1\n").expect("its own");
    let report = report(&fx);
    let found = lacking(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    assert!(
        finding
            .message
            .starts_with("2 worktrees pando did not create have no .env"),
        "{}",
        finding.message
    );
    assert!(
        finding.message.ends_with(": feat-a, feat-b"),
        "{}",
        finding.message
    );
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(&format!(
            "`for w in {} {}; do cp {} \"$w\"/.env; done`",
            a.display(),
            b.display(),
            fx.root.join(".env").display()
        )),
        "{fix}"
    );
}

// A seeded example is copied whatever the mode says, as `new` copies it:
// a link to a tracked file makes every edit in the worktree an edit to
// the repository.
#[test]
fn an_adopted_worktree_lacking_a_seeded_file_is_given_a_copy_of_the_example() {
    let fx = fixture();
    std::fs::write(fx.root.join(".gitignore"), ".env\n").expect("gitignore");
    std::fs::write(fx.root.join(".env.example"), "PORT=3000\n").expect("example");
    git(&fx.root, &["add", "."]);
    git(&fx.root, &["commit", "--quiet", "-m", "an example"]);
    write_project_config(
        &fx,
        "[project]\nprovision = [\".env\"]\nprovision_from = { \".env\" = \".env.example\" }\n",
    );
    let dir = adopted_worktree(&fx, "feat-a");
    let report = report(&fx);
    let found = lacking(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    // Seeded from the example, not something the main checkout has.
    assert!(
        finding
            .message
            .contains("never seeded its .env from the main checkout's .env.example"),
        "{}",
        finding.message
    );
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(
        fix.contains(&format!(
            "`cp {} {}`",
            fx.root.join(".env.example").display(),
            dir.join(".env").display()
        )),
        "{fix}"
    );
}

// A branch made before the app a path belongs to has nothing that reads
// it, and a `cp` into a directory that is not there fails: not named.
#[test]
fn an_adopted_worktree_without_the_directory_a_path_goes_in_is_not_named() {
    let fx = fixture();
    git(&fx.root, &["branch", "before-mobile"]);
    std::fs::create_dir_all(fx.root.join("apps/mobile")).expect("app dir");
    std::fs::write(fx.root.join(".gitignore"), ".env\n").expect("gitignore");
    std::fs::write(fx.root.join("apps/mobile/app.json"), "{}\n").expect("app");
    git(&fx.root, &["add", "."]);
    git(&fx.root, &["commit", "--quiet", "-m", "the mobile app"]);
    std::fs::write(fx.root.join("apps/mobile/.env"), "X=1\n").expect("env");
    write_project_config(&fx, "[project]\nprovision = [\"apps/mobile/.env\"]\n");
    let old = adopted_worktree(&fx, "before-mobile");
    assert!(!old.join("apps/mobile").exists());
    adopted_worktree(&fx, "feat-a");
    let report = report(&fx);
    let found = lacking(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    assert!(
        finding.message.starts_with("feat-a: "),
        "{}",
        finding.message
    );
}

// Invariant 1 is about the worktree the file lands in: a branch whose
// .gitignore does not ignore the path gets no command that writes it.
#[test]
fn an_adopted_worktree_that_does_not_ignore_the_path_gets_no_command() {
    let fx = provisioned_fixture("[project]\nprovision = [\".env\"]\n");
    git(&fx.root, &["branch", "loose"]);
    let dir = adopted_worktree(&fx, "loose");
    std::fs::write(dir.join(".gitignore"), "node_modules/\n").expect("gitignore");
    let report = report(&fx);
    let found = lacking(&report);
    let [finding] = found.as_slice() else {
        panic!("{:?}", messages(&report));
    };
    assert!(
        finding.message.contains("does not ignore"),
        "{}",
        finding.message
    );
    let fix = finding.fix.as_deref().expect("a fix");
    assert!(!fix.contains("cp ") && !fix.contains("ln -s"), "{fix}");
}

// A worktree pando created gets the file at its next `start`: not news
// here.
#[test]
fn a_worktree_pando_created_is_not_reported_for_a_file_it_lacks() {
    let fx = provisioned_fixture("[project]\nprovision = [\".env\"]\n");
    let dir = adopted_worktree(&fx, "feat-a");
    write_state(
        &fx,
        &one_worktree("feat-a", state::WorktreeRecord::new(dir, true)),
    );
    let report = report(&fx);
    assert!(lacking(&report).is_empty(), "{:?}", messages(&report));
}

// ---- adoption ---------------------------------------------------

// The notice was one string literal whose line continuations had lost
// their backslashes, so it carried two runs of eighteen spaces into the
// middle of a sentence.
#[test]
fn the_records_left_behind_notice_is_one_clean_sentence() {
    let text = super::adopt::records_left_behind(&anyhow::anyhow!("state.json is read-only"));
    assert!(!text.contains("  "), "{text:?}");
    assert!(
        text.contains(": state.json is read-only — `pando rm`"),
        "{text}"
    );
}

/// A project folder for a repository that is not where it was: the
/// shape a move leaves behind, built by hand because making one for
/// real means moving a repository.
fn stale_project_folder(fx: &Fx, id: &str, old_root: &Path, worktree: &str) -> PathBuf {
    let dir = fx.home.join("projects").join(id);
    let wt = dir.join("worktrees").join(worktree);
    std::fs::create_dir_all(&wt).expect("worktree dir");
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}/.git/worktrees/{worktree}\n", old_root.display()),
    )
    .expect("git marker");
    std::fs::write(dir.join("pando.toml"), "[project]\ninstall = \"true\"\n").expect("config");
    dir
}

#[test]
fn a_project_folder_whose_repository_moved_is_offered_for_adoption() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let gone = fx.root.parent().expect("a parent").join("somewhere-else");
    stale_project_folder(&fx, &old_id, &gone, "feat+one");
    let report = report(&fx);
    assert_eq!(report.adoption.len(), 1, "{:?}", report.adoption);
    assert_eq!(report.adoption[0].id, old_id);
    assert_eq!(report.adoption[0].worktrees, vec!["feat+one".to_string()]);
    assert_eq!(
        report.adoption[0].old_root.as_deref(),
        Some(gone.display().to_string().as_str())
    );
    let fix = report
        .findings
        .iter()
        .find(|f| f.section == Section::Adoption)
        .and_then(|f| f.fix.clone())
        .unwrap_or_default();
    assert!(
        fix.contains(&format!("`pando doctor --adopt {old_id}`")),
        "{fix}"
    );
    assert!(report.healthy(), "a folder to adopt breaks nothing");
}

#[test]
fn a_folder_whose_repository_is_still_there_is_left_alone() {
    let fx = fixture();
    let other = fx.root.parent().expect("a parent").join("still-here");
    std::fs::create_dir_all(&other).expect("the other checkout");
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    stale_project_folder(&fx, &old_id, &other, "feat+one");
    assert!(
        report(&fx).adoption.is_empty(),
        "two checkouts of one repository is ordinary, and adopting one would steal it"
    );
}

// A live project's folder often has no worktree inside it: one kept in a
// configured `worktrees_dir` lives elsewhere, and a started main checkout
// is only a record. Both are in its state file, and both name a
// repository that is still there.
#[test]
fn a_folder_whose_recorded_checkouts_are_still_there_is_left_alone() {
    let fx = fixture();
    let base = fx.root.parent().expect("a parent");
    let live = base.join("elsewhere").join("repo");
    std::fs::create_dir_all(live.join(".git")).expect("the live checkout");
    let outside = base.join("custom-worktrees").join("feat+one");
    std::fs::create_dir_all(&outside).expect("a worktree outside the folder");
    std::fs::write(
        outside.join(".git"),
        format!("gitdir: {}/.git/worktrees/feat+one\n", live.display()),
    )
    .expect("git marker");
    for (name, checkout) in [("feat+one", &outside), ("repo", &live)] {
        let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
        let dir = fx.home.join("projects").join(&old_id);
        std::fs::create_dir_all(&dir).expect("project folder");
        let record = state::WorktreeRecord::new(checkout, true);
        state::save(&dir.join("state.json"), &one_worktree(name, record)).expect("state");

        let report = report(&fx);
        assert!(report.adoption.is_empty(), "{name}: {:?}", report.adoption);
        let err = adopt(&fx.paths, &old_id, &yes).unwrap_err();
        assert!(format!("{err:#}").contains("is still there"), "{err:#}");
        assert!(dir.is_dir(), "{name}: and nothing moved");
        std::fs::remove_dir_all(&dir).expect("clear");
    }
}

#[test]
fn a_folder_that_names_no_repository_and_still_runs_something_is_not_moved() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let dir = fx.home.join("projects").join(&old_id);
    std::fs::create_dir_all(&dir).expect("project folder");
    let mut record = state::WorktreeRecord::new(dir.join("gone"), true);
    record.processes.insert(
        "dev".to_string(),
        state::ProcessRecord {
            // This test's own process: one that is certainly running.
            pid: std::process::id(),
            pgid: Group::from_raw(999_999),
            started_at: chrono::Utc::now(),
            log_path: dir.join("dev.log"),
            ready_port: None,
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: state::Phase::Running {
                since: chrono::Utc::now(),
            },
        },
    );
    state::save(&dir.join("state.json"), &one_worktree("gone", record)).expect("state");

    assert!(report(&fx).adoption.is_empty(), "it is not offered");
    let err = adopt(&fx.paths, &old_id, &yes).unwrap_err();
    assert!(format!("{err:#}").contains("still running"), "{err:#}");
    assert!(dir.is_dir(), "and nothing moved");
    assert!(!fx.paths.project_dir().exists());
}

// After a move, `git worktree repair` points a worktree kept in a
// configured `worktrees_dir` at the repository where it is now: this one.
// That read as a live other checkout using the folder, so the folder the
// move left behind was not offered, and `--adopt` refused it by naming
// the repository it was run from.
#[test]
fn a_folder_whose_recorded_worktree_names_this_repository_is_adopted() {
    let fx = fixture();
    let outside = fx
        .root
        .parent()
        .expect("a parent")
        .join("custom-worktrees")
        .join("feat+one");
    std::fs::create_dir_all(&outside).expect("a worktree outside the folder");
    std::fs::write(
        outside.join(".git"),
        format!("gitdir: {}/.git/worktrees/feat+one\n", fx.root.display()),
    )
    .expect("git marker");
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let dir = fx.home.join("projects").join(&old_id);
    std::fs::create_dir_all(&dir).expect("project folder");
    let mut record = state::WorktreeRecord::new(&outside, false);
    // Still running from before the move, which is no other checkout's
    // either: the worktree it runs in is this repository's.
    record.processes.insert(
        "dev".to_string(),
        state::ProcessRecord {
            pid: std::process::id(),
            pgid: Group::from_raw(999_999),
            started_at: chrono::Utc::now(),
            log_path: dir.join("dev.log"),
            ready_port: None,
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: state::Phase::Running {
                since: chrono::Utc::now(),
            },
        },
    );
    state::save(&dir.join("state.json"), &one_worktree("feat+one", record)).expect("state");

    let report = report(&fx);
    assert_eq!(report.adoption.len(), 1, "{:?}", report.adoption);
    assert_eq!(
        report.adoption[0].old_root, None,
        "this repository is not where it was"
    );
    adopt(&fx.paths, &old_id, &yes).expect("adopt");
    assert!(!dir.exists(), "the old folder moved");
    assert!(
        fx.paths.project_dir().join("state.json").is_file(),
        "under this repository's id"
    );
}

#[test]
fn a_git_marker_that_does_not_name_an_absolute_repository_says_it_does_not_know() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let dir = fx.home.join("projects").join(&old_id);
    let wt = dir.join("worktrees/feat+one");
    std::fs::create_dir_all(&wt).expect("worktree dir");
    std::fs::write(wt.join(".git"), "gitdir: .git/worktrees/feat+one\n").expect("marker");
    let report = report(&fx);
    assert_eq!(report.adoption.len(), 1);
    assert_eq!(
        report.adoption[0].old_root, None,
        "an empty path is not a repository it can name"
    );
    assert!(
        report
            .render()
            .contains("nothing in it says which repository it belonged to"),
        "{}",
        report.render()
    );
    // Nor does its finding say the repository moved: it cannot know.
    let finding = report
        .findings
        .iter()
        .find(|f| f.section == Section::Adoption)
        .expect("a finding");
    assert!(
        !finding.message.contains("no longer where it was"),
        "{}",
        finding.message
    );
    assert!(
        finding
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains("a checkout that still uses it loses them")),
        "{:?}",
        finding.fix
    );
}

// git 2.48 and later write a worktree's `gitdir:` relative to the worktree
// when `worktree.useRelativePaths` is set. doctor kept only an absolute
// one, so a live clone by the same name whose worktrees were made that way
// named no repository, and with nothing running its folder was offered.
#[test]
fn a_relative_gitdir_names_the_repository_it_climbs_back_to() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let other = fx.root.parent().expect("a parent").join("still-here");
    std::fs::create_dir_all(other.join(".git/worktrees/feat+one")).expect("the other clone");
    let wt = fx
        .home
        .join("projects")
        .join(&old_id)
        .join("worktrees/feat+one");
    std::fs::create_dir_all(&wt).expect("worktree dir");
    // The worktree, `worktrees/`, the folder, `projects/` and the home.
    std::fs::write(
        wt.join(".git"),
        "gitdir: ../../../../../still-here/.git/worktrees/feat+one\n",
    )
    .expect("marker");
    assert!(
        report(&fx).adoption.is_empty(),
        "a live clone's folder is not offered"
    );
    let err = adopt(&fx.paths, &old_id, &yes).unwrap_err();
    assert!(format!("{err:#}").contains("is still there"), "{err:#}");

    // Once that repository has gone, the folder says where it was.
    std::fs::remove_dir_all(&other).expect("the clone moves away");
    let report = report(&fx);
    assert_eq!(report.adoption.len(), 1, "{:?}", report.adoption);
    assert_eq!(
        report.adoption[0].old_root.as_deref(),
        Some(other.display().to_string().as_str())
    );
}

#[test]
fn a_folder_for_a_differently_named_repository_is_not_this_one_moved() {
    let fx = fixture();
    let gone = fx.root.parent().expect("a parent").join("somewhere-else");
    stale_project_folder(&fx, "other-project-deadbeef", &gone, "feat+one");
    assert!(report(&fx).adoption.is_empty());
}

fn yes(_: &AdoptPlan) -> anyhow::Result<bool> {
    Ok(true)
}

#[test]
fn adopting_moves_the_folder_and_everything_in_it() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let gone = fx.root.parent().expect("a parent").join("somewhere-else");
    let from = stale_project_folder(&fx, &old_id, &gone, "feat+one");
    let mut record = state::WorktreeRecord::new(from.join("worktrees/feat+one"), true);
    record.ports.insert("web".to_string(), 17_001);
    let path = from.join("state.json");
    state::save(&path, &one_worktree("feat+one", record)).expect("state");

    let adoption = adopt(&fx.paths, &old_id, &yes).expect("adopt");
    assert_eq!(adoption.to, fx.paths.project_dir());
    assert!(!from.exists(), "the old folder is gone");
    assert!(fx.paths.config_file().exists(), "the config came with it");
    assert!(
        fx.paths.project_dir().join("worktrees/feat+one").is_dir(),
        "and so did the worktrees"
    );

    // Every recorded path that pointed inside the old folder points
    // inside the new one, or nothing would find the worktree again.
    assert_eq!(adoption.rewritten, 1);
    let store = state::load(&fx.paths.state_file()).expect("state");
    assert_eq!(
        store.worktrees["feat+one"].path,
        fx.paths.project_dir().join("worktrees/feat+one")
    );
    assert_eq!(store.worktrees["feat+one"].ports["web"], 17_001);
    // And it is not offered a second time.
    assert!(report(&fx).adoption.is_empty());
}

#[test]
fn adopting_refuses_when_this_repository_already_has_a_folder() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let gone = fx.root.parent().expect("a parent").join("somewhere-else");
    stale_project_folder(&fx, &old_id, &gone, "feat+one");
    write_project_config(&fx, "[project]\ninstall = \"true\"\n");

    let err = adopt(&fx.paths, &old_id, &yes).unwrap_err();
    let text = format!("{err:#}");
    assert!(
        text.contains("already exists, holding pando.toml"),
        "{text}"
    );
    assert!(
        fx.home.join("projects").join(&old_id).is_dir(),
        "and nothing moved"
    );
}

// Opening the TUI once after a move makes this repository's folder, with
// an empty `worktrees/` and a cache in it. `--adopt` refused it as a
// folder it would lose by overwriting, and doctor kept offering the
// `--adopt` it refused.
#[test]
fn adopting_moves_onto_a_folder_that_holds_only_what_pando_rebuilds() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let gone = fx.root.parent().expect("a parent").join("somewhere-else");
    let from = stale_project_folder(&fx, &old_id, &gone, "feat+one");
    let to = fx.paths.project_dir();
    std::fs::create_dir_all(to.join("worktrees")).expect("worktrees dir");
    std::fs::create_dir_all(fx.paths.cache_dir()).expect("cache dir");
    std::fs::write(fx.paths.enrich_cache_file(), "{}").expect("enrich cache");
    std::fs::write(fx.paths.lock_file(), "").expect("lock");

    let adoption = adopt(&fx.paths, &old_id, &yes).expect("adopt");
    assert_eq!(adoption.to, to);
    assert!(!from.exists(), "the old folder moved");
    assert!(fx.paths.config_file().is_file(), "its config came with it");
    assert!(
        to.join("worktrees/feat+one").is_dir(),
        "and so did its worktrees"
    );

    // A worktree directory is something of its own, cache or not.
    let fx = fixture();
    let from = stale_project_folder(&fx, &old_id, &gone, "feat+one");
    std::fs::create_dir_all(fx.paths.project_dir().join("worktrees/feat+two")).expect("worktree");
    std::fs::create_dir_all(fx.paths.cache_dir()).expect("cache dir");
    let err = adopt(&fx.paths, &old_id, &yes).unwrap_err();
    assert!(format!("{err:#}").contains("holding worktrees"), "{err:#}");
    assert!(from.is_dir(), "and nothing moved");
}

#[test]
fn adopting_refuses_a_repository_that_is_still_where_it_was() {
    let fx = fixture();
    let other = fx.root.parent().expect("a parent").join("still-here");
    std::fs::create_dir_all(&other).expect("the other checkout");
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    stale_project_folder(&fx, &old_id, &other, "feat+one");
    let err = adopt(&fx.paths, &old_id, &yes).unwrap_err();
    assert!(format!("{err:#}").contains("is still there"), "{err:#}");
}

#[test]
fn declining_the_confirmation_moves_nothing() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let gone = fx.root.parent().expect("a parent").join("somewhere-else");
    let from = stale_project_folder(&fx, &old_id, &gone, "feat+one");
    let err = adopt(&fx.paths, &old_id, &|_| Ok(false)).unwrap_err();
    assert!(format!("{err:#}").contains("nothing was moved"));
    assert!(from.is_dir());
    assert!(!fx.paths.project_dir().exists());
}

#[test]
fn a_state_file_this_build_cannot_read_stops_the_move_before_it_happens() {
    let fx = fixture();
    let old_id = format!("{}-deadbeef", fx.paths.project.display_name);
    let gone = fx.root.parent().expect("a parent").join("somewhere-else");
    let from = stale_project_folder(&fx, &old_id, &gone, "feat+one");
    // The shape an *old* folder really has: a state file from before
    // a version bump. Every path in it names the folder it is in, so
    // a move that could not rewrite them would leave every worktree
    // pointing at a directory that is no longer there — and `--adopt`
    // finds a folder by its old id, so there is nothing to retry.
    std::fs::write(
        from.join("state.json"),
        "{\"version\": 99, \"worktrees\": {}}",
    )
    .expect("state");

    let err = adopt(&fx.paths, &old_id, &yes).unwrap_err();
    let text = format!("{err:#}");
    assert!(text.contains("before the folder moves"), "{text}");
    assert!(from.is_dir(), "and nothing moved");
    assert!(!fx.paths.project_dir().exists());
}

#[test]
fn adopting_something_that_is_not_a_project_folder_says_so() {
    let fx = fixture();
    // `projects/` exists, so a name that is not one directory name is
    // caught by the check that is about that rather than by an
    // accident of the fixture: `..` and `.` are each exactly one
    // component, and either would make the destination a
    // subdirectory of the source.
    std::fs::create_dir_all(fx.paths.projects_dir()).expect("projects dir");
    for id in ["", ".", "..", "a/b", "./x"] {
        let err = adopt(&fx.paths, id, &yes).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("not a project id"), "{id:?}: {text}");
    }
    let err = adopt(&fx.paths, "no-such-project-0badc0de", &yes).unwrap_err();
    assert!(
        format!("{err:#}").contains("there is no project folder"),
        "{err:#}"
    );
    let err = adopt(&fx.paths, fx.paths.project_id(), &yes).unwrap_err();
    assert!(format!("{err:#}").contains("own project folder"), "{err:#}");
}

#[test]
fn the_report_renders_every_section_even_when_it_has_nothing_to_say() {
    let fx = fixture();
    let text = report(&fx).render();
    for section in Section::ALL {
        assert!(
            text.lines().any(|l| l == section.title()),
            "{} is missing from:\n{text}",
            section.title()
        );
    }
}

// Problems first, then notes, then the facts: a reader who stops after
// the first screen has read everything that is wrong.
#[test]
fn problems_come_first_then_notes_then_the_sections_and_the_verdict_last() {
    let fx = fixture();
    let mut report = report(&fx);
    report.findings = vec![
        Finding::note(Section::Services, "a note about services").with_fix("do a thing"),
        Finding::problem(Section::Tools, "a problem with tools", "install it"),
    ];
    let text = report.render();
    let at = |needle: &str| {
        text.lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} missing from:\n{text}"))
    };
    assert!(text.starts_with("1 problem, 1 note\n"), "{text}");
    assert!(at("problems") < at("a problem with tools"), "{text}");
    assert!(at("a problem with tools") < at("notes"), "{text}");
    assert!(at("notes") < at("a note about services"), "{text}");
    assert!(at("a note about services") < at("project"), "{text}");
    assert!(text.contains("  ! [tools] a problem with tools"), "{text}");
    assert!(text.contains("      fix: install it"), "{text}");
    assert!(text.trim_end().ends_with("1 problem, 1 note"), "{text}");
    assert_eq!(
        text.matches("a problem with tools").count(),
        1,
        "said once: {text}"
    );
    assert!(!text.contains('\x1b'), "plain means plain");

    let painted = report.render_with(&crate::term::Style::with(true, None));
    assert!(painted.contains('\x1b'), "{painted:?}");
    let home = PathBuf::from(&report.project.home);
    let tilded = report.render_with(&crate::term::Style::with(
        false,
        Some(home.parent().unwrap().to_path_buf()),
    ));
    assert!(tilded.contains("home          ~/"), "{tilded}");
}

/// `every_tool`, on a machine whose Docker daemon answers `docker info`
/// with `status` and `said`.
fn daemon_says(status: u8, said: &'static str) -> impl Fn(&str) -> Option<String> {
    move |script: &str| {
        if script.contains(DAEMON_MARK) {
            return Some(format!("{said}\n{DAEMON_MARK}{status}\n"));
        }
        every_tool(script)
    }
}

// "0 problems, 3 notes" on a project whose compose services could not
// start: `docker --version` answers without a daemon.
#[test]
fn a_docker_daemon_that_is_down_is_a_problem_naming_the_way_around_it() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[dev]\ncmd = \"true\"\n\n[[services]]\nkind = \"compose\"\n\
         file = \"docker-compose.yml\"\ninclude = [\"postgres\"]\n",
    );
    write_compose(
        &fx,
        "services:\n  postgres:\n    image: postgres:16\n    healthcheck:\n      test: x\n",
    );
    let down = report_of(
        &fx,
        &daemon_says(1, "Cannot connect to the Docker daemon at unix:///x.sock"),
    );
    assert!(!down.healthy(), "{:?}", messages(&down));
    let problem = down
        .findings
        .iter()
        .find(|f| f.message.contains("Docker daemon is not running"))
        .unwrap_or_else(|| panic!("{:?}", messages(&down)));
    assert_eq!(problem.severity, Severity::Problem);
    assert!(
        problem.message.contains("Cannot connect"),
        "{}",
        problem.message
    );
    let fix = problem.fix.as_deref().unwrap();
    assert!(fix.contains("start Docker"), "{fix}");
    assert!(fix.contains("prefer = \"native\""), "{fix}");
    // `prefer` only settles the services question, and the compose
    // entries have answered it: setting it alone changes nothing, so the
    // way around names the entries to take out, and the file they are in.
    assert!(
        fix.contains("remove the `[[services]] kind = \"compose\"` entries"),
        "{fix}"
    );
    assert!(
        fix.contains(&fx.paths.config_file().display().to_string()),
        "{fix}"
    );
    // The file this run reads, under PANDO_HOME — not `~/.pando`.
    assert!(
        fix.contains(&fx.paths.user_config_file().display().to_string()),
        "{fix}"
    );
    let docker = tool(&down, "docker");
    assert!(
        docker
            .detail
            .as_deref()
            .unwrap()
            .contains("daemon: not running"),
        "{docker:?}"
    );

    let up = report_of(&fx, &daemon_says(0, "27.3.1"));
    assert!(!mentions(&up, "Docker daemon"), "{:?}", messages(&up));
    assert!(
        tool(&up, "docker")
            .detail
            .unwrap()
            .contains("daemon: running")
    );

    // A watchdog kill is a daemon that did not answer.
    assert_eq!(
        parse_daemon(&format!("{DAEMON_MARK}143\n")),
        Daemon::Down(format!("no answer within {DAEMON_WAIT_SECS}s"))
    );
}

#[test]
fn the_daemon_is_not_asked_about_when_no_compose_service_needs_it() {
    let fx = fixture();
    write_project_config(&fx, "[dev]\ncmd = \"true\"\n");
    let asked = std::cell::Cell::new(false);
    let shell = |script: &str| {
        if script.contains(DAEMON_MARK) {
            asked.set(true);
        }
        every_tool(script)
    };
    let report = report_of(&fx, &shell);
    assert!(!asked.get());
    assert!(!mentions(&report, "Docker daemon"));
}

// `include = []` is the written-down "none of them", and an isolated start
// never needs Docker for it. doctor counted it as a compose service, so a
// stopped Docker failed doctor with a problem whose fix threw the answer
// away, and a missing one said `--isolated` could not run.
#[test]
fn a_compose_entry_that_includes_nothing_needs_no_docker() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[dev]\ncmd = \"true\"\n\n[[services]]\nkind = \"compose\"\n\
         file = \"docker-compose.yml\"\ninclude = []\n",
    );
    write_compose(&fx, "services:\n  app:\n    build: .\n");
    let down = report_of(
        &fx,
        &daemon_says(1, "Cannot connect to the Docker daemon at unix:///x.sock"),
    );
    assert!(!mentions(&down, "Docker daemon"), "{:?}", messages(&down));
    assert!(down.healthy(), "{:?}", down.findings);
    let missing = report_of(&fx, &no_tools);
    assert!(
        !mentions(&missing, "docker is not on the PATH"),
        "{:?}",
        messages(&missing)
    );
}

// The probe script itself, run by a real bash against a fake docker: an
// answer, a refusal, and the exit status behind the mark.
#[test]
fn the_daemon_script_reports_the_exit_status_and_what_docker_said() {
    let dir = TempDir::new().unwrap();
    let fake = |name: &str, body: &str| {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    };
    let run = |program: &str| {
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(daemon_script(program, ""))
            .output()
            .unwrap();
        parse_daemon(&String::from_utf8_lossy(&out.stdout))
    };
    assert_eq!(run(&fake("up", "echo 27.3.1")), Daemon::Up);
    assert_eq!(
        run(&fake(
            "down",
            "echo 'Cannot connect to the Docker daemon' >&2; exit 1"
        )),
        Daemon::Down("Cannot connect to the Docker daemon".to_string())
    );
}

// Doctor said "set `[isolation] prefer = \"native\"` in ~/.pando/config.toml"
// with PANDO_HOME somewhere else.
#[test]
fn doctor_names_the_machine_config_this_run_reads() {
    let fx = fixture();
    std::fs::write(
        fx.root.join(".env.example"),
        "DATABASE_URL=postgres://acme:acme@localhost:5432/acme\n",
    )
    .unwrap();
    write_compose(&fx, "services:\n  postgres:\n    image: postgres:16\n");
    let report = report_of(&fx, &shell_with(&["docker", "postgres"]));
    let said = report.services.isolation.evidence.join(" | ");
    let real = fx.paths.user_config_file().display().to_string();
    assert!(said.contains(&real), "{said}");
    let text = report.render();
    assert!(!text.contains("~/.pando"), "{text}");
}

#[test]
fn nothing_to_report_is_said_once() {
    let fx = fixture();
    write_project_config(&fx, "[dev]\ncmd = \"true\"\n");
    let report = report(&fx);
    let text = report.render();
    assert_eq!(text.matches("nothing to report").count(), 1, "{text}");
}

// A library with nothing to run was "nothing to report", and `start` then
// said "no processes configured" with no path and no example.
#[test]
fn a_project_with_nothing_to_run_gets_a_note_with_the_file_and_the_lines() {
    let fx = fixture();
    let report = report(&fx);
    assert!(report.healthy());
    let note = report
        .findings
        .iter()
        .find(|f| f.message.starts_with("nothing to run"))
        .unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert_eq!(note.severity, Severity::Note);
    let fix = note.fix.as_deref().unwrap();
    assert!(
        fix.contains(&fx.paths.config_file().display().to_string()),
        "{fix}"
    );
    assert!(fix.contains("[dev]\ncmd = \""), "{fix}");

    write_project_config(&fx, "[dev]\ncmd = \"true\"\n");
    assert!(!mentions(&report_of(&fx, &every_tool), "nothing to run"));
}

// doctor prints every key of every layer, and `--json` is piped into
// things: a login's password is named there and never shown.
#[test]
fn a_namespace_login_is_reported_without_its_password() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[namespaced.mariadb]  # answered: 2026-09-26\nuser = \"root\"\npassword = \"hunter2\"\n",
    );
    std::fs::write(
        fx.root.join("pando.toml"),
        "[namespaced.redis]\npassword = \"committed-secret\"\ndb_env = [\"REDIS_DB\"]\n",
    )
    .unwrap();
    let report = report(&fx);
    let project = &report.config.layers[2];
    let password = project
        .keys
        .iter()
        .find(|k| k.key == "namespaced.mariadb.password")
        .expect("the key is named");
    assert_eq!(password.value.as_deref(), Some("(hidden)"));
    assert!(password.raw.is_none());
    let committed = &report.config.layers[0];
    let leaked = committed
        .keys
        .iter()
        .find(|k| k.key == "namespaced.redis.password")
        .expect("named in the committed layer too");
    assert!(leaked.ignored, "and ignored there");
    // Only the login goes: which key names the app's slot is no secret,
    // and the committed file's is used.
    let db_env = committed
        .keys
        .iter()
        .find(|k| k.key == "namespaced.redis.db_env")
        .expect("named");
    assert!(!db_env.ignored, "{db_env:?}");
    let json = serde_json::to_string(&report).unwrap();
    let text = report.render();
    for shown in [&json, &text] {
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("committed-secret"), "{shown}");
    }
    assert!(text.contains("namespaced.mariadb.password"), "{text}");
}

#[test]
fn a_namespace_login_written_inline_is_reported_without_its_password() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[namespaced]\nmariadb = { user = \"root\", password = \"hunter2\" }\n",
    );
    std::fs::write(
        fx.root.join("pando.toml"),
        "namespaced = { redis = { password = \"committed-secret\" } }\n",
    )
    .unwrap();
    let report = report(&fx);
    let project = &report.config.layers[2];
    let password = project
        .keys
        .iter()
        .find(|k| k.key == "namespaced.mariadb.password")
        .expect("the key is named");
    assert_eq!(password.value.as_deref(), Some("(hidden)"));
    assert!(password.raw.is_none());
    let user = project
        .keys
        .iter()
        .find(|k| k.key == "namespaced.mariadb.user")
        .expect("the user is named");
    assert_eq!(user.value.as_deref(), Some("\"root\""));
    let committed = &report.config.layers[0];
    let leaked = committed
        .keys
        .iter()
        .find(|k| k.key == "namespaced.redis.password")
        .expect("named in the committed layer too");
    assert!(leaked.ignored, "and ignored there");
    let json = serde_json::to_string(&report).unwrap();
    let text = report.render();
    for shown in [&json, &text] {
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("committed-secret"), "{shown}");
    }
}

fn worker_notes(report: &Report) -> Vec<&Finding> {
    report
        .findings
        .iter()
        .filter(|f| f.message.contains("jobs off a queue, and a shared start"))
        .collect()
}

// A FastAPI backend with an ARQ worker beside it, both in `backend/`: the
// worker is `python -m …worker`, not the `arq` command, and the manifest
// is what says which queue it is. ARQ only queues on Redis, so in a
// shared start every worktree's worker takes the same jobs.
#[test]
fn a_queue_worker_on_the_shared_redis_is_a_note() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("backend")).unwrap();
    std::fs::write(
        fx.root.join("backend/pyproject.toml"),
        "[project]\nname = \"api\"\ndependencies = [\"fastapi>=0.110\", \"arq>=0.26\"]\n",
    )
    .unwrap();
    write_project_config(
        &fx,
        "[processes.api]\ncmd = \"uv run uvicorn app.main:app\"\ncwd = \"backend\"\n\
         ports = { PORT = \"web\" }\n\n\
         [processes.jobs]\ncmd = \"uv run python -m src.scripts.worker\"\ncwd = \"backend\"\n\
         ports = []\n",
    );
    let report = report(&fx);
    let notes = worker_notes(&report);
    assert_eq!(notes.len(), 1, "{:?}", messages(&report));
    let note = notes[0];
    assert_eq!(note.section, Section::Services);
    assert_eq!(note.severity, Severity::Note);
    assert!(
        note.message
            .starts_with("the process \"jobs\" takes ARQ jobs off a queue"),
        "{}",
        note.message
    );
    let fix = note.fix.as_deref().unwrap();
    assert!(fix.contains("--namespaced"), "{fix}");
    assert!(fix.contains("--only jobs"), "{fix}");
    // A namespaced start moves only an app that reads a slot setting.
    assert!(fix.contains("`--isolated` for a Redis of its own"), "{fix}");
    assert!(fix.contains("reads a Redis slot setting"), "{fix}");
}

// Celery's broker may be RabbitMQ: its worker is only on a shared Redis
// where the project addresses one.
#[test]
fn a_celery_worker_is_a_note_only_where_the_project_talks_to_redis() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[processes.web]\ncmd = \"python manage.py runserver\"\nports = []\n\n\
         [processes.celery]\ncmd = \"celery -A shop worker -l info\"\nports = []\n",
    );
    assert!(worker_notes(&report(&fx)).is_empty());

    std::fs::write(
        fx.root.join(".env.example"),
        "CELERY_BROKER_URL=redis://localhost:6379/0\n",
    )
    .unwrap();
    let report = report(&fx);
    let notes = worker_notes(&report);
    assert_eq!(notes.len(), 1, "{:?}", messages(&report));
    assert!(notes[0].message.contains("Celery"), "{}", notes[0].message);
}

// A process called a worker in a project that declares no queue library
// is somebody's own loop, and doctor says nothing about it.
#[test]
fn a_process_named_worker_with_no_queue_library_is_not_a_note() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("package.json"),
        r#"{ "dependencies": { "express": "^4" } }"#,
    )
    .unwrap();
    write_project_config(
        &fx,
        "[processes.worker]\ncmd = \"node worker.js\"\nports = []\n",
    );
    assert!(worker_notes(&report(&fx)).is_empty());

    std::fs::write(
        fx.root.join("package.json"),
        r#"{ "dependencies": { "express": "^4", "bullmq": "^5" } }"#,
    )
    .unwrap();
    let report = report(&fx);
    assert_eq!(worker_notes(&report).len(), 1, "{:?}", messages(&report));
}

// pando's Expo rule proposed `CI = "1"` until 0.6.0, and Metro under it
// neither reloads nor watches a file: a config written then is told.
#[test]
fn ci_on_expos_metro_is_a_note_that_says_what_it_turns_off() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[processes.mobile]\ncmd = \"npx expo start\"\nports = { RCT_METRO_PORT = \"metro\" }\n\
         env = { CI = \"1\" }\n\n\
         [processes.api]\ncmd = \"npm run dev\"\nports = { PORT = \"web\" }\nenv = { CI = \"1\" }\n",
    );
    let report = report(&fx);
    let found: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.message.contains("sets CI"))
        .collect();
    assert_eq!(found.len(), 1, "{:?}", messages(&report));
    assert_eq!(found[0].severity, Severity::Note);
    assert!(
        found[0].message.contains("\"mobile\""),
        "{}",
        found[0].message
    );
    assert!(found[0].message.contains("reloads"), "{}", found[0].message);
    assert!(
        found[0]
            .fix
            .as_deref()
            .is_some_and(|fix| fix.contains("remove CI")),
        "{:?}",
        found[0].fix
    );
}

/// `every_tool`, with `program` found at `path` by every probe that asks
/// for it: docker's and `docker compose`'s ask for the same one.
fn every_tool_with(program: &'static str, path: &'static str) -> impl Fn(&str) -> Option<String> {
    move |script: &str| {
        let answer = every_tool(script)?;
        let asked = format!("command -v '{program}' ");
        let found: Vec<String> = script
            .lines()
            .filter(|l| l.contains(&asked))
            .filter_map(|l| l.split(TOOL_PATH_MARK).nth(1))
            .filter_map(|rest| rest.split(' ').next())
            .map(|index| format!("{TOOL_PATH_MARK}{index} "))
            .collect();
        let mut out = String::new();
        for line in answer.lines() {
            match found.iter().find(|mark| line.starts_with(mark.as_str())) {
                Some(mark) => writeln!(out, "{mark}{path}").unwrap(),
                None => writeln!(out, "{line}").unwrap(),
            }
        }
        Some(out)
    }
}

// Docker Desktop leaves a `docker` script on Windows' PATH that, in a WSL
// distro without its integration, only says to turn the integration on;
// and `docker compose` is the same program asked a second question.
#[test]
fn under_wsl_docker_desktops_own_script_says_to_turn_the_integration_on_once() {
    let fx = fixture();
    wsl_system(&fx.machine_home, WSL_RELEASE, WSL_MOUNTS);
    let report = report_of(
        &fx,
        &every_tool_with(
            "docker",
            "/mnt/c/Program Files/Docker/Docker/resources/bin/docker",
        ),
    );
    let found: Vec<&Finding> = report
        .findings
        .iter()
        .filter(|f| f.message.contains("Windows' copy"))
        .collect();
    assert_eq!(found.len(), 1, "{:?}", messages(&report));
    assert!(found[0].message.starts_with("docker is Windows' copy"));
    assert_eq!(
        found[0].severity,
        Severity::Note,
        "nothing in this project runs a container"
    );
    let fix = found[0].fix.as_deref().unwrap();
    assert!(
        fix.starts_with("turn on Docker Desktop's WSL integration"),
        "{fix}"
    );
}

// Under WSL, Windows' PATH comes after Linux's, and with no Linux pnpm the
// shell found nvm-windows' shim, which died with `node: not found`. doctor
// listed it among the tools as if it were fine.
#[test]
fn under_wsl_a_tool_found_on_a_windows_drive_is_as_bad_as_missing() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[project]\ninstall = \"pnpm install --frozen-lockfile\"\n",
    );
    let windows = "/mnt/c/Users/me/AppData/Roaming/nvm/v20.19.3/pnpm";
    let shell = every_tool_with("pnpm", windows);
    let off_wsl = report_of(&fx, &shell);
    assert!(
        !mentions(&off_wsl, "Windows' copy"),
        "off WSL a path is a path: {:?}",
        messages(&off_wsl)
    );

    wsl_system(&fx.machine_home, WSL_RELEASE, WSL_MOUNTS);
    let report = report_of(&fx, &shell);
    assert!(!report.healthy(), "{:?}", messages(&report));
    let finding = report
        .findings
        .iter()
        .find(|f| f.message.starts_with("pnpm is Windows' copy"))
        .unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert_eq!(finding.section, Section::Tools);
    assert_eq!(
        finding.severity,
        Severity::Problem,
        "every new worktree's install step runs it"
    );
    assert!(finding.message.contains(windows), "{}", finding.message);
    assert!(
        finding.message.contains("the drive at /mnt/c"),
        "{}",
        finding.message
    );
    let fix = finding.fix.as_deref().unwrap();
    assert!(fix.contains("install pnpm inside WSL"), "{fix}");
    assert!(fix.contains("appendWindowsPath = false"), "{fix}");
    assert!(
        fix.contains("clip.exe off PATH"),
        "what the setting also takes away is said: {fix}"
    );
}

// On a Windows drive, git was several times slower and inotify sent no
// event for an edit, from Windows or from Linux.
#[test]
fn under_wsl_a_repository_on_a_windows_drive_is_a_note_with_the_way_out() {
    let fx = fixture();
    // The fixture's directory, which holds the repository and pando's
    // home, mounted as drive C.
    let drive = fx.root.parent().unwrap().display().to_string();
    let mounts = format!("C:\\134 {drive} 9p rw,noatime,aname=drvfs;path=C:\\ 0 0\n");
    wsl_system(&fx.machine_home, "6.8.0-45-generic\n", &mounts);
    assert!(
        !mentions(&report(&fx), "Windows drive"),
        "a 9p mount off WSL is no drive"
    );

    wsl_system(&fx.machine_home, WSL_RELEASE, &mounts);
    let report = report(&fx);
    assert!(
        report.healthy(),
        "slow is not broken: {:?}",
        messages(&report)
    );
    let repository = report
        .findings
        .iter()
        .find(|f| {
            f.message
                .starts_with("the repository is on the Windows drive")
        })
        .unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert_eq!(repository.section, Section::Project);
    assert_eq!(repository.severity, Severity::Note);
    assert!(
        repository.message.contains(&format!("drive at {drive}")),
        "{}",
        repository.message
    );
    assert!(
        repository.fix.as_deref().unwrap().contains("under ~"),
        "{:?}",
        repository.fix
    );
    let worktrees = report
        .findings
        .iter()
        .find(|f| {
            f.message
                .starts_with("this project's worktrees go on the Windows drive")
        })
        .unwrap_or_else(|| panic!("{:?}", messages(&report)));
    assert!(
        worktrees.fix.as_deref().unwrap().contains("PANDO_HOME"),
        "they are under pando's home: {:?}",
        worktrees.fix
    );
}

// Docker Desktop's daemon runs on Windows, and a distro's own `docker`
// reaches it only with the WSL integration on: "start Docker" said nothing
// to someone whose Docker Desktop was up the whole time.
#[test]
fn under_wsl_a_docker_daemon_that_does_not_answer_names_the_wsl_integration() {
    let fx = fixture();
    write_project_config(
        &fx,
        "[dev]\ncmd = \"true\"\n\n[[services]]\nkind = \"compose\"\n\
         file = \"docker-compose.yml\"\ninclude = [\"postgres\"]\n",
    );
    write_compose(
        &fx,
        "services:\n  postgres:\n    image: postgres:16\n    healthcheck:\n      test: x\n",
    );
    wsl_system(&fx.machine_home, WSL_RELEASE, WSL_MOUNTS);
    let down = report_of(
        &fx,
        &daemon_says(
            1,
            "failed to connect to the docker API at unix:///var/run/docker.sock",
        ),
    );
    let problem = down
        .findings
        .iter()
        .find(|f| f.message.contains("Docker daemon is not running"))
        .unwrap_or_else(|| panic!("{:?}", messages(&down)));
    let fix = problem.fix.as_deref().unwrap();
    assert!(
        fix.starts_with("start Docker Desktop with its WSL integration on"),
        "{fix}"
    );
    assert!(
        fix.contains("prefer = \"native\""),
        "the way around it stays: {fix}"
    );
}
