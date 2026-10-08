use super::answers::answer_from;
use super::answers::reads_as_process_list;
use super::answers::slot_named;
use super::answers::slot_names;
use super::logs::silence_notes;
use super::logs::{leading_timestamp, level_word};
use super::ls::ORDER;
use super::open::Want;
use super::prompt::asker;
use super::prompt::prompt_with;
use super::status::{human_duration, phase_word};
use super::*;
use crate::actions;
use crate::actions::worktree_url;
use crate::cache;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::process::Group;
use crate::project::ProjectRef;
use crate::state::{Phase, ProcessRecord, WorktreeRecord};
use crate::testutil::git;
use crate::worktree::PrState;
use anyhow::Result;
use chrono::Utc;
use clap::CommandFactory;
use std::collections::BTreeMap;
use std::path::PathBuf;
use tempfile::{TempDir, tempdir};

struct Fx {
    _dir: TempDir,
    root: PathBuf,
    paths: PandoPaths,
    config: Config,
}

fn fixture() -> Fx {
    let dir = tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    std::fs::write(root.join(".gitignore"), ".env\n").unwrap();
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "root"]);
    let project = ProjectRef::from_root(&root).unwrap();
    let paths = PandoPaths::new(dir.path().join("pando-home"), project);
    Fx {
        root: paths.root().to_path_buf(),
        paths,
        config: Config::default(),
        _dir: dir,
    }
}

fn capture(f: impl FnOnce(&mut Vec<u8>) -> Result<()>) -> String {
    let mut buf = Vec::new();
    f(&mut buf).unwrap();
    String::from_utf8(buf).unwrap()
}

/// The notice channel, for a test that is not about it.
fn quiet(_: &str) {}

/// stdout and the notices, separately — which is the whole point of
/// there being two channels.
fn capture_both(
    f: impl FnOnce(&mut Vec<u8>, &dyn Fn(&str)) -> Result<()>,
) -> (String, Vec<String>) {
    let notes = std::cell::RefCell::new(Vec::new());
    let mut buf = Vec::new();
    f(&mut buf, &|line: &str| {
        notes.borrow_mut().push(line.to_string())
    })
    .unwrap();
    (String::from_utf8(buf).unwrap(), notes.into_inner())
}

#[test]
fn the_cli_definition_is_valid() {
    Cli::command().debug_assert();
}

// A person opening `--help` is looking for how to use it, so the top
// level and every verb with a non-obvious shape show examples, and every
// example is a command the binary really takes.
#[test]
fn help_carries_examples_that_parse() {
    let mut cli = Cli::command();
    let top = cli.render_help().to_string();
    assert!(top.contains("Examples:"), "{top}");
    for verb in [
        "new",
        "ls",
        "start",
        "stop",
        "status",
        "open",
        "logs",
        "completions",
    ] {
        let sub = cli
            .find_subcommand_mut(verb)
            .unwrap_or_else(|| panic!("no {verb}"));
        let help = sub.render_help().to_string();
        assert!(help.contains("Example"), "{verb} has no example:\n{help}");
    }
    for line in top.lines().chain(
        cli.get_subcommands()
            .flat_map(|s| s.get_after_help().map(|h| h.to_string()))
            .collect::<Vec<_>>()
            .iter()
            .flat_map(|h| h.lines()),
    ) {
        let Some(rest) = line.trim_start().strip_prefix("pando ") else {
            continue;
        };
        // The command is everything before the two-space gap that starts
        // its description.
        let command = rest.split("  ").next().unwrap().trim();
        let mut argv = vec!["pando"];
        argv.extend(command.split_whitespace().filter(|w| !w.starts_with('>')));
        let argv: Vec<&str> = argv
            .into_iter()
            .take_while(|w| !w.starts_with('~'))
            .collect();
        Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} does not parse: {e}"));
    }
}

#[test]
fn a_typo_is_matched_to_the_names_it_was_probably_meant_to_be() {
    use super::names::{edit_distance, suggest};
    assert_eq!(edit_distance("feat+one", "feat+one"), 0);
    assert_eq!(edit_distance("feat+on", "feat+one"), 1);
    assert_eq!(edit_distance("faet+one", "feat+one"), 2);

    let fx = fixture();
    for branch in ["feat/one", "feat/two", "fix/login"] {
        actions::new(&fx.paths, &fx.config, branch, None, &|_| {}).unwrap();
    }
    let worktrees = crate::worktree::discover(&fx.paths.project).unwrap();
    // A prefix of the branch or the directory name.
    let mut prefix = suggest("feat", &worktrees);
    prefix.sort();
    assert_eq!(prefix, ["feat/one", "feat/two"]);
    // A typo.
    assert_eq!(suggest("fix/logni", &worktrees), ["fix/login"]);
    // Something in the middle.
    assert_eq!(suggest("login", &worktrees), ["fix/login"]);
    // Nothing close.
    assert!(suggest("zzzzzz", &worktrees).is_empty());
}

// A branch literally called `a+b`, beside the directory `a+b` of the
// branch `a/b`: the typed string names two worktrees, and picking the
// directory silently is how `rm a+b` removes the one that was not meant.
#[test]
fn a_name_that_is_one_worktrees_directory_and_anothers_branch_is_refused() {
    let fx = fixture();
    let first = actions::new(&fx.paths, &fx.config, "a/b", None, &|_| {}).unwrap();
    assert_eq!(first, "a+b");
    let elsewhere = fx.root.parent().unwrap().join("other");
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "a+b",
            elsewhere.to_str().unwrap(),
        ],
    );

    let err = super::names::resolve(&fx.paths, "a+b").unwrap_err();
    assert!(err.downcast_ref::<UsageError>().is_some(), "{err:#}");
    let msg = format!("{err:#}");
    assert!(msg.contains("names two worktrees"), "{msg}");
    assert!(msg.contains("`a/b`") && msg.contains("`other`"), "{msg}");
    // Each has an unambiguous name, and both resolve.
    assert_eq!(super::names::resolve(&fx.paths, "a/b").unwrap(), "a+b");
    assert_eq!(super::names::resolve(&fx.paths, "other").unwrap(), "other");
}

// The heuristic is on `--help`, where a script author will look for it.
#[test]
fn start_help_says_how_a_portless_process_is_judged_ready() {
    use clap::CommandFactory;
    let mut cli = super::Cli::command();
    let start = cli.find_subcommand_mut("start").unwrap();
    let help = start.render_long_help().to_string();
    assert!(help.contains("no port"), "{help}");
    assert!(help.contains("5s") && help.contains("10s"), "{help}");
}

// A database that crashed during `start --wait` was forgotten by the
// wait's refresh and saved as gone, and the wait never said so: the
// failure it caused came with no cause, and no later command had one.
#[test]
fn a_wait_tells_what_its_refresh_forgot() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let mut exited = std::process::Command::new("true").spawn().unwrap();
    exited.wait().unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    let mut dev = listening(std::process::id() as i32, &[]);
    dev.started_at = Utc::now() - chrono::Duration::seconds(60);
    record.processes.insert("dev".to_string(), dev);
    record.services.push(crate::state::ServiceRecord {
        name: "mariadb".into(),
        kind: crate::state::ServiceKind::Native,
        port: Some(17_004),
        pid: Some(exited.id()),
        pgid: None,
        compose_project: None,
    });
    crate::state::save(&fx.paths.state_file(), &store).unwrap();

    let said = std::cell::RefCell::new(Vec::new());
    let named = super::names::target_named(&fx.paths, Some("feat/one"), "start").unwrap();
    super::wait::wait_ready(&fx.paths, &named, None, &[], &|line| {
        said.borrow_mut().push(line.to_string())
    })
    .unwrap();
    let said = said.into_inner();
    assert!(
        said.iter().any(|l| l.contains("its mariadb exited")),
        "{said:?}"
    );
}

// A server that bound its port while the wait's first scan was still
// running was never seen starting, so `start --wait` printed "starting
// dev", returned 0 and never said dev was ready. On a loaded machine that
// is the common case. What the command itself spawned is news however
// fast it came up; only what was running before it is not.
#[test]
fn a_wait_says_a_process_it_spawned_is_ready_even_when_the_first_look_finds_it_ready() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    // Already Running when the wait first looks, as a server that bound
    // before the scan finished is.
    let mut dev = listening(std::process::id() as i32, &[]);
    dev.ready_port = Some(17_342);
    record.processes.insert("dev".to_string(), dev);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let named = super::names::target_named(&fx.paths, Some("feat/one"), "start").unwrap();

    let said = std::cell::RefCell::new(Vec::new());
    let note = |line: &str| said.borrow_mut().push(line.to_string());
    super::wait::wait_ready(&fx.paths, &named, None, &["dev".to_string()], &note).unwrap();
    assert!(
        said.borrow().iter().any(|l| l.starts_with("dev is ready")),
        "{:?}",
        said.borrow()
    );

    // The same worktree, when this command spawned nothing: dev was up
    // before it, and there is nothing new to say.
    said.borrow_mut().clear();
    super::wait::wait_ready(&fx.paths, &named, None, &[], &note).unwrap();
    assert!(said.borrow().is_empty(), "{:?}", said.borrow());
}

// A refresh that read the state and could not save what it changed was
// taken for one that could not read it: `open` of a worktree never
// started answered with the save's error, not "not running", and a wait
// whose worktree stopped did not say it stopped.
#[test]
fn a_refresh_that_only_failed_to_save_still_answers_about_the_worktree() {
    let fx = fixture();
    let one = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let two = actions::new(&fx.paths, &fx.config, "feat/two", None, &|_| {}).unwrap();
    let mut exited = std::process::Command::new("true").spawn().unwrap();
    exited.wait().unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    // Forgotten by every refresh, so every refresh has something to save.
    store
        .worktrees
        .get_mut(&one)
        .unwrap()
        .services
        .push(crate::state::ServiceRecord {
            name: "mariadb".into(),
            kind: crate::state::ServiceKind::Native,
            port: Some(17_004),
            pid: Some(exited.id()),
            pgid: None,
            compose_project: None,
        });
    store.worktrees.remove(&two);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    // A save writes here first, and cannot.
    std::fs::create_dir(fx.paths.state_file().with_extension("json.tmp")).unwrap();
    let refreshed = actions::refresh(&fx.paths);
    assert!(refreshed.warning.is_some() && !refreshed.unreadable);

    let named = super::names::target_named(&fx.paths, Some("feat/two"), "open").unwrap();
    let err =
        super::open::url_to_open(&fx.paths, &with_dev(&fx.config), &named, Want::Page).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "feat/two is not running — `pando start feat/two` starts it"
    );
    let err = super::wait::wait_ready(&fx.paths, &named, None, &[], &quiet).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "feat/two has nothing running — it stopped while pando waited"
    );
}

#[test]
fn a_name_resolves_by_directory_by_branch_or_by_the_directory_it_is_run_in() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    assert_eq!(super::names::resolve(&fx.paths, "feat+one").unwrap(), name);
    assert_eq!(super::names::resolve(&fx.paths, "feat/one").unwrap(), name);
    let err = super::names::resolve(&fx.paths, "feat/on").unwrap_err();
    assert!(
        format!("{err:#}").contains("did you mean feat/one?"),
        "{err:#}"
    );

    let inside = fx.paths.worktrees_dir().join(&name).join("sub");
    std::fs::create_dir_all(&inside).unwrap();
    assert_eq!(
        super::names::containing(&fx.paths, &inside).unwrap(),
        Some(name.clone())
    );
    assert_eq!(super::names::containing(&fx.paths, &fx.root).unwrap(), None);
}

// The main checkout resolves for every verb, by its directory's name or
// its branch: pando runs it too. It used to resolve for `path` alone, and
// before that like a worktree nothing could act on.
#[test]
fn the_main_checkout_resolves_by_name_or_branch_and_an_empty_name_is_a_usage_error() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    for typed in ["acme-shop", "main"] {
        assert_eq!(
            super::names::resolve(&fx.paths, typed).unwrap(),
            "acme-shop",
            "{typed}"
        );
        assert_eq!(
            super::names::path(&fx.paths, typed).unwrap().file_name(),
            Some(std::ffi::OsStr::new("acme-shop")),
            "{typed}"
        );
    }
    assert_eq!(super::names::resolve(&fx.paths, "feat/one").unwrap(), name);
    // `stop` with no name inside it stops every one, as it always has.
    assert_eq!(super::names::containing(&fx.paths, &fx.root).unwrap(), None);

    let err = super::names::resolve(&fx.paths, "").unwrap_err();
    assert!(err.downcast_ref::<UsageError>().is_some(), "{err:#}");
    assert!(!format!("{err:#}").contains("did you mean"), "{err:#}");
}

// A pty nobody sized reports zero columns, and `ls` fitted its table to
// that: every column but NAME and STATUS shed and the name cut to its
// floor. COLUMNS, when it is a number, is what a person asked for.
#[test]
fn the_listing_width_survives_a_zero_sized_terminal_and_reads_columns() {
    use super::ls::width_from;
    assert_eq!(width_from(true, None, Some(0)), 80);
    assert_eq!(width_from(true, None, None), 80);
    assert_eq!(width_from(true, None, Some(120)), 120);
    assert_eq!(width_from(true, Some("60"), Some(120)), 60);
    for garbage in ["", "0", "wide", "-5"] {
        assert_eq!(
            width_from(true, Some(garbage), Some(120)),
            120,
            "{garbage:?}"
        );
    }
    assert_eq!(
        width_from(false, Some("60"), Some(120)),
        usize::MAX,
        "a pipe"
    );
}

#[test]
fn logs_default_to_dev_else_to_the_only_process() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    // No log at all: `dev`, so the error names it.
    assert_eq!(
        super::logs::default_sources(&fx.paths, &name, "dev"),
        super::logs::Sources::One("dev".to_string())
    );
    // One process, not called dev.
    with_two_processes(&fx, &name, Phase::Running { since: Utc::now() });
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    record.processes.remove("api");
    record.roles.remove("api");
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    write_log(&fx, &name, "web", "web line\n");
    assert_eq!(
        super::logs::default_sources(&fx.paths, &name, "dev"),
        super::logs::Sources::One("web".to_string())
    );
    // A dev log wins whenever there is one.
    write_log(&fx, &name, "dev", "dev line\n");
    assert_eq!(
        super::logs::default_sources(&fx.paths, &name, "dev"),
        super::logs::Sources::One("dev".to_string())
    );
}

// A worktree with an api and a web process and no `dev` used to answer a
// plain `pando logs` with an error. It gets both, compose-style.
#[test]
fn logs_with_several_processes_and_no_dev_merge_them() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_two_processes(&fx, &name, Phase::Running { since: Utc::now() });
    write_log(&fx, &name, "api", "a1\na2\n");
    write_log(&fx, &name, "web", "w1\n");
    // A hook's log is not a process's, and stays out of the merge.
    write_log(&fx, &name, "install", "installed\n");
    let sources = match super::logs::default_sources(&fx.paths, &name, "dev") {
        super::logs::Sources::Merged(sources) => sources,
        other => panic!("{other:?}"),
    };
    assert_eq!(sources, vec!["api".to_string(), "web".to_string()]);

    // No timestamps to order by: one source after the other.
    let (text, notes) = capture_both(|b, n| {
        super::logs::logs_merged(&fx.paths, &name, &sources, 50, false, false, b, n)
    });
    assert_eq!(text, "api | a1\napi | a2\nweb | w1\n");
    assert!(notes.iter().any(|n| n.contains("-s api")), "{notes:?}");

    // `-n` counts per source, as compose's `--tail` does.
    let text = capture(|b| {
        super::logs::logs_merged(&fx.paths, &name, &sources, 1, false, false, b, &quiet)
    });
    assert_eq!(text, "api | a2\nweb | w1\n");

    // Timestamps on every line: one timeline.
    write_log(
        &fx,
        &name,
        "api",
        "2026-09-20T10:00:00Z api first\n2026-09-20T10:00:02Z api third\n",
    );
    write_log(&fx, &name, "web", "2026-09-20T10:00:01Z web second\n");
    let text = capture(|b| {
        super::logs::logs_merged(&fx.paths, &name, &sources, 50, false, false, b, &quiet)
    });
    assert_eq!(
        text,
        "api | 2026-09-20T10:00:00Z api first\n\
         web | 2026-09-20T10:00:01Z web second\n\
         api | 2026-09-20T10:00:02Z api third\n"
    );

    // And as JSON, each line says where it came from.
    let text = capture(|b| {
        super::logs::logs_merged(&fx.paths, &name, &sources, 50, false, true, b, &quiet)
    });
    let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(first["source"], "api");
    assert_eq!(first["version"], JSON_VERSION);
    // A read of one named log carries no `source`, as before.
    let one = capture(|b| logs(&fx.paths, &name, "api", 1, false, true, b, &quiet));
    let one: serde_json::Value = serde_json::from_str(one.trim()).unwrap();
    assert!(one.get("source").is_none(), "{one}");
}

// The merged stream's `source` key is published, so the contract says so.
#[test]
fn agent_json_documents_the_merged_logs_source_key() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    let section = doc
        .split("## `pando logs <name> --json`")
        .nth(1)
        .expect("the logs section")
        .split("\n## ")
        .next()
        .unwrap();
    assert!(section.contains("\"source\": \"api\""), "{section}");
    assert!(section.contains("merged"), "{section}");
}

// Every key `pando signals` publishes is one an agent reads, so the
// contract names each of them.
#[test]
fn agent_json_documents_every_signals_key() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    let section = doc
        .split("## `pando signals`")
        .nth(1)
        .expect("the signals section")
        .split("\n## ")
        .next()
        .unwrap();
    let published = serde_json::to_value(crate::detect::Signals::default()).unwrap();
    for key in published.as_object().unwrap().keys() {
        assert!(
            section.contains(&format!("\"{key}\"")),
            "agent/json.md never documents signals.{key}"
        );
    }
}

#[test]
fn start_waits_on_a_terminal_unless_told_not_to() {
    assert!(super::waits_on(false, false, true), "a terminal waits");
    assert!(!super::waits_on(false, true, true), "--no-wait");
    assert!(!super::waits_on(false, false, false), "a script does not");
    assert!(super::waits_on(true, false, false), "--wait, from a script");
}

#[test]
fn open_wants_something_up_or_a_share() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let config = with_dev(&fx.config);
    let named = super::names::target_named(&fx.paths, Some("feat/one"), "open").unwrap();
    let open = |config: &Config, want| super::open::url_to_open(&fx.paths, config, &named, want);
    let err = open(&config, Want::Page).unwrap_err();
    // Named as a person knows it, and the command echoes what was typed.
    assert_eq!(
        format!("{err:#}"),
        "feat/one is not running — `pando start feat/one` starts it"
    );
    let err = open(&config, Want::Public).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "feat/one is not shared — `pando share feat/one` publishes it"
    );

    with_share(&fx, &name, None);
    assert_eq!(
        open(&config, Want::Page).unwrap(),
        super::open::Opening::Url("http://localhost:17342".into())
    );
    assert_eq!(
        open(&config, Want::Public).unwrap(),
        super::open::Opening::Url("https://fake-host.trycloudflare.com".into())
    );
}

fn with_dev(config: &Config) -> Config {
    let mut config = config.clone();
    config.processes.insert(
        "dev".into(),
        crate::config::ProcessConfig {
            cmd: "true".into(),
            ..Default::default()
        },
    );
    config
}

// `pando open feat/r` on a project with nothing to run suggested
// "`pando start feat+r` starts it", which cannot work.
#[test]
fn open_on_a_project_with_nothing_to_run_says_so_and_how_to_add_one() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/r", None, &|_| {}).unwrap();
    let named = super::names::target_named(&fx.paths, Some("feat/r"), "open").unwrap();
    let err = super::open::url_to_open(&fx.paths, &fx.config, &named, Want::Page).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.starts_with("feat/r is not running, and this project has nothing to run"));
    assert!(!msg.contains("pando start"), "{msg}");
    assert!(
        msg.contains(&fx.paths.config_file().display().to_string()),
        "{msg}"
    );
    assert!(msg.contains("[dev]\ncmd = \""), "{msg}");
}

// `init --dry-run` said "nothing left to answer — … already says it all"
// above "schema command (unanswered) …".
#[test]
fn init_never_says_nothing_is_left_above_an_unanswered_slot() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("pando.toml");
    std::fs::write(&file, "").unwrap();
    let slot = |slot, label, value: &str| actions::SlotSummary {
        slot,
        label,
        value: Some(value.to_string()),
        answered_now: false,
    };
    let mut report = actions::InitReport {
        config_file: file.clone(),
        user_file: None,
        slots: vec![
            slot(crate::detect::Slot::DevCmd, "dev command", "cargo run"),
            slot(
                crate::detect::Slot::SchemaHook,
                "schema command",
                "(unanswered) run the schema step?",
            ),
        ],
        warnings: Vec::new(),
    };
    let text = super::answers::render_init(&report, "would write");
    assert!(!text.contains("nothing left"), "{text}");
    assert!(text.starts_with("1 question is still unanswered"), "{text}");
    assert!(text.contains("schema command  (unanswered)"), "{text}");

    report.slots.pop();
    let text = super::answers::render_init(&report, "would write");
    assert!(text.starts_with("nothing left to answer"), "{text}");
}

// `start` on a library said "no processes configured; add [dev] to
// pando.toml" — no path, no example — and a failed install said only that
// it failed.
#[test]
fn a_start_error_says_which_file_to_edit_and_what_to_write() {
    let fx = fixture();
    let err = super::with_a_way_past(
        &fx.paths,
        anyhow::anyhow!("no processes configured; add [dev] to pando.toml"),
    );
    let msg = format!("{err:#}");
    assert!(msg.starts_with("nothing to run: "), "{msg}");
    assert!(
        msg.contains(&format!(
            "{}:\n[dev]\ncmd = \"",
            fx.paths.config_file().display()
        )),
        "{msg}"
    );

    let err = super::with_a_way_past(
        &fx.paths,
        anyhow::anyhow!("exited 2: ERR_PNPM_OUTDATED_LOCKFILE").context("the install hook failed"),
    );
    let msg = format!("{err:#}");
    assert!(
        msg.starts_with("the install hook failed: exited 2: ERR_PNPM"),
        "{msg}"
    );
    assert!(
        msg.contains(&format!(
            "`project.install` in {} is the command; fix it there, or set it to \"\" to skip \
             installing",
            fx.paths.config_file().display()
        )),
        "{msg}"
    );
    // The file that really says it, when a committed one does.
    std::fs::write(fx.root.join("pando.toml"), "[project]\ninstall = \"x\"\n").unwrap();
    let msg = format!(
        "{:#}",
        super::with_a_way_past(
            &fx.paths,
            anyhow::anyhow!("the install hook failed: exited 2")
        )
    );
    assert!(
        msg.contains(&fx.root.join("pando.toml").display().to_string()),
        "{msg}"
    );

    // Anything else, including a question, is left exactly as it was.
    let usage = super::with_a_way_past(&fx.paths, UsageError("x".into()).into());
    assert!(usage.downcast_ref::<UsageError>().is_some());
}

// An error from below the CLI knew only the directory: the sentence gets
// the branch, a suggested command what was typed, and a path stays a path.
#[test]
fn an_error_is_reworded_to_the_name_a_person_knows() {
    let named = super::names::Named {
        dir: "feat+m".into(),
        shown: "feat/m".into(),
        typed: "feat+m".into(),
    };
    assert_eq!(
        named.in_words(
            "feat+m is still starting — `pando status feat+m` says when; log in /h/logs/feat+m/dev.log"
        ),
        "feat/m is still starting — `pando status feat+m` says when; log in /h/logs/feat+m/dev.log"
    );
    let typed_branch = super::names::Named {
        typed: "feat/m".into(),
        ..named.clone()
    };
    assert_eq!(
        typed_branch.in_words("feat+m has no port yet — start it first"),
        "feat/m has no port yet — start it first"
    );
    assert_eq!(
        typed_branch.in_words("feat+mx is other"),
        "feat+mx is other"
    );
    // A question keeps its type, so its exit code survives.
    let question = anyhow::Error::new(UsageError("feat+m".into()));
    assert!(
        named
            .reword(question)
            .downcast_ref::<UsageError>()
            .is_some()
    );
}

#[test]
fn completions_cover_every_verb() {
    let mut out = Vec::new();
    completions(clap_complete::Shell::Zsh, &mut out).unwrap();
    let script = String::from_utf8(out).unwrap();
    for sub in Cli::command().get_subcommands() {
        if sub.is_hide_set() {
            continue;
        }
        assert!(script.contains(sub.get_name()), "{}", sub.get_name());
    }
}

fn completion_script(shell: clap_complete::Shell) -> String {
    let mut out = Vec::new();
    completions(shell, &mut out).unwrap();
    String::from_utf8(out).unwrap()
}

// `completions zsh` completed `<name>` with file names.
#[test]
fn a_worktree_argument_completes_worktree_names_not_files() {
    let verbs = super::completion::verbs_taking_a_worktree();
    for verb in [
        "start", "stop", "restart", "logs", "open", "share", "unshare", "status", "rm", "path",
    ] {
        assert!(verbs.iter().any(|v| v == verb), "{verb}: {verbs:?}");
    }
    assert!(!verbs.iter().any(|v| v == "new"), "new takes a new branch");

    let zsh = completion_script(clap_complete::Shell::Zsh);
    assert!(zsh.contains("_pando_worktrees() {"), "{zsh}");
    assert!(zsh.contains("pando ls --names 2>/dev/null"), "{zsh}");
    // Defined before the function that calls it.
    assert!(zsh.find("_pando_worktrees() {") < zsh.find("_pando() {"));
    let name_lines: Vec<&str> = zsh.lines().filter(|l| l.contains("name -- ")).collect();
    assert_eq!(name_lines.len(), verbs.len(), "{name_lines:#?}");
    for line in name_lines {
        assert!(line.ends_with(":_pando_worktrees' \\"), "{line}");
    }

    let bash = completion_script(clap_complete::Shell::Bash);
    assert_eq!(
        bash.matches("$(pando ls --names 2>/dev/null)").count(),
        verbs.len(),
        "{bash}"
    );
    let fish = completion_script(clap_complete::Shell::Fish);
    assert!(
        fish.contains("__fish_seen_subcommand_from rm path start") && fish.contains("ls --names"),
        "{fish}"
    );
}

#[test]
fn ls_names_lists_what_a_worktree_argument_accepts() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| super::completion::names(&fx.paths, b));
    let names: Vec<&str> = text.lines().collect();
    assert_eq!(names, ["acme-shop", "feat/one"], "{text}");
    // Every one resolves for every verb, the main checkout included.
    for name in &names {
        super::names::path(&fx.paths, name).unwrap();
        super::names::resolve(&fx.paths, name).unwrap();
    }
}

// A menu entry is a line: the long `--help` paragraph belongs to `--help`.
#[test]
fn completion_menus_get_the_short_help_only() {
    let zsh = completion_script(clap_complete::Shell::Zsh);
    for line in zsh.lines().filter(|l| l.starts_with('\'')) {
        let Some(open) = line.find('[') else { continue };
        let Some(close) = line.rfind(']') else {
            continue;
        };
        if close <= open {
            continue;
        }
        let description = &line[open + 1..close];
        assert!(
            description.chars().count() <= 100,
            "a menu description of {} characters: {line}",
            description.chars().count()
        );
    }
}

/// The exit 3 with no options is documented where the exit codes are,
/// naming the question it asks; `tests/cli.rs` holds the binary to it.
#[test]
fn the_contract_documents_the_exit_3_for_a_project_that_would_run_nothing() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .expect("read agent/json.md");
    let codes = &doc[doc
        .find("## stdout, stderr, and exit codes")
        .expect("an exit codes section")..];
    let codes = &codes[..codes[3..].find("\n## ").map_or(codes.len(), |at| at + 3)];
    let flat = codes.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(
            "One exit 3 has no options: `pando init` on a project that would run nothing"
        ),
        "{flat}"
    );
    assert!(
        flat.contains(&format!(
            "The question is `{}`",
            slot_name(crate::detect::Slot::DevCmd)
        )),
        "{flat}"
    );
}

/// The exit 3 at `processes` for an app directory nothing starts is
/// documented beside it, quoting the words its evidence ends with.
#[test]
fn the_contract_documents_the_exit_3_for_an_app_directory_nothing_starts() {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .expect("read agent/json.md");
    let flat = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(&format!(
            "One exit 3 has options and nothing preferred: `{}`",
            slot_name(crate::detect::Slot::Processes)
        )),
        "{flat}"
    );
    assert!(
        flat.contains(&format!(
            "`backend has uv.lock but no dev script: {}`",
            crate::detect::UNSTARTED
        )),
        "{flat}"
    );
}

#[test]
fn help_documents_the_needs_answer_exit_code() {
    let help = Cli::command().render_help().to_string();
    assert!(help.contains("Exit codes"), "{help}");
    assert!(
        help.contains("3  needs an answer"),
        "an agent has to be able to tell a question from a failure: {help}"
    );
}

// ---- ls columns ------------------------------------------------------

fn widths(pairs: &[(Col, usize)]) -> BTreeMap<Col, usize> {
    pairs.iter().copied().collect()
}

fn every_column(path: usize) -> BTreeMap<Col, usize> {
    widths(&[
        (Col::Name, 10),
        (Col::Status, 8),
        (Col::Url, 22),
        (Col::Ports, 5),
        (Col::Mode, 8),
        (Col::Public, 40),
        (Col::Git, 5),
        (Col::Branch, 10),
        (Col::Head, 7),
        (Col::Path, path),
    ])
}

#[test]
fn a_wide_terminal_keeps_every_column() {
    assert_eq!(keep_columns(400, &every_column(40)), ORDER.to_vec());
}

// A tmux split is the normal case, so the listing has to survive one:
// what a worktree is doing outlives what git thinks of it, and the URL is
// what somebody came to copy.
#[test]
fn a_narrow_terminal_sheds_columns_and_keeps_name_status_and_url() {
    let w = every_column(60);
    let mid = keep_columns(100, &w);
    assert!(
        !mid.contains(&Col::Path) && !mid.contains(&Col::Head),
        "the path and the sha go first: {mid:?}"
    );
    assert!(mid.contains(&Col::Url), "{mid:?}");

    let tight = keep_columns(44, &w);
    assert_eq!(tight, vec![Col::Name, Col::Status, Col::Url]);
    let tighter = keep_columns(24, &w);
    assert_eq!(tighter, vec![Col::Name, Col::Status]);
    let sliver = keep_columns(4, &w);
    assert_eq!(
        sliver,
        vec![Col::Name, Col::Status],
        "the name and the status are never dropped"
    );
}

#[test]
fn ls_shows_the_ports_and_status_of_a_running_worktree() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    record.ports.insert("web".to_string(), 17_342);
    record.processes.insert(
        "dev".to_string(),
        crate::state::ProcessRecord {
            pid: std::process::id(),
            pgid: Group::from_raw(std::process::id() as i32),
            started_at: Utc::now(),
            log_path: fx.paths.log_file(&name, "dev"),
            ready_port: Some(17_342),
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: Phase::Running { since: Utc::now() },
        },
    );
    crate::state::save(&fx.paths.state_file(), &store).unwrap();

    let text = capture(|b| ls_text_at(&fx.paths, b, 200));
    assert!(text.contains("PORTS") && text.contains("STATUS"), "{text}");
    // One port is named by its role like many are: a bare "17342" beside
    // another row's "api:29496 web:29497" read as a different thing.
    assert!(text.contains("web:17342"), "{text}");
    assert!(text.contains("running"), "{text}");

    let narrow = capture(|b| ls_text_at(&fx.paths, b, 24));
    assert!(narrow.contains("feat/one"), "{narrow}");
    assert!(
        !narrow.contains("PATH"),
        "a narrow listing sheds the path: {narrow}"
    );
}

#[test]
fn a_worktree_with_nothing_running_shows_dashes() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| ls_text_at(&fx.paths, b, 200));
    assert!(text.contains("feat/one"), "{text}");
    assert!(text.contains(" -"), "{text}");
}

/// A process record for a live group with the sockets it was seen
/// holding.
fn listening(pgid: i32, observed: &[u16]) -> crate::state::ProcessRecord {
    crate::state::ProcessRecord {
        pid: std::process::id(),
        pgid: Group::from_raw(pgid),
        started_at: Utc::now(),
        log_path: PathBuf::from("/does/not/exist/dev.log"),
        ready_port: None,
        ready_timeout_s: None,
        observed_ports: observed.to_vec(),
        swept: false,
        phase: Phase::Running { since: Utc::now() },
    }
}

/// A worktree shaped like a one-process start: `dev` owning `web`.
fn one_process_record(observed: &[u16]) -> WorktreeRecord {
    let mut record = WorktreeRecord::new("/trees/feat+one", true);
    record.ports.insert("web".to_string(), 17_342);
    record
        .roles
        .insert("dev".to_string(), vec!["web".to_string()]);
    record
        .processes
        .insert("dev".to_string(), listening(101, observed));
    record.observed_ports = observed.to_vec();
    record
}

/// A worktree shaped like a two-process start: `web` and `api`, each
/// owning its own role and running in its own group.
fn two_process_record(web: &[u16], api: &[u16]) -> WorktreeRecord {
    let mut record = WorktreeRecord::new("/trees/feat+one", true);
    record.ports.insert("web".to_string(), 17_342);
    record.ports.insert("api".to_string(), 17_343);
    record
        .roles
        .insert("web".to_string(), vec!["web".to_string()]);
    record
        .roles
        .insert("api".to_string(), vec!["api".to_string()]);
    record
        .processes
        .insert("web".to_string(), listening(101, web));
    record
        .processes
        .insert("api".to_string(), listening(102, api));
    let mut union: Vec<u16> = web.iter().chain(api).copied().collect();
    union.sort_unstable();
    union.dedup();
    record.observed_ports = union;
    record
}

// The documented behaviour — "what it is really listening on when that
// is known" — was two identical match arms, so a framework that ignored
// `PORT` and bound something else still had the assigned port printed
// as its URL.
#[test]
fn the_url_prefers_a_port_the_process_is_really_listening_on() {
    assert_eq!(
        worktree_url(&one_process_record(&[17_342, 17_399])).as_deref(),
        Some("http://localhost:17342"),
        "the assigned port is among them, so it is the one"
    );
    assert_eq!(
        worktree_url(&one_process_record(&[3_000])).as_deref(),
        Some("http://localhost:3000"),
        "it ignored the port pando gave it; the URL follows the process"
    );
    assert_eq!(
        worktree_url(&one_process_record(&[])).as_deref(),
        Some("http://localhost:17342"),
        "nothing observed at all falls back to what was assigned"
    );
    // And nothing running at all: the port survives the stop, so the
    // URL the developer bookmarked is still the one they get.
    let mut record = one_process_record(&[3_000]);
    record.processes.clear();
    record.observed_ports.clear();
    assert_eq!(
        worktree_url(&record).as_deref(),
        Some("http://localhost:17342")
    );
}

// Phase 2b review, finding 6. One rule with three implementations:
// `start` took the first role of the first process, `status` and `ls`
// took the alphabetically first *role*, and the TUI took that and never
// looked at what was really listening. Two processes whose role names
// sort the other way round from their own names were all it took for
// `pando start` and `pando status`, seconds apart, to hand out two
// different URLs.
#[test]
fn the_url_is_the_first_role_of_the_first_process_when_nothing_owns_web() {
    let mut record = WorktreeRecord::new("/trees/feat+url2", true);
    record.ports.insert("srv".to_string(), 19_056);
    record.ports.insert("admin".to_string(), 19_057);
    record
        .roles
        .insert("alpha".to_string(), vec!["srv".to_string()]);
    record
        .roles
        .insert("beta".to_string(), vec!["admin".to_string()]);
    assert_eq!(
        worktree_url(&record).as_deref(),
        Some("http://localhost:19056"),
        "alpha comes first, so alpha's first role is the worktree's URL"
    );

    // `web` still wins wherever anything owns it, whatever it sorts
    // against.
    record.ports.insert("web".to_string(), 19_058);
    record
        .roles
        .insert("zeta".to_string(), vec!["web".to_string()]);
    assert_eq!(
        worktree_url(&record).as_deref(),
        Some("http://localhost:19058")
    );
}

// Phase 2b review, finding 4. `f6093df` narrowed "prefer an observed
// port" to "prefer one no role claims", but the observed list was one
// flat set per worktree, so a socket the *api* opened — an HMR socket,
// `node --inspect`, a metrics port — was indistinguishable from one the
// web process opened, and became the worktree's URL while the web
// server was not serving at all.
#[test]
fn the_url_follows_a_listener_only_in_the_group_that_owns_the_role() {
    assert_eq!(
        worktree_url(&two_process_record(&[17_342], &[17_343])).as_deref(),
        Some("http://localhost:17342"),
        "both up, each on its own port"
    );

    assert_eq!(
        worktree_url(&two_process_record(&[17_342], &[9876, 17_343])).as_deref(),
        Some("http://localhost:17342"),
        "the api's second socket is the api's, whatever claims it"
    );

    // The review's reproduction: the web process stopped, the api kept
    // serving, and it holds a port no role claims.
    let mut record = two_process_record(&[], &[9876, 17_343]);
    record.processes.remove("web");
    assert_eq!(
        worktree_url(&record).as_deref(),
        Some("http://localhost:17342"),
        "the web role's own port, not whatever the api happens to hold"
    );

    // And a framework that ignored `PORT` is still followed, because
    // there it is the process that owns the role doing the ignoring.
    assert_eq!(
        worktree_url(&two_process_record(&[3000], &[17_343])).as_deref(),
        Some("http://localhost:3000"),
        "the web process itself bound 3000"
    );

    // A port another role already has is never a candidate either.
    assert_eq!(
        worktree_url(&two_process_record(&[17_343], &[17_343])).as_deref(),
        Some("http://localhost:17342")
    );
}

// ---- questions -------------------------------------------------------

fn dev_question(options: &[&str]) -> actions::Question {
    actions::Question {
        slot: crate::detect::Slot::DevCmd,
        prompt: "Which command starts the local development server?".to_string(),
        options: options
            .iter()
            .map(|v| (v.to_string(), "a signal".to_string()))
            .collect(),
        preselect: (!options.is_empty()).then_some(0),
        allow_custom: true,
        allow_none: false,
        multi: false,
        checked: Vec::new(),
        details: Vec::new(),
        answer_file: None,
        snippet: String::new(),
    }
}

/// The services question of fixture 6: two ticked by a rule, two the
/// rules could not place.
fn services_question() -> actions::Question {
    actions::Question {
        slot: crate::detect::Slot::Services,
        prompt: "Run private copies of these services for each worktree?".to_string(),
        options: ["cache", "db", "mail", "queue"]
            .iter()
            .map(|v| (v.to_string(), "docker-compose.yml".to_string()))
            .collect(),
        preselect: Some(0),
        allow_custom: false,
        allow_none: true,
        multi: true,
        checked: vec![1, 2],
        details: Vec::new(),
        answer_file: None,
        snippet: String::new(),
    }
}

/// The prompt driven by a script of typed lines, as a terminal would.
fn answer_with(question: &actions::Question, lines: &[&str]) -> (Result<actions::Answer>, String) {
    let mut typed = lines.iter().map(|l| format!("{l}\n"));
    let mut out = Vec::new();
    let answer = prompt_with(question, &mut out, || Ok(typed.next()));
    (answer, String::from_utf8(out).unwrap())
}

// ---- the multi-select question ---------------------------------------

#[test]
fn a_set_question_starts_from_what_the_rules_resolved() {
    let question = services_question();
    let (answer, printed) = answer_with(&question, &[""]);
    assert_eq!(answer.unwrap(), actions::Answer::Many(vec![1, 2]));
    assert!(printed.contains("[ ] 1) cache"), "{printed}");
    assert!(printed.contains("[x] 2) db"), "{printed}");
    assert!(printed.contains("[x] 3) mail"), "{printed}");
    assert!(printed.contains("[ ] 4) queue"), "{printed}");
    assert!(printed.contains("accepts [db, mail]"), "{printed}");
}

#[test]
fn a_number_toggles_one_option_and_enter_takes_the_rest() {
    let question = services_question();
    // Tick `cache`, untick `mail`, accept.
    let (answer, _) = answer_with(&question, &["1", "3", ""]);
    assert_eq!(answer.unwrap(), actions::Answer::Many(vec![0, 1]));
}

#[test]
fn unticking_everything_is_the_answer_none() {
    let question = services_question();
    let (answer, _) = answer_with(&question, &["2", "3", ""]);
    assert_eq!(answer.unwrap(), actions::Answer::None);
    // And so is saying so outright.
    let (answer, _) = answer_with(&services_question(), &["n"]);
    assert_eq!(answer.unwrap(), actions::Answer::None);
}

#[test]
fn a_number_out_of_range_reprints_the_range_and_ticks_nothing() {
    let question = services_question();
    let (answer, printed) = answer_with(&question, &["9", "not-a-number", ""]);
    assert_eq!(answer.unwrap(), actions::Answer::Many(vec![1, 2]));
    assert_eq!(
        printed
            .matches("a number between 1 and 4 toggles one")
            .count(),
        2,
        "{printed}"
    );
}

// `Auto`, not `Many`: the resolver turns `Auto` into the ticked set
// *and* into a comment saying a flag took it. A `Many` would be
// written down as if a human had chosen, which is a config nobody can
// review.
#[test]
fn yes_takes_the_ticked_set_as_an_auto_answer_not_a_choice() {
    let question = services_question();
    assert_eq!(asker(true)(&question).unwrap(), actions::Answer::Auto(0));
}

#[test]
fn exit_three_shows_a_set_question_with_its_boxes() {
    let needs = actions::NeedsAnswer {
        question: services_question(),
    };
    let text = render_needs_answer(&needs);
    assert!(text.contains("[ ] 1) cache"), "{text}");
    assert!(text.contains("[x] 2) db"), "{text}");
    assert!(text.contains("[[services]]"), "{text}");
    assert!(
        text.contains("--yes to take the ticked ones"),
        "an agent has to be told what --yes would do: {text}"
    );
}

// A fat-fingered number used to fall through to "it must be a command",
// so `5` on a four-option question became `cmd = "5"`, dated as if a
// human had meant it, and `start` reported success over a shell error.
#[test]
fn a_number_at_the_prompt_is_always_a_choice() {
    let question = dev_question(&["pnpm dev", "pnpm dev:web"]);
    let (answer, printed) = answer_with(&question, &["5", "0", "99", "2"]);
    assert_eq!(answer.unwrap(), actions::Answer::Choice(1));
    assert_eq!(
        printed.matches("pick a number between 1 and 2").count(),
        3,
        "every out-of-range number reprints the range: {printed}"
    );
    assert!(
        !printed.contains("command > "),
        "and none of them is a command: {printed}"
    );
}

// The way a command that is only digits is still reachable.
#[test]
fn a_number_typed_after_c_is_taken_as_the_command() {
    let question = dev_question(&["pnpm dev", "pnpm dev:web"]);
    let (answer, _) = answer_with(&question, &["c", "5"]);
    assert_eq!(answer.unwrap(), actions::Answer::Custom("5".to_string()));
}

// Unchanged: anything that is not a number is the command itself, so
// nobody has to discover that `c` exists first.
#[test]
fn a_line_that_is_not_a_number_is_still_the_command() {
    let question = dev_question(&["pnpm dev"]);
    let (answer, _) = answer_with(&question, &["./my-own-server"]);
    assert_eq!(
        answer.unwrap(),
        actions::Answer::Custom("./my-own-server".to_string())
    );
}

// With nothing on offer, a number cannot be a choice at all.
#[test]
fn a_question_with_no_options_takes_a_number_as_the_command() {
    let question = dev_question(&[]);
    let (answer, _) = answer_with(&question, &["5"]);
    assert_eq!(answer.unwrap(), actions::Answer::Custom("5".to_string()));
}

// `--yes` takes the first option; it cannot take one that is not there.
#[test]
fn a_question_with_nothing_to_offer_does_not_point_at_yes() {
    let needs = actions::NeedsAnswer {
        question: dev_question(&[]),
    };
    let text = render_needs_answer(&needs);
    assert!(
        !text.contains("--yes"),
        "there is nothing for --yes to take: {text}"
    );
    assert!(text.contains("pando.toml"), "{text}");
}

#[test]
fn a_question_with_options_still_points_at_yes() {
    let needs = actions::NeedsAnswer {
        question: dev_question(&["pnpm dev", "pnpm dev:web"]),
    };
    assert!(render_needs_answer(&needs).contains("--yes"));
}

// `check` never answers a question itself and takes no `--yes`, so the
// way out it prints goes through `pando init` and back to `pando check`,
// and a program's answer through stdin: the brief forbids an answers file
// in the repository.
#[test]
fn a_question_from_check_sends_the_reader_through_init_and_back() {
    let needs = actions::NeedsAnswer {
        question: dev_question(&["pnpm dev", "pnpm dev:web"]),
    };
    let text = super::render_needs_answer_for(&needs, super::Rerun::InitThenCheck);
    assert!(!text.contains("rerun with --yes"), "{text}");
    assert!(
        text.contains(
            "or let `pando init --yes` take the first option; then run `pando check` again"
        ),
        "{text}"
    );
    assert!(
        text.contains("`pando init --answers -` with, on stdin,"),
        "{text}"
    );
    assert!(!text.contains("<file.json>"), "{text}");
    // Every other command keeps its own way out.
    let plain = render_needs_answer(&needs);
    assert!(
        plain.contains("rerun with --yes to take the first option"),
        "{plain}"
    );
    assert!(!plain.contains("pando check"), "{plain}");

    let nothing = actions::NeedsAnswer {
        question: dev_question(&[]),
    };
    let text = super::render_needs_answer_for(&nothing, super::Rerun::InitThenCheck);
    assert!(!text.contains("--yes"), "nothing for --yes to take: {text}");
    assert!(text.contains("then run `pando check` again"), "{text}");
}

// ---- status ----------------------------------------------------------

#[test]
fn status_json_carries_the_documented_shape() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    record.ports.insert("web".to_string(), 17_342);
    record.observed_ports = vec![17_342, 17_399];
    // Alive, so the read path leaves it Running, with a process group
    // that no longer exists — so a scan that runs and finds nothing is
    // an answer, and the ports it is really listening on are none. The
    // last good answer survives only a scan that could not run at all.
    record.processes.insert(
        "dev".to_string(),
        crate::state::ProcessRecord {
            pid: std::process::id(),
            pgid: Group::from_raw(999_998),
            started_at: Utc::now(),
            log_path: fx.paths.log_file(&name, "dev"),
            ready_port: Some(17_342),
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: Phase::Running { since: Utc::now() },
        },
    );
    record.processes.insert(
        "worker".to_string(),
        crate::state::ProcessRecord {
            pid: 4242,
            pgid: Group::from_raw(4242),
            started_at: Utc::now(),
            log_path: fx.paths.log_file(&name, "worker"),
            ready_port: None,
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: Phase::Failed {
                at: Utc::now(),
                reason: "process exited".to_string(),
            },
        },
    );
    record.hooks.insert(
        "install".to_string(),
        crate::state::HookRecord {
            fingerprint: Some("md5:abc".to_string()),
            ran_at: Utc::now(),
        },
    );
    crate::state::save(&fx.paths.state_file(), &store).unwrap();

    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    // Pinned as a literal on purpose: a bump must fail here, at the
    // commit that makes it, rather than passing quietly.
    assert_eq!(v["version"], 2);
    assert_eq!(v["project"]["name"], "acme-shop");
    let wt = &v["worktrees"][0];
    assert_eq!(wt["name"], "feat+one");
    assert_eq!(wt["branch"], "feat/one");
    assert_eq!(wt["ports"]["web"], 17_342);
    assert_eq!(wt["observed_ports"], serde_json::json!([]));
    assert_eq!(wt["url"], "http://localhost:17342");
    let dev = &wt["processes"]["dev"];
    assert_eq!(dev["pid"], std::process::id());
    assert_eq!(dev["phase"], "running");
    assert_eq!(dev["reason"], serde_json::Value::Null);
    assert!(dev["since"].is_string());
    assert!(dev["log"].as_str().unwrap().ends_with("dev.log"));
    let worker = &wt["processes"]["worker"];
    assert_eq!(worker["phase"], "failed");
    assert_eq!(worker["reason"], "process exited");
    assert_eq!(wt["hooks"]["install"]["fingerprint"], "md5:abc");
}

/// A worktree with a live share recorded, so the status shapes can be
/// asserted without a tunnel. Every pid is this process: alive, so the
/// refresh leaves the record alone — the application it publishes
/// included, because a share whose application is gone is closed.
fn with_share(fx: &Fx, name: &str, proxy: Option<u16>) {
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(name).unwrap();
    record.ports.insert("web".to_string(), 17_342);
    record
        .roles
        .insert("dev".to_string(), vec!["web".to_string()]);
    record
        .processes
        .insert("dev".to_string(), listening(std::process::id() as i32, &[]));
    record.share_port = proxy;
    record.share = Some(crate::state::ShareRecord {
        tunnel_pid: std::process::id(),
        tunnel_pgid: Group::from_raw(999_998),
        public_url: "https://fake-host.trycloudflare.com".to_string(),
        local_port: 17_342,
        started_at: Utc::now(),
        log_path: fx.paths.log_file(name, "tunnel"),
        proxy_pid: proxy.map(|_| std::process::id()),
        proxy_pgid: proxy.map(|_| Group::from_raw(999_997)),
        proxy_port: proxy,
    });
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
}

#[test]
fn status_json_carries_the_public_url_and_never_a_cookie() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_share(&fx, &name, Some(17_349));

    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let share = &v["worktrees"][0]["share"];
    assert_eq!(share["url"], "https://fake-host.trycloudflare.com");
    assert_eq!(share["local_port"], 17_342);
    assert_eq!(share["proxy_port"], 17_349);
    assert!(share["since"].is_string());
    assert!(
        !text.to_lowercase().contains("cookie"),
        "a credential must never reach a shape that gets piped into things:\n{text}"
    );
}

#[test]
fn status_json_says_null_for_a_worktree_that_is_not_shared() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["worktrees"][0]["share"], serde_json::Value::Null);
}

#[test]
fn status_json_reports_a_share_with_no_proxy_in_front_of_it() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_share(&fx, &name, None);
    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["worktrees"][0]["share"]["proxy_port"],
        serde_json::Value::Null
    );
}

#[test]
fn status_text_prints_the_public_url_under_its_worktree() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_share(&fx, &name, Some(17_349));

    let text = capture(|b| status_text_at(&fx.paths, None, b, 120));
    assert!(text.contains("share"), "{text}");
    assert!(
        text.contains("https://fake-host.trycloudflare.com"),
        "{text}"
    );
    assert!(
        text.contains("through a proxy on 17349"),
        "the proxy is worth saying: a visitor arrives authenticated: {text}"
    );
}

// The same degradation every other row has: truncated, never wrapped.
#[test]
fn the_share_row_truncates_on_a_narrow_terminal() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_share(&fx, &name, Some(17_349));

    let text = capture(|b| status_text_at(&fx.paths, None, b, 40));
    for line in text.lines() {
        assert!(
            line.chars().count() <= 40,
            "a row wider than the terminal: {line:?}"
        );
    }
}

#[test]
fn status_json_reports_a_worktree_that_was_never_started() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let wt = &v["worktrees"][0];
    assert!(wt["processes"].as_object().unwrap().is_empty());
    assert_eq!(wt["url"], serde_json::Value::Null);
    assert!(wt["observed_ports"].as_array().unwrap().is_empty());
    assert_eq!(wt["mode"], "shared", "never started is shared");
    assert_eq!(wt["isolated"], false);
}

/// A worktree running an Expo app beside its backend, as the settings in
/// `config` say: `mobile` owning `mobile` on 18081, `api` owning `web`.
fn with_mobile_app(fx: &Fx, config: &str) -> String {
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    std::fs::write(fx.paths.config_file(), config).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    for (process, role, port) in [("api", "web", 17_342), ("mobile", "mobile", 18_081)] {
        record.ports.insert(role.to_string(), port);
        record
            .roles
            .insert(process.to_string(), vec![role.to_string()]);
        record
            .processes
            .insert(process.to_string(), listening(999_998, &[]));
    }
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    name
}

const MOBILE_BY_VARIABLE: &str = "[processes.api]\ncmd = \"npm run dev\"\n\
     ports = { PORT = \"web\" }\n\n\
     [processes.mobile]\ncmd = \"npm run start\"\ncwd = \"apps/mobile\"\n\
     ports = [\"mobile\"]\nenv = { RCT_METRO_PORT = \"{port:mobile}\", \
     EXPO_PUBLIC_API_URL = \"http://127.0.0.1:{port:web}\" }\n";

// Expo's "press i" is gone under pando, which runs it with no terminal,
// so `status` says how its app is opened on the simulator: the process is
// known from the settings alone, by Metro's port variable, and its link
// names the port the worktree gave it. Nothing else gets one.
#[test]
fn status_gives_the_link_that_opens_an_expo_app_on_the_simulator() {
    let fx = fixture();
    with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let processes = &v["worktrees"][0]["processes"];
    assert_eq!(
        processes["mobile"]["app"],
        serde_json::json!({
            "client": "Expo Go",
            "url": "exp://127.0.0.1:18081",
            "simulator": "xcrun simctl openurl booted 'exp://127.0.0.1:18081'",
            "android": "adb reverse tcp:18081 tcp:18081 && adb shell am start -a \
                        android.intent.action.VIEW -d 'exp://127.0.0.1:18081'",
            "development_build":
                "exp+<slug>://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A18081",
            "native": null,
            "installed": null,
        })
    );
    assert_eq!(processes["api"]["app"], serde_json::Value::Null);
    // Published: the contract names the key and each key under it.
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    let section = doc
        .split("## `pando status --json`")
        .nth(1)
        .expect("the status section")
        .split("\n## ")
        .next()
        .unwrap();
    let app = processes["mobile"]["app"].as_object().unwrap();
    for key in std::iter::once(&"app".to_string()).chain(app.keys()) {
        assert!(
            section.contains(&format!("\"{key}\"")),
            "agent/json.md never documents status's process {key}"
        );
    }

    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    let apps: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("simctl"))
        .collect();
    assert_eq!(apps.len(), 1, "{text}");
    assert!(apps[0].trim_start().starts_with("mobile  app"), "{text}");
    assert!(
        apps[0].contains(
            "xcrun simctl openurl booted 'exp://127.0.0.1:18081' — opens it in Expo Go on the \
             simulator; for a development build, open exp+<slug>://expo-development-client/"
        ),
        "{text}"
    );
    // And under it, the Android device's or emulator's.
    let android = text
        .lines()
        .skip_while(|line| !line.contains("simctl"))
        .nth(1)
        .unwrap_or_default();
    assert!(android.trim_start().starts_with("mobile  app"), "{text}");
    assert!(
        android.ends_with(
            "adb reverse tcp:18081 tcp:18081 && adb shell am start -a android.intent.action.VIEW \
             -d 'exp://127.0.0.1:18081' — opens it on an Android device or emulator"
        ),
        "{text}"
    );
    // A narrow terminal cuts the development build's form, never the
    // command in front of it.
    let narrow = capture(|b| status_text_at(&fx.paths, None, b, 80));
    assert!(
        narrow.contains("xcrun simctl openurl booted 'exp://127.0.0.1:18081'"),
        "{narrow}"
    );
}

// An app with `expo-dev-client` is opened by its development build, which
// registers `exp+` and its slug, lowercased — not `expo.scheme`: the link
// is filled in from the worktree's own `app.json`, and it is the one the
// simulator command opens.
#[test]
fn status_opens_an_app_with_the_development_client_in_its_development_build() {
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    let app = record.path.join("apps/mobile");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("app.json"),
        r#"{ "expo": { "name": "Drivee", "slug": "DriveeSafeCall", "scheme": "drivee" } }"#,
    )
    .unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{ "dependencies": { "expo": "~57.0.0", "expo-dev-client": "~6.0.0" } }"#,
    )
    .unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    let url = "exp+driveesafecall://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A18081";
    let mut app = v["worktrees"][0]["processes"]["mobile"]["app"].clone();
    // A new `app.json` is a native change of the branch's own, said apart.
    app.as_object_mut().unwrap().remove("native");
    assert_eq!(
        app,
        serde_json::json!({
            "client": "its development build",
            "url": url,
            "simulator": format!("xcrun simctl openurl booted '{url}'"),
            "android": format!(
                "adb reverse tcp:18081 tcp:18081 && adb shell am start -a \
                 android.intent.action.VIEW -d '{url}'"
            ),
            "development_build": url,
            "installed": null,
        })
    );
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    let line = text.lines().find(|line| line.contains("simctl")).unwrap();
    assert!(
        line.ends_with(&format!(
            "xcrun simctl openurl booted '{url}' — opens it in its development build on the \
             simulator"
        )),
        "{text}"
    );

    // Configured in an `app.config.ts` that computes it, with no build on
    // a simulator, the slug is not known: `open` runs nothing for a link
    // that holds `exp+<slug>`, which no build registers, and prints the
    // commands to fill in.
    let app = record.path.join("apps/mobile");
    std::fs::remove_file(app.join("app.json")).unwrap();
    std::fs::write(
        app.join("app.config.ts"),
        "export default { slug: process.env.APP_SLUG };\n",
    )
    .unwrap();
    let config: Config = toml::from_str(MOBILE_BY_VARIABLE).unwrap();
    let links = actions::app_links(&config, &record)["mobile"].clone();
    assert!(links.url.starts_with("exp+<slug>://"), "{}", links.url);
    let said = std::cell::RefCell::new(Vec::new());
    let text = capture(|b| {
        super::open::open_apps(
            &fx.paths,
            &[("mobile".to_string(), links.clone())],
            b,
            &|line| said.borrow_mut().push(line.to_string()),
        )
    });
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        [
            "mobile: neither its app.json nor a `slug: \"…\"` literal in its config's code \
             names its expo.slug, and no build of it is on a booted simulator, so the scheme \
             its development build registers is not known: `exp+<slug>` stands for it — \
             filled in, this opens its app in its development build:"
                .to_string(),
            format!("  on the booted iOS simulator: {}", links.simulator),
            format!(
                "  on the connected Android device or emulator: {}",
                links.android
            ),
        ]
    );
    assert_eq!(
        *said.borrow(),
        ["opening mobile's app in its development build"]
    );
}

// An app configured in `app.config.ts` alone gets its development build's
// scheme from the slug the file gives as a literal, read and never run;
// one that computes its slug keeps the placeholder.
#[test]
fn status_reads_the_slug_an_app_config_gives_as_a_literal() {
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    let app = record.path.join("apps/mobile");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{ "dependencies": { "expo": "~57.0.0", "expo-dev-client": "~6.0.0" } }"#,
    )
    .unwrap();
    let url_of = || -> String {
        let v: serde_json::Value =
            serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
        v["worktrees"][0]["processes"]["mobile"]["app"]["url"]
            .as_str()
            .unwrap()
            .to_string()
    };
    std::fs::write(
        app.join("app.config.ts"),
        "import type { ExpoConfig } from \"expo/config\";\n\
         // slug: \"OldName\",\n\
         const config: ExpoConfig = { name: \"Drivee\", slug: \"DriveeSafeCall\" };\n\
         export default config;\n",
    )
    .unwrap();
    assert_eq!(
        url_of(),
        "exp+driveesafecall://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A18081"
    );
    std::fs::write(
        app.join("app.config.ts"),
        "export default { slug: process.env.APP_SLUG ?? \"drivee\" };\n",
    )
    .unwrap();
    assert_eq!(
        url_of(),
        "exp+<slug>://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A18081"
    );
}

/// A stand-in `xcrun` in the test's pando bin whose `simctl list` prints
/// `listing`. Every call is appended to the file returned.
fn fake_xcrun(paths: &PandoPaths, listing: &serde_json::Value) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = paths.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let calls = paths.home.join("xcrun-calls");
    let listed = paths.home.join("simctl-list.json");
    std::fs::write(&listed, listing.to_string()).unwrap();
    let script = bin.join("xcrun");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncat '{}'\n",
            calls.display(),
            listed.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    calls
}

/// A simulator's data directory at `data`, one install per app: its
/// `<name>.app` bundle, holding the config Expo embeds where there is one.
fn simulator_with(data: &std::path::Path, apps: &[(&str, Option<serde_json::Value>)]) {
    for (i, (name, config)) in apps.iter().enumerate() {
        let bundle = data
            .join("Containers/Bundle/Application")
            .join(format!("INSTALL-{i}"))
            .join(format!("{name}.app"));
        std::fs::create_dir_all(bundle.join("EXConstants.bundle")).unwrap();
        if let Some(config) = config {
            std::fs::write(
                bundle.join("EXConstants.bundle/app.config"),
                config.to_string(),
            )
            .unwrap();
        }
    }
}

/// `simctl list -j devices`'s shape, for `(name, state, data directory)`.
fn simctl_listing(devices: &[(&str, &str, &std::path::Path)]) -> serde_json::Value {
    let devices: Vec<serde_json::Value> = devices
        .iter()
        .enumerate()
        .map(|(i, (name, state, data))| {
            serde_json::json!({
                "name": name,
                "udid": format!("0000-{i}"),
                "state": state,
                "dataPath": data.display().to_string(),
                "isAvailable": true,
            })
        })
        .collect();
    serde_json::json!({ "devices": { "com.apple.CoreSimulator.SimRuntime.iOS-27-0": devices } })
}

/// The config Expo embeds in a development build.
fn embedded(slug: &str, bundle_id: &str, sdk: &str) -> serde_json::Value {
    serde_json::json!({
        "name": slug,
        "slug": slug,
        "scheme": "drivee",
        "sdkVersion": sdk,
        "ios": { "bundleIdentifier": bundle_id },
    })
}

// A development build made for an older SDK loads the worktree's newer
// JavaScript and crashes on the first native call it lacks. `status`
// reads the build off the booted simulator's disk, runs nothing on it,
// and says which SDK it is, which the worktree needs, and the command
// that builds its own. The simulators are listed once per `status`,
// however many worktrees run an app.
#[test]
fn status_says_when_the_installed_development_build_is_for_another_sdk() {
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let first = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    // A second worktree running the same app.
    let second = actions::new(&fx.paths, &fx.config, "feat/two", None, &|_| {}).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let mut copy = first.clone();
    copy.path = store.worktrees[&second].path.clone();
    store.worktrees.insert(second.clone(), copy);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    for record in [&store.worktrees[&name], &store.worktrees[&second]] {
        let app = record.path.join("apps/mobile");
        std::fs::create_dir_all(app.join("node_modules/expo")).unwrap();
        std::fs::write(
            app.join("app.config.ts"),
            "export default { name: \"Drivee\", slug: \"DriveeSafeCall\" };\n",
        )
        .unwrap();
        std::fs::write(
            app.join("package.json"),
            r#"{ "dependencies": { "expo": "~57.0.0", "expo-dev-client": "~6.0.0" } }"#,
        )
        .unwrap();
        // What is installed wins over the range.
        std::fs::write(
            app.join("node_modules/expo/package.json"),
            r#"{ "name": "expo", "version": "57.0.3" }"#,
        )
        .unwrap();
    }
    let booted = fx._dir.path().join("sim-booted");
    simulator_with(
        &booted,
        &[
            ("Safari", None),
            (
                "Other",
                Some(embedded("other-app", "com.example.other", "57.0.0")),
            ),
            (
                "DriveeSafeCall",
                Some(embedded("DriveeSafeCall", "com.example.drivee", "55.0.0")),
            ),
        ],
    );
    // A simulator that is shut down is never read.
    let shut = fx._dir.path().join("sim-shut");
    simulator_with(
        &shut,
        &[(
            "DriveeSafeCall",
            Some(embedded("DriveeSafeCall", "com.example.drivee", "57.0.0")),
        )],
    );
    let calls = fake_xcrun(
        &fx.paths,
        &simctl_listing(&[
            ("iPad Air", "Shutdown", &shut),
            ("iPhone 17 Pro", "Booted", &booted),
        ]),
    );

    let v: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    for worktree in v["worktrees"].as_array().unwrap() {
        assert_eq!(
            worktree["processes"]["mobile"]["app"]["installed"],
            serde_json::json!({
                "device": "iPhone 17 Pro",
                "sdk": 55,
                "expected_sdk": 57,
                "build": "npx expo run:ios --port 18081",
            }),
            "{worktree}"
        );
        assert_eq!(worktree["processes"]["api"]["app"], serde_json::Value::Null);
    }
    let listed = std::fs::read_to_string(&calls).unwrap();
    assert_eq!(listed, "simctl list -j devices booted\n");

    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    let rows: Vec<&str> = text
        .lines()
        .filter(|line| line.contains("the development build on"))
        .collect();
    assert_eq!(rows.len(), 2, "{text}");
    assert!(rows[0].trim_start().starts_with("mobile  build"), "{text}");
    assert!(
        rows[0].ends_with(
            "the development build on iPhone 17 Pro is SDK 55 and this worktree needs SDK 57 \
             — `npx expo run:ios --port 18081` in the app's directory builds its own"
        ),
        "{text}"
    );
    assert_eq!(std::fs::read_to_string(&calls).unwrap().lines().count(), 2);

    // Published: the contract names the key and each key under it.
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    let installed = v["worktrees"][0]["processes"]["mobile"]["app"]["installed"]
        .as_object()
        .unwrap();
    for key in std::iter::once(&"installed".to_string()).chain(installed.keys()) {
        assert!(
            doc.contains(&format!("\"{key}\"")),
            "agent/json.md never documents status's app {key}"
        );
    }
}

// An app whose config computes its slug and its bundle id opens in the
// build on the simulator whose name the config quotes: that build's
// scheme fills the link, since it was made for the worktree's SDK. With
// no running Metro, nothing asks the simulators at all.
#[test]
fn status_opens_an_app_that_computes_its_slug_in_the_build_installed_for_it() {
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    let app = record.path.join("apps/mobile");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("app.config.ts"),
        "const base = \"DriveeSafeCall\";\n\
         export default {\n\
           slug: process.env.APP_SLUG ?? base,\n\
           ios: { bundleIdentifier: process.env.BUNDLE_ID ?? base, buildNumber: \"\" },\n\
         };\n",
    )
    .unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{ "dependencies": { "expo": "^57.0.0", "expo-dev-client": "~6.0.0" } }"#,
    )
    .unwrap();
    let booted = fx._dir.path().join("sim-booted");
    simulator_with(
        &booted,
        &[
            (
                "Other",
                Some(embedded("other-app", "com.example.other", "55.0.0")),
            ),
            (
                "DriveeSafeCall",
                Some(embedded("DriveeSafeCall", "com.example.drivee", "57.0.0")),
            ),
            // Named nothing: the config's empty string is not its name.
            ("Unnamed", Some(embedded("", "", "55.0.0"))),
        ],
    );
    let calls = fake_xcrun(
        &fx.paths,
        &simctl_listing(&[("iPhone 17 Pro", "Booted", &booted)]),
    );
    let v: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    let got = &v["worktrees"][0]["processes"]["mobile"]["app"];
    assert_eq!(
        got["url"],
        "exp+driveesafecall://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A18081"
    );
    assert_eq!(got["installed"]["sdk"], 57);
    assert_eq!(got["installed"]["expected_sdk"], 57);
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    assert!(!text.contains("the development build on"), "{text}");

    // Metro stopped: its app is not looked for.
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store
        .worktrees
        .get_mut(&name)
        .unwrap()
        .processes
        .remove("mobile");
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    std::fs::remove_file(&calls).unwrap();
    capture(|b| status_json(&fx.paths, None, b));
    capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    assert!(!calls.exists(), "xcrun was asked with no Metro running");
}

// `open` reads the booted simulator the way `status` does: a build made
// for the worktree's SDK fills the scheme a computed config leaves out,
// and one made for another SDK is not opened, since it would crash on
// this worktree's JavaScript; the refusal gives the build that replaces
// it.
#[test]
fn open_takes_the_installed_builds_scheme_and_refuses_one_for_another_sdk() {
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let config: Config = toml::from_str(MOBILE_BY_VARIABLE).unwrap();
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    let app = record.path.join("apps/mobile");
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("app.config.ts"),
        "const base = \"DriveeSafeCall\";\nexport default { slug: process.env.APP_SLUG ?? base };\n",
    )
    .unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{ "dependencies": { "expo": "~57.0.0", "expo-dev-client": "~6.0.0" } }"#,
    )
    .unwrap();
    let named = super::names::target_named(&fx.paths, Some("feat/one"), "open").unwrap();
    let booted = fx._dir.path().join("sim-booted");
    fake_xcrun(
        &fx.paths,
        &simctl_listing(&[("iPhone 17 Pro", "Booted", &booted)]),
    );

    simulator_with(
        &booted,
        &[(
            "DriveeSafeCall",
            Some(embedded("DriveeSafeCall", "com.example.drivee", "57.0.0")),
        )],
    );
    let super::open::Opening::Apps { apps, refused, .. } =
        super::open::url_to_open(&fx.paths, &config, &named, Want::App).unwrap()
    else {
        panic!("no apps to open");
    };
    assert_eq!(refused, Vec::<String>::new());
    assert_eq!(
        apps[0].1.url,
        "exp+driveesafecall://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A18081"
    );
    assert_eq!(apps[0].1.unknown, None);

    std::fs::remove_dir_all(&booted).unwrap();
    simulator_with(
        &booted,
        &[(
            "DriveeSafeCall",
            Some(embedded("DriveeSafeCall", "com.example.drivee", "55.0.0")),
        )],
    );
    let super::open::Opening::Apps { apps, refused, .. } =
        super::open::url_to_open(&fx.paths, &config, &named, Want::App).unwrap()
    else {
        panic!("no apps to open");
    };
    assert!(apps.is_empty(), "{apps:?}");
    assert_eq!(
        refused,
        [
            "mobile: the development build on iPhone 17 Pro is SDK 55 and this worktree needs SDK \
          57 — `npx expo run:ios --port 18081` in the app's directory builds its own"
        ]
    );
}

// xcrun that fails, prints nonsense or stalls is nothing known: no build,
// no line, and the stall costs no more than its deadline.
#[test]
fn a_failing_or_stalled_xcrun_is_nothing_known() {
    use std::os::unix::fs::PermissionsExt;
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let config: Config = toml::from_str(MOBILE_BY_VARIABLE).unwrap();
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    let bin = fx.paths.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for body in [
        "echo 'xcrun: error: unable to find utility \"simctl\"' >&2\nexit 72",
        "echo 'not json'",
        "exec sleep 30",
    ] {
        let script = bin.join("xcrun");
        std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = std::time::Instant::now();
        let simulators =
            actions::Simulators::within(&fx.paths, std::time::Duration::from_millis(300));
        assert!(
            actions::installed_builds(&config, &record, &simulators).is_empty(),
            "{body}"
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{body}"
        );
    }
}

// A branch that changes an app's native code needs a build of its own,
// or Metro's bundle loads into a build that lacks the module: `status`
// says which files, against which base, and the command that builds it.
// A change to the app's JavaScript alone is no native change.
#[test]
fn status_says_when_a_branch_changes_its_apps_native_code() {
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    let app = record.path.join("apps/mobile");
    let v: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    assert_eq!(
        v["worktrees"][0]["processes"]["mobile"]["app"]["native"],
        serde_json::Value::Null,
        "nothing changed yet"
    );

    for (file, text) in [
        ("ios/Podfile", "pod 'Call'\n"),
        ("app/index.tsx", "export {}\n"),
    ] {
        std::fs::create_dir_all(app.join(file).parent().unwrap()).unwrap();
        std::fs::write(app.join(file), text).unwrap();
    }
    git(&record.path, &["add", "-A"]);
    git(&record.path, &["commit", "-qm", "native"]);
    // And one not committed yet, in a local module.
    let module = app.join("modules/call/android");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(module.join("CallModule.kt"), "class CallModule\n").unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    let native = &v["worktrees"][0]["processes"]["mobile"]["app"]["native"];
    assert_eq!(native["base"], "main");
    assert_eq!(
        native["changed"],
        serde_json::json!([
            "apps/mobile/ios/Podfile",
            "apps/mobile/modules/call/android/CallModule.kt"
        ])
    );
    // `build` is the iOS one, as it was before there were two.
    assert_eq!(native["build"], "npx expo run:ios --port 18081");
    assert_eq!(
        native["builds"],
        serde_json::json!({
            "ios": "npx expo run:ios --port 18081",
            "android": "npx expo run:android --port 18081",
        })
    );
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    let line = text.lines().find(|line| line.contains("native")).unwrap();
    assert!(
        line.contains(
            "this branch changes native code against main (apps/mobile/ios/Podfile and 1 more)"
        ),
        "{text}"
    );
    assert!(
        line.contains(
            "`npx expo run:ios --port 18081` (ios) or `npx expo run:android --port 18081` \
             (android) in the app's directory"
        ),
        "{text}"
    );
    // Published: the contract names each key under `native`.
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    for key in native.as_object().unwrap().keys() {
        assert!(
            doc.contains(&format!("\"{key}\"")),
            "agent/json.md never documents app.native's {key}"
        );
    }
    assert!(
        !doc.contains("--no-bundler"),
        "Expo refuses it beside --port"
    );
}

// An Expo-only worktree has no page: `status` gives no URL, and `open`
// opens its app rather than hand a browser Metro's root. With nothing
// here to open it on (a unit test runs no command), it says why and
// gives each target's command, the ones `status` prints.
#[test]
fn open_opens_the_app_of_a_worktree_that_serves_no_page() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let config_text = "[processes.mobile]\ncmd = \"npm run start\"\n\
                       ports = { RCT_METRO_PORT = \"metro\" }\n";
    std::fs::write(fx.paths.config_file(), config_text).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    record.ports.insert("metro".to_string(), 18_081);
    record
        .roles
        .insert("mobile".to_string(), vec!["metro".to_string()]);
    record.pageless.insert("mobile".to_string());
    record
        .processes
        .insert("mobile".to_string(), listening(999_998, &[]));
    crate::state::save(&fx.paths.state_file(), &store).unwrap();

    let v: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    assert_eq!(v["worktrees"][0]["url"], serde_json::Value::Null);
    let config: Config = toml::from_str(config_text).unwrap();
    let named = super::names::target_named(&fx.paths, Some("feat/one"), "open").unwrap();
    let opening = super::open::url_to_open(&fx.paths, &config, &named, Want::Page).unwrap();
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    let links = actions::app_links(&config, &record)["mobile"].clone();
    assert_eq!(
        opening,
        super::open::Opening::Apps {
            apps: vec![("mobile".to_string(), links.clone())],
            said: Vec::new(),
            refused: Vec::new(),
        }
    );

    let said = std::cell::RefCell::new(Vec::new());
    let text = capture(|b| {
        super::open::open_apps(
            &fx.paths,
            &[("mobile".to_string(), links.clone())],
            b,
            &|line| said.borrow_mut().push(line.to_string()),
        )
    });
    assert_eq!(said.borrow()[0], "opening mobile's app in Expo Go");
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].starts_with(
            "mobile: no iOS simulator is booted and no Android device or emulator is connected"
        ),
        "{text}"
    );
    assert!(
        lines[0].ends_with("— once one is, this opens its app in Expo Go:"),
        "{text}"
    );
    assert_eq!(
        lines[1..],
        [
            format!("  on the booted iOS simulator: {}", links.simulator),
            format!(
                "  on the connected Android device or emulator: {}",
                links.android
            ),
        ]
    );

    // A process that says `page = false` and is no device's app has
    // nothing to open, and says so, as before; and an app that stopped
    // says how to start it.
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    record.pageless.insert("worker".to_string());
    record.processes.remove("mobile");
    record
        .processes
        .insert("worker".to_string(), listening(999_998, &[]));
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    assert_eq!(
        super::open::url_to_open(&fx.paths, &config, &named, Want::Page).unwrap(),
        super::open::Opening::Apps {
            apps: Vec::new(),
            said: vec![
                "feat/one serves no page to open in a browser".to_string(),
                "mobile: its app is not running — `pando start feat/one --only mobile` starts it"
                    .to_string(),
                "worker: its settings say `page = false`".to_string(),
            ],
            refused: Vec::new(),
        }
    );
}

// `--app` opens a worktree's app beside its page: a backend and a mobile
// app, where plain `open` opens the backend's page. A worktree with no
// app a device runs is refused, with the command that opens its page.
#[test]
fn open_app_opens_the_app_beside_a_page() {
    let fx = fixture();
    let name = with_mobile_app(&fx, MOBILE_BY_VARIABLE);
    let config: Config = toml::from_str(MOBILE_BY_VARIABLE).unwrap();
    let named = super::names::target_named(&fx.paths, Some("feat/one"), "open").unwrap();
    assert_eq!(
        super::open::url_to_open(&fx.paths, &config, &named, Want::Page).unwrap(),
        super::open::Opening::Url("http://localhost:17342".into())
    );
    let record = crate::state::load(&fx.paths.state_file())
        .unwrap()
        .worktrees[&name]
        .clone();
    assert_eq!(
        super::open::url_to_open(&fx.paths, &config, &named, Want::App).unwrap(),
        super::open::Opening::Apps {
            apps: vec![(
                "mobile".to_string(),
                actions::app_links(&config, &record)["mobile"].clone()
            )],
            said: Vec::new(),
            refused: Vec::new(),
        }
    );

    let web_only: Config =
        toml::from_str("[processes.api]\ncmd = \"npm run dev\"\nports = { PORT = \"web\" }\n")
            .unwrap();
    let err = super::open::url_to_open(&fx.paths, &web_only, &named, Want::App).unwrap_err();
    assert_eq!(
        format!("{err:#}"),
        "feat/one runs no app a simulator or a device opens — `pando open feat/one` opens its \
         page"
    );
}

// The flag is clap's, so completions offer it, and `--public` beside it
// is refused rather than one of them silently winning.
#[test]
fn open_takes_app_and_completions_offer_it() {
    let open = Cli::command()
        .get_subcommands()
        .find(|sub| sub.get_name() == "open")
        .unwrap()
        .clone();
    assert!(open.get_arguments().any(|a| a.get_long() == Some("app")));
    for shell in [
        clap_complete::Shell::Zsh,
        clap_complete::Shell::Bash,
        clap_complete::Shell::Fish,
    ] {
        let script = completion_script(shell);
        let flag = match shell {
            clap_complete::Shell::Fish => "-l app",
            _ => "--app",
        };
        assert!(script.contains(flag), "{shell:?}");
    }
    let err = Cli::try_parse_from(["pando", "open", "feat/one", "--app", "--public"]).unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
}

// A command that runs Expo's server by name is known the same way, and
// its link takes the port of the one role the process owns.
#[test]
fn status_knows_an_expo_app_by_its_command_too() {
    let fx = fixture();
    with_mobile_app(
        &fx,
        "[processes.api]\ncmd = \"npm run dev\"\nports = { PORT = \"web\" }\n\n\
         [processes.mobile]\ncmd = \"npx expo start --port {port:mobile}\"\n\
         ports = [\"mobile\"]\n",
    );
    let v: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    assert_eq!(
        v["worktrees"][0]["processes"]["mobile"]["app"]["url"],
        "exp://127.0.0.1:18081"
    );
}

// With no process a device runs, there is no link, in either shape; and
// none while the bundler is not running, since nothing would answer it.
#[test]
fn status_gives_no_app_link_for_a_browser_app_or_a_stopped_bundler() {
    let fx = fixture();
    let name = with_mobile_app(
        &fx,
        "[processes.api]\ncmd = \"npm run dev\"\nports = { PORT = \"web\" }\n\n\
         [processes.mobile]\ncmd = \"npm run dev\"\nports = { PORT = \"mobile\" }\n",
    );
    let json = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    for process in ["api", "mobile"] {
        assert_eq!(
            v["worktrees"][0]["processes"][process]["app"],
            serde_json::Value::Null
        );
    }
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    assert!(!text.contains("simctl"), "{text}");

    std::fs::write(fx.paths.config_file(), MOBILE_BY_VARIABLE).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(&name).unwrap();
    record.processes.get_mut("mobile").unwrap().phase = Phase::Failed {
        at: Utc::now(),
        reason: "process exited".to_string(),
    };
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    assert!(!text.contains("simctl"), "{text}");
}

// `mode` came after `isolated`, which programs already read: the new
// word is added beside it, and the old flag stays true for isolated alone.
#[test]
fn status_and_ls_json_publish_the_mode_beside_the_old_isolated_flag() {
    use crate::state::ServiceMode;
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    for mode in ServiceMode::ALL {
        let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
        store.worktrees.get_mut(&name).unwrap().mode = Some(mode);
        crate::state::save(&fx.paths.state_file(), &store).unwrap();

        let text = capture(|b| status_json(&fx.paths, None, b));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let wt = &v["worktrees"][0];
        assert_eq!(wt["mode"], mode.word(), "{text}");
        assert_eq!(wt["isolated"], mode == ServiceMode::Isolated, "{text}");

        let text = capture(|b| ls_json(&fx.paths, b));
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["worktrees"][1]["mode"], mode.word(), "{text}");
    }
}

#[test]
fn status_can_be_asked_about_one_worktree() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    actions::new(&fx.paths, &fx.config, "feat/two", None, &|_| {}).unwrap();
    let text = capture(|b| status_json(&fx.paths, Some("feat+two"), b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["worktrees"].as_array().unwrap().len(), 1);
    assert_eq!(v["worktrees"][0]["name"], "feat+two");

    let err = status_text(&fx.paths, Some("nope"), &mut Vec::new()).unwrap_err();
    assert!(format!("{err:#}").contains("no worktree named"), "{err:#}");
}

// A worktree removed with `git worktree remove --force` while its dev
// server ran still resolves, from its record, so `stop` can clean up.
// `status` of it said "no worktree named" and exited 0, and `--json`
// printed an empty list, while its processes still held their ports.
#[test]
fn status_of_a_worktree_git_no_longer_lists_fails_and_names_what_still_runs() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_share(&fx, &name, None);
    let dir = fx.config.worktrees_dir(&fx.paths).join(&name);
    git(
        &fx.root,
        &["worktree", "remove", "--force", dir.to_str().unwrap()],
    );
    assert_eq!(super::names::resolve(&fx.paths, "feat+one").unwrap(), name);

    let err = status_text(&fx.paths, Some(&name), &mut Vec::new()).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("git no longer lists feat+one"), "{msg}");
    assert!(
        msg.contains("runs dev") && msg.contains("`pando stop feat+one`"),
        "{msg}"
    );
    let err = status_json(&fx.paths, Some(&name), &mut Vec::new()).unwrap_err();
    assert_eq!(format!("{err:#}"), msg);
}

#[test]
fn status_text_names_what_each_worktree_is_doing() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| status_text(&fx.paths, None, b));
    assert!(text.contains("feat/one"), "{text}");
    assert!(text.contains("stopped"), "{text}");
}

/// A worktree running `web` and `api`, recorded as a refresh would
/// leave it. `pid` is this test process, which really is alive, so the
/// read path does not turn the phase into a failure underneath.
fn with_two_processes(fx: &Fx, name: &str, api_phase: Phase) {
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(name).unwrap();
    record.ports.insert("web".to_string(), 17_342);
    record.ports.insert("api".to_string(), 17_343);
    record.observed_ports = vec![17_342, 17_343];
    record.processes.insert(
        "web".to_string(),
        crate::state::ProcessRecord {
            pid: std::process::id(),
            pgid: Group::from_raw(999_998),
            started_at: Utc::now(),
            log_path: fx.paths.log_file(name, "web"),
            ready_port: Some(17_342),
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: Phase::Running { since: Utc::now() },
        },
    );
    record.processes.insert(
        "api".to_string(),
        crate::state::ProcessRecord {
            pid: std::process::id(),
            pgid: Group::from_raw(999_997),
            started_at: Utc::now(),
            log_path: fx.paths.log_file(name, "api"),
            ready_port: Some(17_343),
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: api_phase,
        },
    );
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
}

// `the_url_follows_a_listener_only_when_no_other_role_owns_that_port`
// lived here. It asserted that a port no role claims becomes the
// worktree's URL, which is the bug finding 4 reproduces: that port
// belongs to whichever group opened it.
// `the_url_follows_a_listener_only_in_the_group_that_owns_the_role`
// above is the rule it should have pinned.

#[test]
fn status_text_lists_every_process_under_its_worktree() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_two_processes(&fx, &name, Phase::Running { since: Utc::now() });

    let text = capture(|b| status_text_at(&fx.paths, None, b, 200));
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        3,
        "one worktree line and two process lines:\n{text}"
    );
    assert!(lines[0].starts_with("feat/one"), "{text}");
    assert!(lines[0].contains("running"), "{text}");
    assert!(
        lines[0].contains("api 17343") && lines[0].contains("web 17342"),
        "the worktree line carries every role: {text}"
    );
    assert!(
        lines[0].contains("http://localhost:17342"),
        "and one URL, the web role's: {text}"
    );
    // Indented, in config order, each with its own pid.
    assert!(lines[1].starts_with("  api"), "{text}");
    assert!(lines[2].starts_with("  web"), "{text}");
    for line in &lines[1..] {
        assert!(
            line.contains(&format!("pid {}", std::process::id())),
            "{text}"
        );
        assert!(line.contains("running"), "{text}");
    }
}

// Phase 2b review, finding 9. `ls` sheds columns and the TUI detail
// pane truncates; `status` had no width parameter at all, and the
// per-process rows 2b added grow the block it prints. In the tmux split
// the TUI is designed for, it wrapped.
#[test]
fn status_text_sheds_the_url_and_then_the_ports_as_the_terminal_narrows() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_two_processes(&fx, &name, Phase::Running { since: Utc::now() });

    let wide = capture(|b| status_text_at(&fx.paths, None, b, 200));
    assert!(wide.contains("http://localhost:17342"), "{wide}");
    assert!(
        wide.contains("api 17343") && wide.contains("web 17342"),
        "{wide}"
    );

    for width in [60, 44, 32, 24, 12] {
        let text = capture(|b| status_text_at(&fx.paths, None, b, width));
        for line in text.lines() {
            assert!(
                line.chars().count() <= width,
                "{line:?} is wider than {width} columns:\n{text}"
            );
        }
        assert!(
            text.contains("feat/one"),
            "the name is the identifier and never goes: {text}"
        );
    }

    // The URL is the longest cell and `--json` still carries it, so it
    // is the first thing to go; the ports cell is truncated after that.
    let narrow = capture(|b| status_text_at(&fx.paths, None, b, 44));
    assert!(!narrow.contains("http://"), "{narrow}");
    assert!(narrow.contains("running"), "{narrow}");
}

// `status` measured in characters, as `ls` once did: a name with wide
// characters pushed its phase word right of its neighbours', and a
// failure reason in Japanese, cut to the width in characters, still ran
// past the terminal and wrapped.
#[test]
fn status_text_lines_up_and_fits_wide_characters_by_the_columns_they_take() {
    let fx = fixture();
    let wide = actions::new(&fx.paths, &fx.config, "feat/日本語ログイン", None, &|_| {}).unwrap();
    actions::new(&fx.paths, &fx.config, "feat/api", None, &|_| {}).unwrap();
    with_two_processes(
        &fx,
        &wide,
        Phase::Failed {
            at: Utc::now(),
            reason: "ポートはすでに別のプロセスが使っています".repeat(4),
        },
    );

    let text = capture(|b| status_text_at(&fx.paths, None, b, 80));
    let phase_columns: Vec<usize> = text
        .lines()
        .filter(|line| line.starts_with("feat/"))
        .map(|line| {
            let at = line.find("failed").or(line.find("stopped")).unwrap();
            crate::term::text_width(&line[..at])
        })
        .collect();
    assert_eq!(phase_columns.len(), 2, "{text}");
    assert_eq!(phase_columns[0], phase_columns[1], "{text}");
    for line in text.lines() {
        assert!(
            crate::term::text_width(line) <= 80,
            "{line:?} is wider than 80 columns:\n{text}"
        );
    }
}

// A name as wide as the terminal was printed whole, leaving the rest of
// its line no room at all, so every line of it wrapped.
#[test]
fn status_text_cuts_a_name_too_long_for_the_terminal() {
    let fx = fixture();
    let branch = format!("feat/{}", "long-".repeat(20));
    actions::new(&fx.paths, &fx.config, &branch, None, &|_| {}).unwrap();

    let text = capture(|b| status_text_at(&fx.paths, None, b, 80));
    let line = text.lines().next().unwrap();
    assert!(crate::term::text_width(line) <= 80, "{line:?}");
    assert!(line.contains("stopped"), "{line:?}");
}

#[test]
fn status_text_says_which_process_failed() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_two_processes(
        &fx,
        &name,
        Phase::Failed {
            at: Utc::now(),
            reason: "process exited".to_string(),
        },
    );

    let text = capture(|b| status_text_at(&fx.paths, None, b, 200));
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines[0].contains("failed") && lines[0].contains("api: process exited"),
        "a worktree with a dead api is failed, and says which: {text}"
    );
    assert!(
        lines[1].contains("api") && lines[1].contains("failed"),
        "{text}"
    );
    assert!(
        lines[2].contains("web") && lines[2].contains("running"),
        "the process that is still up says so: {text}"
    );
}

#[test]
fn the_listing_shows_the_aggregate_not_the_first_process() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    // `api` sorts first and is running; `web` is the failed one, so a
    // row that showed the first process would read "running".
    with_two_processes(&fx, &name, Phase::Running { since: Utc::now() });
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store
        .worktrees
        .get_mut(&name)
        .unwrap()
        .processes
        .get_mut("web")
        .unwrap()
        .phase = Phase::Failed {
        at: Utc::now(),
        reason: "process exited".to_string(),
    };
    crate::state::save(&fx.paths.state_file(), &store).unwrap();

    let text = capture(|b| ls_text_at(&fx.paths, b, 200));
    assert!(
        text.contains("failed"),
        "the row is the worst of its processes: {text}"
    );
}

#[test]
fn logs_read_the_source_they_are_asked_for() {
    let fx = fixture();
    write_log(&fx, "feat+one", "web", "web line\n");
    write_log(&fx, "feat+one", "api", "api line\n");
    let text = capture(|b| logs(&fx.paths, "feat+one", "api", 5, false, false, b, &quiet));
    assert_eq!(text, "api line\n");

    let mut out = Vec::new();
    let err = logs(
        &fx.paths, "feat+one", "worker", 5, false, false, &mut out, &quiet,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("worker"), "{msg}");
    assert!(
        msg.contains("api") && msg.contains("web"),
        "an unknown source lists the ones there are: {msg}"
    );
}

#[test]
fn uptime_reads_in_the_unit_that_fits() {
    use chrono::TimeDelta;
    assert_eq!(human_duration(TimeDelta::seconds(9)), "9s");
    assert_eq!(human_duration(TimeDelta::seconds(70)), "1m10s");
    assert_eq!(human_duration(TimeDelta::seconds(3_700)), "1h1m");
    assert_eq!(human_duration(TimeDelta::seconds(90_000)), "1d1h");
    assert_eq!(human_duration(TimeDelta::seconds(-5)), "0s");
}

// ---- logs ------------------------------------------------------------

fn write_log(fx: &Fx, name: &str, source: &str, text: &str) {
    let path = fx.paths.log_file(name, source);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

// The read side of finding 1: `--source` is the other half of the same
// path component, so a traversal there reads a file outside the
// worktree's log directory — one `available_sources` never lists, so
// nothing even suggests it is reachable.
#[test]
fn a_log_source_that_escapes_the_log_directory_is_refused() {
    let fx = fixture();
    write_log(&fx, "feat+one", "web", "web line\n");
    // A real file the traversal would reach, so the refusal is about
    // the name rather than about the file not being there.
    std::fs::create_dir_all(fx.paths.project_dir()).unwrap();
    std::fs::write(fx.paths.project_dir().join("outside.log"), "secret\n").unwrap();

    let mut out = Vec::new();
    let err = logs(
        &fx.paths,
        "feat+one",
        "../../outside",
        5,
        false,
        false,
        &mut out,
        &quiet,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("\"../../outside\""), "{msg}");
    assert!(msg.contains("logs/<worktree>"), "{msg}");
    assert!(
        out.is_empty(),
        "nothing outside the log directory may be printed: {}",
        String::from_utf8_lossy(&out)
    );

    // And the hook logs pando writes itself are still readable by name.
    write_log(&fx, "feat+one", "install", "install line\n");
    let text = capture(|b| logs(&fx.paths, "feat+one", "install", 5, false, false, b, &quiet));
    assert_eq!(text, "install line\n");
}

#[test]
fn logs_prints_the_last_lines() {
    let fx = fixture();
    write_log(&fx, "feat+one", "dev", "one\ntwo\nthree\nfour\n");
    let text = capture(|b| logs(&fx.paths, "feat+one", "dev", 2, false, false, b, &quiet));
    assert_eq!(text, "three\nfour\n");
}

/// The third dead end of the first contact run, and the worst of them:
/// `doctor` said the process failed and pointed at `pando logs`; that
/// printed nothing and exited 0. Following pando's own advice led to
/// silence, with no way to tell an empty log from a wrong worktree
/// name, a wrong `--source`, or a broken command.
#[test]
fn an_empty_log_says_that_it_is_empty() {
    let fx = fixture();
    write_log(&fx, "feat+one", "dev", "");
    let (text, notes) =
        capture_both(|b, n| logs(&fx.paths, "feat+one", "dev", 10, false, false, b, n));
    assert_eq!(text, "", "stdout is still only the log");
    assert!(
        notes.iter().any(|n| n.contains("is empty")),
        "an empty log is a fact pando knows: {notes:?}"
    );
    assert!(
        notes
            .iter()
            .any(|n| n.contains("dev") && n.contains("feat+one")),
        "and it names what was read: {notes:?}"
    );
}

/// `-n 0` asks for no lines, and gets none — without being told that a
/// log full of them is empty. And a `-n` far past the file is the whole
/// file, not a buffer sized for it up front: `usize::MAX` panicked with
/// "capacity overflow".
#[test]
fn a_tail_of_zero_or_of_everything_reads_the_log_as_it_is() {
    let fx = fixture();
    write_log(&fx, "feat+one", "dev", "one\ntwo\n");
    let (text, notes) =
        capture_both(|b, n| logs(&fx.paths, "feat+one", "dev", 0, false, false, b, n));
    assert_eq!(text, "");
    assert!(notes.is_empty(), "the log is not empty: {notes:?}");

    let (text, notes) =
        capture_both(|b, n| logs(&fx.paths, "feat+one", "dev", usize::MAX, false, false, b, n));
    assert_eq!(text, "one\ntwo\n");
    assert!(notes.is_empty(), "{notes:?}");
}

/// A process killed mid-line leaves its last words without a newline.
/// The failure classifier reads them — `snapshot` flushes the pending
/// line — and `pando logs` used to withhold them and print nothing, so
/// pando knew more about the crash than the developer could see.
#[test]
fn a_last_line_with_no_newline_is_still_printed() {
    let fx = fixture();
    write_log(&fx, "feat+one", "dev", "done\nSegmentation fault");
    let (text, notes) =
        capture_both(|b, n| logs(&fx.paths, "feat+one", "dev", 10, false, false, b, n));
    assert_eq!(text, "done\nSegmentation fault\n");
    assert!(
        notes.is_empty(),
        "there was something to print, so nothing to explain: {notes:?}"
    );
}

/// Which leaves one state a one-shot read cannot reach and a follower
/// can: `-f` on a file holding an unterminated first line, where the
/// rest of it really is still coming.
#[test]
fn a_log_with_no_complete_line_is_described_by_its_size() {
    let fx = fixture();
    write_log(&fx, "feat+one", "dev", "half a line with no newline");
    let notes = silence_notes(
        &fx.paths,
        "feat+one",
        "dev",
        &fx.paths.log_file("feat+one", "dev"),
    );
    assert!(
        notes.iter().any(|n| n.contains("27 bytes")),
        "the size is the whole difference from an empty file: {notes:?}"
    );
    assert!(
        !notes.iter().any(|n| n.contains("is empty")),
        "and it is not empty: {notes:?}"
    );
}

/// The sentence the developer came for: why the log is empty. The
/// record already knows, because `explain_failure` wrote it there.
#[test]
fn an_empty_log_carries_the_reason_the_record_knows() {
    let fx = fixture();
    write_log(&fx, "feat+one", "dev", "");
    let mut store = crate::state::State::new();
    let mut record = WorktreeRecord::new(fx.paths.worktree_path("feat+one"), true);
    record.processes.insert(
        "dev".to_string(),
        ProcessRecord {
            pid: 1,
            pgid: Group::from_raw(1),
            started_at: Utc::now(),
            log_path: fx.paths.log_file("feat+one", "dev"),
            ready_port: None,
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase: Phase::Failed {
                at: Utc::now(),
                reason: "process exited with status 0 — it printed nothing at all".to_string(),
            },
        },
    );
    store.worktrees.insert("feat+one".to_string(), record);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();

    let (_, notes) =
        capture_both(|b, n| logs(&fx.paths, "feat+one", "dev", 10, false, false, b, n));
    assert!(
        notes.iter().any(|n| n.contains("status 0")),
        "the reason is already written down; this is where it is wanted: {notes:?}"
    );
}

/// `--json` is a stream of objects, one per line. A note about the log
/// is not one of them, so it goes to the other channel and stdout
/// stays parseable — empty is a valid answer there.
#[test]
fn an_empty_log_in_json_keeps_stdout_clean() {
    let fx = fixture();
    write_log(&fx, "feat+one", "dev", "");
    let (text, notes) =
        capture_both(|b, n| logs(&fx.paths, "feat+one", "dev", 10, false, true, b, n));
    assert_eq!(text, "", "nothing that is not a log line may be on stdout");
    assert!(
        !notes.is_empty(),
        "and the developer is still told: {notes:?}"
    );
}

#[test]
fn logs_json_emits_one_object_per_line() {
    let fx = fixture();
    write_log(
        &fx,
        "feat+one",
        "dev",
        "2026-09-20T10:00:00Z ready in 412ms\nError: it broke\n",
    );
    let text = capture(|b| logs(&fx.paths, "feat+one", "dev", 10, false, true, b, &quiet));
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["ts"], "2026-09-20T10:00:00+00:00");
    assert_eq!(lines[0]["level"], "info");
    assert!(lines[0]["line"].as_str().unwrap().contains("ready in"));
    assert_eq!(lines[1]["ts"], serde_json::Value::Null);
    assert_eq!(lines[1]["level"], "error");
}

#[test]
fn a_line_with_no_timestamp_pando_can_read_gets_null() {
    assert_eq!(
        leading_timestamp("2026-09-20T10:00:00Z ready"),
        Some("2026-09-20T10:00:00+00:00".to_string())
    );
    assert_eq!(
        leading_timestamp("[2026-09-20T10:00:00+02:00] ready"),
        Some("2026-09-20T08:00:00+00:00".to_string())
    );
    assert_eq!(leading_timestamp("ready in 412ms"), None);
    assert_eq!(leading_timestamp(""), None);
    assert_eq!(leading_timestamp("20/09/2026 10:00:00 ready"), None);
}

#[test]
fn logs_names_the_sources_a_worktree_has() {
    let fx = fixture();
    let err = logs(
        &fx.paths,
        "feat+one",
        "dev",
        10,
        false,
        false,
        &mut Vec::new(),
        &quiet,
    )
    .unwrap_err();
    assert!(
        format!("{err:#}").contains("no logs for feat+one"),
        "{err:#}"
    );

    write_log(&fx, "feat+one", "install", "installing\n");
    let err = logs(
        &fx.paths,
        "feat+one",
        "dev",
        10,
        false,
        false,
        &mut Vec::new(),
        &quiet,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("install"),
        "it says what is there instead: {msg}"
    );
}

#[test]
fn ls_text_says_so_when_there_are_no_worktrees() {
    let fx = fixture();
    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    assert!(text.contains("no worktrees"), "{text}");
}

// The lead's first read of the old table: a PATH column that wrapped every
// row, and a STATE column that said `pando`. The branch is the name a
// person knows, said once; the path is one flag away.
#[test]
fn ls_text_names_by_branch_says_what_runs_and_leaves_the_path_to_long() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    let header: Vec<&str> = text.lines().next().unwrap().split_whitespace().collect();
    assert_eq!(header, ["NAME", "STATUS", "URL", "PORTS", "GIT"], "{text}");
    assert!(text.contains("feat/one"), "{text}");
    assert!(
        !text.contains("feat+one"),
        "the directory spelling is not repeated: {text}"
    );
    assert!(text.contains("stopped"), "{text}");
    assert!(text.contains("clean"), "{text}");
    assert!(
        !text.contains(" pando"),
        "no word only pando understands: {text}"
    );
    let path = fx.paths.worktrees_dir().join("feat+one");
    assert!(!text.contains(&path.display().to_string()), "{text}");

    let long = LsView {
        long: true,
        ..LsView::plain(usize::MAX)
    };
    let text = capture(|b| ls_text_with(&fx.paths, b, &long));
    assert!(text.contains("HEAD") && text.contains("PATH"), "{text}");
    let canonical = std::fs::canonicalize(&path).unwrap();
    assert!(text.contains(&canonical.display().to_string()), "{text}");
}

#[test]
fn a_long_listing_writes_home_as_a_tilde() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let path = std::fs::canonicalize(fx.paths.worktrees_dir().join("feat+one")).unwrap();
    let home = path.parent().unwrap().to_path_buf();
    let view = LsView {
        long: true,
        style: crate::term::Style::with(false, Some(home)),
        ..LsView::plain(usize::MAX)
    };
    let text = capture(|b| ls_text_with(&fx.paths, b, &view));
    assert!(text.contains("~/feat+one"), "{text}");
}

// A worktree whose directory is not its branch — adopted, or detached —
// keeps its directory name, and gets a BRANCH column to say what it has
// checked out.
#[test]
fn a_worktree_not_named_for_its_branch_brings_the_branch_column() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let elsewhere = fx.root.parent().unwrap().join("scratch");
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "topic/x",
            elsewhere.to_str().unwrap(),
        ],
    );
    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    assert!(text.lines().next().unwrap().contains("BRANCH"), "{text}");
    let row = text.lines().find(|l| l.starts_with("scratch")).unwrap();
    assert!(row.contains("topic/x"), "{text}");
    assert!(row.contains("adopted"), "{text}");
}

// Two long branch names that differ late must not print as the same
// truncated name.
#[test]
fn long_names_that_share_a_prefix_stay_distinct_on_a_narrow_terminal() {
    let fx = fixture();
    for n in 1..=2 {
        let branch = format!("feature/very-long-branch-name-number-{n}-with-extra-words");
        actions::new(&fx.paths, &fx.config, &branch, None, &|_| {}).unwrap();
    }
    let wide = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    assert!(
        wide.contains("feature/very-long-branch-name-number-1-with-extra-words"),
        "never cut when not a terminal: {wide}"
    );
    let narrow = capture(|b| ls_text_at(&fx.paths, b, 50));
    // The main checkout first, then the two.
    let names: Vec<&str> = narrow
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert_eq!(names.len(), 3, "{narrow}");
    assert_eq!(names[0], "main", "{narrow}");
    assert_ne!(names[1], names[2], "{narrow}");
    for line in narrow.lines() {
        assert!(line.chars().count() <= 50, "{line:?} in\n{narrow}");
    }
}

#[test]
fn colour_is_only_there_when_asked_for_and_never_skews_a_column() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let plain = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    assert!(!plain.contains('\x1b'), "{plain:?}");
    let view = LsView {
        style: crate::term::Style::with(true, None),
        ..LsView::plain(usize::MAX)
    };
    let painted = capture(|b| ls_text_with(&fx.paths, b, &view));
    assert!(painted.contains('\x1b'), "{painted:?}");
    let stripped: Vec<usize> = painted.lines().map(crate::term::visible_width).collect();
    let widths: Vec<usize> = plain.lines().map(|l| l.chars().count()).collect();
    assert_eq!(stripped, widths, "{painted:?}");
}

#[test]
fn the_listing_shows_mode_and_public_only_when_some_worktree_has_them() {
    let fx = fixture();
    let one = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    actions::new(&fx.paths, &fx.config, "feat/two", None, &|_| {}).unwrap();
    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    assert!(!text.contains("MODE") && !text.contains("PUBLIC"), "{text}");

    with_share(&fx, &one, None);
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store.worktrees.get_mut(&one).unwrap().mode = Some(crate::state::ServiceMode::Isolated);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    assert!(text.contains("MODE") && text.contains("isolated"), "{text}");
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store.worktrees.get_mut(&one).unwrap().mode = Some(crate::state::ServiceMode::Namespaced);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    assert!(
        text.contains("MODE") && text.contains("namespaced"),
        "{text}"
    );
    assert!(
        text.contains("https://fake-host.trycloudflare.com"),
        "{text}"
    );
    assert!(
        text.contains("http://localhost:17342"),
        "a running worktree has its URL: {text}"
    );
    // Narrow: the public URL shortens to `yes` before anything else of
    // weight goes.
    let narrow = capture(|b| ls_text_at(&fx.paths, b, 60));
    assert!(narrow.contains("PUBLIC"), "{narrow}");
    assert!(!narrow.contains("trycloudflare"), "{narrow}");
    assert!(narrow.contains("yes"), "{narrow}");
}

#[test]
fn ls_text_marks_adopted_uncommitted_and_prunable_worktrees() {
    let fx = fixture();
    let adopted = fx.root.parent().unwrap().join("adopted");
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "adopted",
            adopted.to_str().unwrap(),
        ],
    );
    let dirty = actions::new(&fx.paths, &fx.config, "feat/dirty", None, &|_| {}).unwrap();
    std::fs::write(
        fx.paths.worktrees_dir().join(&dirty).join("scratch.txt"),
        "wip",
    )
    .unwrap();
    let gone = actions::new(&fx.paths, &fx.config, "feat/gone", None, &|_| {}).unwrap();
    std::fs::remove_dir_all(fx.paths.worktrees_dir().join(&gone)).unwrap();

    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    for word in ["adopted", "uncommitted", "prunable"] {
        assert!(text.contains(word), "missing {word:?} in:\n{text}");
    }
}

#[test]
fn ls_json_emits_the_documented_shape() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| ls_json(&fx.paths, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();

    // Pinned as a literal on purpose: a bump must fail here, at the
    // commit that makes it, rather than passing quietly.
    assert_eq!(v["version"], 2);
    assert_eq!(v["project"]["id"], fx.paths.project.id.as_str());
    assert_eq!(v["project"]["name"], "acme-shop");
    assert_eq!(v["project"]["root"], fx.root.display().to_string().as_str());

    // The main checkout first, marked, and never pando's.
    let main = &v["worktrees"][0];
    assert_eq!(main["name"], "acme-shop");
    assert_eq!(main["main"], true);
    assert_eq!(main["branch"], "main");
    assert_eq!(main["created_by_pando"], false);
    assert_eq!(main["mode"], "shared");
    assert_eq!(main["path"], fx.root.display().to_string().as_str());

    let w = &v["worktrees"][1];
    assert_eq!(w["name"], "feat+one");
    assert_eq!(w["main"], false);
    assert_eq!(w["branch"], "feat/one");
    assert_eq!(w["detached"], false);
    assert_eq!(w["dirty"], false);
    assert_eq!(w["ahead"], 0);
    assert_eq!(w["behind"], 0);
    assert_eq!(w["created_by_pando"], true);
    assert_eq!(w["mode"], "shared");
    assert_eq!(w["prunable"], false);
    assert_eq!(w["locked"], serde_json::Value::Null);
    assert_eq!(w["pr"], serde_json::Value::Null);
    assert!(w["head"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(w["path"].as_str().is_some_and(|s| s.starts_with('/')));
}

#[test]
fn ls_json_lists_the_main_checkout_alone_rather_than_an_error_with_no_worktrees() {
    let fx = fixture();
    let text = capture(|b| ls_json(&fx.paths, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let listed = v["worktrees"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{text}");
    assert_eq!(listed[0]["main"], true, "{text}");
}

#[test]
fn ls_json_reports_a_locked_worktree_with_its_reason() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    git(
        &fx.root,
        &[
            "worktree",
            "lock",
            "--reason",
            "benchmarking",
            fx.paths.worktrees_dir().join(&name).to_str().unwrap(),
        ],
    );
    let text = capture(|b| ls_json(&fx.paths, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["worktrees"][1]["locked"], "benchmarking");
}

// The CLI never spawns `gh`; chips come from whatever the TUI last saw.
#[test]
fn ls_json_fills_the_pr_field_from_the_cache() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let mut prs = cache::PrCacheFile::new();
    prs.prs.insert(
        "feat/one".into(),
        crate::worktree::PrInfo {
            number: 42,
            title: "feat: one".into(),
            branch: "feat/one".into(),
            author: "dev".into(),
            draft: false,
            state: PrState::Open,
            url: "https://example.test/pull/42".into(),
            cross_repository: false,
            base: "main".into(),
        },
    );
    cache::save_prs(&fx.paths.pr_cache_file(), &prs).unwrap();

    let text = capture(|b| ls_json(&fx.paths, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["worktrees"][1]["pr"]["number"], 42);
    assert_eq!(v["worktrees"][1]["pr"]["state"], "open");
    assert_eq!(
        v["worktrees"][1]["pr"]["url"],
        "https://example.test/pull/42"
    );
}

// Only stdout's own broken pipe is a reader that stopped. One to a
// process pando runs is a failure, and still has to say so.
#[test]
fn only_a_broken_stdout_ends_quietly() {
    use std::io::{Error, ErrorKind};
    let closed = anyhow::Error::from(super::stdout_error(Error::from(ErrorKind::BrokenPipe)))
        .context("print a line");
    assert!(stdout_closed(&closed));
    let child = anyhow::Error::from(Error::from(ErrorKind::BrokenPipe)).context("feed a client");
    assert!(!stdout_closed(&child));
    let other = anyhow::Error::from(super::stdout_error(Error::from(ErrorKind::Other)));
    assert!(!stdout_closed(&other));
}

// `head` is documented as "abc1234". Porcelain's sha is all forty, and
// enrichment's is git's own abbreviation, which is longer in a large
// repository or under `core.abbrev`: one listing used to publish both
// shapes in the same field.
#[test]
fn the_json_head_is_always_the_short_sha() {
    let mut w = crate::tui::app::tests::wt("feat+one");
    w.head = Some("0123456789012345678901234567890123456789".into());
    w.head_sha = None;
    assert_eq!(short_head(&w).as_deref(), Some("0123456"));

    w.head_sha = Some("0123456789ab".into());
    assert_eq!(short_head(&w).as_deref(), Some("0123456"));

    w.head = None;
    assert_eq!(short_head(&w).as_deref(), Some("0123456"));

    w.head_sha = None;
    assert_eq!(short_head(&w), None);
}

// ---- an answers file -------------------------------------------------

// The names in the file are the names `signals` publishes, because
// they are the same function of the same type.
#[test]
fn every_question_has_one_name_that_round_trips() {
    for slot in actions::ALL_SLOTS {
        let name = slot_name(slot);
        assert!(!name.is_empty(), "{slot:?} has no name");
        assert_eq!(slot_named(&name), Some(slot), "{name} does not round trip");
    }
    assert_eq!(slot_names().len(), actions::ALL_SLOTS.len());
}

/// The twelve names, written out.
///
/// `signals` publishes them and `--answers` takes them, and both get
/// them from `Slot`'s own serde names — so a rename stays invisible to
/// every test that only compares the two against each other, while
/// breaking every program ever written against them. This is the
/// assertion a rename has to walk past, and the list is also published
/// in `agent/json.md`, which the test below holds to the same order.
#[test]
fn the_twelve_question_names_are_frozen() {
    assert_eq!(
        slot_names(),
        [
            "install",
            "version_files",
            "prelude",
            "processes",
            "dev_cmd",
            "port_env",
            "services",
            "schema_hook",
            "provision",
            "clone",
            "base",
            "namespaced",
        ]
    );
}

/// Every `pando …` an agent-facing document tells a reader to run,
/// from its fenced blocks and its inline code spans.
fn commands_named_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            let line = line.trim().split('#').next().unwrap_or("").trim();
            if let Some(rest) = line.strip_prefix("pando ") {
                out.push(rest.trim().to_string());
            }
            continue;
        }
        // Inline: `pando doctor --json` in the middle of a sentence.
        for span in line.split('`').skip(1).step_by(2) {
            if let Some(rest) = span.strip_prefix("pando ") {
                out.push(rest.trim().to_string());
            }
        }
    }
    out
}

/// Holds a document's commands to what the binary really takes.
///
/// Instructions for a language model are the one kind of code that
/// fails silently and plausibly: a flag renamed in `cli.rs` leaves a
/// document that still reads perfectly and no longer works. clap is
/// asked rather than a list kept beside it, so there is nothing to
/// keep in step.
fn assert_every_documented_command_is_real(file: &str) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_every_command_is_real(file, &text);
}

/// [`assert_every_documented_command_is_real`], of text that is not a
/// file: `file` is only what a failure calls it.
fn assert_every_command_is_real(file: &str, text: &str) {
    use clap::CommandFactory;
    let cli = Cli::command();
    let mut checked = 0;
    for command in commands_named_in(text) {
        let mut tokens = command.split_whitespace();
        let Some(verb) = tokens.next() else { continue };
        // A bare `pando` with a flag of its own, like --version.
        if verb.starts_with('-') {
            continue;
        }
        let sub = cli
            .get_subcommands()
            .find(|c| c.get_name() == verb)
            .unwrap_or_else(|| {
                panic!("{file} says `pando {command}`, and pando has no {verb:?} command")
            });
        for token in tokens {
            let Some(flag) = token.strip_prefix("--") else {
                continue;
            };
            // `--answers answers.json` — the value is the next token
            // and is not a flag; nothing here needs to know that.
            assert!(
                sub.get_arguments().any(|a| a.get_long() == Some(flag)),
                "{file} says `pando {command}`, and `pando {verb}` has no --{flag}"
            );
        }
        checked += 1;
    }
    assert!(
        checked > 0,
        "{file} names no commands at all — did the format change?"
    );
}

#[test]
fn the_contract_only_names_commands_pando_has() {
    assert_every_documented_command_is_real("agent/json.md");
}

// The host wrappers are glue, but glue that names commands: the same
// rename that would rot the brief rots them.
#[test]
fn every_host_wrapper_only_names_commands_pando_has() {
    // The repository's own README is prose for people, whose command
    // table is not in this shape. Everything here is a document an
    // agent is pointed at and follows literally.
    for file in [
        "agent/README.md",
        "agent/skills/pando-setup/SKILL.md",
        "agent/skills/pando-operate/SKILL.md",
        "agent/codex/pando-setup/SKILL.md",
        "agent/codex/pando-operate/SKILL.md",
    ] {
        assert_every_documented_command_is_real(file);
    }
}

// The brief is a procedure written for a language model, which is the
// one kind of reader that will follow a command that does not exist
// and report that it worked.
#[test]
fn the_brief_only_names_commands_pando_has() {
    assert_every_documented_command_is_real("agent/brief.md");
}

// The setup prompt is the one line a developer copies into their agent,
// and it names one command. A flag renamed under it would leave every
// setup screen, header hint and README handing agents a command that
// fails.
#[test]
fn the_setup_prompt_only_names_commands_pando_has() {
    assert_every_command_is_real("setup::SETUP_PROMPT", crate::setup::SETUP_PROMPT);
    assert!(
        crate::setup::SETUP_PROMPT.contains("`pando init --agent`"),
        "the prompt hands the job to `init --agent`: {}",
        crate::setup::SETUP_PROMPT
    );
}

// One prompt, everywhere: the README a person reads first hands them the
// same line the setup screen copies, word for word.
#[test]
fn the_readme_carries_the_setup_prompt_as_it_is() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let text = std::fs::read_to_string(&path).expect("the README");
    assert!(
        text.lines().any(|line| line == crate::setup::SETUP_PROMPT),
        "README.md does not carry the setup prompt on a line of its own: {}",
        crate::setup::SETUP_PROMPT
    );
    assert_every_command_is_real("README.md's setup prompt", crate::setup::SETUP_PROMPT);
}

// Who made pando is one fact: the wordmark's credit, the README and
// Cargo.toml say the same name and the same link.
#[test]
fn the_creator_is_credited_the_same_everywhere() {
    use crate::art::{CREATOR, CREATOR_URL};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let readme = std::fs::read_to_string(root.join("README.md")).unwrap();
    assert!(
        readme.contains(&format!("Created by [{CREATOR}]({CREATOR_URL})")),
        "README.md does not credit {CREATOR}"
    );
    let manifest = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        manifest.contains(&format!("authors = [\"{CREATOR} <{CREATOR_URL}>\"]")),
        "Cargo.toml's authors"
    );
    assert_eq!(
        crate::art::credit(),
        format!("created by {CREATOR} · github.com/mertkaradayi")
    );
}

// The job an agent follows lists the commands it will run; the same
// rename would rot it.
#[test]
fn the_setup_job_only_names_commands_pando_has() {
    let fx = fixture();
    assert_every_command_is_real("init --agent", &super::agent::job(&fx.paths));
}

// The block an agent saves and follows in every later session, so a
// command in it that does not parse fails there for as long as the copy
// is kept. Checked outside the job's fence, where the spans are read.
#[test]
fn the_memory_block_names_the_project_its_root_and_only_real_commands() {
    let fx = fixture();
    let block = crate::setup::memory_block(&fx.paths, None);
    assert_every_command_is_real("init --agent --reference memory", &block);
    assert!(
        block.starts_with(&format!(
            "## pando runs acme-shop ({})\n",
            fx.paths.root().display()
        )),
        "{block}"
    );
    let flat = block.split_whitespace().collect::<Vec<_>>().join(" ");
    for (phrase, why) in [
        ("`pando start <name> --wait`", "how to start one"),
        ("`pando stop <name>`", "how to stop one"),
        ("`pando restart <name> --wait`", "how to restart one"),
        ("`pando status <name> --json`", "what runs, and the URL"),
        ("`pando open <name>`", "the URL for a person"),
        ("`pando logs <name>`", "the logs"),
        ("`--follow`", "following a log"),
        ("`--source <process>`", "one process's log"),
        ("`pando new <branch>`", "a worktree for a branch"),
        (
            "the main checkout",
            "that the main checkout runs the same way",
        ),
        (
            "never with the dev command by hand",
            "that pando starts them",
        ),
        ("exit 3", "the code that is a question"),
        (
            "`pando init --agent --reference brief`",
            "where the rest is",
        ),
    ] {
        assert!(flat.contains(phrase), "the block never says {why}: {block}");
    }
    let lines = block.lines().count();
    assert!(
        (12..=20).contains(&lines),
        "the block is {lines} lines: short enough to keep, long enough to follow"
    );
}

/// A login shell that resolves node `version`, from `/n/bin/node`, and
/// counts how often it was asked.
fn node_shell(
    version: &'static str,
    asked: std::rc::Rc<std::cell::Cell<usize>>,
) -> impl Fn(&str) -> Option<String> {
    move |_: &str| {
        asked.set(asked.get() + 1);
        Some(crate::runtime::probe_reply("/n/bin/node", version))
    }
}

// A project that pins nothing needs no prelude, and the job says so in a
// few words without asking a shell anything.
#[test]
fn the_job_says_a_project_that_pins_nothing_needs_no_prelude() {
    let fx = fixture();
    let asked = std::rc::Rc::new(std::cell::Cell::new(0));
    let shell = node_shell("24.21.0", asked.clone());
    let home = fx.root.join("no-such-home");
    let job = super::agent::job_on(&fx.paths, &crate::actions::Machine::at(&shell, home));
    assert!(
        job.contains("- prelude: not needed: the project pins no runtime\n"),
        "{job}"
    );
    assert_eq!(asked.get(), 0, "nothing pinned, nothing to ask a shell");
}

// The job asks what `init` would: the pin in an app directory, from the
// version files `init --yes` would write, against what `bash -lc`
// resolves here. When they differ the line says so, and that the answer
// is this machine's; when they agree it stays short. Nothing is written,
// the probe cache included.
#[test]
fn the_job_says_when_this_machine_needs_a_prelude() {
    let fx = fixture();
    std::fs::create_dir_all(fx.root.join("backend")).unwrap();
    std::fs::write(
        fx.root.join("backend/package.json"),
        r#"{ "scripts": { "dev": "tsx watch src/index.ts" } }"#,
    )
    .unwrap();
    std::fs::write(fx.root.join("backend/.nvmrc"), "25\n").unwrap();
    let home = fx.root.join("no-such-home");
    let asked = std::rc::Rc::new(std::cell::Cell::new(0));

    let shell = node_shell("24.21.0", asked.clone());
    let job = super::agent::job_on(
        &fx.paths,
        &crate::actions::Machine::at(&shell, home.clone()),
    );
    assert!(
        job.contains(
            "- prelude: open: node 25 asked (backend/.nvmrc), `bash -lc` resolves 24.21.0. \
             Answer it with the first line `pando doctor` lists, or `null` to run on what this \
             machine has when it lists none or pando refuses the line: it runs in every project \
             on this machine, so name it in your report\n"
        ),
        "{job}"
    );
    assert!(
        !job.contains("Open questions: none."),
        "an open prelude is an open question: {job}"
    );

    let shell = node_shell("25.8.2", asked.clone());
    let job = super::agent::job_on(&fx.paths, &crate::actions::Machine::at(&shell, home));
    assert!(
        job.contains(
            "- prelude: not asked on this machine: `bash -lc` here resolves what the project \
             pins\n"
        ),
        "{job}"
    );
    assert_eq!(asked.get(), 2, "one probe each time");
    assert!(
        !fx.paths.home.exists(),
        "the job writes nothing, not even pando's home"
    );
}

// Where a version manager's line works, the job names the line `--yes`
// would take, since that is what it will do; nothing is written by
// trying it.
#[test]
fn the_job_names_the_managers_line_init_yes_would_take() {
    let fx = fixture();
    std::fs::write(fx.root.join(".nvmrc"), "25\n").unwrap();
    let home = fx.root.join("machine");
    std::fs::create_dir_all(home.join(".volta/bin")).unwrap();
    let shell = |command: &str| {
        let version = match command.contains(".volta/bin") {
            true => "25.8.2",
            false => "24.21.0",
        };
        Some(crate::runtime::probe_reply("/n/bin/node", version))
    };
    let job = super::agent::job_on(
        &fx.paths,
        &crate::actions::Machine::at(&shell, home.clone()),
    );
    let line = format!(
        "export PATH=\"{}:$PATH\"",
        home.join(".volta/bin").display()
    );
    assert!(
        job.contains(&format!(
            "- prelude: open: node 25 asked (.nvmrc), `bash -lc` resolves 24.21.0. `pando init \
             --yes` takes `{line}`; it runs in every project on this machine, so name it in \
             your report\n"
        )),
        "{job}"
    );
    assert!(!fx.paths.home.exists(), "trying the line writes nothing");
}

// After a passing check the job says nothing is left, and nothing below
// sends the agent back to `init --yes` and a check. Process tables that
// name no `dev` settle the dev command and its ports without either key
// being written, and the lines say so rather than "set".
#[test]
fn the_job_after_a_passing_check_has_nothing_left_to_do() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("pando.toml"),
        "[processes.web]\ncmd = \"npm run web\"\nports = [\"web\"]\n\n\
         [processes.api]\ncmd = \"npm run api\"\nports = [\"api\"]\n",
    )
    .unwrap();
    let config = crate::config::load(&fx.paths).unwrap().config;
    let before = super::agent::job(&fx.paths);
    assert!(before.contains("Open questions: none."), "{before}");

    let mut record = crate::setup::CheckRecord::begin(
        crate::setup::fingerprint(&config),
        crate::setup::RanBy::Program,
    );
    record.finished_at = Some(chrono::Utc::now());
    record.outcome = crate::setup::CheckOutcome::Passed;
    record.save(&fx.paths).unwrap();

    let job = super::agent::job(&fx.paths);
    assert!(job.contains("there is nothing left to set up"), "{job}");
    assert!(!job.contains("Open questions"), "{job}");
    assert!(!job.contains("`pando init --yes` saves"), "{job}");
    for name in ["dev_cmd", "port_env"] {
        assert!(
            job.contains(&format!("- {name}: covered by processes\n")),
            "{job}"
        );
    }
    assert!(job.contains("- processes: set: `api, web`"), "{job}");
}

// A `[dev]` whose command runs a framework's server and that has no
// `ports` has settled the port question, and still runs every worktree's
// server on the framework's own port: the job says so, with doctor's fix,
// rather than "covered by processes".
#[test]
fn the_job_names_a_server_with_no_port_rather_than_calling_it_covered() {
    let fx = fixture();
    std::fs::write(
        fx.root.join("package.json"),
        r#"{"dependencies":{"expo":"57.0.0"},"scripts":{"start":"expo start"}}"#,
    )
    .unwrap();
    std::fs::write(
        fx.root.join("pando.toml"),
        "[dev]\ncmd = \"npm run start\"\n",
    )
    .unwrap();
    let job = super::agent::job(&fx.paths);
    let line = job
        .lines()
        .find(|line| line.starts_with("- port_env: "))
        .unwrap_or_else(|| panic!("{job}"));
    assert!(line.contains("not given one"), "{line}");
    assert!(line.contains("8081"), "{line}");
    assert!(
        line.contains(r#"echo '{"port_env":"RCT_METRO_PORT"}' | pando init --answers - --replace"#),
        "{line}"
    );
}

// `--reference memory` prints the block and nothing else, and the job's
// last section is the same block, fenced, so what an agent saves from
// either is the same text.
#[test]
fn reference_memory_prints_the_block_the_job_ends_with() {
    let fx = fixture();
    let block = crate::setup::memory_block(&fx.paths, None);
    let mut out = Vec::new();
    super::agent::agent(&fx.paths, Some(super::agent::Reference::Memory), &mut out).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), block);
    let job = super::agent::job(&fx.paths);
    let section = &job[job
        .find("\n## Remember how to run acme-shop\n")
        .expect("the job has a section for the block")..];
    assert!(
        section.ends_with(&format!("```markdown\n{block}```\n")),
        "{section}"
    );
    assert!(section.contains("\"Remember how to run it\""), "{section}");
    assert!(
        section.contains("save it only when the developer tells you to"),
        "{section}"
    );
}

// A passing setup never meets a phone, so for an app a device runs the job
// says the LAN address once in its list and once in the block, and
// `--reference memory` prints the same block. For any other app, neither.
#[test]
fn the_job_says_a_devices_address_only_for_an_app_a_device_runs() {
    let fx = fixture();
    let job = super::agent::job(&fx.paths);
    assert!(!job.contains("REACT_NATIVE_PACKAGER_HOSTNAME"), "{job}");

    std::fs::write(
        fx.root.join("package.json"),
        r#"{ "scripts": { "start": "expo start" }, "dependencies": { "expo": "~57.0.0" } }"#,
    )
    .unwrap();
    std::fs::write(
        fx.root.join("app.json"),
        r#"{ "expo": { "name": "mobile" } }"#,
    )
    .unwrap();
    let job = super::agent::job(&fx.paths);
    let (list, block) = job
        .split_once("\n## Remember how to run acme-shop\n")
        .expect("the job has a section for the block");
    assert!(
        list.contains("\n- a phone or tablet reaches Metro"),
        "{list}"
    );
    assert!(list.contains("never guess the address"), "{list}");
    assert!(block.contains("REACT_NATIVE_PACKAGER_HOSTNAME"), "{block}");
    // And where the simulator's command is, since Expo's keypress for it
    // is gone under pando.
    for text in [list, block] {
        assert!(
            text.contains(
                "`pando open <name>` opens the app; `pando status <name>` gives the commands"
            ),
            "{text}"
        );
    }
    let mut out = Vec::new();
    super::agent::agent(&fx.paths, Some(super::agent::Reference::Memory), &mut out).unwrap();
    let memory = String::from_utf8(out).unwrap();
    assert!(
        block.ends_with(&format!("```markdown\n{memory}```\n")),
        "{block}"
    );
}

/// `CLAUDE.md` names the CLI verbs and calls them canonical — "used
/// identically in every document" — which is exactly the claim that
/// rots. It was missing `restart` for as long as `restart` existed,
/// in the file that tells every other document what the list is.
///
/// The README has its own check; this is the second place the verbs
/// are written down by hand, and the last one that was unguarded.
///
/// Only the list itself counts, and it is held to clap both ways: the
/// rest of the file says "new", "start" and "open" in prose, so a search
/// of the whole file passed with any of the three gone from the list.
#[test]
fn claude_md_lists_every_verb_pando_has() {
    use clap::CommandFactory;
    use std::collections::BTreeSet;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("CLAUDE.md");
    let text = std::fs::read_to_string(&path).expect("CLAUDE.md");
    // Hard-wrapped, so the list can straddle lines.
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let marker = "CLI verbs, used identically in every document: `";
    let at = flat
        .find(marker)
        .expect("CLAUDE.md has its canonical verb list");
    let rest = &flat[at + marker.len()..];
    let listed: BTreeSet<&str> = rest[..rest.find('`').expect("the list's closing backtick")]
        .split_whitespace()
        .collect();
    let command = Cli::command();
    let verbs: BTreeSet<&str> = command
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help" && !sub.is_hide_set())
        .map(|sub| sub.get_name())
        .collect();
    assert_eq!(
        listed, verbs,
        "CLAUDE.md calls its verb list canonical, and it is not the verbs pando has"
    );
}

/// The README's own command list, against clap.
///
/// It is prose for people, so it is not in the shape
/// [`assert_every_documented_command_is_real`] parses — the
/// description runs on after the verb, and the first line is a bare
/// `pando` that opens the TUI. But the Status section under the list
/// says every command in it is implemented, and that is a claim worth
/// failing over: the line above it said "there is no code yet" for
/// eight phases of code, because nothing read it.
#[test]
fn the_readme_lists_only_commands_pando_has() {
    use clap::CommandFactory;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("README.md");
    let text = std::fs::read_to_string(&path).expect("the README");
    let cli = Cli::command();
    let mut checked = 0;
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        // Only the fenced list. "pando never writes into your
        // repository" is a sentence, and the promise it belongs to is
        // not a command table.
        if !fenced {
            continue;
        }
        let Some(rest) = line.strip_prefix("pando ") else {
            continue;
        };
        // The one line with no verb: `pando` alone, padded out to the
        // description column.
        if rest.starts_with(' ') {
            continue;
        }
        let verb = rest.split_whitespace().next().unwrap_or_default();
        assert!(
            cli.get_subcommands().any(|c| c.get_name() == verb),
            "README.md lists `pando {verb}`, and pando has no {verb:?} command"
        );
        checked += 1;
    }
    assert!(
        checked >= 13,
        "only {checked} commands were found in README.md — did the list move?"
    );

    // And the other direction, which is the half that was missing:
    // the Status section says every command in the list is
    // implemented, and a reader takes a checked list to be a whole
    // one. `help` is clap's own, and a hidden subcommand is hidden
    // precisely because it is not for people.
    for sub in cli.get_subcommands() {
        let name = sub.get_name();
        if name == "help" || sub.is_hide_set() {
            continue;
        }
        assert!(
            text.contains(&format!("\npando {name} "))
                || text.contains(&format!("\npando {name}\n")),
            "pando has a {name:?} command and README.md does not list it — a list that is \
                 checked reads as a complete one"
        );
    }
}

/// The brief is the only place the reasoning lives, so the things it
/// has to teach are worth failing over if somebody trims it.
///
/// Phrases, not sentences: this is a guard against a section being
/// deleted, not a style checker.
#[test]
fn the_brief_teaches_the_things_only_it_teaches() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/brief.md");
    let text = std::fs::read_to_string(&path).expect("the brief");
    // Collapsed, because the document is hard-wrapped and a phrase it
    // makes is as likely as not to straddle two lines.
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for (phrase, why) in [
        ("init --answers", "the one write path"),
        ("never edit", "and that nothing else is"),
        ("by value", "how an option is named"),
        ("non-frozen", "the install guardrail"),
        (
            "built from the repository",
            "a compose file that only packages the app",
        ),
        ("[isolation] prefer", "the preference an agent cannot write"),
        (
            "machine-wide",
            "that the preference is not a fact about this repository",
        ),
        (
            "gap in the corpus",
            "the one answer the decisions log cannot hold",
        ),
        ("decisions.jsonl", "what pando records about the answerer"),
        ("Prove it by running", "a setup is proved by starting it"),
        (
            "listening on … instead",
            "the failure a config that reads right hides",
        ),
        ("exit 3", "the code that means a question is open"),
        ("--json", "never parse human-readable output"),
        ("[processes.", "the process table a processes object writes"),
        ("{port:<role>}", "how one process finds another's port"),
        ("`http_status: null`", "that a role need not serve a page"),
        (
            "REACT_NATIVE_PACKAGER_HOSTNAME",
            "that a phone needs the machine's LAN address",
        ),
        (
            "expo-development-client",
            "how a development build of an Expo app is opened",
        ),
        ("kind: \"base\"", "a failure no setting fixes"),
    ] {
        assert!(
            text.to_lowercase().contains(&phrase.to_lowercase()),
            "the brief no longer teaches {why}: it never says {phrase:?}"
        );
    }
    // And every question it tells a reader to answer.
    for name in slot_names() {
        assert!(text.contains(&name), "the brief never mentions {name}");
    }
}

/// The brief's process table is one pando reads: every key it shows is a
/// key of the schema, and the whole of it loads, so the lines an agent
/// hands a developer to paste are lines pando takes.
#[test]
fn the_briefs_process_table_is_a_config_pando_loads() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/brief.md");
    let text = std::fs::read_to_string(&path).expect("the brief");
    let section = &text[text
        .find("## 9. The process table")
        .expect("the brief has the process table section")..];
    let start = section.find("```toml\n").expect("a TOML block") + "```toml\n".len();
    let block = &section[start..start + section[start..].find("```").unwrap()];
    let fx = fixture();
    std::fs::create_dir_all(fx.paths.config_file().parent().unwrap()).unwrap();
    std::fs::write(fx.paths.config_file(), block).unwrap();
    let loaded = crate::config::load(&fx.paths).unwrap_or_else(|e| panic!("{e:#}\n{block}"));
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let processes = &loaded.config.processes;
    assert_eq!(
        processes.keys().map(String::as_str).collect::<Vec<_>>(),
        ["api", "mobile", "web", "worker"]
    );
    assert_eq!(
        processes["web"].env["VITE_API_URL"],
        "http://127.0.0.1:{port:api}"
    );
    assert_eq!(processes["worker"].roles(), Vec::<String>::new());
    assert!(processes["web"].serves_page());
    assert!(!processes["mobile"].serves_page());
}

/// The contract file says the same names, in the same order.
///
/// A document is the one part of a contract nothing compiles, so it is
/// the part that rots. Reading it from the test is what makes a rename
/// fail in the commit that does it rather than in somebody's agent a
/// month later.
#[test]
fn the_published_contract_names_every_question_in_order() {
    let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md");
    let text =
        std::fs::read_to_string(&doc).unwrap_or_else(|e| panic!("read {}: {e}", doc.display()));
    assert!(
        text.contains(&slot_names().join("  ")),
        "agent/json.md does not list the twelve questions in the order pando asks them"
    );
    for name in slot_names() {
        assert!(text.contains(&name), "agent/json.md never mentions {name}");
    }
}

/// `--replace` as the contract describes it: an `init` flag that needs
/// `--answers`, and that the preview takes as well. clap is asked, so the
/// document and the parser cannot say two things.
#[test]
fn the_contract_describes_replace_as_clap_has_it() {
    let doc = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md");
    let text =
        std::fs::read_to_string(&doc).unwrap_or_else(|e| panic!("read {}: {e}", doc.display()));
    let section = &text[text
        .find("## The answers file")
        .expect("agent/json.md has an answers file section")..];
    for command in [
        "pando init --answers - --replace",
        "pando init --answers - --dry-run --replace",
    ] {
        assert!(
            section.contains(&format!("`{command}`")),
            "the answers file section never shows `{command}`"
        );
        let argv: Vec<&str> = command.split_whitespace().collect();
        Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?} does not parse: {e}"));
    }
    assert!(
        section.contains("`--replace`\nneeds `--answers`")
            || section.contains("`--replace` needs `--answers`"),
        "the section says --replace needs --answers"
    );
    let alone = Cli::try_parse_from(["pando", "init", "--replace"])
        .expect_err("--replace without --answers is refused");
    assert_eq!(
        alone.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
    // And beside `--yes`, which only ever answers what the file does not.
    Cli::try_parse_from(["pando", "init", "--answers", "-", "--replace", "--yes"])
        .expect("--replace combines with --yes");
    // The one slot it never changes is named where the refusals are.
    assert!(
        section.contains("a `prelude` that is already answered"),
        "{section}"
    );
    // And an answered slot without the flag is a refusal, not a note: a
    // program that reads exit codes has to learn its answer did not land.
    assert!(
        section.contains("already answered is **refused with exit 2,\nand nothing is written**"),
        "{section}"
    );
}

// Every answers command `doctor` prints is a file this parser takes for
// its slot: a list at a list slot, the tables at the process list, text
// everywhere else.
#[test]
fn the_answer_a_doctor_fix_prints_is_one_the_answers_file_takes() {
    use crate::detect::{Candidate, Slot, answers_value};
    let candidate = |value: &str| Candidate {
        value: value.to_string(),
        ..Candidate::default()
    };
    let processes = Candidate {
        value: "api: npm run dev in apps/api".to_string(),
        processes: Some(BTreeMap::from([(
            "api".to_string(),
            crate::config::ProcessConfig {
                cmd: "npm run dev".to_string(),
                cwd: Some("apps/api".to_string()),
                ports: Some(crate::config::PortsSpec::List(vec!["api".to_string()])),
                ..Default::default()
            },
        )])),
        ..Candidate::default()
    };
    let cases = [
        (Slot::Install, candidate("pnpm install --frozen-lockfile")),
        (Slot::VersionFiles, candidate(".nvmrc,.tool-versions")),
        (Slot::Provision, candidate(".env,.env.local")),
        (Slot::DevCmd, candidate("pnpm dev")),
        (Slot::PortEnv, candidate("RCT_METRO_PORT")),
        (Slot::Services, candidate("postgres")),
        (Slot::SchemaHook, candidate("pnpm prisma migrate deploy")),
        (Slot::Base, candidate("develop")),
        (Slot::Processes, processes),
    ];
    for (slot, candidate) in cases {
        let command = crate::doctor::answers_command(slot, answers_value(slot, &candidate));
        let file = command
            .strip_prefix("echo '")
            .and_then(|rest| rest.split_once("' | pando init --answers - --replace"))
            .map(|(file, _)| file)
            .unwrap_or_else(|| panic!("{command}"));
        let parsed = Answers::parse(file).unwrap_or_else(|e| panic!("{slot:?}: {file}: {e:#}"));
        let question = actions::Question {
            slot,
            prompt: slot.prompt().to_string(),
            options: vec![(candidate.value.clone(), "a signal".to_string())],
            preselect: Some(0),
            allow_custom: slot.allows_custom(),
            allow_none: slot.allows_none(),
            multi: slot.is_multi(),
            checked: Vec::new(),
            details: Vec::new(),
            answer_file: None,
            snippet: String::new(),
        };
        let answer = parsed
            .for_question(&question)
            .unwrap_or_else(|| panic!("{slot:?}: {file}"))
            .unwrap_or_else(|e| panic!("{slot:?}: {file}: {e:#}"));
        // The option itself, not a command of the same text: what is
        // chosen carries the roles and the tables the candidate does.
        let chosen = match answer {
            actions::Answer::Program(inner) => *inner,
            other => other,
        };
        assert!(
            matches!(
                chosen,
                actions::Answer::Choice(0)
                    | actions::Answer::Many(_)
                    | actions::Answer::Processes(_)
            ),
            "{slot:?}: {file}: {chosen:?}"
        );
    }
}

// The refusal an answered slot earns: exit 2's type, every slot named, and
// the way through for each — `--replace` would only refuse the prelude again.
#[test]
fn an_answer_for_an_answered_slot_is_a_usage_error_naming_the_way_through() {
    let mut config = Config::default();
    config.project.install = Some("make deps".into());
    config.runtime.prelude = Some(String::new());
    let answers =
        Answers::parse(r#"{"install": "npm ci", "prelude": "export A=1", "base": "main"}"#)
            .unwrap();
    let err = super::answers::refuse_answered(&answers, &config, false).unwrap_err();
    assert!(err.downcast_ref::<UsageError>().is_some(), "{err:#}");
    let text = format!("{err:#}");
    assert!(
        text.contains(
            "install is already answered, so your answer was not applied — add --replace"
        ),
        "{text}"
    );
    assert!(text.contains("prelude is already answered"), "{text}");
    assert!(text.contains("only a person changes it"), "{text}");
    assert!(
        !text.contains("base"),
        "an open slot is not refused: {text}"
    );
    assert!(text.ends_with("nothing was written"), "{text}");

    super::answers::refuse_answered(&answers, &config, true)
        .expect("under --replace an answered slot is what the flag is for");
}

fn parse_err(json: &str) -> String {
    format!("{:#}", Answers::parse(json).unwrap_err())
}

#[test]
fn a_name_pando_does_not_ask_about_is_a_usage_error_naming_it() {
    let err = parse_err(r#"{"dev_command": "pnpm dev"}"#);
    assert!(err.contains("dev_command"), "{err}");
    assert!(err.contains("dev_cmd"), "and what it does ask about: {err}");
    assert!(
        Answers::parse(r#"{"dev_command": "x"}"#)
            .unwrap_err()
            .downcast_ref::<UsageError>()
            .is_some(),
        "a name that is not a question is a usage error, not a failure"
    );
}

// Caught when the file is read, not when a question happens to reach
// the slot: a shape this slot cannot take is knowable from the slot.
#[test]
fn a_shape_the_slot_cannot_take_is_refused_before_anything_is_written() {
    let err = parse_err(r#"{"install": 42}"#);
    assert!(err.contains("install"), "{err}");
    assert!(err.contains("string"), "{err}");

    let err = parse_err(r#"{"install": null}"#);
    assert!(err.contains("no \"none\" answer"), "{err}");

    let err = parse_err(r#"{"install": ["a", "b"]}"#);
    assert!(err.contains("not a list"), "{err}");

    let err = parse_err(r#"{"services": "db"}"#);
    assert!(err.contains("list of the options"), "{err}");

    let err = parse_err(r#"{"install": "   "}"#);
    assert!(err.contains("empty string"), "{err}");

    // And the shapes that are fine everywhere they are offered.
    assert!(Answers::parse(r#"{"port_env": null}"#).is_ok());
    assert!(Answers::parse(r#"{"services": []}"#).is_ok());
    assert!(Answers::parse(r#"{"provision": [".env"]}"#).is_ok());
    assert!(Answers::parse(r#"{"version_files": [".nvmrc"]}"#).is_ok());
}

/// The process-list question with nothing on offer: a project with no
/// manifest pando reads.
fn processes_question() -> actions::Question {
    actions::Question {
        slot: crate::detect::Slot::Processes,
        prompt: crate::detect::Slot::Processes.prompt().to_string(),
        ..dev_question(&[])
    }
}

// An object mirroring `[processes.<name>]` was refused as a shape no
// question takes. At the process list it is the answer a project of
// several processes needs.
#[test]
fn an_object_at_processes_is_whole_process_tables() {
    let answers = Answers::parse(
        r#"{"processes": {
            "api": {"cmd": "uv run uvicorn app:app --port {port}", "cwd": "backend",
                    "ports": ["api"]},
            "web": {"cmd": "npm run dev", "cwd": "frontend", "ports": {"PORT": "web"},
                    "env": {"API_URL": "http://127.0.0.1:{port:api}"},
                    "ready": {"timeout_s": 90}}
        }}"#,
    )
    .unwrap();
    let answer = answers
        .for_question(&processes_question())
        .expect("the file answers it")
        .unwrap();
    let actions::Answer::Program(inner) = answer else {
        panic!("a program's answer says so: {answer:?}");
    };
    let actions::Answer::Processes(tables) = *inner else {
        panic!("process tables: {inner:?}");
    };
    assert_eq!(tables.keys().collect::<Vec<_>>(), ["api", "web"]);
    assert_eq!(tables["api"].roles(), ["api"]);
    assert_eq!(tables["web"].port_env()["PORT"], "{port:web}");
    assert_eq!(tables["web"].ready.as_ref().unwrap().timeout_s, Some(90));
}

#[test]
fn process_tables_are_read_as_strictly_as_the_toml_they_become() {
    for (json, says) in [
        (r#"{"processes": {}}"#, "empty object"),
        (
            r#"{"processes": {"api": {"command": "x"}}}"#,
            "processes.api is not a process table: unknown field `command`",
        ),
        (
            r#"{"processes": {"api": {"cwd": "backend"}}}"#,
            "processes.api has no cmd",
        ),
        (
            r#"{"processes": {"api": "uvicorn"}}"#,
            "processes.api is not a process table",
        ),
        (
            r#"{"processes": {"api": {"cmd": "x", "ports": {"API-PORT": "api"}}}}"#,
            "\"API-PORT\", which is not an environment variable name",
        ),
        (
            r#"{"processes": {"api": {"cmd": "x", "env": {"A B": "1"}}}}"#,
            "not an environment variable name",
        ),
    ] {
        let e = Answers::parse(json).unwrap_err();
        assert!(e.downcast_ref::<UsageError>().is_some(), "{json}");
        let e = format!("{e:#}");
        assert!(e.contains(says), "{json}: {e}");
    }
}

// Each refusal says what the question does take. The two a program met
// back to back said "a string, a list of strings, or null" and then "one
// answer, not a list of them", and neither one was true of the question.
#[test]
fn a_refused_shape_says_what_the_question_takes() {
    let err = parse_err(r#"{"processes": ["api: x", "web: y"]}"#);
    assert!(err.contains("not a list"), "{err}");
    assert!(err.contains("an object of process tables"), "{err}");
    assert!(!err.contains("list of strings"), "{err}");

    let err = parse_err(r#"{"processes": 1}"#);
    assert!(err.contains("an object of process tables"), "{err}");
    assert!(!err.contains("list of strings"), "{err}");

    // Only the process list takes tables.
    let err = parse_err(r#"{"dev_cmd": {"api": {"cmd": "x"}}}"#);
    assert!(err.contains("dev_cmd takes one string"), "{err}");
    assert!(!err.contains("object"), "{err}");

    let err = parse_err(r#"{"port_env": ["PORT"]}"#);
    assert!(err.contains("null for none"), "{err}");
    let err = parse_err(r#"{"provision": 1}"#);
    assert!(err.contains("a list of file names"), "{err}");
}

// The per-app option is shown as `name: cmd in dir; …`. Typed with
// processes of one's own in it, it matched nothing and became one shell
// command that runs `api:` and fails.
#[test]
fn a_typed_process_list_in_the_options_own_form_is_refused_for_the_object_form() {
    let typed = "api: uv run uvicorn app:app in backend; worker: uv run python -m worker in \
                 backend; web: npm run dev in frontend";
    for question in [processes_question(), dev_question(&["pnpm dev"])] {
        let err = answer_from(&question, &serde_json::json!(typed)).unwrap_err();
        assert!(err.downcast_ref::<UsageError>().is_some());
        let err = format!("{err:#}");
        assert!(err.contains("object of process tables"), "{err}");
    }
    // Named as an option it is still that option.
    let question = actions::Question {
        options: vec![(typed.to_string(), "a dev script in each app".to_string())],
        ..processes_question()
    };
    assert_eq!(
        answer_from(&question, &serde_json::json!(typed)).unwrap(),
        actions::Answer::Program(Box::new(actions::Answer::Choice(0)))
    );
    // And a command that only has a colon or a semicolon in it is a
    // command.
    for command in [
        "npm run dev",
        "cd backend; uv run uvicorn app:app",
        "echo ready: yes",
        "PORT=3000 node server.js in-memory",
    ] {
        assert!(!reads_as_process_list(command), "{command}");
    }
}

// The recogniser is held to the text the rules' own per-app option
// really has, so the two cannot drift apart.
#[test]
fn the_per_app_option_reads_as_a_process_list() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("package.json"), r#"{ "workspaces": ["apps/*"] }"#).unwrap();
    for (app, dev) in [("web", "vite"), ("api", "node --watch src/index.js")] {
        std::fs::create_dir_all(root.join("apps").join(app)).unwrap();
        std::fs::write(
            root.join("apps").join(app).join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{dev}" }} }}"#),
        )
        .unwrap();
    }
    let proposal = crate::detect::propose(root, &crate::detect::signals(root))
        .into_iter()
        .find(|p| p.slot == crate::detect::Slot::Processes)
        .expect("a workspace has a per-app option");
    let option = &proposal.candidates[0];
    assert!(option.processes.as_ref().is_some_and(|p| p.len() == 2));
    assert!(reads_as_process_list(&option.value), "{}", option.value);
}

#[test]
fn a_file_that_is_not_a_json_object_is_a_usage_error() {
    assert!(parse_err("[1, 2]").contains("JSON object"));
    assert!(parse_err("{").contains("not JSON"));
}

// By value, never by index: the option carries the roles a command
// owns and the process tables a workspace answer is, and a list of
// indexes is a contract that breaks the day a rule finds one more
// candidate.
#[test]
fn an_option_is_answered_by_its_own_text() {
    let question = dev_question(&["pnpm dev", "pnpm dev:web"]);
    let answer = answer_from(&question, &serde_json::json!("pnpm dev:web")).unwrap();
    assert_eq!(
        answer,
        actions::Answer::Program(Box::new(actions::Answer::Choice(1)))
    );
}

// Every question has a custom answer, and a program gets the same one.
#[test]
fn a_value_no_option_has_is_a_command_of_your_own() {
    let question = dev_question(&["pnpm dev"]);
    let answer = answer_from(&question, &serde_json::json!("./serve.sh")).unwrap();
    assert_eq!(
        answer,
        actions::Answer::Program(Box::new(actions::Answer::Custom("./serve.sh".to_string())))
    );
}

// Except at the set question, where there is nothing to type: a
// service the compose file does not declare is not one pando can run.
#[test]
fn a_set_answer_that_names_nothing_on_offer_says_what_is() {
    let question = services_question();
    let err = answer_from(&question, &serde_json::json!(["postgres"])).unwrap_err();
    let printed = format!("{err:#}");
    assert!(printed.contains("postgres"), "{printed}");
    assert!(printed.contains("cache, db, mail, queue"), "{printed}");
    assert!(err.downcast_ref::<UsageError>().is_some());
}

#[test]
fn a_set_answer_is_the_options_it_names_and_an_empty_one_is_none_of_them() {
    let question = services_question();
    assert_eq!(
        answer_from(&question, &serde_json::json!(["db", "cache"])).unwrap(),
        actions::Answer::Program(Box::new(actions::Answer::Many(vec![1, 0])))
    );
    assert_eq!(
        answer_from(&question, &serde_json::json!([])).unwrap(),
        actions::Answer::Program(Box::new(actions::Answer::None))
    );
    assert_eq!(
        answer_from(&question, &serde_json::Value::Null).unwrap(),
        actions::Answer::Program(Box::new(actions::Answer::None))
    );
}

// A list slot takes a JSON array, joined into the one value the slot
// writes — so a program never has to know the separator.
#[test]
fn a_list_slot_takes_an_array_and_joins_it_the_way_the_slot_splits_it() {
    let question = actions::Question {
        slot: crate::detect::Slot::Provision,
        prompt: crate::detect::Slot::Provision.prompt().to_string(),
        options: vec![(".env,.env.local".to_string(), "here".to_string())],
        preselect: Some(0),
        allow_custom: true,
        allow_none: true,
        multi: false,
        checked: Vec::new(),
        details: Vec::new(),
        answer_file: None,
        snippet: String::new(),
    };
    // The option's own text, reached without spelling the separator.
    assert_eq!(
        answer_from(&question, &serde_json::json!([".env", ".env.local"])).unwrap(),
        actions::Answer::Program(Box::new(actions::Answer::Choice(0)))
    );
    // And a list nothing offered is still an answer.
    assert_eq!(
        answer_from(&question, &serde_json::json!([".env", ".envrc"])).unwrap(),
        actions::Answer::Program(Box::new(actions::Answer::Custom(".env,.envrc".to_string())))
    );
}

// An answer nothing asked about is reported rather than dropped: a
// program that answered a question pando did not ask has to hear it.
#[test]
fn the_answers_a_run_never_used_are_the_ones_nothing_asked_about() {
    let answers = Answers::parse(r#"{"install": "npm ci", "dev_cmd": "pnpm dev"}"#).unwrap();
    let question = dev_question(&["pnpm dev"]);
    assert!(answers.for_question(&question).is_some());
    assert_eq!(answers.unasked(), vec![crate::detect::Slot::Install]);
}

/// The values `agent/json.md` offers for one enum-valued field, as it
/// spells them: the first `"key": "a|b|c"` in the document.
fn documented(doc: &str, key: &str) -> Vec<String> {
    let needle = format!("\"{key}\": \"");
    let value = doc
        .match_indices(&needle)
        .map(|(at, _)| {
            let rest = &doc[at + needle.len()..];
            &rest[..rest.find('"').expect("a closing quote")]
        })
        .find(|value| value.contains('|'))
        .unwrap_or_else(|| panic!("agent/json.md lists no values for `{key}`"));
    let mut values: Vec<String> = value.split('|').map(str::to_string).collect();
    values.sort();
    values
}

fn serde_word<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .expect("serialises")
        .as_str()
        .expect("a unit variant serialises to a string")
        .to_string()
}

/// agent/json.md quotes every enum-valued field by hand, and one of those
/// strings was once wrong for as long as it shipped. This holds each list
/// to the type that prints it or, where the words are spelled by hand, to
/// what the binary prints. The matches are exhaustive on purpose: a new
/// variant does not compile here until somebody decides what the document
/// says about it.
#[test]
fn every_enum_value_agent_json_documents_is_one_the_binary_prints() {
    use crate::decisions::Shape;
    use crate::doctor::{Section, Severity};
    use crate::log_tail::LogLevel;
    use crate::state::ServiceKind;

    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .expect("read agent/json.md");
    let check = |key: &str, printed: Vec<String>| {
        let mut printed = printed;
        printed.sort();
        assert_eq!(
            documented(&doc, key),
            printed,
            "agent/json.md's `{key}` values and the binary's disagree"
        );
    };

    let sections = [
        Section::Project,
        Section::Config,
        Section::Runtime,
        Section::Tools,
        Section::Worktrees,
        Section::Services,
        Section::Hooks,
        Section::Adoption,
    ];
    for s in sections {
        match s {
            Section::Project
            | Section::Config
            | Section::Runtime
            | Section::Tools
            | Section::Worktrees
            | Section::Services
            | Section::Hooks
            | Section::Adoption => {}
        }
    }
    check("section", sections.map(serde_word).to_vec());

    let severities = [Severity::Problem, Severity::Note];
    for s in severities {
        match s {
            Severity::Problem | Severity::Note => {}
        }
    }
    check("severity", severities.map(serde_word).to_vec());

    let kinds = [ServiceKind::Compose, ServiceKind::Native];
    for k in kinds {
        match k {
            ServiceKind::Compose | ServiceKind::Native => {}
        }
    }
    // `status --json` spells the kind with a match of its own, not through
    // the enum's serde, so the words are read back from what it prints for
    // one service of each kind.
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store.worktrees.get_mut(&name).unwrap().services = kinds
        .map(|kind| crate::state::ServiceRecord {
            name: serde_word(kind),
            kind,
            port: None,
            pid: None,
            pgid: None,
            compose_project: None,
        })
        .to_vec();
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let status: serde_json::Value =
        serde_json::from_str(&capture(|b| status_json(&fx.paths, None, b))).unwrap();
    let printed = kinds.map(|kind| {
        status["worktrees"][0]["services"][serde_word(kind)]["kind"]
            .as_str()
            .unwrap_or_else(|| panic!("status --json prints no kind for a {kind:?} service"))
            .to_string()
    });
    assert_eq!(
        printed,
        kinds.map(serde_word),
        "status --json names a kind by another word"
    );
    check("kind", printed.to_vec());

    // Every mode, and the published words for them: `status --json`,
    // `ls --json` and `doctor --json` print these, and a program reading
    // them decides by them which services a worktree is on.
    for m in crate::state::ServiceMode::ALL {
        match m {
            crate::state::ServiceMode::Shared
            | crate::state::ServiceMode::Namespaced
            | crate::state::ServiceMode::Isolated => {}
        }
    }
    check(
        "mode",
        crate::state::ServiceMode::ALL.map(serde_word).to_vec(),
    );

    let states = [PrState::Open, PrState::Merged, PrState::Closed];
    for s in states {
        match s {
            PrState::Open | PrState::Merged | PrState::Closed => {}
        }
    }
    check("state", states.map(serde_word).to_vec());

    let shapes = [Shape::Choice, Shape::Custom, Shape::Set, Shape::None];
    for s in shapes {
        match s {
            Shape::Choice | Shape::Custom | Shape::Set | Shape::None => {}
        }
    }
    check("shape", shapes.map(serde_word).to_vec());

    let levels = [
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ];
    for l in levels {
        match l {
            LogLevel::Debug | LogLevel::Info | LogLevel::Warn | LogLevel::Error => {}
        }
    }
    check("level", levels.map(|l| level_word(l).to_string()).to_vec());

    let since = Utc::now();
    let phases = [
        Phase::Starting { since },
        Phase::Running { since },
        Phase::Failed {
            at: since,
            reason: String::new(),
        },
    ];
    for p in &phases {
        match p {
            Phase::Starting { .. } | Phase::Running { .. } | Phase::Failed { .. } => {}
        }
    }
    check(
        "phase",
        phases.iter().map(|p| phase_word(p).to_string()).collect(),
    );

    // Plain strings in the report rather than an enum, so they are read
    // from a report doctor makes, as `doctor --json` prints it; doctor's
    // own tests pin the order. The shell finds nothing, so no login shell
    // runs.
    let finds_nothing = |_: &str| None;
    let report = crate::doctor::run_on(
        &fx.paths,
        &crate::actions::Machine::at(&finds_nothing, fx.root.join("no-such-home")),
    );
    let report = serde_json::to_value(&report).unwrap();
    let layers = report["config"]["layers"]
        .as_array()
        .expect("doctor --json lists the config layers")
        .iter()
        .map(|layer| layer["layer"].as_str().expect("a layer's name").to_string())
        .collect();
    check("layer", layers);
}

// "answer it in pando.toml" named no key and no real path. Exit 3 now
// names the absolute file (under whatever home `PANDO_HOME` names), what
// the first option would be written as there, and the answers-file line
// a program would send instead.
#[test]
fn exit_three_names_the_absolute_file_the_key_and_the_answers_file() {
    let proposal = crate::detect::Proposal::of(
        crate::detect::Slot::DevCmd,
        vec![crate::detect::Candidate {
            value: "pnpm dev".to_string(),
            why: "package.json scripts.dev".to_string(),
            ..Default::default()
        }],
        false,
    );
    let mut question = actions::question_for(&proposal, &[]);
    question.answer_file = Some(std::path::PathBuf::from(
        "/somewhere/pando-home/projects/p-1/pando.toml",
    ));
    let text = render_needs_answer(&actions::NeedsAnswer { question });
    for wanted in [
        "/somewhere/pando-home/projects/p-1/pando.toml",
        "[dev]",
        "cmd = \"pnpm dev\"",
        "pando init --answers",
        "{\"dev_cmd\": \"pnpm dev\"}",
    ] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }
}

// The port question's typed answer is variable names, not a command, and
// a choice is echoed so the transcript shows what was taken.
#[test]
fn the_port_question_asks_for_variable_names_and_echoes_the_choice() {
    let mut question = dev_question(&["WEB_PORT", "PORT"]);
    question.slot = crate::detect::Slot::PortEnv;
    let (answer, printed) = answer_with(&question, &["2"]);
    assert_eq!(answer.unwrap(), actions::Answer::Choice(1));
    assert!(printed.contains("type the variable names"), "{printed}");
    assert!(printed.contains("→ PORT"), "{printed}");
}

// A CJK branch takes two columns a character; sized by characters, its
// row pushed STATUS and everything after it out of line.
#[test]
fn a_wide_branch_name_keeps_the_ls_columns_straight() {
    let fx = fixture();
    actions::new(
        &fx.paths,
        &fx.config,
        "feat/日本語のブランチ",
        None,
        &|_| {},
    )
    .unwrap();
    actions::new(&fx.paths, &fx.config, "feat/ascii-branch", None, &|_| {}).unwrap();
    let text = capture(|b| ls_text_at(&fx.paths, b, usize::MAX));
    let status_column = |needle: &str| {
        let line = text.lines().find(|l| l.contains(needle)).unwrap();
        let at = line.find("stopped").unwrap();
        crate::term::text_width(&line[..at])
    };
    assert_eq!(
        status_column("日本語"),
        status_column("ascii-branch"),
        "{text}"
    );
}

// ---- the namespace login question -----------------------------------------------

fn login_question_for_tests() -> actions::Question {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    crate::testutil::init_repo(&root);
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        ProjectRef::from_root(&root).unwrap(),
    );
    actions::login_question(&paths, "mariadb", &["DATABASE_PORT".to_string()])
}

// Nothing to choose: the prompt asks for the login outright, says it is
// not shown, and takes the whole line — colons and all — as the answer.
#[test]
fn the_login_question_asks_for_a_login_that_is_not_shown_as_it_is_typed() {
    let question = login_question_for_tests();
    let (answer, printed) = answer_with(&question, &["root:p@ss:word"]);
    assert_eq!(
        answer.unwrap(),
        actions::Answer::Custom("root:p@ss:word".to_string())
    );
    assert!(printed.contains("not shown as you type"), "{printed}");
    assert!(printed.contains("user:password"), "{printed}");
    assert!(!printed.contains("c) something else"), "{printed}");
    assert!(
        !printed.contains("p@ss"),
        "the prompt never prints it back: {printed}"
    );
}

// A script gets exit 3 with the table to write and the file to write it
// in — and no answers-file line, because `init` never asks this one.
#[test]
fn the_login_question_at_exit_3_names_the_table_to_write_and_not_an_answers_file() {
    let question = login_question_for_tests();
    let file = question.answer_file.clone().unwrap();
    let text = render_needs_answer(&actions::NeedsAnswer { question });
    for wanted in [
        "[namespaced.mariadb]",
        "user = \"<user>\"",
        "password = \"<password>\"",
        file.to_str().unwrap(),
    ] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }
    assert!(!text.contains("init --answers"), "{text}");
}

// A namespace login lives in pando's own config; nothing `status` prints
// reads it, and nothing it prints may carry it.
//
// The worktree runs namespaced and its config declares a service pando
// cannot reach, so the one path in `status` that reads the config — the
// namespace lines, and the shared service's reason among them — runs.
#[test]
fn status_never_prints_a_namespace_login() {
    use crate::state::ServiceMode;
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    std::fs::write(
        fx.paths.config_file(),
        "[[services]]\nkind = \"native\"\nname = \"mariadb\"\n\n\
         [namespaced.mariadb]\nuser = \"root\"\npassword = \"hunter2\"\n",
    )
    .unwrap();
    with_namespaces(&fx, &name, ServiceMode::Namespaced);
    let json = capture(|b| status_json(&fx.paths, None, b));
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    assert!(
        text.lines()
            .any(|l| l.contains("mariadb") && l.contains("shared")),
        "the namespace lines ran with the config: {text}"
    );
    for shown in [&json, &text] {
        assert!(!shown.contains("hunter2"), "{shown}");
    }
}

#[test]
fn an_answers_file_cannot_answer_the_login_a_namespaced_start_asks_for() {
    let e = crate::cli::answers::Answers::parse(r#"{"login": "root:hunter2"}"#).unwrap_err();
    let e = format!("{e:#}");
    assert!(e.contains("not a question pando asks"), "{e}");
    assert!(!e.contains("hunter2"), "{e}");
}

// A setup agent answers how each service gets data of a namespaced
// worktree's own — an object of services, and never a login in it.
#[test]
fn an_answers_file_says_namespaced_settings_as_an_object_and_never_a_login() {
    let parse = crate::cli::answers::Answers::parse;
    assert!(
        parse(
            r#"{"namespaced": {"search": {"recipe": "elasticsearch",
                "prefix_env": ["SEARCH_INDEX_PREFIX"]}, "cache": {"db_env": ["REDIS_DB"]}}}"#
        )
        .is_ok()
    );
    for (json, says) in [
        (
            r#"{"namespaced": {"db": {"user": "root", "password": "hunter2"}}}"#,
            "a login is never an answer",
        ),
        (
            r#"{"namespaced": "elasticsearch"}"#,
            "an object of services",
        ),
        (r#"{"namespaced": null}"#, "an object of services"),
        (r#"{"namespaced": {}}"#, "name at least one service"),
        (
            r#"{"namespaced": {"db": {"db_env": "REDIS_DB"}}}"#,
            "is not a service's settings",
        ),
        (
            r#"{"namespaced": {"db": {"schema": "x"}}}"#,
            "is not a service's settings",
        ),
    ] {
        let e = format!("{:#}", parse(json).unwrap_err());
        assert!(e.contains(says), "{json}: {e}");
        assert!(!e.contains("hunter2"), "{e}");
    }
}

// A slot to free has nothing to write down and nothing `--yes` may take:
// a script is told how a person answers it, and what it can do instead.
#[test]
fn the_slot_question_at_exit_3_says_how_it_is_answered_and_offers_no_flag() {
    let question = actions::Question {
        slot: crate::detect::Slot::FreeSlot,
        prompt: "Every slot of redis on 127.0.0.1:6379 is held. Which stopped worktree gives up \
                 its slot?"
            .into(),
        options: vec![("feat+old".into(), "slot 3, last ran 4 days ago".into())],
        preselect: None,
        allow_custom: false,
        allow_none: true,
        multi: false,
        checked: Vec::new(),
        details: vec!["the one chosen has its slot emptied".into()],
        answer_file: None,
        snippet: String::new(),
    };
    assert!(actions::recommended(&question).is_none());
    let text = render_needs_answer(&actions::NeedsAnswer {
        question: question.clone(),
    });
    for wanted in ["feat+old", "slot 3, last ran 4 days ago", "pando rm"] {
        assert!(text.contains(wanted), "{wanted}: {text}");
    }
    for unwanted in ["--yes", "init --answers", "pando.toml"] {
        assert!(!text.contains(unwanted), "{unwanted}: {text}");
    }
    let (answer, printed) = answer_with(&question, &["n"]);
    assert_eq!(answer.unwrap(), actions::Answer::None);
    assert!(printed.contains("free nothing"), "{printed}");
}

// ---- namespaces in status ---------------------------------------------------------

fn with_namespaces(fx: &Fx, name: &str, mode: crate::state::ServiceMode) {
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let record = store.worktrees.get_mut(name).unwrap();
    record.mode = Some(mode);
    for (service, kind, ns, host, port) in [
        (
            "mariadb",
            crate::state::NamespaceKind::Database,
            "shop__feat_one",
            "localhost",
            3306,
        ),
        (
            "redis",
            crate::state::NamespaceKind::Slot,
            "3",
            "127.0.0.1",
            6379,
        ),
    ] {
        record.namespaces.push(crate::state::NamespaceRecord {
            service: service.into(),
            recipe: service.into(),
            kind,
            host: host.into(),
            port,
            name: ns.into(),
            main: "shop".into(),
            mains: Vec::new(),
            keys: Vec::new(),
            used_at: Utc::now(),
        });
    }
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
}

// Each namespace a worktree holds is in `status --json`, database by name
// and slot by number, on the server it was made on — and whether the
// worktree runs on it now or keeps it for the way back.
#[test]
fn status_json_lists_the_namespaces_a_worktree_holds() {
    use crate::state::ServiceMode;
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_namespaces(&fx, &name, ServiceMode::Namespaced);
    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let namespaces = &v["worktrees"][0]["namespaces"];
    assert_eq!(namespaces[0]["service"], "mariadb");
    assert_eq!(namespaces[0]["database"], "shop__feat_one");
    assert!(namespaces[0].get("slot").is_none());
    assert_eq!(namespaces[0]["host"], "localhost");
    assert_eq!(namespaces[0]["port"], 3306);
    assert_eq!(namespaces[0]["in_use"], true);
    assert_eq!(namespaces[1]["slot"], 3);
    assert!(namespaces[1].get("database").is_none());

    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store.worktrees.get_mut(&name).unwrap().mode = Some(ServiceMode::Shared);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let text = capture(|b| status_json(&fx.paths, None, b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["worktrees"][0]["namespaces"][0]["in_use"], false);

    // Every field is in the contract, and the contract says them.
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    for field in ["\"namespaces\"", "\"database\"", "\"slot\"", "\"in_use\""] {
        assert!(doc.contains(field), "agent/json.md never mentions {field}");
    }
}

// `status` says per service what the worktree holds: its own database and
// slot while it runs namespaced, and the same, kept until `rm`, once it
// runs in another mode.
#[test]
fn status_text_says_what_a_worktree_holds_in_each_service() {
    use crate::state::ServiceMode;
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    with_namespaces(&fx, &name, ServiceMode::Namespaced);
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    assert!(
        text.lines().any(|l| l.contains("mariadb")
            && l.contains("own")
            && l.contains("database shop__feat_one on localhost:3306")),
        "{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.contains("redis") && l.contains("slot 3 on 127.0.0.1:6379")),
        "{text}"
    );

    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store.worktrees.get_mut(&name).unwrap().mode = Some(ServiceMode::Shared);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let text = capture(|b| status_text_at(&fx.paths, None, b, usize::MAX));
    assert!(
        text.lines()
            .any(|l| l.contains("kept") && l.contains("shop__feat_one") && l.contains("until rm")),
        "{text}"
    );
}

#[test]
fn three_flags_mean_three_modes_and_none_is_the_remembered_one() {
    use actions::Mode;
    assert_eq!(Mode::of(false, false, false), Mode::Remembered);
    assert_eq!(Mode::of(true, false, false), Mode::Isolated);
    assert_eq!(Mode::of(false, true, false), Mode::Namespaced);
    assert_eq!(Mode::of(false, false, true), Mode::Shared);
}

// `pando check --json`.

/// The `pando check --json` section of the contract.
fn check_section() -> String {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    doc.split("## `pando check --json`")
        .nth(1)
        .expect("the check section")
        .split("\n## ")
        .next()
        .unwrap()
        .to_string()
}

/// A finished check with every field the shape has filled in.
fn full_check_record() -> crate::setup::CheckRecord {
    use crate::setup::{CheckOutcome, CheckRecord, FailureKind, ProcessResult, RanBy};
    let mut record = CheckRecord::begin("a".to_string(), RanBy::Program);
    record.finished_at = Some(Utc::now());
    record.fingerprint_after = Some("b".to_string());
    record.commit = Some("a1b2c3d4e5".to_string());
    record.base_ref = Some("origin/main".to_string());
    record.outcome = CheckOutcome::Failed {
        kind: FailureKind::Settings,
        reason: "web answered its first page with HTTP 500".to_string(),
    };
    record.processes = vec![ProcessResult {
        name: "web".to_string(),
        ready: false,
        port: Some(17008),
        http_status: Some(500),
        secs: 1.25,
    }];
    record.failed_process = Some("web".to_string());
    record.failed_tail = vec!["Error: boom".to_string()];
    record.notes = vec!["skipped the hooks that run after services (migrate)".to_string()];
    record
}

// Every key `check --json` prints is one a program reads, so the contract
// names each of them, the process entries' too.
#[test]
fn agent_json_documents_every_check_key() {
    let fx = fixture();
    let section = check_section();
    let record = full_check_record();
    let printed = serde_json::to_value(super::check::check_json(&fx.paths, &record)).unwrap();
    let process = printed["processes"][0].clone();
    for object in [&printed, &process] {
        for key in object.as_object().unwrap().keys() {
            assert!(
                section.contains(&format!("\"{key}\"")),
                "agent/json.md never documents check.{key}"
            );
        }
    }
    assert_eq!(printed["version"], JSON_VERSION);
    assert_eq!(printed["result"], "failed");
    assert_eq!(printed["kind"], "settings");
    assert_eq!(printed["settings_changed"], true, "a != b");
    assert_eq!(printed["mode"], "shared");
    assert_eq!(process["secs"], 1.3, "tenths of a second");
    // The wait it documents is the one the probe waits.
    let wait = format!("{} seconds", crate::ports::PAGE_WAIT.as_secs());
    assert!(section.contains(&wait), "the section says {wait}");
}

// The words `result`, `kind` and `ran_by` take in the check's section are
// the ones the binary prints, and no others.
#[test]
fn every_check_value_agent_json_documents_is_one_the_binary_prints() {
    use crate::setup::{CheckMode, CheckOutcome, FailureKind, RanBy};
    let section = check_section();
    let fx = fixture();
    let mut record = full_check_record();
    let mut results: Vec<String> = Vec::new();
    for outcome in [
        CheckOutcome::Passed,
        CheckOutcome::Failed {
            kind: FailureKind::Machine,
            reason: String::new(),
        },
        CheckOutcome::NotSetUp {
            slot: "dev_cmd".to_string(),
        },
        CheckOutcome::Interrupted,
        CheckOutcome::Running,
    ] {
        record.outcome = outcome;
        let printed = serde_json::to_value(super::check::check_json(&fx.paths, &record)).unwrap();
        let result = printed["result"].as_str().unwrap().to_string();
        if !results.contains(&result) {
            results.push(result);
        }
    }
    results.sort();
    let mut known: Vec<String> = super::check::RESULTS
        .iter()
        .map(|r| r.to_string())
        .collect();
    known.sort();
    assert_eq!(results, known);
    assert_eq!(documented(&section, "result"), known);
    let kinds = [
        FailureKind::Settings,
        FailureKind::Machine,
        FailureKind::Base,
    ];
    let mut kinds: Vec<String> = kinds.into_iter().map(serde_word).collect();
    kinds.sort();
    assert_eq!(documented(&section, "kind"), kinds);
    let mut ran_by: Vec<String> = [RanBy::Tui, RanBy::Terminal, RanBy::Program]
        .into_iter()
        .map(serde_word)
        .collect();
    ran_by.sort();
    assert_eq!(documented(&section, "ran_by"), ran_by);
    let modes = [CheckMode::Shared, CheckMode::Namespaced];
    for m in modes {
        match m {
            CheckMode::Shared | CheckMode::Namespaced => {}
        }
    }
    let mut modes: Vec<String> = modes.into_iter().map(serde_word).collect();
    modes.sort();
    assert_eq!(documented(&section, "mode"), modes);
    record.mode = CheckMode::Namespaced;
    let printed = serde_json::to_value(super::check::check_json(&fx.paths, &record)).unwrap();
    assert_eq!(printed["mode"], "namespaced");
}

// The check's throwaway worktree is hidden where worktrees are listed for
// a person, and a name cannot reach it; `doctor` still sees it.
#[test]
fn a_checks_worktree_is_in_no_list_and_answers_to_no_name() {
    let fx = fixture();
    let name = actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let check = fx.config.check_worktree_path(&fx.paths);
    git(
        &fx.root,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            check.to_str().unwrap(),
            "HEAD",
        ],
    );
    let listed = crate::worktree::discover(&fx.paths.project).unwrap();
    assert!(
        listed.iter().any(|w| crate::worktree::is_check(&w.name)),
        "discovery still finds it: a start needs it"
    );

    let ls: Vec<String> = actions::ls(&fx.paths)
        .unwrap()
        .into_iter()
        .map(|w| w.name)
        .collect();
    assert_eq!(ls, vec![name.clone()]);
    let names = capture(|b| super::completion::names(&fx.paths, b));
    assert!(!names.contains(crate::paths::CHECK_WORKTREE), "{names}");
    assert!(names.contains("feat/one"), "{names}");
    let status = capture(|b| status_json(&fx.paths, None, b));
    assert!(!status.contains(crate::paths::CHECK_WORKTREE), "{status}");
    let text = capture(|b| status_text(&fx.paths, None, b));
    assert!(!text.contains(crate::paths::CHECK_WORKTREE), "{text}");

    let err = super::names::resolve(&fx.paths, crate::paths::CHECK_WORKTREE).unwrap_err();
    assert!(
        format!("{err:#}").starts_with("no worktree named"),
        "{err:#}"
    );
    // Nor by its logs, which the check keeps, nor by a probe's.
    for logs in crate::paths::CHECK_LOG_DIRS {
        std::fs::create_dir_all(fx.paths.logs_dir(logs)).unwrap();
        assert!(super::names::resolve(&fx.paths, logs).is_err(), "{logs}");
    }

    let finds_nothing = |_: &str| None;
    let report = crate::doctor::run_on(
        &fx.paths,
        &crate::actions::Machine::at(&finds_nothing, fx.root.join("no-such-home")),
    );
    assert!(
        report
            .findings
            .iter()
            .any(|f| f.message.contains("a `pando check` that did not finish")),
        "{:?}",
        report.findings
    );
}

// ---- init --agent -----------------------------------------------------

// The binary carries the brief and the contract so `--reference` prints
// what this pando was built with. An embedded copy that drifted from the
// file would be a second brief, which is the one thing the agent layer
// is built never to have.
#[test]
fn the_embedded_brief_and_contract_are_the_files() {
    let read = |file: &str| {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"))
    };
    assert_eq!(super::agent::BRIEF, read("agent/brief.md"));
    assert_eq!(super::agent::JSON_CONTRACT, read("agent/json.md"));
}

// The job's rules and steps are the brief's first-run section, printed
// from it: one copy. Every step the plan gives an agent is in it, and
// nothing of the brief past it.
#[test]
fn the_job_carries_the_briefs_first_run_section_and_only_that() {
    let section = super::agent::first_run_section();
    assert!(section.starts_with("## First run"), "{section}");
    let flat = section.split_whitespace().collect::<Vec<_>>().join(" ");
    for (phrase, why) in [
        (
            "`pando signals` and `pando doctor --json`",
            "where it starts",
        ),
        ("Do not re-derive", "that the evidence is not re-derived"),
        // A setup asks the developer nothing, start to end: the
        // maintainer's call, after the first real run asked about apps
        // and services, and again on 2026-10-05 for every question left.
        (
            "Ask the developer nothing, from the first command to the last",
            "that the developer is not asked",
        ),
        (
            "Never end on a question",
            "that the done message asks nothing either",
        ),
        (
            "What I set, in `~/.pando/projects/<id>/pando.toml`",
            "that the report lists what was set",
        ),
        (
            "To change any of it, tell me, or run > `pando init --answers - --replace`",
            "how the developer changes a setting",
        ),
        ("`pando init --yes`", "pando's choices saved in one step"),
        (
            "pando runs every app it found a command for",
            "the apps question settled, not asked",
        ),
        // Both first runs of issue-shaped fixtures ended green with an
        // app left out: an app with no dev script is the agent's to add.
        (
            "`app_dirs` that no process covers",
            "that an app pando found no command for is the agent's to add",
        ),
        (
            "object of process tables",
            "how several processes are answered",
        ),
        ("{port:<role>}", "how a process finds another's address"),
        (
            "What the docs do not say, decide from `signals`",
            "that the agent decides what the docs leave open",
        ),
        (
            "the first line `pando doctor --json` lists as the fix",
            "that the agent answers the runtime line itself",
        ),
        (
            "Answer `base` with the main checkout's branch",
            "that the agent answers the base itself",
        ),
        ("pando init --answers - --dry-run", "the preview"),
        (
            "through stdin, never an answers file",
            "that answers never go in a file",
        ),
        ("at least 10 minutes", "the check's timeout"),
        ("consent to this test", "that the prompt is consent"),
        ("never rerun unchanged", "fix, then rerun"),
        (
            "pando init --answers - --replace",
            "how a setting is corrected",
        ),
        ("kind: \"machine\"", "whose a machine failure is"),
        ("three changed attempts", "when to stop"),
        (
            "pando is set up and tested for <project>. > You're ready: run `pando`.",
            "the two lines that say it is done",
        ),
        ("Change no file", "the rule about the repository"),
        ("Remember how to run it", "that the agent keeps the block"),
        (
            "`pando init --agent --reference memory`",
            "where the block is printed alone",
        ),
        ("`~/.claude/CLAUDE.md`", "where Claude Code keeps it"),
        ("`~/.codex/AGENTS.md`", "where Codex keeps it"),
        (
            "replacing an earlier pando block for the same project root",
            "that a second setup replaces the first block",
        ),
        (
            "Never a `CLAUDE.md`, `AGENTS.md` or any other file inside the repository",
            "that the block never goes in the repository",
        ),
        // The memory file is the developer's, read in every session:
        // the maintainer's call, after agents wrote it without asking.
        (
            "write nothing to it without their yes",
            "that the block is saved only on a yes",
        ),
        (
            "I can also save how to run it > with pando to `~/.claude/CLAUDE.md`",
            "the offer the done message makes without asking",
        ),
        ("Until then, write nothing", "that nothing is saved unasked"),
    ] {
        assert!(
            flat.contains(phrase),
            "the job never says {why}: {phrase:?}\n{section}"
        );
    }
    assert!(
        !section.contains("## 0."),
        "the section runs on into the brief's next one:\n{section}"
    );
    // The job prints this section and no other, so a pointer to one of
    // the brief's numbered sections points at nothing the reader has.
    assert!(
        !section.contains('§'),
        "the section sends its reader to a part of the brief the job does not print:\n{section}"
    );

    let fx = fixture();
    let job = super::agent::job(&fx.paths);
    assert!(
        job.starts_with(&format!(
            "# Set up pando for acme-shop (pando {})\n",
            env!("CARGO_PKG_VERSION")
        )),
        "{job}"
    );
    assert!(job.contains(section.trim_end()), "{job}");
    assert!(
        job.contains("`pando init --agent --reference brief`"),
        "{job}"
    );
    assert!(
        job.contains("`pando init --agent --reference json`"),
        "{job}"
    );
    // What each command writes, with this project's own paths.
    assert!(
        job.contains(&fx.paths.project_dir().display().to_string()),
        "{job}"
    );
    assert!(job.contains("Codex's workspace-write"), "{job}");
}

/// A setup in `state` whose last check ended with `outcome`.
fn setup_after(
    state: crate::setup::SetupState,
    outcome: crate::setup::CheckOutcome,
    tail: &[&str],
) -> crate::setup::Setup {
    let mut record =
        crate::setup::CheckRecord::begin("today".to_string(), crate::setup::RanBy::Program);
    record.outcome = outcome;
    record.failed_tail = tail.iter().map(|line| line.to_string()).collect();
    crate::setup::Setup {
        state,
        last_check: Some(record),
        memory: crate::setup::SetupMemory::default(),
        fingerprint: "today".to_string(),
    }
}

// After a failed check the same prompt brings the failure to the agent:
// the reason, the failed process's last lines as the check recorded them,
// and whose it is to fix — a settings failure the agent's, a machine one
// the developer's.
#[test]
fn a_failed_check_leads_with_its_reason_its_last_lines_and_whose_it_is() {
    use crate::setup::{CheckOutcome, FailureKind, SetupState};
    let failed = |kind| CheckOutcome::Failed {
        kind,
        reason: "web exited after 0.8s".to_string(),
    };
    let text = super::agent::last_check(&setup_after(
        SetupState::Failing,
        failed(FailureKind::Settings),
        &["Error: Cannot find module 'dotenv'"],
    ))
    .expect("a failure is said");
    assert!(
        text.starts_with("## The last test failed\n\nweb exited after 0.8s\n"),
        "{text}"
    );
    assert!(
        text.contains("    Error: Cannot find module 'dotenv'\n"),
        "{text}"
    );
    assert!(
        text.contains("`pando init --answers - --replace`"),
        "{text}"
    );
    assert!(text.contains("never rerun it unchanged"), "{text}");

    let text = super::agent::last_check(&setup_after(
        SetupState::Failing,
        failed(FailureKind::Machine),
        &[],
    ))
    .unwrap();
    assert!(text.contains("the developer's"), "{text}");
    assert!(text.contains("change no setting"), "{text}");
    assert!(!text.contains("--replace"), "{text}");
    assert!(!text.contains("Its last lines"), "{text}");

    // The base's is the developer's too, and the trap is named: a looser
    // install would pass on the wrong commit.
    let text = super::agent::last_check(&setup_after(
        SetupState::Failing,
        failed(FailureKind::Base),
        &["error: Unable to find lockfile at `uv.lock`"],
    ))
    .unwrap();
    assert!(text.contains("`kind: \"base\"`"), "{text}");
    assert!(
        text.contains("never drop a frozen install's flag"),
        "{text}"
    );
    assert!(text.contains("answer `base` with it"), "{text}");
    // `--replace` only for a base already answered, which a plain answer
    // would be refused for; never the settings' "correct them" advice.
    assert!(
        text.contains("with `--replace` if `base` is already answered"),
        "{text}"
    );
    assert!(!text.contains("correct them"), "{text}");
    assert!(
        text.lines()
            .filter(|line| !line.starts_with("    "))
            .all(|line| !line.contains("  ")),
        "no run of spaces inside a sentence: {text}"
    );

    let text = super::agent::last_check(&setup_after(
        SetupState::Failing,
        CheckOutcome::NotSetUp {
            slot: "dev_cmd".to_string(),
        },
        &[],
    ))
    .unwrap();
    assert!(text.contains("`dev_cmd` has no answer"), "{text}");
}

// The other states get a line, and a project never checked gets none:
// the job itself is the whole story there.
#[test]
fn every_other_check_state_is_one_line_or_nothing() {
    use crate::setup::{CheckOutcome, SetupState};
    for (state, outcome, says) in [
        (
            SetupState::Testing,
            CheckOutcome::Running,
            "A test is running now",
        ),
        (
            SetupState::Interrupted,
            CheckOutcome::Interrupted,
            "never finished",
        ),
        (
            SetupState::Stale,
            CheckOutcome::Passed,
            "settings changed since the last test",
        ),
        (
            SetupState::Ready,
            CheckOutcome::Passed,
            "The last test passed",
        ),
    ] {
        let text = super::agent::last_check(&setup_after(state, outcome, &[]))
            .unwrap_or_else(|| panic!("{state:?} says nothing"));
        assert!(text.contains(says), "{state:?}: {text}");
        assert!(
            !text.contains("## The last test failed"),
            "{state:?}: {text}"
        );
    }
    let never = crate::setup::Setup {
        state: SetupState::Untested,
        last_check: None,
        memory: crate::setup::SetupMemory::default(),
        fingerprint: "today".to_string(),
    };
    assert_eq!(super::agent::last_check(&never), None);
}

// `--agent` prints; it never answers, writes or previews a write, so
// the flags that do are refused beside it, and `--reference` means
// nothing without it.
#[test]
fn init_agent_refuses_the_flags_that_answer_or_write() {
    for extra in [&["--yes"][..], &["--dry-run"][..], &["--answers", "-"][..]] {
        let mut argv = vec!["pando", "init", "--agent"];
        argv.extend_from_slice(extra);
        let err = Cli::try_parse_from(&argv).unwrap_err();
        assert_eq!(
            err.kind(),
            clap::error::ErrorKind::ArgumentConflict,
            "{argv:?}"
        );
    }
    assert!(
        Cli::try_parse_from(["pando", "init", "--agent", "--answers", "-", "--replace"]).is_err()
    );
    let err = Cli::try_parse_from(["pando", "init", "--reference", "brief"]).unwrap_err();
    assert_eq!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument);
    for doc in ["brief", "json", "memory"] {
        Cli::try_parse_from(["pando", "init", "--agent", "--reference", doc])
            .unwrap_or_else(|e| panic!("--reference {doc}: {e}"));
    }
}

// A config pando cannot read is the first thing the job reports, so the
// command that reports it cannot be one a broken config stops.
#[test]
fn init_agent_runs_on_a_config_pando_cannot_read() {
    let command = |argv: &[&str]| Cli::try_parse_from(argv).unwrap().command.unwrap();
    assert!(!command(&["pando", "init", "--agent"]).needs_config());
    assert!(!command(&["pando", "init", "--agent", "--reference", "json"]).needs_config());
    assert!(!command(&["pando", "init", "--agent", "--reference", "memory"]).needs_config());
    assert!(command(&["pando", "init"]).needs_config());
    assert!(command(&["pando", "init", "--answers", "-", "--dry-run"]).needs_config());
}

// `check --base` is a flag of its own, beside `--json`, as `new --base`
// is; the base names a branch, so it takes a value.
#[test]
fn check_takes_a_base_for_the_one_run() {
    let command = |argv: &[&str]| Cli::try_parse_from(argv).unwrap().command.unwrap();
    assert!(matches!(
        command(&["pando", "check", "--base", "dev", "--json"]),
        Command::Check { json: true, base: Some(ref base) } if base == "dev"
    ));
    assert!(matches!(
        command(&["pando", "check"]),
        Command::Check {
            json: false,
            base: None
        }
    ));
    assert!(Cli::try_parse_from(["pando", "check", "--base"]).is_err());
}

// With the base open, `check` exits 3 with that question, through `init`
// and back, and `--json` still prints the result: `not_set_up`, at
// `base`.
#[test]
fn check_with_the_base_open_exits_with_the_base_question() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::drifted_repo(
        &root,
        crate::worktree::FAR_AHEAD,
        crate::worktree::STALE_DAYS,
        None,
    );
    let project = ProjectRef::from_root(&root).unwrap();
    let paths = PandoPaths::new(dir.path().join("pando-home"), project);
    std::fs::create_dir_all(paths.project_dir()).unwrap();
    std::fs::write(paths.config_file(), "[dev]\ncmd = \"exit 3\"\nports = []\n").unwrap();
    let config = crate::config::load(&paths).unwrap().config;

    let mut out = Vec::new();
    let err = check::check(&paths, &config, true, None, &mut out).unwrap_err();
    let needs = err.downcast_ref::<CheckNeedsAnswer>().expect("exit 3");
    assert_eq!(needs.0.question.slot, crate::detect::Slot::Base);
    let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(v["result"], "not_set_up");
    assert_eq!(v["slot"], "base");
    let text = render_needs_answer_for(&needs.0, Rerun::InitThenCheck);
    assert!(text.contains("then run `pando check` again"), "{text}");
}

/// What the first-time tip said, line by line, and whether it said it.
fn tip_lines(fx: &Fx, config: &Config, terminal: bool) -> (bool, Vec<String>) {
    let (shown, said, _) = tip_with_picture(fx, config, terminal);
    (shown, said)
}

/// The tip's lines, and the picture drawn above them.
fn tip_with_picture(fx: &Fx, config: &Config, terminal: bool) -> (bool, Vec<String>, Vec<String>) {
    let said = std::cell::RefCell::new(Vec::new());
    let drawn = std::cell::RefCell::new(Vec::new());
    let shown = super::tip::first_time_tip(
        &fx.paths,
        config,
        terminal,
        &|line: &str| said.borrow_mut().push(line.to_string()),
        &|line: &str| drawn.borrow_mut().push(line.to_string()),
    );
    (shown, said.into_inner(), drawn.into_inner())
}

// The CLI's banner is the TUI's pictures in sixteen colours: the PANDO
// wordmark over the grove, gold letters and leaves, the roots dark while
// the setup waits and lit green once a check passed; plain text with
// colour off, and nothing on a terminal too narrow for it.
#[test]
fn the_cli_banner_is_painted_for_its_moment() {
    let seed = crate::art::seed_of("acme-shop");
    let colour = crate::term::Style::with(true, None);
    let waiting = super::art::banner_lines(80, seed, false, &colour).concat();
    let alive = super::art::banner_lines(80, seed, true, &colour).concat();
    assert!(waiting.contains("\x1b[33m"), "gold letters and leaves");
    assert!(!waiting.contains("\x1b[32m"), "no green before it is ready");
    assert!(alive.contains("\x1b[32m"), "the roots lit once it is");
    let plain = super::art::banner_lines(80, seed, true, &crate::term::Style::plain());
    assert!(plain.iter().all(|l| !l.contains('\x1b')));
    // The big wordmark, the credit, a blank row, then the grove.
    let mark = crate::art::to_text(&crate::art::wordmark(crate::art::WordmarkSize::Big, 0));
    for (line, row) in plain.iter().zip(mark.lines()) {
        assert_eq!(line, &format!("  {row}").trim_end().to_string());
    }
    // Who made it, under the wordmark, then a blank row.
    assert_eq!(
        plain[crate::art::WORDMARK_HEIGHT],
        format!("  {}", crate::art::credit())
    );
    assert_eq!(plain[crate::art::WORDMARK_HEIGHT + 1], "");
    assert!(plain.last().unwrap().contains('┻'), "{plain:?}");
    // Narrower: the compact wordmark; too narrow: nothing.
    let narrow = super::art::banner_lines(40, seed, false, &crate::term::Style::plain());
    assert!(narrow[0].contains("█▀█ ▄▀█"), "{narrow:?}");
    assert!(super::art::banner_lines(20, seed, false, &colour).is_empty());
}

// A first run from the CLI opens on the same picture as one from the
// TUI: the project's grove, above the three lines, and only then.
#[test]
fn the_first_time_tip_opens_on_the_projects_grove() {
    let fx = fixture();
    let (shown, said, drawn) = tip_with_picture(&fx, &fx.config, true);
    assert!(shown);
    assert_eq!(said.len(), 3, "{said:?}");
    let picture: Vec<&String> = drawn.iter().filter(|l| !l.is_empty()).collect();
    assert!(picture.len() >= crate::art::GROVE_MIN_HEIGHT, "{drawn:?}");
    let all = picture.iter().map(|l| l.as_str()).collect::<String>();
    for shade in ['█', '░'] {
        assert!(all.contains(shade), "{drawn:?}");
    }
    // Tests' stderr is no terminal: the picture comes plain, no escapes.
    assert!(!all.contains('\x1b'), "{drawn:?}");
    // Said once: the second run draws nothing either.
    let (shown, said, drawn) = tip_with_picture(&fx, &fx.config, true);
    assert!(!shown && said.is_empty() && drawn.is_empty());
    // And never on a pipe.
    let fx = fixture();
    let (_, _, drawn) = tip_with_picture(&fx, &fx.config, false);
    assert!(drawn.is_empty());
}

#[test]
fn the_first_time_tip_is_said_once_on_a_terminal_and_remembered() {
    let fx = fixture();
    let (shown, said) = tip_lines(&fx, &fx.config, true);
    assert!(shown);
    assert_eq!(
        said,
        [
            "first time in acme-shop. To set it up with your coding agent, paste:".to_string(),
            format!("  {}", crate::setup::SETUP_PROMPT),
            "`pando check` tests the setup at any time.".to_string(),
        ]
    );
    let memory = crate::setup::SetupMemory::load(&fx.paths);
    assert!(memory.tip_shown_at.is_some(), "{memory:?}");

    assert_eq!(tip_lines(&fx, &fx.config, true), (false, Vec::new()));
}

// The tip tells a person what to run; a renamed verb or flag would leave
// it reading perfectly and pointing nowhere.
#[test]
fn the_first_time_tip_only_names_commands_pando_has() {
    let fx = fixture();
    let (_, said) = tip_lines(&fx, &fx.config, true);
    assert_every_command_is_real("the first-time tip", &said.join("\n"));
}

// A developer who pressed esc on the setup screen skipped that screen, not
// the tip.
#[test]
fn the_first_time_tip_is_said_after_the_setup_screen_was_skipped() {
    let fx = fixture();
    crate::setup::SetupMemory {
        skipped_at: Some(Utc::now()),
        ..Default::default()
    }
    .save(&fx.paths)
    .unwrap();
    assert!(tip_lines(&fx, &fx.config, true).0);
    let memory = crate::setup::SetupMemory::load(&fx.paths);
    assert!(memory.skipped_at.is_some(), "the skip is kept: {memory:?}");
    assert!(memory.tip_shown_at.is_some(), "{memory:?}");
}

#[test]
fn the_first_time_tip_is_never_said_on_a_pipe_and_writes_nothing() {
    let fx = fixture();
    assert_eq!(tip_lines(&fx, &fx.config, false), (false, Vec::new()));
    assert!(!fx.paths.setup_file().exists());
    assert!(!fx.paths.project_dir().exists());
    // Not used up by the script, either: the person gets it later.
    assert!(tip_lines(&fx, &fx.config, true).0);
}

#[test]
fn the_first_time_tip_is_not_said_for_a_project_with_something_to_run() {
    let fx = fixture();
    assert_eq!(
        tip_lines(&fx, &with_dev(&fx.config), true),
        (false, Vec::new())
    );
    assert!(!fx.paths.setup_file().exists());
}

// A tip that cannot be remembered would be said on every run, so it is
// not said at all.
#[test]
fn the_first_time_tip_is_not_said_when_it_cannot_be_remembered() {
    let fx = fixture();
    std::fs::create_dir_all(fx.paths.setup_file()).unwrap();
    assert_eq!(tip_lines(&fx, &fx.config, true), (false, Vec::new()));
}

// The tip is advice, never a gate: `new` and `start` on a new project take
// pando's first choices exactly as they did before it.
#[test]
fn new_and_start_still_work_on_a_new_project_after_the_tip() {
    let fx = fixture();
    std::fs::write(fx.root.join("Makefile"), "dev:\n\t@sleep 30\n").unwrap();
    git(&fx.root, &["add", "."]);
    git(&fx.root, &["commit", "--quiet", "-m", "makefile"]);
    assert!(tip_lines(&fx, &fx.config, true).0);

    let yes = super::prompt::asker(true);
    let config = actions::resolve_for_new(&fx.paths, &fx.config, &yes, &quiet).unwrap();
    let name = actions::new(&fx.paths, &config, "feat/tip", None, &quiet).unwrap();
    // The start that follows still loads a project with nothing to run,
    // and says nothing: the tip was said once.
    assert!(!tip_lines(&fx, &fx.config, true).0);
    let config = actions::resolve_for_start(
        &fx.paths,
        &config,
        &name,
        actions::Mode::of(false, false, false),
        &yes,
        &quiet,
    )
    .unwrap();
    // The Makefile's one-line recipe, run as itself: no make needed.
    assert_eq!(config.processes["dev"].cmd, "sleep 30");
    let report = actions::start(
        &fx.paths,
        &config,
        &name,
        None,
        actions::Mode::of(false, false, false),
        &quiet,
    )
    .unwrap();
    actions::stop(&fx.paths, &name, None, &quiet).unwrap();
    assert!(!report.started_nothing());
}

// ---- the main checkout -------------------------------------------------

/// One section of the contract, by its heading.
fn contract_section(heading: &str) -> String {
    let doc = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"),
    )
    .unwrap();
    doc.split(heading)
        .nth(1)
        .unwrap_or_else(|| panic!("agent/json.md has no {heading}"))
        .split("\n## ")
        .next()
        .unwrap()
        .to_string()
}

// Every key a worktree entry of `ls --json` and `status --json` prints is
// one the contract names — the main checkout's `main` among them — and
// the main checkout is the first entry of both once pando has run it.
#[test]
fn agent_json_documents_every_ls_and_status_worktree_key_and_main_comes_first() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    let mut main = crate::state::WorktreeRecord::new(&fx.root, false);
    main.ports.insert("web".to_string(), 17_342);
    store.worktrees.insert("acme-shop".to_string(), main);
    crate::state::save(&fx.paths.state_file(), &store).unwrap();

    for (heading, text) in [
        ("## `pando ls --json`", capture(|b| ls_json(&fx.paths, b))),
        (
            "## `pando status --json`",
            capture(|b| status_json(&fx.paths, None, b)),
        ),
    ] {
        let section = contract_section(heading);
        let v: serde_json::Value = serde_json::from_str(&text).unwrap();
        let listed = v["worktrees"].as_array().unwrap();
        assert_eq!(listed.len(), 2, "{text}");
        assert_eq!(listed[0]["name"], "acme-shop", "{text}");
        assert_eq!(listed[0]["main"], true, "{text}");
        assert_eq!(listed[1]["main"], false, "{text}");
        for entry in listed {
            for key in entry.as_object().unwrap().keys() {
                assert!(
                    section.contains(&format!("\"{key}\"")),
                    "agent/json.md never documents {heading} worktrees[].{key}"
                );
            }
        }
    }
}

// `status` lists the main checkout once pando has anything recorded for
// it, first and said to be it; asked about by name, always.
#[test]
fn status_shows_the_main_checkout_once_it_has_a_record() {
    let fx = fixture();
    actions::new(&fx.paths, &fx.config, "feat/one", None, &|_| {}).unwrap();
    let text = capture(|b| status_text_at(&fx.paths, None, b, 200));
    assert!(!text.contains("main checkout"), "{text}");
    let text = capture(|b| status_json(&fx.paths, Some("acme-shop"), b));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["worktrees"][0]["main"], true, "{text}");

    let mut store = crate::state::load(&fx.paths.state_file()).unwrap();
    store.worktrees.insert(
        "acme-shop".to_string(),
        crate::state::WorktreeRecord::new(&fx.root, false),
    );
    crate::state::save(&fx.paths.state_file(), &store).unwrap();
    let text = capture(|b| status_text_at(&fx.paths, None, b, 200));
    let first = text.lines().next().unwrap();
    assert!(first.starts_with("main (main checkout)"), "{text}");
    assert!(text.contains("feat/one"), "{text}");
}

// `PANDO_CHECK` is how a process knows it runs under `pando check`: said
// on the website beside the other variables every process gets, in the
// brief's step that runs the check, and in `check --help`.
#[test]
fn the_check_variable_is_documented_where_the_others_are() {
    let var = crate::actions::CHECK_ENV;
    let read = |file: &str| {
        std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file))
            .unwrap()
    };
    let site = read("site/index.html");
    let listed = site
        .lines()
        .find(|line| line.contains("<code>PANDO_NAME</code>"))
        .expect("the website lists the variables every process gets");
    assert!(
        listed.contains(&format!("<code>{var}=1</code>")),
        "{listed}"
    );
    assert!(read("agent/brief.md").contains(&format!("`{var}=1`")));
    let mut cli = Cli::command();
    let help = cli
        .find_subcommand_mut("check")
        .unwrap()
        .render_long_help()
        .to_string();
    assert!(help.contains(&format!("{var}=1")), "{help}");
}

mod update {
    use super::super::update::{check_text, updated_text};
    use crate::actions::{Install, Survey, Updated, Version};

    fn survey(running: &str, install: Install, latest: Result<&str, &str>) -> Survey {
        Survey {
            running: Version::parse(running).unwrap(),
            install,
            latest: latest
                .map(|l| Version::parse(l).unwrap())
                .map_err(str::to_string),
        }
    }

    fn brew() -> Install {
        Install::Homebrew {
            prefix: "/opt/homebrew".into(),
        }
    }

    #[test]
    fn check_says_what_is_out_and_the_command_that_would_install_it() {
        let text = check_text(&survey("0.8.1", brew(), Ok("0.9.0")));
        assert_eq!(
            text,
            "pando 0.8.1, installed with Homebrew\n\
             0.9.0 is out\n\
             update with: pando update   (runs /opt/homebrew/bin/brew upgrade \
             mertkaradayi/tap/pando)\n"
        );
        let text = check_text(&survey("0.8.1", brew(), Ok("0.8.1")));
        assert!(text.ends_with("that is the latest release\n"), "{text}");
        let text = check_text(&survey("0.9.0", brew(), Ok("0.8.1")));
        assert!(
            text.ends_with("newer than the latest release, 0.8.1\n"),
            "{text}"
        );
    }

    #[test]
    fn check_on_a_checkouts_build_says_how_to_update_it_there() {
        let install = Install::Checkout {
            root: "/src/pando".into(),
        };
        let text = check_text(&survey("0.8.1", install, Ok("0.9.0")));
        assert!(text.contains("0.9.0 is out\n"), "{text}");
        assert!(
            text.contains("built in the checkout at /src/pando"),
            "{text}"
        );
        assert!(!text.contains("update with"), "{text}");
    }

    #[test]
    fn an_update_says_where_it_went_or_why_it_went_nowhere() {
        let s = survey("0.8.1", brew(), Ok("0.9.0"));
        assert_eq!(
            updated_text(&s, &Updated::To(Version::parse("0.9.0").unwrap())).unwrap(),
            "pando 0.8.1 → 0.9.0\n"
        );
        let err = updated_text(&s, &Updated::Unchanged)
            .unwrap_err()
            .to_string();
        assert!(err.contains("Homebrew's formula for 0.9.0"), "{err}");

        let s = survey("0.8.1", brew(), Ok("0.8.1"));
        assert_eq!(
            updated_text(&s, &Updated::Current).unwrap(),
            "pando 0.8.1 is the latest release\n"
        );
        // Not knowing the latest, brew finding nothing newer is the answer.
        let s = survey("0.8.1", brew(), Err("offline"));
        assert_eq!(
            updated_text(&s, &Updated::Unchanged).unwrap(),
            "pando 0.8.1: nothing newer to install\n"
        );
    }
}
