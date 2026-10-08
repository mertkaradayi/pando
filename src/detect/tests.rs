//! Tests for `detect`.

use std::collections::BTreeMap;
use std::path::Path;
use tempfile::{TempDir, tempdir};

use crate::catalog::frameworks::PortMechanism;
use crate::catalog::package_managers;
use crate::config::{Config, PortsSpec, ProcessConfig};

use super::*;
use super::{apply::*, dev::*, services::*, signals::*, workspaces::*};

fn scripts(pairs: &[(&str, &str)]) -> Signals {
    Signals {
        scripts: pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    }
}

/// Env example keys with values nothing reads, for the port rules.
fn env_pairs(keys: &[&str]) -> Vec<(String, String)> {
    keys.iter()
        .map(|k| (k.to_string(), "1".to_string()))
        .collect()
}

fn with_lock(mut signals: Signals, lock: &str) -> Signals {
    signals.lockfiles.push(lock.to_string());
    signals
}

fn values(proposal: &Proposal) -> Vec<&str> {
    proposal
        .candidates
        .iter()
        .map(|c| c.value.as_str())
        .collect()
}

/// The dev command for signals built by hand, which have no root to read.
fn dev_of(signals: &Signals, rule: Option<&'static FrameworkRule>) -> Proposal {
    dev_cmd_proposal(Path::new("/nonexistent"), signals, rule).expect("a dev command")
}

/// The dev command detection proposes for the fixture at `root`.
fn dev_in(root: &Path, signals: &Signals) -> Proposal {
    dev_cmd_proposal(root, signals, framework(root, signals)).expect("a dev command")
}

// ---- parsing ---------------------------------------------------------

#[test]
fn scripts_come_out_of_a_package_json() {
    let manifest = r#"{"name":"x","scripts":{"dev":"next dev","build":"next build"}}"#;
    let parsed = parse_scripts(manifest);
    assert_eq!(parsed["dev"], "next dev");
    assert_eq!(parsed.len(), 2);
    assert!(parse_scripts("not json").is_empty());
    assert!(parse_scripts(r#"{"name":"x"}"#).is_empty());
}

#[test]
fn make_and_just_targets_yield_their_recipes() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        ".PHONY: run\nrun:\n\tgo run .\n\nbuild:\n\tgo build ./...\n\nCFLAGS := -O2\n",
    )
    .unwrap();
    let targets = parse_targets(dir.path());
    assert_eq!(targets["run"].recipe, ["go run ."]);
    assert_eq!(targets["run"].tool, "make");
    assert_eq!(targets["build"].recipe, ["go build ./..."]);
    assert!(
        !targets.contains_key("CFLAGS"),
        "a variable assignment is not a target: {targets:?}"
    );
    assert!(
        !targets.contains_key(".PHONY"),
        "a directive about other targets is not one: {targets:?}"
    );
}

// Text after the colon is the prerequisite list in make and in just,
// never the recipe. `run: build fmt` used to yield `build fmt` as the
// command that starts the dev server — accepted silently, and written
// to config with a comment claiming the run target said so.
#[test]
fn what_follows_a_target_name_is_prerequisites_not_the_recipe() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        "run: build fmt\n\tgo run .\n\nbuild:\n\tgo build ./...\n\n\
             serve: ; python3 -m http.server\n\nall: build run\nnext:\n\techo hi\n",
    )
    .unwrap();
    let targets = parse_targets(dir.path());
    assert_eq!(
        targets["run"].recipe,
        ["go run ."],
        "the indented line below the target is the recipe: {targets:?}"
    );
    assert_eq!(
        targets["run"].prereqs,
        ["build", "fmt"],
        "and what follows the colon is what runs first: {targets:?}"
    );
    assert_eq!(
        targets["serve"].recipe,
        ["python3 -m http.server"],
        "make's one-liner form puts the recipe after a semicolon: {targets:?}"
    );
    assert!(
        targets["serve"].prereqs.is_empty(),
        "the semicolon ends the prerequisites: {targets:?}"
    );
    assert!(
        !targets.contains_key("all"),
        "a target whose next line is another target has no recipe: {targets:?}"
    );
}

/// The shape a first run met: a `dev` target whose recipe is five
/// lines, the first of them a guard that exits 0 when the tool it
/// checks for is present. Taking that line alone spawned something
/// that succeeded and returned in a millisecond, and left an empty log
/// behind for the developer to read.
#[test]
fn a_multi_line_recipe_is_kept_whole_and_started_through_make() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        "APP := demo\nBIN := build\n\n.PHONY: dev\n\
             dev:\n\
             \t@command -v watcher >/dev/null || { echo \"install watcher\"; exit 1; }\n\
             \t@echo \"watching\"\n\
             \t@# a comment line\n\
             \t@killall $(APP) 2>/dev/null; $(BIN)/$(APP) &\n\
             \t@watcher -o -r Sources/ | while read; do \\\n\
             \t\tkillall $(APP) 2>/dev/null; \\\n\
             \t\t$(BIN)/$(APP) & \\\n\
             \tdone\n",
    )
    .unwrap();
    let targets = parse_targets(dir.path());
    let dev = &targets["dev"];
    assert_eq!(
        dev.recipe.len(),
        4,
        "the comment is not a command and the continuation is one line: {dev:?}"
    );
    assert!(
        dev.recipe[0].contains("command -v watcher"),
        "the guard is the first line, not the whole recipe: {dev:?}"
    );
    assert!(
        dev.recipe[3].starts_with("@watcher") && dev.recipe[3].ends_with("done"),
        "a trailing backslash continues one command: {dev:?}"
    );
    assert_eq!(
        dev.sole_command(),
        None,
        "four commands are not one command: {dev:?}"
    );
    assert_eq!(dev.command("dev"), "make dev");
}

#[test]
fn a_target_that_is_one_plain_line_is_still_proposed_as_that_line() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("Makefile"), "dev:\n\t@npm run dev\n").unwrap();
    let targets = parse_targets(dir.path());
    assert_eq!(
        targets["dev"].command("dev"),
        "npm run dev",
        "no prerequisites, one line, nothing make expands: {targets:?}"
    );
}

/// Three reasons a recipe line is not the command, one per test case.
/// Each on its own would be enough to make the recipe the wrong thing
/// to propose.
#[test]
fn a_prerequisite_a_second_line_or_a_make_variable_all_mean_make() {
    let cases: [(&str, &str); 4] = [
        ("run: build\n\t./app\n", "run"),
        ("run:\n\t./build.sh\n\t./app\n", "run"),
        ("run:\n\t$(BIN)/app\n", "run"),
        // `$$` is how a makefile escapes a `$` for the shell, so the
        // raw line is not what the shell should see either.
        ("run:\n\techo $$PATH\n", "run"),
    ];
    for (makefile, name) in cases {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Makefile"), makefile).unwrap();
        let targets = parse_targets(dir.path());
        assert_eq!(
            targets[name].command(name),
            "make run",
            "{makefile:?} is not representable by one of its lines"
        );
    }
}

/// A justfile is the same judgement with just's spellings: `{{ … }}` is
/// its interpolation, and `$VAR` is not — just hands that to the shell
/// exactly as written.
#[test]
fn a_justfile_target_is_judged_by_justs_own_expansion() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("justfile"),
        "dev:\n    ./serve --port $PORT\n\nweb:\n    ./serve {{ bin }}\n",
    )
    .unwrap();
    let targets = parse_targets(dir.path());
    assert_eq!(targets["dev"].tool, "just");
    assert_eq!(
        targets["dev"].command("dev"),
        "./serve --port $PORT",
        "just passes a shell variable through untouched"
    );
    assert_eq!(
        targets["web"].command("web"),
        "just web",
        "but `{{{{ … }}}}` is just's own and means nothing to a shell"
    );
}

/// `-` tells make to carry on when the command fails and `+` tells it
/// to run the line even under `-n`. Both vanish if the line is lifted
/// out, and both are about the command rather than part of it.
#[test]
fn a_line_prefix_that_is_a_directive_keeps_the_target_with_its_runner() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("Makefile"), "dev:\n\t-@./serve\n").unwrap();
    let targets = parse_targets(dir.path());
    assert_eq!(targets["dev"].recipe, ["-@./serve"], "the prefix is kept");
    assert_eq!(targets["dev"].command("dev"), "make dev");
}

/// `include .env` and `export` hand every recipe the project's
/// variables, and `set dotenv-load` does the same in a justfile. pando
/// does not load that file into the process, so the recipe line on its
/// own would start the server without them.
#[test]
fn a_file_that_sets_every_recipes_environment_keeps_its_runner() {
    let cases: [(&str, &str, &str, &str); 6] = [
        (
            "Makefile",
            "include .env\nexport\n\nrun:\n\tgo run ./cmd/server\n",
            "run",
            "make run",
        ),
        (
            "Makefile",
            "export DATABASE_URL ?= postgres://localhost/app\nrun:\n\t./app\n",
            "run",
            "make run",
        ),
        (
            "Makefile",
            ".EXPORT_ALL_VARIABLES:\nrun:\n\t./app\n",
            "run",
            "make run",
        ),
        (
            "Makefile",
            "SHELL := /bin/zsh\nrun:\n\t./app\n",
            "run",
            "make run",
        ),
        (
            "justfile",
            "set dotenv-load\n\ndev:\n    uv run app\n",
            "dev",
            "just dev",
        ),
        (
            "justfile",
            "export RUST_LOG := \"debug\"\n\ndev:\n    cargo run\n",
            "dev",
            "just dev",
        ),
    ];
    for (file, text, name, expected) in cases {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(file), text).unwrap();
        let targets = parse_targets(dir.path());
        assert_eq!(targets[name].command(name), expected, "{text:?}");
    }
    // A plain variable is make's alone, and a recipe that does not read
    // it through `$(…)` never sees it: the line is still the command.
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        "BIN := app\nexporter:\n\t./export.sh\nrun:\n\tgo run .\n",
    )
    .unwrap();
    assert_eq!(parse_targets(dir.path())["run"].command("run"), "go run .");
}

/// A blank line and a column-zero comment sit among recipe lines
/// without ending the recipe — make ignores both — so a recipe read as
/// ending at the first of them would be truncated all over again.
#[test]
fn a_blank_line_or_a_column_zero_comment_does_not_end_a_recipe() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("Makefile"),
        "dev:\n\techo one\n\n# still the dev recipe\n\techo two\n\nbuild:\n\techo three\n",
    )
    .unwrap();
    let targets = parse_targets(dir.path());
    assert_eq!(targets["dev"].recipe, ["echo one", "echo two"]);
    assert_eq!(targets["build"].recipe, ["echo three"]);
}

#[test]
fn env_example_keys_are_read_in_file_order() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env.example"),
        "# a comment\nPORT=3000\n\nDB_PORT=5432\nEMPTY\n",
    )
    .unwrap();
    assert_eq!(
        env_example(dir.path()),
        vec![
            ("PORT".to_string(), "3000".to_string()),
            ("DB_PORT".to_string(), "5432".to_string())
        ]
    );
}

// ---- the dev command -------------------------------------------------

#[test]
fn a_production_start_script_is_not_a_dev_server() {
    let signals = with_lock(
        scripts(&[
            ("dev", "next dev"),
            ("start", "next start"),
            ("build", "next build"),
            ("lint", "next lint"),
        ]),
        "pnpm-lock.yaml",
    );
    let proposal = dev_of(&signals, None);
    assert_eq!(values(&proposal), vec!["pnpm dev"]);
    assert!(proposal.decided);
}

// A TypeScript dev loop compiles and then runs its output, so its body
// reads like the production `start` beside it. The name says which one
// the project develops with; without it there was nothing left to offer.
#[test]
fn a_dev_script_that_compiles_and_runs_its_output_is_still_the_dev_server() {
    let signals = scripts(&[
        ("dev", "tsc-watch --onSuccess \"node dist/index.js\""),
        ("dev:api", "tsc && node build/api.js"),
        ("start", "node dist/index.js"),
    ]);
    let proposal = dev_of(&signals, None);
    assert_eq!(values(&proposal), vec!["npm run dev", "npm run dev:api"]);
    assert!(proposal.decided);
}

#[test]
fn a_workspace_app_whose_dev_runs_its_build_output_is_still_an_app() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::write(
        dir.path().join("apps/api/package.json"),
        r#"{ "scripts": { "dev": "nodemon --exec 'tsc && node dist/index.js'" } }"#,
    )
    .unwrap();
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    assert_eq!(
        apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        vec!["api", "web"]
    );
}

// Several candidates with an unambiguous `dev` is still a decision: the
// developer named it, and pando is only confirming.
#[test]
fn an_exact_dev_script_wins_over_its_siblings() {
    let signals = with_lock(
        scripts(&[
            ("dev", "next dev"),
            ("serve", "http-server"),
            ("start", "node server.js"),
        ]),
        "pnpm-lock.yaml",
    );
    let proposal = dev_of(&signals, None);
    assert_eq!(
        values(&proposal),
        vec!["pnpm dev", "pnpm serve", "pnpm start"],
        "serve outranks start, and both stay on offer"
    );
    assert!(proposal.decided, "the one named dev is the answer");
}

// A `dev` that starts several servers gives one log and one readiness
// rule for two processes. It works, so it is offered first — but it is
// not assumed.
#[test]
fn a_dev_script_that_fans_out_is_asked_about() {
    for body in [
        "concurrently \"npm:dev:*\"",
        "npm-run-all -p dev:*",
        "turbo run dev",
        "pnpm -r --parallel dev",
    ] {
        let signals = with_lock(
            scripts(&[("dev", body), ("dev:web", "next dev")]),
            "pnpm-lock.yaml",
        );
        let proposal = dev_of(&signals, None);
        assert!(!proposal.decided, "{body:?} should be asked about");
        assert_eq!(proposal.preferred().unwrap().value, "pnpm dev");
    }
}

#[test]
fn the_runner_comes_from_the_lockfile() {
    for (lock, expected) in [
        ("pnpm-lock.yaml", "pnpm dev"),
        ("package-lock.json", "npm run dev"),
        ("yarn.lock", "yarn dev"),
        ("bun.lockb", "bun run dev"),
    ] {
        let signals = with_lock(scripts(&[("dev", "next dev")]), lock);
        assert_eq!(values(&dev_of(&signals, None)), vec![expected]);
    }
    // No lockfile at all: npm is the safe default.
    assert_eq!(
        values(&dev_of(&scripts(&[("dev", "next dev")]), None)),
        vec!["npm run dev"]
    );
}

#[test]
fn a_project_with_nothing_to_run_proposes_nothing() {
    assert!(dev_cmd_proposal(Path::new("/nonexistent"), &Signals::default(), None).is_none());
}

// ---- the port --------------------------------------------------------

#[test]
fn a_service_port_is_never_offered_as_the_web_port() {
    let signals = Signals {
        env_example: env_pairs(&["PORT", "API_PORT", "DB_PORT", "SMTP_PORT", "REDIS_PORT"]),
        ..Default::default()
    };
    let proposal = port_proposal(&signals, None).unwrap();
    assert_eq!(values(&proposal), vec!["PORT", "API_PORT"]);
    assert!(!proposal.decided, "two candidates is a question");
}

// A substring match made `SUPPORT_EMAIL` and `REPORT_URL` port
// candidates, which turned a slot that resolved silently into a
// question — and a non-interactive `start` that exited 0 into an exit 3.
#[test]
fn only_a_key_that_is_or_ends_with_port_is_a_port() {
    let signals = Signals {
        env_example: env_pairs(&[
            "PORT",
            "SUPPORT_EMAIL",
            "REPORT_URL",
            "IMPORT_PATH",
            "EXPORT_DIR",
            "PASSPORT_SECRET",
            "API_PORT",
        ]),
        ..Default::default()
    };
    let proposal = port_proposal(&signals, None).unwrap();
    assert_eq!(
        values(&proposal),
        vec!["PORT", "API_PORT"],
        "support, report, import, export and passport are not ports"
    );
}

#[test]
fn a_framework_convention_answers_the_port_with_no_env_file_at_all() {
    let go = RULES.iter().find(|r| r.name == "Go").unwrap();
    let proposal = port_proposal(&Signals::default(), Some(go)).unwrap();
    assert_eq!(values(&proposal), vec!["PORT"]);
    assert!(proposal.decided);
}

// The monorepo shape this was built for: the project names its ports by
// role in its own env example, and there is no `PORT` to say which of
// them is *the* one. Two variables are two roles, not two guesses at a
// single answer.
#[test]
fn port_variables_named_by_role_become_several_roles() {
    let signals = Signals {
        env_example: env_pairs(&["WEB_PORT", "API_PORT", "DATABASE_PORT"]),
        ..Default::default()
    };
    let proposal = port_proposal(&signals, None).unwrap();
    let first = proposal.preferred().unwrap();
    assert_eq!(first.value, "WEB_PORT, API_PORT");
    assert_eq!(first.why, "WEB_PORT and API_PORT in the env example");
    assert_eq!(
        first.ports,
        Some(PortsSpec::Map(BTreeMap::from([
            ("WEB_PORT".to_string(), "web".to_string()),
            ("API_PORT".to_string(), "api".to_string()),
        ]))),
        "the database's port belongs to the services slot, never to a role"
    );
    assert_eq!(
        values(&proposal),
        vec!["WEB_PORT, API_PORT", "WEB_PORT", "API_PORT"],
        "the single keys stay on offer: pando cannot know one process owns them all"
    );

    let mut config = Config::default();
    apply(Slot::PortEnv, first, &mut config);
    assert_eq!(config.processes[DEV].roles(), vec!["api", "web"]);
    assert_eq!(
        config.processes[DEV].port_env()["WEB_PORT"],
        "{port:web}",
        "the map form is sugar for the env the app really reads"
    );
}

// The option's text, typed in another order, is the same answer: one
// function splits a typed list and reads each variable's role, and it is
// the one the option is built with.
#[test]
fn a_typed_list_of_port_variables_is_the_map_the_option_would_be() {
    let signals = Signals {
        env_example: env_pairs(&["WEB_PORT", "API_PORT"]),
        ..Default::default()
    };
    let proposal = port_proposal(&signals, None).unwrap();
    let option = proposal.preferred().unwrap();
    assert_eq!(typed_ports(&option.value).ok(), option.ports);
    assert_eq!(typed_ports("API_PORT,WEB_PORT").ok(), option.ports);
    let typed = custom(Slot::PortEnv, "API_PORT , WEB_PORT");
    assert_eq!(typed.ports, option.ports);

    // One variable owns `web` whatever it is called, as it always has.
    assert_eq!(
        typed_ports("LISTEN").unwrap(),
        PortsSpec::Map(BTreeMap::from([("LISTEN".to_string(), "web".to_string())]))
    );
    // And a bare `PORT` among several is the web role, as the rules read it.
    assert_eq!(
        typed_ports("PORT, API_PORT").unwrap().roles(),
        vec!["api".to_string(), "web".to_string()]
    );
}

#[test]
fn a_typed_port_variable_must_be_a_variable_name() {
    for bad in [
        "PORT API_PORT",
        "API-PORT",
        "9PORT",
        "PORT;API_PORT",
        "",
        " , ",
    ] {
        assert!(typed_ports(bad).is_err(), "{bad:?}");
    }
    // Several variables each say their own role, or none of them does.
    let e = typed_ports("PORT, LISTEN").unwrap_err();
    assert!(e.contains("LISTEN") && e.contains("<ROLE>_PORT"), "{e}");
    for good in ["PORT", "_PORT", "api_port", "WEB_PORT, API_PORT"] {
        assert!(typed_ports(good).is_ok(), "{good:?}");
    }
}

// The project's own declaration beats the convention pando brought with
// it — and says so, because the note in the file is the only place a
// developer sees which of the two won.
#[test]
fn the_projects_own_port_variables_beat_the_framework_guess() {
    let next = RULES.iter().find(|r| r.name == "Next.js").unwrap();
    let signals = Signals {
        env_example: env_pairs(&["WEB_PORT", "API_PORT"]),
        ..Default::default()
    };
    let proposal = port_proposal(&signals, Some(next)).unwrap();
    assert_eq!(
        values(&proposal),
        vec!["WEB_PORT, API_PORT", "PORT", "WEB_PORT", "API_PORT"],
        "the framework's PORT is still an option, just not the first one"
    );
    assert_eq!(
        proposal.preferred().unwrap().why,
        "WEB_PORT and API_PORT in the env example, over the Next.js convention"
    );
    assert!(!proposal.decided);
}

// Next reads `-p` over `PORT`: given a role, the process would be waited
// on for a port it never binds.
#[test]
fn a_dev_script_that_fixes_its_own_port_is_not_given_the_framework_one() {
    let next = RULES.iter().find(|r| r.name == "Next.js").unwrap();
    for body in [
        "next dev -p 3001",
        "next dev --port=3001",
        "next dev --port '3001'",
    ] {
        let signals = scripts(&[("dev", body)]);
        assert!(port_proposal(&signals, Some(next)).is_none(), "{body}");
    }
    // A port it reads from the environment is still one it is given.
    let signals = scripts(&[("dev", "next dev -p $PORT")]);
    assert_eq!(
        values(&port_proposal(&signals, Some(next)).unwrap()),
        vec!["PORT"]
    );
}

// The env example's own `PORT` moves the fixed port no more than the
// framework's does: decided, it was a role waited on for a port Next
// never binds.
#[test]
fn a_dev_script_that_fixes_its_own_port_is_not_given_the_env_examples_one() {
    let next = RULES.iter().find(|r| r.name == "Next.js").unwrap();
    for keys in [&["PORT"][..], &["WEB_PORT", "API_PORT"]] {
        let signals = Signals {
            env_example: env_pairs(keys),
            ..scripts(&[("dev", "next dev -p 3001")])
        };
        assert!(port_proposal(&signals, Some(next)).is_none(), "{keys:?}");
        assert!(port_proposal(&signals, None).is_none(), "{keys:?}");
    }
}

// A project that writes `PORT` has said where its web server's port
// comes from. The role reading is for a project that named its ports
// instead, so this one keeps the question it always had.
#[test]
fn a_project_that_names_port_keeps_the_single_answer() {
    let signals = Signals {
        env_example: env_pairs(&["PORT", "API_PORT"]),
        ..Default::default()
    };
    let proposal = port_proposal(&signals, None).unwrap();
    assert_eq!(values(&proposal), vec!["PORT", "API_PORT"]);
    assert!(
        proposal.candidates.iter().all(|c| c.ports.is_none()),
        "no candidate here answers with a whole map"
    );
}

// Every spelling of a service's port a real project uses. One of these
// becoming a role would hand the app a port with no database behind it.
#[test]
fn a_service_family_port_is_never_one_of_the_apps_roles() {
    for key in [
        "DATABASE_PORT",
        "DATABASE_REPLICA_PORT",
        "DB_PORT",
        "READ_DB_PORT",
        // Every `<something>DB_PORT`: the suffix list this replaced
        // caught these with a plain `ends_with`, and a word boundary
        // here would hand each of them a role with nothing behind it.
        "INFLUXDB_PORT",
        "COUCHDB_PORT",
        "DYNAMODB_PORT",
        "REPLICA_PORT",
        "REDIS_PORT",
        "MONGO_PORT",
        "MONGODB_PORT",
        "POSTGRES_PORT",
        "PG_PORT",
        "MYSQL_PORT",
        "MARIADB_PORT",
        "SMTP_PORT",
        "MAIL_PORT",
    ] {
        assert!(is_service_port(key), "{key} is a service's port");
    }
    // The prefix arm is word-bounded, so a family that merely *starts*
    // a longer word is not a match.
    for key in [
        "PORT",
        "WEB_PORT",
        "API_PORT",
        "ADMIN_PORT",
        "DBX_PORT",
        "PGADMIN_PORT",
        "METRICS_PORT",
        "GRPC_PORT",
    ] {
        assert!(!is_service_port(key), "{key} is the application's own");
    }
}

#[test]
fn a_multi_role_answer_is_written_as_one_inline_table() {
    let signals = Signals {
        env_example: env_pairs(&["WEB_PORT", "API_PORT"]),
        ..Default::default()
    };
    let proposal = port_proposal(&signals, None).unwrap();
    let edits = edits(Slot::PortEnv, proposal.preferred().unwrap());
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].table, vec!["dev"]);
    assert_eq!(edits[0].key, "ports");
    assert_eq!(
        edits[0].value.to_string().trim(),
        r#"{ API_PORT = "api", WEB_PORT = "web" }"#
    );
}

#[test]
fn a_framework_that_takes_its_port_in_the_command_has_no_env_question() {
    let django = RULES.iter().find(|r| r.name == "Django").unwrap();
    assert!(port_proposal(&Signals::default(), Some(django)).is_none());

    let mut config = Config::default();
    apply(
        Slot::DevCmd,
        &Candidate {
            value: "python manage.py runserver 127.0.0.1:{port:web}".into(),
            why: "the Django rule".into(),
            ports: Some(PortsSpec::List(vec!["web".into()])),
            ..Candidate::default()
        },
        &mut config,
    );
    assert!(
        !still_needed(Slot::PortEnv, &config),
        "the command already carries the port"
    );
}

// ---- framework rules -------------------------------------------------

fn marker_fixture(files: &[(&str, &str)]) -> (TempDir, Signals) {
    let dir = tempdir().unwrap();
    for (name, body) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
    let signals = signals(dir.path());
    (dir, signals)
}

#[test]
fn a_marker_file_names_the_framework() {
    let (dir, s) = marker_fixture(&[("manage.py", "")]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Django");
    let (dir, s) = marker_fixture(&[("nuxt.config.ts", "")]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Nuxt");
}

#[test]
fn a_script_body_names_the_framework_when_no_marker_file_does() {
    let signals = scripts(&[("dev", "next dev")]);
    let dir = tempdir().unwrap();
    assert_eq!(framework(dir.path(), &signals).unwrap().name, "Next.js");
}

// A crate with no binary has nothing to serve, and proposing
// `cargo run` for it would be an invention.
#[test]
fn a_library_crate_matches_no_framework() {
    let (dir, s) = marker_fixture(&[("Cargo.toml", "[package]\nname = \"x\"\n\n[lib]\n")]);
    assert!(framework(dir.path(), &s).is_none());

    let (dir, s) = marker_fixture(&[
        ("Cargo.toml", "[package]\nname = \"x\"\n"),
        ("src/main.rs", "fn main() {}"),
    ]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Rust");
}

// A server in src/main.rs and a seed tool in src/bin is a common crate,
// and cargo refuses a bare `cargo run` there: "could not determine which
// binary to run". Proposed as decided, every start failed.
#[test]
fn a_crate_with_several_binaries_is_offered_each_by_name() {
    let (dir, s) = marker_fixture(&[
        ("Cargo.toml", "[package]\nname = \"api\"\n"),
        ("src/main.rs", "fn main() {}"),
        ("src/bin/seed.rs", "fn main() {}"),
        ("src/bin/migrate/main.rs", "fn main() {}"),
        ("src/bin/migrate/sql.rs", ""),
    ]);
    let proposal = dev_in(dir.path(), &s);
    assert_eq!(
        values(&proposal),
        vec![
            "cargo run --bin api",
            "cargo run --bin migrate",
            "cargo run --bin seed"
        ],
        "the crate's own binary leads"
    );
    assert!(!proposal.decided);

    // `[[bin]]` entries count, and one that points at src/main.rs is
    // that binary, not a second one.
    let (dir, s) = marker_fixture(&[
        (
            "Cargo.toml",
            "[package]\nname = \"x\"\n\n[[bin]]\nname = \"worker\"\npath = \"src/worker.rs\"\n\n\
             [[bin]]\nname = \"server\"\npath = \"src/main.rs\"\n",
        ),
        ("src/main.rs", "fn main() {}"),
    ]);
    assert_eq!(
        values(&dev_in(dir.path(), &s)),
        vec!["cargo run --bin server", "cargo run --bin worker"]
    );

    // One binary, or a `default-run`, is what a bare `cargo run` runs.
    for (manifest, extra) in [
        ("[package]\nname = \"api\"\n", "src/main.rs"),
        (
            "[package]\nname = \"api\"\ndefault-run = \"api\"\n",
            "src/bin/seed.rs",
        ),
        (
            "[package]\nname = \"api\"\nautobins = false\n",
            "src/bin/seed.rs",
        ),
    ] {
        let (dir, s) = marker_fixture(&[
            ("Cargo.toml", manifest),
            ("src/main.rs", "fn main() {}"),
            (extra, "fn main() {}"),
        ]);
        let proposal = dev_in(dir.path(), &s);
        assert_eq!(values(&proposal), vec!["cargo run"], "{manifest}");
        assert!(proposal.decided, "{manifest}");
    }
}

// `go run .` in a library fails on every start: "is not a main package".
#[test]
fn a_go_module_whose_root_is_not_a_main_package_matches_no_framework() {
    for files in [
        &[("go.mod", "module x\n"), ("lib.go", "package lib\n")][..],
        &[
            ("go.mod", "module x\n"),
            ("cmd/README.md", "The commands.\n"),
            ("cmd/shared/flags.go", "package shared\n"),
            ("internal/db/db.go", "package db\n"),
        ],
        &[
            ("go.mod", "module x\n"),
            ("x.go", "package x\n"),
            ("main_test.go", "package main\n"),
        ],
        &[
            ("go.mod", "module x\n"),
            ("x.go", "// package main\npackage x\n"),
        ],
        // A generator `go run gen.go` runs by name is in no package.
        &[
            ("go.mod", "module x\n"),
            ("lib.go", "package lib\n"),
            ("gen.go", "//go:build ignore\n\npackage main\n"),
            ("mkerrors.go", "// +build ignore\n\npackage main\n"),
        ],
    ] {
        let (dir, s) = marker_fixture(files);
        assert!(framework(dir.path(), &s).is_none(), "{files:?}");
        assert!(dev_cmd_proposal(dir.path(), &s, framework(dir.path(), &s)).is_none());
    }

    let (dir, s) = marker_fixture(&[
        ("go.mod", "module x\n"),
        (
            "main.go",
            "// Copyright the authors.\n\n//go:build !windows\n\n/* The server.\n   It serves. */\n\
             package main // the command\n",
        ),
    ]);
    let rule = framework(dir.path(), &s);
    assert_eq!(rule.unwrap().name, "Go");
    assert_eq!(values(&dev_in(dir.path(), &s)), vec!["go run ."]);

    // A constraint that some builds pass keeps the file in the package.
    let (dir, s) = marker_fixture(&[
        ("go.mod", "module x\n"),
        ("main.go", "//go:build !ignore\n\npackage main\n"),
    ]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Go");
}

// The standard layout keeps a service's commands under `cmd/` and
// nothing at the root for `go run .` to run: "no Go files". With no
// command at all, such a service was given nothing to run.
#[test]
fn a_go_module_whose_commands_live_under_cmd_is_offered_each_by_path() {
    let (dir, s) = marker_fixture(&[
        ("go.mod", "module x\n"),
        ("cmd/worker/main.go", "package main\n"),
        ("cmd/api/main.go", "package main\n"),
        ("cmd/api/routes.go", "package main\n"),
        ("cmd/gen/gen.go", "//go:build ignore\n\npackage main\n"),
        ("cmd/shared/flags.go", "package shared\n"),
        ("cmd/check/main_test.go", "package main\n"),
        ("internal/db/db.go", "package db\n"),
    ]);
    let rule = framework(dir.path(), &s);
    assert_eq!(rule.unwrap().name, "Go");
    let proposal = dev_in(dir.path(), &s);
    assert_eq!(
        values(&proposal),
        vec!["go run ./cmd/api", "go run ./cmd/worker"],
        "the server first"
    );
    assert!(!proposal.decided);
    assert_eq!(values(&port_proposal(&s, rule).unwrap()), vec!["PORT"]);

    // One command is still a guess at what serves: `cmd/` holds tools as
    // often as servers.
    let (dir, s) = marker_fixture(&[
        ("go.mod", "module x\n"),
        ("cmd/api/main.go", "package main\n"),
    ]);
    let proposal = dev_in(dir.path(), &s);
    assert_eq!(values(&proposal), vec!["go run ./cmd/api"]);
    assert!(!proposal.decided);

    // A root main package is what `go run .` runs, whatever `cmd/` holds.
    let (dir, s) = marker_fixture(&[
        ("go.mod", "module x\n"),
        ("main.go", "package main\n"),
        ("cmd/migrate/main.go", "package main\n"),
    ]);
    let proposal = dev_in(dir.path(), &s);
    assert_eq!(values(&proposal), vec!["go run ."]);
    assert!(proposal.decided);
}

// The first command is the one a start takes. In directory order a
// `cmd/migrate` beside a `cmd/server` came first, and a first start ran
// the migration tool against the env's database as the dev server.
#[test]
fn a_go_modules_server_under_cmd_is_offered_before_its_tools() {
    let first = |files: &[(&str, &str)]| {
        let (dir, s) = marker_fixture(files);
        dev_cmd_proposal(dir.path(), &s, framework(dir.path(), &s))
            .map(|proposal| values(&proposal).join(", "))
    };
    assert_eq!(
        first(&[
            ("go.mod", "module x\n"),
            ("cmd/migrate/main.go", "package main\n"),
            ("cmd/server/main.go", "package main\n"),
        ])
        .as_deref(),
        Some("go run ./cmd/server, go run ./cmd/migrate")
    );
    assert_eq!(
        first(&[
            ("go.mod", "module x\n"),
            ("cmd/admin/main.go", "package main\n"),
            ("cmd/api/main.go", "package main\n"),
            ("cmd/cli/main.go", "package main\n"),
            ("cmd/seed/main.go", "package main\n"),
        ])
        .as_deref(),
        Some("go run ./cmd/api, go run ./cmd/admin, go run ./cmd/cli, go run ./cmd/seed")
    );
    // The command named after the module is its own.
    assert_eq!(
        first(&[
            ("go.mod", "module example.com/acme/shop/v2 // the shop\n"),
            ("cmd/gen/main.go", "package main\n"),
            ("cmd/shop/main.go", "package main\n"),
        ])
        .as_deref(),
        Some("go run ./cmd/shop, go run ./cmd/gen")
    );

    // Several commands and none a server: there is nothing pando can tell
    // is the one to run, and a tool is no guess at it.
    for files in [
        &[
            ("go.mod", "module x\n"),
            ("cmd/migrate/main.go", "package main\n"),
            ("cmd/seed/main.go", "package main\n"),
        ][..],
        &[
            ("go.mod", "module example.com/acme/migrate\n"),
            ("cmd/migrate/main.go", "package main\n"),
            ("cmd/lint/main.go", "package main\n"),
        ],
        // A library's lone tool is no more a server than several are.
        &[
            ("go.mod", "module example.com/acme/lib\n"),
            ("cmd/gen/main.go", "package main\n"),
        ],
        &[
            ("go.mod", "module example.com/acme/migrate\n"),
            ("cmd/migrate/main.go", "package main\n"),
        ],
    ] {
        let (dir, s) = marker_fixture(files);
        assert!(framework(dir.path(), &s).is_none(), "{files:?}");
        assert_eq!(first(files), None, "{files:?}");
    }
}

// `config.ru` is every Rack app's and `bin/dev` is a helper in any
// language: read as Rails, a Sinatra app and a Go repo were both given
// `bin/rails server`, which is not there to run.
#[test]
fn a_file_other_projects_have_too_does_not_make_one_rails() {
    let (dir, s) = marker_fixture(&[("Gemfile", ""), ("config.ru", "run App\n")]);
    assert!(framework(dir.path(), &s).is_none());
    let (dir, s) = marker_fixture(&[
        ("go.mod", "module x\n"),
        ("main.go", "package main\n"),
        ("bin/dev", "#!/bin/sh\n"),
    ]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Go");
    let (dir, s) = marker_fixture(&[
        ("config.ru", ""),
        ("bin/dev", ""),
        ("bin/rails", ""),
        ("config/application.rb", ""),
    ]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Rails");
}

// Every Mix project has a mix.exs, and `mix phx.server` is a task only
// Phoenix defines.
#[test]
fn a_mix_project_is_phoenix_only_when_it_depends_on_phoenix() {
    let (dir, s) = marker_fixture(&[(
        "mix.exs",
        "defp deps do\n  [{:jason, \"~> 1.4\"}, {:phoenix_pubsub, \"~> 2.1\"}]\nend\n",
    )]);
    assert!(framework(dir.path(), &s).is_none());
    let (dir, s) = marker_fixture(&[(
        "mix.exs",
        "defp deps do\n  [{:phoenix, \"~> 1.7.14\"}, {:jason, \"~> 1.4\"}]\nend\n",
    )]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Phoenix");
    // An umbrella's root mix.exs names no dependency; its lockfile does.
    let (dir, s) = marker_fixture(&[
        ("mix.exs", "def project do\n  [apps_path: \"apps\"]\nend\n"),
        (
            "mix.lock",
            "%{\n  \"phoenix\": {:hex, :phoenix, \"1.7.14\", \"abc\", [:mix], [], \"hexpm\"},\n}\n",
        ),
    ]);
    assert_eq!(framework(dir.path(), &s).unwrap().name, "Phoenix");
}

/// A stock Laravel app: `artisan`, and Vite building its assets.
const LARAVEL: [(&str, &str); 3] = [
    ("artisan", "#!/usr/bin/env php\n"),
    ("vite.config.js", "export default {}\n"),
    (
        "package.json",
        r#"{ "private": true, "scripts": { "build": "vite build", "dev": "vite" } }"#,
    ),
];

// Read as Vite, a Laravel app was started as its asset server alone,
// which serves a placeholder page, and the PHP app never ran.
#[test]
fn a_backend_building_its_assets_with_vite_is_run_by_its_own_server() {
    let (dir, s) = marker_fixture(&LARAVEL);
    let rule = framework(dir.path(), &s);
    assert_eq!(rule.unwrap().name, "Laravel");
    let proposal = dev_in(dir.path(), &s);
    assert_eq!(
        values(&proposal),
        vec!["php artisan serve --port {port:web}", "npm run dev"],
        "the asset server stays on offer, behind the app"
    );
    assert_eq!(
        proposal.candidates[0].ports,
        Some(PortsSpec::List(vec!["web".to_string()]))
    );
    assert!(
        !proposal.decided,
        "the app alone or with its asset server is the developer's call"
    );

    let (dir, s) = marker_fixture(&[
        ("config.ru", ""),
        ("bin/rails", ""),
        ("config/application.rb", ""),
        ("vite.config.ts", "export default {}\n"),
    ]);
    assert_eq!(
        values(&dev_in(dir.path(), &s)),
        vec!["bin/rails server -p {port:web}"]
    );

    let (dir, s) = marker_fixture(&[
        ("manage.py", ""),
        ("package.json", r#"{ "scripts": { "dev": "vite" } }"#),
    ]);
    assert_eq!(
        dev_in(dir.path(), &s).candidates[0].value,
        "python manage.py runserver 127.0.0.1:{port:web}"
    );
}

// Run as `npm run dev` with no port, a worktree's Vite moved itself to
// the next free port and pando held no role for it: no URL, nothing to
// share, and a second Angular worktree found 4200 taken.
#[test]
fn a_single_apps_script_is_given_the_port_flag_its_framework_takes() {
    for (files, expected) in [
        (
            &[
                ("vite.config.ts", "export default {}\n"),
                (
                    "package.json",
                    r#"{ "scripts": { "dev": "vite", "build": "vite build" } }"#,
                ),
                ("package-lock.json", "{}\n"),
            ][..],
            "npm run dev -- --port {port:web}",
        ),
        (
            &[
                ("svelte.config.js", "export default {}\n"),
                ("package.json", r#"{ "scripts": { "dev": "vite dev" } }"#),
                ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            ],
            "pnpm dev --port {port:web}",
        ),
        (
            &[
                ("astro.config.mjs", "export default {}\n"),
                ("package.json", r#"{ "scripts": { "dev": "astro dev" } }"#),
                ("package-lock.json", "{}\n"),
            ],
            "npm run dev -- --port {port:web}",
        ),
        (
            &[
                ("angular.json", "{}\n"),
                (
                    "package.json",
                    r#"{ "scripts": { "start": "ng serve", "build": "ng build" } }"#,
                ),
                ("package-lock.json", "{}\n"),
            ],
            "npm run start -- --port {port:web}",
        ),
    ] {
        let (dir, s) = marker_fixture(files);
        let proposal = dev_in(dir.path(), &s);
        assert_eq!(values(&proposal), vec![expected]);
        assert!(proposal.decided, "{expected}");
        let first = proposal.preferred().unwrap();
        assert_eq!(first.ports, Some(PortsSpec::List(vec!["web".to_string()])));
        let mut config = Config::default();
        apply(Slot::DevCmd, first, &mut config);
        assert!(
            !still_needed(Slot::PortEnv, &config),
            "the command already carries the port: {expected}"
        );
    }

    // `dev` is still the answer among its siblings once it carries the flag.
    let (dir, s) = marker_fixture(&[
        ("vite.config.ts", "export default {}\n"),
        (
            "package.json",
            r#"{ "scripts": { "dev": "vite", "start": "vite" } }"#,
        ),
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
    ]);
    let proposal = dev_in(dir.path(), &s);
    assert_eq!(
        values(&proposal),
        vec!["pnpm dev --port {port:web}", "pnpm start --port {port:web}"]
    );
    assert!(proposal.decided);
}

// The flag is Vite's, and only Vite's own CLI is handed it: a custom
// server would be given an option it never reads, a fan-out would hand
// it to whichever process comes last, and a port the script names would
// be said twice. In `vite & node api.js` the flag reached `node`, Vite
// bound a port of its own, and the wait on the reserved one failed the
// start.
#[test]
fn a_script_that_is_not_the_frameworks_own_server_is_not_given_its_flag() {
    let first = |body: &str| {
        let manifest = format!(r#"{{ "scripts": {{ "dev": "{body}" }} }}"#);
        let (dir, s) = marker_fixture(&[
            ("vite.config.ts", "export default {}\n"),
            ("package.json", &manifest),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ]);
        dev_in(dir.path(), &s).candidates[0].clone()
    };
    for body in [
        "node server.js",
        "vite --port 5173",
        "concurrently \\\"vite\\\" \\\"tsc -w\\\"",
        "vite & node api.js",
        "vite | tee dev.log",
        "vite; vite build --watch",
    ] {
        let candidate = first(body);
        assert_eq!(candidate.value, "pnpm dev", "{body}");
        assert_eq!(candidate.ports, None, "{body}");
    }
    // The last command is Vite's: the flag reaches it.
    for body in ["tsc -b && vite", "node gen.js & vite", "vite 2>&1"] {
        let candidate = first(body);
        assert_eq!(candidate.value, "pnpm dev --port {port:web}", "{body}");
    }
}

// The port a `dev: vite --port 5173` fixes is the asset server's. The
// app is Phoenix's own server, and PORT is still how it is told one.
#[test]
fn an_asset_script_that_fixes_its_port_leaves_the_apps_port_alone() {
    let (dir, s) = marker_fixture(&[
        ("mix.exs", "defp deps do\n  [{:phoenix, \"~> 1.7\"}]\nend\n"),
        (
            "package.json",
            r#"{ "scripts": { "dev": "vite --port 5173" } }"#,
        ),
    ]);
    let rule = framework(dir.path(), &s);
    assert_eq!(values(&port_proposal(&s, rule).unwrap()), vec!["PORT"]);
}

// In a workspace pando runs the app's own `dev` script, so the rule
// that tells it the port is the one the script names.
#[test]
fn a_workspace_app_with_a_backend_marker_is_given_its_scripts_port_flag() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::write(dir.path().join("apps/web/manage.py"), "").unwrap();
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    let web = apps.iter().find(|app| app.name == "web").unwrap();
    assert_eq!(web.cmd, "pnpm dev --port {port:web}");
}

// Only the `dev` script says what it runs. Read from every script, the
// `css` one's `node` made a Django app Node, handed PORT, which
// runserver ignores, and waited on a port it never bound.
#[test]
fn a_workspace_apps_other_scripts_do_not_say_what_its_dev_script_runs() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::create_dir_all(dir.path().join("apps/admin")).unwrap();
    std::fs::write(dir.path().join("apps/admin/manage.py"), "").unwrap();
    std::fs::write(
        dir.path().join("apps/admin/package.json"),
        r#"{ "scripts": { "dev": "python manage.py runserver", "css": "node build-css.js" } }"#,
    )
    .unwrap();
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    let admin = apps.iter().find(|app| app.name == "admin").unwrap();
    assert_eq!(admin.port, PortMechanism::Ask);
    assert_eq!(admin.default_port, Some(8000));
}

// Handed `--port`, a library's `vite build --watch` exited on an option
// Vite's build does not take, and without it the watcher would still
// bind no port, so the readiness wait failed the whole worktree. The
// `vite` of a config file's name, of a directory the build writes to, or
// of a cache the script clears, read as Vite's server and brought the
// flag back.
#[test]
fn a_package_whose_dev_script_only_builds_is_given_no_server_port() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    for (app, marker, dev) in [
        ("ui", "vite.config.ts", "vite build --watch"),
        ("widgets", "angular.json", "ng build --watch"),
        (
            "lib",
            "vite.config.ts",
            "vite build --watch --config vite.lib.config.ts",
        ),
        (
            "icons",
            "vite.config.ts",
            "rm -rf node_modules/.vite && vite build --watch",
        ),
        (
            "assets",
            "vite.config.ts",
            "vite build --watch --outDir dist/vite",
        ),
    ] {
        std::fs::create_dir_all(dir.path().join("apps").join(app)).unwrap();
        std::fs::write(dir.path().join("apps").join(app).join(marker), "{}\n").unwrap();
        std::fs::write(
            dir.path().join("apps").join(app).join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{dev}" }} }}"#),
        )
        .unwrap();
    }
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    for app in ["ui", "widgets", "lib", "icons", "assets"] {
        let process = &processes[app];
        assert_eq!(process.cmd, "pnpm dev", "{app}");
        assert!(process.roles().is_empty(), "{app}: {:?}", process.ports);
        assert!(process.ready.is_none(), "{app}");
    }
    assert_eq!(processes["web"].cmd, "pnpm dev --port {port:web}");

    for dev in [
        "vite build --watch",
        "vite build --watch --config vite.lib.config.ts",
        "vite build --watch -c vite.config.lib.ts",
        "rm -rf node_modules/.vite && vite build --watch",
        "vite build --watch --outDir dist/vite",
        "vite build --watch --outDir ../api/public/vite",
        "vite build --watch --outDir=dist/vite",
    ] {
        let manifest = format!(r#"{{ "scripts": {{ "dev": "{dev}" }} }}"#);
        let (dir, s) = marker_fixture(&[
            ("vite.config.ts", "export default {}\n"),
            ("package.json", &manifest),
        ]);
        let proposal = dev_in(dir.path(), &s);
        assert_eq!(values(&proposal), vec!["npm run dev"], "{dev}");
        assert_eq!(proposal.candidates[0].ports, None, "{dev}");
    }
}

// Read as build-only, a `vite build && vite preview` lost the `--port`
// its preview server takes, bound Vite's own port in every worktree, and
// two worktrees clashed on it.
#[test]
fn a_package_whose_dev_script_builds_and_then_serves_is_given_its_port() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    for (app, marker, dev) in [
        ("docs", "vite.config.ts", "vite build && vite preview"),
        ("admin", "angular.json", "ng build shared && ng serve"),
    ] {
        std::fs::create_dir_all(dir.path().join("apps").join(app)).unwrap();
        std::fs::write(dir.path().join("apps").join(app).join(marker), "{}\n").unwrap();
        std::fs::write(
            dir.path().join("apps").join(app).join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{dev}" }} }}"#),
        )
        .unwrap();
    }
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    for app in ["docs", "admin"] {
        let process = &processes[app];
        assert_eq!(process.cmd, format!("pnpm dev --port {{port:{app}}}"));
        assert_eq!(process.roles(), vec![app.to_string()], "{app}");
    }

    for dev in [
        "vite build && vite preview",
        "vite build -c vite.lib.config.ts && ./node_modules/.bin/vite preview",
        "vite build --outDir dist/vite && vite preview --outDir dist/vite",
    ] {
        let manifest = format!(r#"{{ "scripts": {{ "dev": "{dev}" }} }}"#);
        let (dir, s) = marker_fixture(&[
            ("vite.config.ts", "export default {}\n"),
            ("package.json", &manifest),
        ]);
        let proposal = dev_in(dir.path(), &s);
        assert_eq!(
            values(&proposal),
            vec!["npm run dev -- --port {port:web}"],
            "{dev}"
        );
        assert_eq!(
            proposal.candidates[0].ports,
            Some(PortsSpec::List(vec!["web".to_string()])),
            "{dev}"
        );
    }
}

// pnpm hands `--port` to a script's last command. In `vite & vite build
// --watch` that is the build, which refused it, while the backgrounded
// Vite bound a port of its own and the wait on the reserved one failed
// the start.
#[test]
fn a_workspace_app_is_given_its_flag_only_where_its_last_command_is_the_server() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    for (app, dev) in [
        ("docs", "vite & vite build --watch"),
        ("site", "vite; vite build --watch"),
        ("shop", "vite & node api.js"),
        ("blog", "tsc -b && vite"),
    ] {
        std::fs::create_dir_all(dir.path().join("apps").join(app)).unwrap();
        std::fs::write(
            dir.path().join("apps").join(app).join("vite.config.ts"),
            "export default {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("apps").join(app).join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{dev}" }} }}"#),
        )
        .unwrap();
    }
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    for app in ["docs", "site", "shop"] {
        let process = &processes[app];
        assert_eq!(process.cmd, "pnpm dev", "{app}");
        assert!(process.roles().is_empty(), "{app}: {:?}", process.ports);
        assert!(process.ready.is_none(), "{app}");
    }
    assert_eq!(processes["blog"].cmd, "pnpm dev --port {port:blog}");
    assert_eq!(processes["blog"].roles(), vec!["blog".to_string()]);
}

// ---- install ---------------------------------------------------------

// Invariant 1: a lockfile can never change because pando ran an
// install. Every command here has been checked against its tool's
// documentation, so the list is the assertion — a new entry has to be
// added here deliberately.
#[test]
fn every_file_the_runtime_reads_is_a_version_file_signal() {
    for language in crate::runtime::LANGUAGES {
        for source in language.files {
            assert!(
                VERSION_FILES.contains(&source.file),
                "{} pins {} but signals never looks for it",
                source.file,
                language.name
            );
        }
    }
    for shared in crate::runtime::SHARED_VERSION_FILES {
        assert!(VERSION_FILES.contains(&shared), "{shared}");
    }
}

#[test]
fn every_install_command_is_a_frozen_one() {
    const KNOWN_FROZEN: [&str; 9] = [
        "pnpm install --frozen-lockfile",
        "npm ci",
        "yarn install --frozen-lockfile",
        "bun install --frozen-lockfile",
        "uv sync --frozen",
        "poetry install --sync",
        "BUNDLE_FROZEN=true bundle install",
        "pipenv sync",
        "composer install",
    ];
    for lock in package_managers::lockfiles() {
        let Some((cmd, _)) = package_managers::install_for(lock) else {
            continue;
        };
        assert!(
            KNOWN_FROZEN.contains(&cmd),
            "{lock} proposes {cmd:?}, which has not been checked against Invariant 1"
        );
    }
}

#[test]
fn a_build_that_resolves_its_own_modules_gets_no_install_step() {
    assert!(package_managers::install_for("go.sum").is_none());
    assert!(package_managers::install_for("Cargo.lock").is_none());
    let signals = Signals {
        lockfiles: vec!["go.sum".to_string()],
        ..Default::default()
    };
    assert!(install_proposal(tempdir().unwrap().path(), &signals).is_none());
}

#[test]
fn two_lockfiles_are_a_question() {
    let signals = Signals {
        lockfiles: vec![
            "pnpm-lock.yaml".to_string(),
            "package-lock.json".to_string(),
        ],
        ..Default::default()
    };
    let proposal = install_proposal(tempdir().unwrap().path(), &signals).unwrap();
    assert_eq!(
        values(&proposal),
        vec!["pnpm install --frozen-lockfile", "npm ci"]
    );
    assert!(!proposal.decided, "pando does not guess which one is live");
}

/// A workspace that starts its own apps: a `predev` step and a script of
/// its own that hands each app its port, beside a library whose `dev` is
/// not a server — the shape a hand-grown monorepo has.
fn orchestrated_workspace(dir: &Path) {
    std::fs::write(
        dir.join("package.json"),
        r#"{ "workspaces": ["apps/api", "apps/web", "packages/sdk"],
             "scripts": { "predev": "node scripts/vendor.mjs",
                          "dev": "node scripts/dev.mjs & npm -w apps/web run dev" } }"#,
    )
    .unwrap();
    for (dir_name, dev) in [
        ("apps/api", "tsx watch src/server.ts"),
        ("apps/web", "node --watch src/server.js"),
        ("packages/sdk", "tsx src/cli-dev.ts"),
    ] {
        std::fs::create_dir_all(dir.join(dir_name)).unwrap();
        std::fs::write(
            dir.join(dir_name).join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{dev}" }} }}"#),
        )
        .unwrap();
    }
    std::fs::write(
        dir.join(".env.example"),
        "PORT=3000\nWEB_PORT=3000\nAPI_PORT=3001\nDATABASE_PORT=3306\n",
    )
    .unwrap();
}

// The project's own `npm run dev` is what runs it: its `predev` builds
// what the apps serve, and its script is what tells each app the other's
// port. It leads, carrying the ports its apps read, so taking it settles
// the port question too — and the library is never started as a server.
#[test]
fn a_workspace_that_starts_its_own_apps_is_run_by_its_own_script() {
    let dir = tempdir().unwrap();
    orchestrated_workspace(dir.path());
    let signals = signals(dir.path());
    assert!(root_orchestrates(&signals));
    let proposal = processes_proposal(dir.path(), &signals).unwrap();
    let first = &proposal.candidates[0];
    assert_eq!(first.value, "npm run dev");
    assert!(
        first.why.contains("starts the workspace's apps itself"),
        "{}",
        first.why
    );
    let dev = &first.processes.as_ref().unwrap()["dev"];
    assert_eq!(
        dev.ports,
        Some(PortsSpec::Map(BTreeMap::from([
            ("API_PORT".to_string(), "api".to_string()),
            ("WEB_PORT".to_string(), "web".to_string()),
        ])))
    );
}

// A root script that only fans out over the apps says nothing the apps'
// own scripts do not: the per-app form still leads.
#[test]
fn a_workspace_that_only_fans_out_still_runs_each_app() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    let signals = signals(dir.path());
    assert!(!root_orchestrates(&signals));
    let proposal = processes_proposal(dir.path(), &signals).unwrap();
    assert_ne!(proposal.candidates[0].value, "pnpm dev");
    assert_eq!(proposal.candidates.last().unwrap().value, "pnpm dev");
}

// A project that gitignores its lockfile: `npm ci` has nothing to be
// frozen against in a new worktree, and the lockfile `npm install` writes
// is one git ignores.
#[test]
fn a_gitignored_lockfile_gets_the_plain_install() {
    let dir = seed_fixture(&[
        ("package.json", r#"{ "scripts": { "dev": "vite" } }"#),
        (".gitignore", "package-lock.json\n"),
    ]);
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["npm install"]);
    assert!(proposal.decided);
    assert!(
        proposal.candidates[0]
            .why
            .contains("package-lock.json is gitignored")
    );

    // Present and ignored is the same: a worktree is checked out without it.
    std::fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["npm install"]);
}

// `bun.lockb` left in the gitignore after the move to `bun.lock`: the
// tracked `bun.lock` is what the plain install would rewrite.
#[test]
fn a_tracked_bun_lock_beside_an_ignored_bun_lockb_gets_the_frozen_install() {
    let dir = seed_fixture(&[
        ("package.json", r#"{ "scripts": { "dev": "vite" } }"#),
        (".gitignore", "bun.lockb\n"),
        ("bun.lock", "{}\n"),
    ]);
    crate::testutil::git(dir.path(), &["add", "bun.lock"]);
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["bun install --frozen-lockfile"]);

    // The ignored `bun.lockb` present beside it changes nothing: the plain
    // install would still rewrite the tracked `bun.lock`.
    std::fs::write(dir.path().join("bun.lockb"), "\0").unwrap();
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["bun install --frozen-lockfile"]);
    assert!(proposal.decided);
}

// The old bun habit: only the binary lockfile is gitignored, and it is the
// one present. The frozen install would fail in every new worktree, which
// is checked out without it, and there the plain install of a bun from 1.2
// on writes a `bun.lock` git does not ignore: the install that writes no
// lockfile is the one that cannot change the repository.
#[test]
fn an_ignored_bun_lockb_present_gets_the_install_that_writes_no_lockfile() {
    let dir = seed_fixture(&[
        ("package.json", r#"{ "scripts": { "dev": "vite" } }"#),
        (".gitignore", "bun.lockb\n"),
        ("bun.lockb", "\0"),
    ]);
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["bun install --no-save"]);
    assert!(proposal.decided);
    assert!(
        proposal.candidates[0]
            .why
            .contains("bun.lockb is gitignored and bun.lock is not"),
        "{}",
        proposal.candidates[0].why
    );

    // With both names ignored, whichever one the plain install writes is.
    std::fs::write(dir.path().join(".gitignore"), "bun.lockb\nbun.lock\n").unwrap();
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["bun install"]);
    assert!(
        proposal.candidates[0]
            .why
            .contains("bun.lockb is gitignored, so bun install"),
        "{}",
        proposal.candidates[0].why
    );
}

// With no lockfile a bun of 1.2 or later writes `bun.lock`, which this
// gitignore does not cover.
#[test]
fn no_lockfile_and_only_one_of_buns_names_ignored_is_no_install() {
    let dir = seed_fixture(&[
        ("package.json", r#"{ "packageManager": "bun@1.2.0" }"#),
        (".gitignore", "bun.lockb\n"),
    ]);
    assert!(install_proposal(dir.path(), &signals(dir.path())).is_none());

    std::fs::write(dir.path().join(".gitignore"), "bun.lockb\nbun.lock\n").unwrap();
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["bun install"]);
    assert!(
        proposal.candidates[0]
            .why
            .contains("bun.lockb and bun.lock are gitignored"),
        "{}",
        proposal.candidates[0].why
    );
}

#[test]
fn the_declared_package_manager_installs_a_project_with_no_lockfile() {
    let dir = seed_fixture(&[
        ("package.json", r#"{ "packageManager": "pnpm@9.1.0" }"#),
        (".gitignore", "pnpm-lock.yaml\n"),
    ]);
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(values(&proposal), vec!["pnpm install"]);
}

// Not ignored, the plain install would leave a lockfile in `git status`:
// still nothing is proposed.
#[test]
fn no_lockfile_and_none_ignored_is_still_no_install() {
    let dir = seed_fixture(&[("package.json", r#"{ "scripts": { "dev": "vite" } }"#)]);
    assert!(install_proposal(dir.path(), &signals(dir.path())).is_none());
}

// ---- workspaces ------------------------------------------------------

/// A workspace with a web app and an api app, the shape the fixture
/// catalogue's `mono-web-api` has.
fn workspace(dir: &Path) {
    std::fs::write(
        dir.join("package.json"),
        r#"{ "workspaces": ["apps/*"], "scripts": { "dev": "pnpm -r --parallel dev" } }"#,
    )
    .unwrap();
    std::fs::write(dir.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    std::fs::create_dir_all(dir.join("apps/web")).unwrap();
    std::fs::write(
        dir.join("apps/web/package.json"),
        r#"{ "scripts": { "dev": "vite" } }"#,
    )
    .unwrap();
    std::fs::write(dir.join("apps/web/vite.config.ts"), "export default {}\n").unwrap();
    std::fs::create_dir_all(dir.join("apps/api")).unwrap();
    std::fs::write(
        dir.join("apps/api/package.json"),
        r#"{ "scripts": { "dev": "node --watch src/index.js" } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.join(".env.example"),
        "WEB_PORT=5173\nAPI_PORT=4000\nVITE_API_URL=http://localhost:4000\n\
             DATABASE_URL=postgres://app:app@localhost:5432/app\n",
    )
    .unwrap();
}

fn proposed_processes(root: &Path) -> Proposal {
    let signals = signals(root);
    propose(root, &signals)
        .into_iter()
        .find(|p| p.slot == Slot::Processes)
        .expect("a processes proposal")
}

#[test]
fn a_workspace_proposes_one_process_per_app() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    let proposal = proposed_processes(dir.path());
    assert!(
        !proposal.decided,
        "two processes instead of one is the developer's call"
    );
    let processes = proposal.candidates[0]
        .processes
        .clone()
        .expect("the per-app form carries processes");
    assert_eq!(
        processes.keys().cloned().collect::<Vec<_>>(),
        vec!["api", "web"]
    );

    let web = &processes["web"];
    assert_eq!(web.cwd.as_deref(), Some("apps/web"));
    assert_eq!(
        web.cmd, "pnpm dev --port {port:web}",
        "Vite takes its port on the command line, so the flag goes on its own script"
    );
    assert_eq!(web.roles(), vec!["web"]);
    assert_eq!(
        web.env["VITE_API_URL"], "http://localhost:{port:api}",
        "a localhost URL pointing at another app becomes that app's role"
    );
    assert!(
        !web.env.contains_key("WEB_PORT"),
        "and nothing says the port twice: {:?}",
        web.env
    );
    assert_eq!(web.ready.clone().unwrap().role.as_deref(), Some("web"));
    assert_eq!(
        web.env["API_PORT"], "{port:api}",
        "a port variable the env example declares for the api is how any app finds it"
    );

    let api = &processes["api"];
    assert_eq!(api.cwd.as_deref(), Some("apps/api"));
    assert_eq!(api.cmd, "pnpm dev");
    assert_eq!(
        api.env["PORT"], "{port:api}",
        "Node reads its port from the environment"
    );
    assert!(
        !api.env.contains_key("VITE_API_URL"),
        "the app a reference points at is the one that need not be told"
    );
    assert_eq!(
        api.env["WEB_PORT"], "{port:web}",
        "and the other way round: the api is told where the web app is"
    );
    assert_eq!(api.env["API_PORT"], "{port:api}", "its own, unchanged");
    assert_eq!(api.ready.clone().unwrap().role.as_deref(), Some("api"));

    // The question shows each process with its directory and command.
    let summary = &proposal.candidates[0].value;
    for needle in ["api", "web", "apps/api", "apps/web", "pnpm dev"] {
        assert!(summary.contains(needle), "{summary}");
    }
}

// pnpm hands a `--` on to the script, where Vite reads it as the end of
// its options and never sees the port; npm needs one, or the flag is its
// own.
#[test]
fn a_port_flag_is_handed_on_the_way_the_package_manager_expects() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::remove_file(dir.path().join("pnpm-lock.yaml")).unwrap();
    std::fs::write(dir.path().join("package-lock.json"), "{}\n").unwrap();
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    assert_eq!(processes["web"].cmd, "npm run dev -- --port {port:web}");
    assert_eq!(processes["api"].cmd, "npm run dev");
}

// An env example read raw kept the quotes: `"4000"` was no port, and
// `"http://localhost:4000"` no URL, so the web app kept pointing at the
// main checkout's api without a word.
#[test]
fn an_env_example_is_read_without_its_quotes_and_export() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::write(
        dir.path().join(".env.example"),
        "export WEB_PORT=5173\nexport API_PORT=\"4000\"\n\
         VITE_API_URL='http://localhost:4000' # the api\n",
    )
    .unwrap();
    assert_eq!(
        signals(dir.path()).env_example,
        vec![
            ("WEB_PORT".to_string(), "5173".to_string()),
            ("API_PORT".to_string(), "4000".to_string()),
            (
                "VITE_API_URL".to_string(),
                "http://localhost:4000".to_string()
            ),
        ]
    );
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    assert_eq!(
        processes["web"].env["VITE_API_URL"],
        "http://localhost:{port:api}"
    );
    assert_eq!(processes["web"].env["API_PORT"], "{port:api}");
    assert_eq!(processes["api"].env["WEB_PORT"], "{port:web}");
}

// A Node api and a Next web app both default to 3000. Pointed at the
// first in directory order, the web app's own NEXTAUTH_URL was rewritten
// to the api's port, and sign-in in the worktree broke.
#[test]
fn a_url_at_a_port_two_apps_default_to_points_at_neither() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("package.json"),
        r#"{ "workspaces": ["apps/*"], "scripts": { "dev": "turbo run dev" } }"#,
    )
    .unwrap();
    std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    for (name, script) in [("api", "tsx watch src/index.ts"), ("web", "next dev")] {
        std::fs::create_dir_all(root.join("apps").join(name)).unwrap();
        std::fs::write(
            root.join("apps").join(name).join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{script}" }} }}"#),
        )
        .unwrap();
    }
    std::fs::write(
        root.join("apps/web/next.config.js"),
        "module.exports = {}\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".env.example"),
        "NEXTAUTH_URL=http://localhost:3000\n",
    )
    .unwrap();
    let processes = proposed_processes(root).candidates[0]
        .processes
        .clone()
        .unwrap();
    assert_eq!(processes["api"].roles(), vec!["api"]);
    assert_eq!(processes["web"].roles(), vec!["web"]);
    for (name, process) in &processes {
        assert!(
            !process.env.contains_key("NEXTAUTH_URL"),
            "{name}: {:?}",
            process.env
        );
    }
}

// The create-turbo layout: each app's own script names its port, and the
// root only fans out with `turbo run dev`. Given `PORT` and a role, each
// app would bind its own port anyway and fail its readiness wait.
#[test]
fn an_app_whose_dev_script_fixes_its_port_owns_no_role() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    std::fs::write(
        root.join("package.json"),
        r#"{ "workspaces": ["apps/*"], "scripts": { "dev": "turbo run dev" } }"#,
    )
    .unwrap();
    std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    for (name, script) in [
        ("web", "next dev --turbopack --port 3000"),
        ("docs", "next dev --turbopack --port 3001"),
        ("site", "vite --port 5174"),
    ] {
        std::fs::create_dir_all(root.join("apps").join(name)).unwrap();
        std::fs::write(
            root.join("apps").join(name).join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{script}" }} }}"#),
        )
        .unwrap();
    }
    std::fs::write(root.join("apps/site/vite.config.ts"), "export default {}\n").unwrap();
    let proposal = proposed_processes(root);
    let processes = proposal.candidates[0].processes.clone().unwrap();
    for (name, process) in &processes {
        assert_eq!(
            process.cmd, "pnpm dev",
            "{name}: the flag is not said twice"
        );
        assert!(process.roles().is_empty(), "{name}: {:?}", process.ports);
        assert!(process.ready.is_none(), "{name}");
        assert!(
            !process.env.contains_key("PORT"),
            "{name}: {:?}",
            process.env
        );
    }
    assert!(
        proposal.candidates[0]
            .why
            .contains("web fixes its own port 3000 in its dev script"),
        "{}",
        proposal.candidates[0].why
    );
}

// The shell that runs `PORT=4000 tsx watch …` sets PORT over the one
// pando exported: given a role, the api bound 4000, the readiness wait
// timed out on the reserved port, and the next worktree hit 4000 too.
#[test]
fn an_app_whose_dev_script_assigns_its_port_owns_no_role() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::write(
        dir.path().join("apps/api/package.json"),
        r#"{ "scripts": { "dev": "PORT=4000 tsx watch src/index.ts" } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("apps/web/package.json"),
        r#"{ "scripts": { "dev": "PORT=3000 vite" } }"#,
    )
    .unwrap();
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    let api = apps.iter().find(|app| app.name == "api").unwrap();
    assert_eq!(api.fixed_port, Some(4000));
    assert_eq!(api.default_port, Some(4000));
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    let api = &processes["api"];
    assert!(api.roles().is_empty(), "{:?}", api.ports);
    assert!(api.ready.is_none());
    assert!(!api.env.contains_key("PORT"), "{:?}", api.env);
    // Vite's flag wins over the variable, so pando's port still does.
    assert_eq!(processes["web"].cmd, "pnpm dev --port {port:web}");
    assert_eq!(processes["web"].roles(), vec!["web"]);
}

// Next reads PORT, and the script's own assignment of it is the one the
// server sees, however it is spelt.
#[test]
fn a_dev_script_that_assigns_its_own_port_is_not_given_the_framework_one() {
    let next = RULES.iter().find(|r| r.name == "Next.js").unwrap();
    for body in [
        "PORT=3001 next dev",
        "cross-env PORT=3001 next dev",
        "env PORT='3001' next dev",
    ] {
        let signals = Signals {
            env_example: env_pairs(&["PORT"]),
            ..scripts(&[("dev", body)])
        };
        assert!(port_proposal(&signals, Some(next)).is_none(), "{body}");
    }
    // A port it reads from what it is given is still one it is given.
    for body in ["PORT=${PORT:-3001} next dev", "NEXT_PORT=3001 next dev"] {
        let signals = scripts(&[("dev", body)]);
        assert_eq!(
            values(&port_proposal(&signals, Some(next)).unwrap()),
            vec!["PORT"],
            "{body}"
        );
    }
}

/// Adds an app with a Node dev script to the `workspace` fixture.
fn add_app(dir: &Path, name: &str) {
    let app = dir.join("apps").join(name);
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{ "scripts": { "dev": "node --watch src/index.js" } }"#,
    )
    .unwrap();
}

// An env name is letters, digits and underscores, so `apps/admin.v2`
// declares its port as `ADMIN_V2_PORT`. Only the dash was translated, so
// the key the env example has for it was never found, and the app got no
// role at all.
#[test]
fn an_app_with_a_dot_in_its_name_finds_its_port_variable() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    add_app(dir.path(), "admin.v2");
    std::fs::write(
        dir.path().join(".env.example"),
        "WEB_PORT=5173\nAPI_PORT=4000\nADMIN_V2_PORT=4100\n",
    )
    .unwrap();
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    let admin = &processes["admin.v2"];
    assert_eq!(admin.roles(), vec!["admin.v2"]);
    assert_eq!(admin.env["ADMIN_V2_PORT"], "{port:admin.v2}");
}

// A per-app answer is written and then loaded back. An app whose name a
// placeholder cannot spell, one named after a log pando keeps for itself,
// or two whose port variables collide would fail there — so the per-app
// form is not offered for them, as for two apps of one name.
#[test]
fn apps_whose_names_cannot_be_roles_get_no_per_app_form() {
    for names in [&["my app"][..], &["proxy"], &["web-app", "web_app"]] {
        let dir = tempdir().unwrap();
        workspace(dir.path());
        for name in names {
            add_app(dir.path(), name);
        }
        let signals = signals(dir.path());
        assert!(workspace_apps(dir.path(), &signals).is_empty(), "{names:?}");
    }
}

#[test]
fn the_root_script_is_offered_beside_the_per_app_form() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    let proposal = proposed_processes(dir.path());
    assert_eq!(proposal.candidates.len(), 2);
    let fallback = &proposal.candidates[1];
    assert_eq!(fallback.value, "pnpm dev");
    let processes = fallback.processes.clone().expect("one process");
    assert_eq!(processes.keys().cloned().collect::<Vec<_>>(), vec!["dev"]);
    assert_eq!(processes["dev"].cmd, "pnpm dev");
    assert!(
        processes["dev"].ports.is_none(),
        "declining leaves the port question to be asked"
    );
}

#[test]
fn one_app_is_not_a_workspace_worth_splitting_up() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::remove_dir_all(dir.path().join("apps/api")).unwrap();
    let signals = signals(dir.path());
    assert!(
        propose(dir.path(), &signals)
            .iter()
            .all(|p| p.slot != Slot::Processes),
        "one app with a dev script is the single-process case"
    );
}

#[test]
fn an_app_with_no_dev_script_is_not_a_process() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::create_dir_all(dir.path().join("apps/tools")).unwrap();
    std::fs::write(
        dir.path().join("apps/tools/package.json"),
        r#"{ "scripts": { "build": "tsc" } }"#,
    )
    .unwrap();
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    assert_eq!(
        apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        vec!["api", "web"]
    );
}

// Two apps with one directory name would claim one role, and
// `config::validate` would refuse the file pando had just written.
#[test]
fn two_apps_with_the_same_name_are_not_proposed_at_all() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "workspaces": ["apps/*", "packages/*"] }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("packages/web")).unwrap();
    std::fs::write(
        dir.path().join("packages/web/package.json"),
        r#"{ "scripts": { "dev": "vite" } }"#,
    )
    .unwrap();
    assert!(workspace_apps(dir.path(), &signals(dir.path())).is_empty());
}

#[test]
fn a_repository_that_is_not_a_workspace_proposes_nothing() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "dev": "next dev" } }"#,
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("apps/web")).unwrap();
    std::fs::write(
        dir.path().join("apps/web/package.json"),
        r#"{ "scripts": { "dev": "vite" } }"#,
    )
    .unwrap();
    assert!(
        workspace_apps(dir.path(), &signals(dir.path())).is_empty(),
        "an apps/ directory is not a workspace; the manifest has to say so"
    );
}

// A member the workspace leaves out with `!` is not proposed: pnpm and
// npm both read it, and an app pando proposes the developer excluded is
// a wrong proposal, not a missing one.
#[test]
fn a_workspace_member_excluded_with_a_negated_glob_is_not_an_app() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "workspaces": ["apps/*", "!apps/api"], "scripts": { "dev": "pnpm -r --parallel dev" } }"#,
    )
    .unwrap();
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    let names: Vec<&str> = apps.iter().map(|app| app.name.as_str()).collect();
    assert!(names.contains(&"web"), "{names:?}");
    assert!(!names.contains(&"api"), "{names:?}");
}

#[test]
fn a_workspace_glob_matches_within_and_across_directories() {
    use super::workspaces::glob_matches;
    for (pattern, dir, matches) in [
        ("apps/api", "apps/api", true),
        ("./apps/api/", "apps/api", true),
        ("apps/*", "apps/api", true),
        ("apps/*", "apps/api/inner", false),
        ("apps/a*", "apps/api", true),
        ("apps/b*", "apps/api", false),
        ("**/test/**", "apps/test", true),
        ("**/test/**", "packages/x/test/fixtures", true),
        ("**/test/**", "apps/tests", false),
        ("**", "apps/web", true),
        ("apps/api", "apps/apiary", false),
    ] {
        assert_eq!(glob_matches(pattern, dir), matches, "{pattern} ~ {dir}");
    }
}

#[test]
fn workspace_globs_are_read_from_every_convention() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("pnpm-workspace.yaml"),
        "packages:\n  - 'apps/*'\n",
    )
    .unwrap();
    assert_eq!(workspace_globs(dir.path()), vec!["apps/*"]);

    // Every other way pnpm accepts the same list. Read only the first way,
    // a plain pnpm workspace had no apps and fell back to the root script
    // without a word.
    for (text, expected) in [
        (
            "packages:\n- apps/*\n- packages/*\n",
            &["apps/*", "packages/*"][..],
        ),
        (
            "# the workspace\npackages: # members\n  - 'apps/*' # web and api\n  - packages/* # shared\n",
            &["apps/*", "packages/*"],
        ),
        (
            "packages: ['apps/*', \"packages/*\"] # both\n",
            &["apps/*", "packages/*"],
        ),
        // The next key ends the list, at any column its items sit at.
        (
            "packages:\n- apps/*\nonlyBuiltDependencies:\n- esbuild\n",
            &["apps/*"],
        ),
    ] {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pnpm-workspace.yaml"), text).unwrap();
        assert_eq!(workspace_globs(dir.path()), expected, "{text:?}");
    }

    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "workspaces": { "packages": ["services/*"] } }"#,
    )
    .unwrap();
    assert_eq!(workspace_globs(dir.path()), vec!["services/*"]);

    // turbo and nx describe pipelines, not membership.
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("turbo.json"), "{}").unwrap();
    assert_eq!(workspace_globs(dir.path()), vec!["apps/*", "packages/*"]);
}

#[test]
fn only_a_localhost_url_names_another_apps_port() {
    assert_eq!(localhost_url_port("http://localhost:4000"), Some(4000));
    assert_eq!(
        localhost_url_port("http://127.0.0.1:4000/api/v1"),
        Some(4000)
    );
    assert_eq!(
        localhost_url_port("postgres://app:app@localhost:5432/app"),
        Some(5432)
    );
    assert_eq!(localhost_url_port("https://api.example.com:443"), None);
    assert_eq!(localhost_url_port("http://localhost"), None);
    assert_eq!(localhost_url_port("4000"), None);
}

#[test]
fn a_url_that_matches_no_app_is_left_alone() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    for (name, process) in &processes {
        assert!(
            !process.env.contains_key("DATABASE_URL"),
            "{name} was told about a database that is not one of the apps: {:?}",
            process.env
        );
    }
}

#[test]
fn the_processes_slot_writes_a_table_per_app() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    let candidate = proposed_processes(dir.path()).candidates[0].clone();
    let edits = edits(Slot::Processes, &candidate);
    let web: Vec<(&str, String)> = edits
        .iter()
        .filter(|e| e.table == vec!["processes", "web"])
        .map(|e| (e.key.as_str(), e.value.to_string().trim().to_string()))
        .collect();
    assert_eq!(
        web,
        vec![
            ("cmd", "\"pnpm dev --port {port:web}\"".to_string()),
            ("cwd", "\"apps/web\"".to_string()),
            ("ports", "[\"web\"]".to_string()),
            (
                "env",
                "{ API_PORT = \"{port:api}\", VITE_API_URL = \"http://localhost:{port:api}\" }"
                    .to_string()
            ),
            ("ready", "{ role = \"web\" }".to_string()),
        ]
    );
    assert!(
        edits.iter().any(|e| e.table == vec!["processes", "api"]),
        "and one for the api"
    );
}

// The single-process fallback keeps the `[dev]` shorthand: that is
// what it is for, and it is the shape every example is written in.
#[test]
fn the_single_process_answer_is_written_as_the_dev_shorthand() {
    let dir = tempdir().unwrap();
    workspace(dir.path());
    let candidate = proposed_processes(dir.path()).candidates[1].clone();
    let edits = edits(Slot::Processes, &candidate);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].table, vec!["dev"]);
    assert_eq!(edits[0].key, "cmd");
}

#[test]
fn a_command_typed_at_the_process_question_is_one_process() {
    let candidate = custom(Slot::Processes, "./scripts/dev.sh --port {port:web}");
    let processes = candidate.processes.clone().expect("one process");
    assert_eq!(processes.keys().cloned().collect::<Vec<_>>(), vec!["dev"]);
    assert_eq!(processes["dev"].roles(), vec!["web"]);
    let mut config = Config::default();
    apply(Slot::Processes, &candidate, &mut config);
    assert_eq!(
        config.processes["dev"].cmd,
        "./scripts/dev.sh --port {port:web}"
    );
}

// A typed answer here used to be accepted and then silently dropped:
// the schema slot's answer is a whole `[[hooks]]` entry, and a
// candidate with no hook on it gives `edits` and `apply` nothing to
// write. Exit 0, and a config with no migration in it.
#[test]
fn a_command_typed_at_the_schema_question_is_a_hook_entry() {
    let candidate = custom(Slot::SchemaHook, "npm run db:migrate");
    let hook = candidate.hook.clone().expect("a hook entry");
    assert_eq!(hook.name, SCHEMA_HOOK);
    assert_eq!(hook.after, crate::config::HookPoint::Services);
    assert_eq!(hook.cmd, "npm run db:migrate");
    assert!(
        hook.fingerprint.is_empty(),
        "a command pando did not propose carries no globs it could key on, and a guessed \
             fingerprint is a migration that never runs"
    );

    let mut config = Config::default();
    apply(Slot::SchemaHook, &candidate, &mut config);
    assert_eq!(config.hooks.len(), 1);
    assert_eq!(config.hooks[0].cmd, "npm run db:migrate");
    let (array, entries) = array_edits(Slot::SchemaHook, &[&candidate]).expect("an entry");
    assert_eq!(array, "hooks");
    assert!(
        entries.iter().any(|(key, _)| key == "cmd"),
        "and it is written out: {entries:?}"
    );
}

// ---- compose services ------------------------------------------------

/// A repository with a compose file and an env example, which is all
/// the services rule reads.
fn compose_fixture(compose: &str, env: &[(&str, &str)]) -> (TempDir, Signals) {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("docker-compose.yml"), compose).unwrap();
    let signals = Signals {
        env_example: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    };
    (dir, signals)
}

// The first-contact failure this rule exists for: the compose file's
// only candidate was the app itself, so the only answer on offer was
// "run a second copy of the thing you are developing".
#[test]
fn a_service_built_from_this_repository_is_not_a_dependency() {
    let (dir, signals) = compose_fixture(
        r#"
services:
  web:
    image: acme/web:dev
    build: .
    ports: ["3000:3000"]
  worker:
    build:
      context: ./services/worker
  postgres:
    image: postgres:16
    ports: ["5432:5432"]
"#,
        &[("DATABASE_URL", "postgres://acme@localhost:5432/acme")],
    );
    let proposal = services_proposal(
        dir.path(),
        &signals,
        None,
        &MachineEvidence::unknown(),
        None,
    )
    .unwrap();
    assert_eq!(
        values(&proposal),
        vec!["postgres"],
        "the app and its worker are this repository, not things it depends on"
    );
    assert!(
        proposal.decided,
        "one resolved dependency is not a question"
    );
}

// A question whose only answer is wrong is worse than silence — but the
// answer still has to be recorded, or the next start works it out again
// and nothing in the file says pando ever looked.
#[test]
fn a_compose_file_of_only_this_project_is_answered_without_being_asked() {
    let (dir, signals) = compose_fixture(
        "services:\n  web:\n    build: .\n  worker:\n    build: ./worker\n",
        &[],
    );
    let proposal = services_proposal(
        dir.path(),
        &signals,
        None,
        &MachineEvidence::unknown(),
        None,
    )
    .unwrap();
    assert!(
        proposal.candidates.is_empty(),
        "nothing survived the filter"
    );
    assert!(proposal.decided, "so there is nothing to ask about");
    assert_eq!(
        proposal.service_file(),
        Some("docker-compose.yml"),
        "the empty answer is still about a file, and has to be writable"
    );
    let why = proposal
        .none_because
        .expect("a reason for the empty answer");
    assert!(why.contains("web and worker"), "{why}");
    assert!(why.contains("built from this repository"), "{why}");
}

// The other way a compose file has nothing to offer: pando can address
// none of what is in it. That used to propose nothing and record
// nothing, so it was worked out again on every start — the same
// re-ask-forever shape a filtered-out app had. One shape for "none".
#[test]
fn a_file_of_services_nothing_addresses_is_answered_without_being_asked() {
    let (dir, signals) = compose_fixture(
        r#"
services:
  mailpit:
    image: axllent/mailpit
    ports: ["1025:1025"]
  dashboard:
    image: grafana/grafana
"#,
        &[("PORT", "3000")],
    );
    let proposal = services_proposal(
        dir.path(),
        &signals,
        None,
        &MachineEvidence::unknown(),
        None,
    )
    .unwrap();
    assert!(proposal.candidates.is_empty());
    assert!(proposal.decided);
    assert_eq!(proposal.service_file(), Some("docker-compose.yml"));
    let why = proposal
        .none_because
        .expect("a reason for the empty answer");
    assert!(why.contains("dashboard and mailpit"), "{why}");
    assert!(
        why.contains("nothing in the env example addresses"),
        "{why}"
    );
}

// The empty answer is permanent — `already_answered` is true from then
// on — so it may only be recorded about a file pando read whole. A
// top-level `include:` brings in services this list does not even have,
// and "none of them" about those is an answer nobody gave.
#[test]
fn a_file_pando_could_not_read_whole_records_no_empty_answer() {
    let (dir, signals) = compose_fixture(
        r#"
include:
  - infra/compose.yml
services:
  web:
    build: .
"#,
        &[],
    );
    assert!(
        services_proposal(
            dir.path(),
            &signals,
            None,
            &MachineEvidence::unknown(),
            None
        )
        .is_none(),
        "nothing is offered and nothing is written down: the next run \
             with Docker present gets to decide"
    );
}

// The filter is about *this project*, not about `build:` existing. A
// service built out of a sibling checkout is a dependency like any
// other, and a worktree wants its own copy.
#[test]
fn a_build_context_outside_the_project_is_still_a_dependency() {
    let (dir, signals) = compose_fixture(
        r#"
services:
  vendor:
    build: ../vendor-service
    ports: ["9000:9000"]
"#,
        &[("VENDOR_URL", "http://localhost:9000")],
    );
    let proposal = services_proposal(
        dir.path(),
        &signals,
        None,
        &MachineEvidence::unknown(),
        None,
    )
    .unwrap();
    assert_eq!(values(&proposal), vec!["vendor"]);
    assert!(proposal.preferred().unwrap().preselected);
}

#[test]
fn a_build_context_is_resolved_against_the_compose_files_own_directory() {
    let dir = tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("deploy")).unwrap();
    for (file, context, inside) in [
        ("docker-compose.yml", ".", true),
        ("docker-compose.yml", "./services/api", true),
        ("docker-compose.yml", "../elsewhere", false),
        // From a file one level down, `..` is still the project.
        ("deploy/docker-compose.yml", "..", true),
        ("deploy/docker-compose.yml", "../..", false),
    ] {
        assert_eq!(
            built_from_project(root, file, context),
            inside,
            "{file} + {context}"
        );
    }
    // What `docker compose config` hands over: already absolute.
    assert!(built_from_project(
        root,
        "docker-compose.yml",
        root.join("apps/api").to_str().unwrap()
    ));
    assert!(!built_from_project(root, "docker-compose.yml", "/tmp"));
}

// ---- what detection may fill in --------------------------------------

#[test]
fn a_lone_dev_with_no_command_may_be_filled_and_anything_else_may_not() {
    let mut empty = Config::default();
    assert!(
        may_fill_dev(&empty),
        "nothing configured is pando's to fill"
    );

    empty.processes.insert(
        DEV.to_string(),
        ProcessConfig {
            cwd: Some("apps/web".to_string()),
            ..Default::default()
        },
    );
    assert!(
        may_fill_dev(&empty),
        "a [dev] with no command is an invitation"
    );
    assert!(still_needed(Slot::DevCmd, &empty));
    assert!(still_needed(Slot::PortEnv, &empty));
    assert!(
        !still_needed(Slot::Processes, &empty),
        "but the shape question has been answered by writing [dev] at all"
    );

    let mut written = Config::default();
    written.processes.insert(
        DEV.to_string(),
        ProcessConfig {
            cmd: "sleep 300".to_string(),
            ..Default::default()
        },
    );
    assert!(
        !may_fill_dev(&written),
        "a command the developer wrote is an answer about its ports too"
    );

    let mut named = Config::default();
    named.processes.insert(
        "web".to_string(),
        ProcessConfig {
            cmd: "vite".to_string(),
            ..Default::default()
        },
    );
    assert!(
        !may_fill_dev(&named),
        "a process under another name means [dev] would land beside [processes]"
    );

    // And after an answer to the shape question.
    let mut per_app = Config::default();
    per_app
        .processes
        .insert("web".to_string(), ProcessConfig::default());
    per_app
        .processes
        .insert("api".to_string(), ProcessConfig::default());
    assert!(!fills_one_dev_process(&per_app));
    let mut single = Config::default();
    single
        .processes
        .insert(DEV.to_string(), ProcessConfig::default());
    assert!(fills_one_dev_process(&single));
}

// ---- writing the answer ----------------------------------------------

#[test]
fn a_chosen_candidate_becomes_both_config_and_a_patch() {
    let candidate = Candidate {
        value: "PORT".to_string(),
        why: "the Next.js convention".to_string(),
        ..Candidate::default()
    };
    let mut config = Config::default();
    apply(Slot::PortEnv, &candidate, &mut config);
    assert_eq!(config.processes["dev"].roles(), vec!["web"]);
    assert_eq!(config.processes["dev"].port_env()["PORT"], "{port:web}");

    let edits = edits(Slot::PortEnv, &candidate);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].table, vec!["dev"]);
    assert_eq!(edits[0].key, "ports");
    assert_eq!(edits[0].value.to_string().trim(), "{ PORT = \"web\" }");
}

#[test]
fn a_list_slot_round_trips_through_its_comma_separated_value() {
    let candidate = Candidate {
        value: ".env,.env.local".to_string(),
        why: "gitignored and present".to_string(),
        ..Candidate::default()
    };
    let mut config = Config::default();
    apply(Slot::Provision, &candidate, &mut config);
    assert_eq!(
        config.project.provision.as_deref(),
        Some(&[".env".to_string(), ".env.local".to_string()][..])
    );
    let edits = edits(Slot::Provision, &candidate);
    assert_eq!(
        edits[0].value.to_string().trim(),
        "[\".env\", \".env.local\"]"
    );
}

// ---- provisioning from an example ------------------------------------

/// A repository that ships an example of a local file, with or without
/// the local file itself.
fn seed_fixture(files: &[(&str, &str)]) -> TempDir {
    let dir = tempdir().unwrap();
    crate::testutil::git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
    for (rel, contents) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    dir
}

// An ignored dependency tree is named once and dropped, not walked: what
// comes back is the root's own ignored files, and nothing from inside
// `node_modules` or from a directory that is not ignored itself.
#[test]
fn only_root_level_ignored_files_are_present_ones() {
    let dir = seed_fixture(&[
        (".gitignore", "node_modules/\n.env\n*.local\n"),
        (".env", "PORT=3000\n"),
        ("config.local", "\n"),
    ]);
    for rel in ["node_modules/pkg/lib", "sub"] {
        std::fs::create_dir_all(dir.path().join(rel)).unwrap();
    }
    for i in 0..50 {
        std::fs::write(dir.path().join(format!("node_modules/pkg/lib/{i}.js")), "").unwrap();
    }
    std::fs::write(dir.path().join("node_modules/.env"), "").unwrap();
    std::fs::write(dir.path().join("sub/x.local"), "").unwrap();
    assert_eq!(ignored_present(dir.path()), [".env", "config.local"]);
}

// A test run leaves a coverage database at the root, gitignored and
// present, and it is not a local setting: taking it was a first run's
// only provision answer, and `--yes` took it.
#[test]
fn a_tool_artifact_is_never_a_provision_file() {
    let dir = seed_fixture(&[
        (".gitignore", ".coverage*\n.env\n.eslintcache\n"),
        (".coverage", "sqlite"),
        (".coverage.laptop.1.2", "sqlite"),
        (".eslintcache", "{}"),
        (".env", "PORT=3000\n"),
    ]);
    assert_eq!(ignored_present(dir.path()), [".env"]);
}

// `eas build --local` leaves its packages at the root, gitignored and
// present, a gigabyte and more of them: never a worktree's local file.
#[test]
fn a_packaged_build_or_a_log_is_never_a_provision_file() {
    let dir = seed_fixture(&[
        (".gitignore", ".env\nbuild-*\n*.log\n"),
        ("build-1759132.aab", "zip"),
        ("build-1759132.ipa", "zip"),
        ("build-1759133.apk", "zip"),
        ("metro.log", "\n"),
        (".env", "PORT=3000\n"),
    ]);
    assert_eq!(ignored_present(dir.path()), [".env"]);
}

// The dependency trees `clone` can share: the root's and each app's,
// named once as git names an ignored directory. Never one inside a
// hidden directory — a worktree kept in the checkout, under `.claude/` —
// nor one deeper than an app's, nor a gitignored directory no package
// manager installs into.
#[test]
fn the_ignored_dependency_trees_are_the_ones_clone_can_share() {
    let dir = seed_fixture(&[
        (".gitignore", "node_modules/\nvendor/\n.claude/\n"),
        ("package.json", "{}"),
        ("apps/web/package.json", "{}"),
        ("apps/api/package.json", "{}"),
        ("tools/a/b/package.json", "{}"),
    ]);
    crate::testutil::git(dir.path(), &["add", "."]);
    crate::testutil::git(dir.path(), &["commit", "--quiet", "-m", "apps"]);
    for rel in [
        "node_modules/pkg",
        "apps/web/node_modules/pkg",
        "apps/api/node_modules/pkg",
        ".claude/worktrees/x/node_modules/pkg",
        "tools/a/b/node_modules/pkg",
        "vendor/lib",
    ] {
        std::fs::create_dir_all(dir.path().join(rel)).unwrap();
        std::fs::write(dir.path().join(rel).join("index.js"), "").unwrap();
    }
    assert_eq!(
        signals(dir.path()).dependency_dirs,
        [
            "node_modules",
            "apps/api/node_modules",
            "apps/web/node_modules"
        ]
    );
}

// Proposed and decided wherever there is a tree to share — and not for
// an install that deletes `node_modules` before it installs, where every
// clone would be thrown away.
#[test]
fn clone_is_proposed_unless_the_install_deletes_the_tree_first() {
    let signals = Signals {
        dependency_dirs: vec!["node_modules".into(), "apps/api/node_modules".into()],
        ..Signals::default()
    };
    for install in [
        None,
        Some("npm install"),
        Some("pnpm install --frozen-lockfile"),
    ] {
        let proposal = clone_proposal(&signals, install).expect("a tree to share");
        assert_eq!(proposal.slot, Slot::Clone);
        assert!(proposal.decided, "{install:?}");
        assert_eq!(
            proposal.candidates[0].value,
            "node_modules,apps/api/node_modules"
        );
    }
    assert!(clone_proposal(&signals, Some("npm ci")).is_none());
    assert!(clone_proposal(&Signals::default(), Some("npm install")).is_none());
}

// A list, written as one like `provision`; "none" is `clone = []`.
#[test]
fn a_clone_answer_is_written_as_a_list() {
    let mut config = Config::default();
    let candidate = Candidate {
        value: "node_modules,apps/api/node_modules".into(),
        ..Candidate::default()
    };
    assert!(still_needed(Slot::Clone, &config));
    apply(Slot::Clone, &candidate, &mut config);
    assert_eq!(
        config.project.clones(),
        [
            "node_modules".to_string(),
            "apps/api/node_modules".to_string()
        ]
    );
    assert!(!still_needed(Slot::Clone, &config));
    assert!(Slot::Clone.is_list() && Slot::Clone.allows_none());
}

// The fresh-clone case: `.env` is gitignored so it never arrives, and
// the example beside it is the only thing that says what it looks like.
#[test]
fn an_example_of_a_missing_gitignored_file_is_a_provision_source() {
    let dir = seed_fixture(&[
        (".gitignore", ".env\n.env.local\n"),
        (".env.example", "PORT=3000\n"),
        (".env.local.example", "FLAG=1\n"),
    ]);
    assert_eq!(
        provision_seeds(dir.path()),
        vec![
            (".env".to_string(), ".env.example".to_string()),
            (".env.local".to_string(), ".env.local.example".to_string()),
        ]
    );
}

#[test]
fn a_file_the_checkout_already_has_is_not_seeded_from_its_example() {
    let dir = seed_fixture(&[
        (".gitignore", ".env\n"),
        (".env.example", "PORT=3000\n"),
        (".env", "PORT=3001\n"),
    ]);
    assert!(
        provision_seeds(dir.path()).is_empty(),
        "the developer's own file is the source; the example is the fallback"
    );
}

// `git check-ignore` is what authorises a write into a worktree, so a
// destination it would refuse must never be offered — proposing a seed
// `new` then refuses is worse than proposing none.
#[test]
fn an_example_of_a_file_that_is_not_gitignored_is_never_offered() {
    let dir = seed_fixture(&[
        (".gitignore", "node_modules/\n"),
        ("config.yml.example", "debug: true\n"),
    ]);
    assert!(provision_seeds(dir.path()).is_empty());
}

#[test]
fn a_seed_makes_the_provision_slot_a_question_with_the_plain_answer_under_it() {
    let signals = Signals {
        ignored_present: vec![".env.local".to_string()],
        provision_seeds: vec![(".env".to_string(), ".env.example".to_string())],
        ..Default::default()
    };
    let proposal = provision_proposal(&signals).unwrap();
    assert!(
        !proposal.decided,
        "copying a tracked example into a worktree is the developer's call"
    );
    assert_eq!(values(&proposal), vec![".env.local", ".env.local,.env"]);

    let plain = proposal.preferred().unwrap();
    assert!(
        plain.provision_from.is_empty() && !plain.needs_a_human,
        "only what is already here leads, and it is what --yes takes"
    );

    let seeded = &proposal.candidates[1];
    assert_eq!(
        seeded.provision_from,
        BTreeMap::from([(".env".to_string(), ".env.example".to_string())])
    );
    assert!(
        seeded.needs_a_human,
        "copying a file pando did not write is not a flag's decision"
    );
    assert!(
        seeded.why.contains(".env copied from .env.example"),
        "the option names the source, so a human can go and read it: {}",
        seeded.why
    );
}

// The case the whole feature is for — a clone with nothing local at all
// — has no plain answer to lead with, so there is nothing `--yes` may
// take and the question is what an unattended run gets.
#[test]
fn a_clone_with_only_a_seed_to_offer_has_nothing_yes_may_take() {
    let signals = Signals {
        provision_seeds: vec![(".env".to_string(), ".env.example".to_string())],
        ..Default::default()
    };
    let proposal = provision_proposal(&signals).unwrap();
    assert_eq!(values(&proposal), vec![".env"]);
    assert!(proposal.candidates[0].needs_a_human);
    assert!(!proposal.decided);
}

#[test]
fn with_nothing_to_seed_the_provision_slot_is_decided_as_it_always_was() {
    let signals = Signals {
        ignored_present: vec![".env".to_string(), ".env.local".to_string()],
        ..Default::default()
    };
    let proposal = provision_proposal(&signals).unwrap();
    assert!(proposal.decided);
    assert_eq!(values(&proposal), vec![".env,.env.local"]);
}

#[test]
fn a_seeded_answer_writes_the_list_and_where_the_missing_file_comes_from() {
    let candidate = Candidate {
        value: ".env".to_string(),
        why: "seeded".to_string(),
        provision_from: BTreeMap::from([(".env".to_string(), ".env.example".to_string())]),
        ..Candidate::default()
    };
    let mut config = Config::default();
    apply(Slot::Provision, &candidate, &mut config);
    assert_eq!(config.project.provision_paths(), [".env".to_string()]);
    assert_eq!(config.project.provision_from[".env"], ".env.example");

    let edits = edits(Slot::Provision, &candidate);
    assert_eq!(edits.len(), 2, "the list, and where the file comes from");
    assert_eq!(edits[1].table, vec!["project"]);
    assert_eq!(edits[1].key, "provision_from");
    assert_eq!(
        edits[1].value.to_string().trim(),
        r#"{ ".env" = ".env.example" }"#
    );
}

#[test]
fn a_command_that_carries_its_port_writes_both_keys() {
    let candidate = Candidate {
        value: "python manage.py runserver 127.0.0.1:{port:web}".to_string(),
        why: "the Django rule".to_string(),
        ports: Some(PortsSpec::List(vec!["web".to_string()])),
        ..Candidate::default()
    };
    let edits = edits(Slot::DevCmd, &candidate);
    assert_eq!(edits.len(), 2, "the command and the role it needs");
    assert_eq!(edits[1].key, "ports");
    assert_eq!(edits[1].value.to_string().trim(), "[\"web\"]");
}

// ---- native versus container, decided from evidence ------------------

/// A project whose env example names these addresses, and optionally
/// a compose file declaring these services.
fn project(env: &[(&str, &str)], compose: Option<&str>) -> (TempDir, Signals) {
    let dir = tempdir().unwrap();
    if let Some(yaml) = compose {
        std::fs::write(dir.path().join("docker-compose.yml"), yaml).unwrap();
    }
    let signals = Signals {
        env_example: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    };
    (dir, signals)
}

/// A machine that has everything, or one that has nothing.
fn machine(docker: bool, engines: &[(&str, bool)]) -> MachineEvidence {
    MachineEvidence {
        probed: true,
        docker,
        engines: engines.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
    }
}

const COMPOSE_PG: &str =
    "services:\n  postgres:\n    image: postgres:16\n  redis:\n    image: redis:7\n";

fn choice_of(
    dir: &TempDir,
    signals: &Signals,
    evidence: &MachineEvidence,
    prefer: Option<&str>,
) -> ServiceChoice {
    let compose = compose_services_proposal(dir.path(), signals, None);
    let native = native_candidates(signals, evidence);
    let file = compose
        .as_ref()
        .and_then(|p| p.service_file())
        .map(str::to_string)
        .or_else(|| crate::compose::find(dir.path()));
    service_choice(
        file.as_deref(),
        compose.as_ref().map(|p| p.candidates.len()).unwrap_or(0),
        &native,
        evidence,
        prefer,
    )
}

// The engine is named by the scheme of the URL or by the port a bare
// number defaults to — never by the key's prefix. `DATABASE_URL` is
// `DATABASE` for Postgres, MySQL and Mongo alike, and using it to
// pick an engine would be a coin toss wearing a rule's clothes.
#[test]
fn the_engine_comes_from_the_scheme_or_the_port_and_never_from_the_key() {
    for (value, expected) in [
        ("postgres://u:p@localhost:5432/app", Some("postgres")),
        ("postgresql://localhost/app", Some("postgres")),
        ("mysql://u:p@localhost:3306/app", Some("mariadb")),
        ("redis://localhost:6379", Some("redis")),
        ("mongodb+srv://localhost/app", Some("mongodb")),
        // No scheme pando knows: the port is the fallback.
        ("5432", Some("postgres")),
        ("27017", Some("mongodb")),
        ("http://localhost:3000", None),
        ("8080", None),
    ] {
        assert_eq!(recipe_for_address(value), expected, "{value}");
    }
    // The same key spelling, three different engines: the proof that
    // the prefix is not what decided any of them.
    for (value, expected) in [
        ("postgres://localhost:5432/a", "postgres"),
        ("mysql://localhost:3306/a", "mariadb"),
        ("mongodb://localhost:27017/a", "mongodb"),
    ] {
        let (dir, signals) = project(&[("DATABASE_URL", value)], None);
        let native = native_candidates(&signals, &MachineEvidence::unknown());
        assert_eq!(values_of(&native), vec![expected], "{value}");
        let _ = dir;
    }
}

// Quoted, the scheme read as `"postgres`, and a port that is not the
// default had nothing left to name the engine by.
#[test]
fn a_quoted_address_in_the_env_example_still_names_its_engine() {
    let dir = seed_fixture(&[(
        ".env.example",
        "DATABASE_URL=\"postgres://localhost:5433/db\"\n",
    )]);
    let native = native_candidates(&signals(dir.path()), &MachineEvidence::unknown());
    assert_eq!(values_of(&native), vec!["postgres"]);
}

// A backend that splits each address over a host and a port key names its
// services as surely as one that writes URLs: the port key is what a
// worktree's own service is pointed at, the host beside it says so.
#[test]
fn a_host_and_port_pair_names_a_service_as_a_url_does() {
    let (dir, signals) = project(
        &[
            ("POSTGRES_SERVER", "localhost"),
            ("POSTGRES_PORT", "5432"),
            ("POSTGRES_DB", "app"),
            ("REDIS_QUEUE_HOST", "localhost"),
            ("REDIS_QUEUE_PORT", "6379"),
        ],
        None,
    );
    let native = native_candidates(&signals, &MachineEvidence::unknown());
    assert_eq!(values_of(&native), vec!["postgres", "redis"]);
    let keys: Vec<Option<&str>> = native
        .iter()
        .map(|c| c.service.as_ref().and_then(|h| h.env_key.as_deref()))
        .collect();
    assert_eq!(keys, vec![Some("POSTGRES_PORT"), Some("REDIS_QUEUE_PORT")]);
    assert_eq!(
        native[0].why,
        ".env.example POSTGRES_SERVER=localhost and POSTGRES_PORT=5432"
    );
    let _ = dir;
}

// Off its default port, a pair's stem names the engine the way a URL's
// scheme does. A bare port on its own, or a stem that names no engine,
// still says nothing.
#[test]
fn a_pairs_stem_names_its_engine_off_the_default_port() {
    for (env, expected) in [
        (
            &[("POSTGRES_HOST", "localhost"), ("POSTGRES_PORT", "5433")][..],
            vec!["postgres"],
        ),
        (
            &[
                ("REDIS_CACHE_HOSTNAME", "127.0.0.1"),
                ("REDIS_CACHE_PORT", "6380"),
            ][..],
            vec!["redis"],
        ),
        (&[("POSTGRES_PORT", "5433")][..], vec![]),
        (
            &[("POSTGRES_HOST", ""), ("POSTGRES_PORT", "5433")][..],
            vec![],
        ),
        (
            &[("DATABASE_HOST", "localhost"), ("DATABASE_PORT", "5433")][..],
            vec![],
        ),
        (&[("API_HOST", "0.0.0.0"), ("API_PORT", "8000")][..], vec![]),
    ] {
        let (dir, signals) = project(env, None);
        let native = native_candidates(&signals, &MachineEvidence::unknown());
        assert_eq!(values_of(&native), expected, "{env:?}");
        let _ = dir;
    }
}

fn values_of(candidates: &[Candidate]) -> Vec<&str> {
    candidates.iter().map(|c| c.value.as_str()).collect()
}

// Two keys naming the same engine are one server, and two engines
// are two candidates, sorted so two runs agree.
#[test]
fn one_candidate_per_engine_however_many_keys_name_it() {
    let (dir, signals) = project(
        &[
            ("REDIS_URL", "redis://localhost:6379"),
            ("DATABASE_URL", "postgres://localhost:5432/app"),
            ("CACHE_URL", "redis://localhost:6379/1"),
        ],
        None,
    );
    let native = native_candidates(&signals, &MachineEvidence::unknown());
    assert_eq!(values_of(&native), vec!["postgres", "redis"]);
    let _ = dir;
}

// A Postgres with an extension built in is still the project's database.
// Unknown, `db` on it matched no key, counted as nothing the app talks
// to, and a plain native postgres took its place without the extension.
#[test]
fn a_postgres_flavoured_image_is_the_database_its_env_example_addresses() {
    let (dir, signals) = project(
        &[("DATABASE_URL", "postgresql://postgres@localhost:5432/app")],
        Some("services:\n  db:\n    image: pgvector/pgvector:pg16\n    ports: [\"5432:5432\"]\n"),
    );
    let compose = compose_services_proposal(dir.path(), &signals, None).unwrap();
    assert_eq!(values(&compose), vec!["db"]);
    assert_eq!(
        compose.candidates[0]
            .service
            .as_ref()
            .and_then(|hint| hint.env_key.as_deref()),
        Some("DATABASE_URL")
    );
    let evidence = machine(true, &[("postgres", true)]);
    let choice = choice_of(&dir, &signals, &evidence, None);
    assert_eq!(choice.mechanism, Some("compose"), "{:?}", choice.evidence);
}

// The shape Phase 6 exists for: a project that needs services and has
// no file describing them. There is no compose option at all, so the
// preference never comes into it.
#[test]
fn a_project_with_no_compose_file_and_a_database_in_its_env_gets_the_recipes() {
    let (dir, signals) = project(
        &[
            ("DATABASE_URL", "postgres://localhost:5432/app"),
            ("REDIS_URL", "redis://localhost:6379"),
        ],
        None,
    );
    let evidence = machine(true, &[("postgres", true), ("redis", true)]);
    let choice = choice_of(&dir, &signals, &evidence, None);
    assert_eq!(choice.mechanism, Some("native"));
    assert!(choice.native_declared);
    assert!(!choice.compose_declared);
    assert!(
        choice.evidence[0].contains("no compose file"),
        "{:?}",
        choice.evidence
    );
    assert!(
        choice.evidence[1].contains("postgres and redis"),
        "{:?}",
        choice.evidence
    );

    let proposal = propose_with(dir.path(), &signals, None, &evidence, None)
        .into_iter()
        .find(|p| p.slot == Slot::Services)
        .expect("a services proposal");
    assert_eq!(proposal.mechanism, Some("native"));
    assert_eq!(values(&proposal), vec!["postgres", "redis"]);
    assert!(proposal.decided, "this machine has both engines");
}

// The common path is unchanged: a compose file is the project's own
// statement about how to run its services, and nobody has said
// otherwise. The evidence line is how a developer learns there was a
// choice at all.
#[test]
fn a_compose_file_wins_when_nobody_has_said_which_to_prefer() {
    let (dir, signals) = project(
        &[
            ("DATABASE_URL", "postgres://localhost:5432/app"),
            ("REDIS_URL", "redis://localhost:6379"),
        ],
        Some(COMPOSE_PG),
    );
    let evidence = machine(true, &[("postgres", true), ("redis", true)]);
    let choice = choice_of(&dir, &signals, &evidence, None);
    assert_eq!(choice.mechanism, Some("compose"));
    assert!(choice.compose_declared && choice.native_declared);
    let said = choice.evidence.join(" | ");
    assert!(said.contains("nobody has said which to prefer"), "{said}");
    assert!(said.contains("[isolation] prefer"), "{said}");
}

#[test]
fn a_machine_that_prefers_the_recipes_gets_them() {
    let (dir, signals) = project(
        &[("DATABASE_URL", "postgres://localhost:5432/app")],
        Some(COMPOSE_PG),
    );
    let evidence = machine(true, &[("postgres", true), ("redis", true)]);
    let choice = choice_of(&dir, &signals, &evidence, Some("native"));
    assert_eq!(choice.mechanism, Some("native"));
    assert!(
        choice.evidence.iter().any(|l| l.contains("prefers native")),
        "{:?}",
        choice.evidence
    );
}

// A preference this machine cannot honour is not overruled quietly.
#[test]
fn a_preference_for_an_engine_that_is_not_installed_falls_back_and_says_so() {
    let (dir, signals) = project(
        &[("DATABASE_URL", "postgres://localhost:5432/app")],
        Some(COMPOSE_PG),
    );
    let evidence = machine(true, &[("postgres", false)]);
    let choice = choice_of(&dir, &signals, &evidence, Some("native"));
    assert_eq!(choice.mechanism, Some("compose"));
    let said = choice.evidence.join(" | ");
    assert!(said.contains("postgres is not installed here"), "{said}");
    assert!(said.contains("so compose it is"), "{said}");
}

// And the mirror: a project that declares a compose file, on a
// machine with no docker and every engine. Not a preference and not
// a guess — the mechanism the project declares is one this machine
// has been shown not to have.
#[test]
fn no_docker_and_every_engine_takes_the_recipes_without_being_asked() {
    let (dir, signals) = project(
        &[
            ("DATABASE_URL", "postgres://localhost:5432/app"),
            ("REDIS_URL", "redis://localhost:6379"),
        ],
        Some(COMPOSE_PG),
    );
    let evidence = machine(false, &[("postgres", true), ("redis", true)]);
    let choice = choice_of(&dir, &signals, &evidence, None);
    assert_eq!(choice.mechanism, Some("native"));
    assert!(
        choice
            .evidence
            .iter()
            .any(|l| l.contains("docker is not on this machine")),
        "{:?}",
        choice.evidence
    );

    // …and the other way, for a machine that prefers compose and has
    // no docker.
    let choice = choice_of(&dir, &signals, &evidence, Some("compose"));
    assert_eq!(choice.mechanism, Some("native"));
    assert!(
        choice
            .evidence
            .iter()
            .any(|l| l.contains("docker is not on this machine")),
        "{:?}",
        choice.evidence
    );
}

// An engine the project wants and this machine lacks is still
// offered — the project plainly needs one — but it is not ticked, so
// the question is asked rather than the answer taken.
#[test]
fn an_engine_this_machine_lacks_is_offered_unticked_and_asks() {
    let (dir, signals) = project(&[("DATABASE_URL", "postgres://localhost:5432/app")], None);
    let evidence = machine(false, &[("postgres", false)]);
    let proposal = propose_with(dir.path(), &signals, None, &evidence, None)
        .into_iter()
        .find(|p| p.slot == Slot::Services)
        .expect("a services proposal");
    assert_eq!(proposal.mechanism, Some("native"));
    assert!(!proposal.decided, "it was taken without asking");
    assert!(proposal.preselected().is_empty());
    assert!(
        proposal.candidates[0].why.contains("not installed here"),
        "{:?}",
        proposal.candidates[0].why
    );
}

// Detection's own tests must not depend on the laptop they run on,
// so "nobody looked" is a distinct answer from "nothing is there".
#[test]
fn evidence_nobody_gathered_answers_maybe_rather_than_no() {
    let unknown = MachineEvidence::unknown();
    assert_eq!(unknown.can_run("postgres"), None);
    assert_eq!(unknown.has_docker(), None);
    let looked = machine(false, &[]);
    assert_eq!(looked.can_run("postgres"), Some(false));
    assert_eq!(looked.has_docker(), Some(false));

    // With nobody having looked, a project with both options keeps
    // the compose file its own repository declares.
    let (dir, signals) = project(
        &[("DATABASE_URL", "postgres://localhost:5432/app")],
        Some(COMPOSE_PG),
    );
    assert_eq!(
        choice_of(&dir, &signals, &unknown, None).mechanism,
        Some("compose")
    );
}

// The entry a native answer writes: the recipe is implied by the
// name when they match, and spelled out when they do not.
#[test]
fn a_native_entry_names_its_recipe_only_when_it_has_to() {
    let (dir, signals) = project(&[("DATABASE_URL", "postgres://localhost:5432/app")], None);
    let native = native_candidates(&signals, &MachineEvidence::unknown());
    let (array, entries) = native_entry(&native[0]);
    assert_eq!(array, "services");
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["kind", "name", "env"]);
    assert_eq!(entries[0].1.as_str(), Some("native"));
    assert_eq!(entries[1].1.as_str(), Some("postgres"));

    // A MySQL URL wants the `mariadb` recipe under a name of its
    // own, so the preset has to be written down.
    let (dir2, signals) = project(&[("DB_URL", "mysql://localhost:3306/app")], None);
    let native = native_candidates(&signals, &MachineEvidence::unknown());
    let (_, entries) = native_entry(&native[0]);
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["kind", "name", "env"]);
    assert_eq!(entries[1].1.as_str(), Some("mariadb"));
    let _ = (dir, dir2);
}

/// The brief quotes pando's no-preference line verbatim, twice.
///
/// It sits in a fenced block there, so nothing about it is code and
/// nothing compares it to the string pando emits. Reword
/// [`NO_PREFERENCE_EVIDENCE`] with the brief left alone and the brief
/// goes on quoting a sentence pando no longer says — silently, with
/// every other test green. This is the same rot the README's clap
/// check exists to stop, in the one other document that quotes the
/// binary word for word.
#[test]
fn the_brief_quotes_the_preference_line_pando_actually_says() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/brief.md");
    let brief = std::fs::read_to_string(&path).expect("the brief");
    // The brief is hard-wrapped, so the sentence straddles lines.
    let collapsed = brief.split_whitespace().collect::<Vec<_>>().join(" ");
    let quotes = collapsed.matches(NO_PREFERENCE_EVIDENCE).count();
    assert_eq!(
        quotes, 2,
        "agent/brief.md quotes pando's no-preference line {quotes} times, expected 2 — \
             if NO_PREFERENCE_EVIDENCE was reworded, reword the brief's two fenced copies \
             with it; the brief is the one document that quotes this string verbatim"
    );
}

#[test]
fn signals_looks_for_every_file_name_compose_does() {
    let mut ours = super::signals::COMPOSE_FILES.to_vec();
    let mut compose = crate::compose::COMPOSE_FILES.to_vec();
    ours.sort();
    compose.sort();
    assert_eq!(ours, compose);
}

// ---- schema hooks from the project's own scripts ----------------------

// A migration runner no rule knows by its files is still spelled as a
// package script. Offered after the tools pando does know, never one
// that writes migration files, and keyed on the migrations it applies.
#[test]
fn a_projects_own_schema_script_is_offered_after_the_known_tools() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": {
            "dev": "node server.js",
            "db:migrate": "prisma migrate dev",
            "db:init": "node scripts/init-db.js",
            "db:seed": "node scripts/seed.js"
        } }"#,
    )
    .unwrap();
    std::fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
    std::fs::create_dir_all(dir.path().join("migrations")).unwrap();
    let signals = signals(dir.path());
    let proposal = schema_hook_proposal(dir.path(), &signals).expect("a schema question");
    assert_eq!(
        values(&proposal),
        vec!["npm run db:init"],
        "a generating script is not a hook, and a seed is not the schema"
    );
    assert!(!proposal.decided, "it touches data, so it is always asked");
    let hook = proposal.candidates[0].hook.clone().unwrap();
    assert_eq!(hook.fingerprint, vec!["migrations/**"]);
    assert_eq!(hook.after, crate::config::HookPoint::Services);

    // A known tool comes first; the script is still offered beside it.
    std::fs::create_dir_all(dir.path().join("prisma")).unwrap();
    std::fs::write(dir.path().join("prisma/schema.prisma"), "").unwrap();
    let proposal = schema_hook_proposal(dir.path(), &signals).unwrap();
    assert_eq!(
        values(&proposal),
        vec!["npx prisma migrate deploy", "npm run db:init"]
    );
}

// Keyed on `*/migrations/*.py`, a migration added to an app one level
// down left the fingerprint as it was: migrate was skipped, and the
// worktree's own database kept the schema it had.
#[test]
fn djangos_migrate_is_keyed_on_every_apps_migrations_at_any_depth() {
    let dir = tempdir().unwrap();
    for path in [
        "manage.py",
        "core/migrations/0001_initial.py",
        "apps/billing/migrations/0001_initial.py",
    ] {
        let path = dir.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let proposal = schema_hook_proposal(dir.path(), &signals(dir.path())).unwrap();
    let hook = proposal.candidates[0].hook.clone().unwrap();
    assert_eq!(hook.cmd, "python manage.py migrate");
    let before = crate::hooks::fingerprint(dir.path(), &hook.fingerprint, &hook.cmd);
    assert!(before.is_some());
    std::fs::write(
        dir.path()
            .join("apps/billing/migrations/0002_add_invoice_total.py"),
        "",
    )
    .unwrap();
    let after = crate::hooks::fingerprint(dir.path(), &hook.fingerprint, &hook.cmd);
    assert_ne!(before, after, "{:?}", hook.fingerprint);
}

#[test]
fn a_schema_script_with_no_migrations_directory_is_keyed_on_the_manifest() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "scripts": { "migrate": "knex migrate:latest" } }"#,
    )
    .unwrap();
    let signals = signals(dir.path());
    let proposal = schema_hook_proposal(dir.path(), &signals).unwrap();
    assert_eq!(values(&proposal), vec!["npm run migrate"]);
    assert_eq!(
        proposal.candidates[0].hook.clone().unwrap().fingerprint,
        vec!["package.json"]
    );
}

// ---- the root env file, for workspace apps with none -------------------

/// A workspace whose apps load `.env` from their own directory, with one
/// `.env` at the root: the monorepo shape dotenv's default meets.
fn workspace_with_root_env() -> TempDir {
    let dir = seed_fixture(&[
        (".gitignore", ".env\nnode_modules/\n"),
        (".env", "PORT=3000\n"),
        (
            "package.json",
            r#"{ "workspaces": ["apps/*"], "scripts": { "dev": "node dev.js" } }"#,
        ),
    ]);
    for (app, script) in [
        ("api", "tsx watch src/server.ts"),
        ("web", "node src/server.js"),
    ] {
        let path = dir.path().join("apps").join(app);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("package.json"),
            format!(r#"{{ "scripts": {{ "dev": "{script}" }} }}"#),
        )
        .unwrap();
    }
    dir
}

#[test]
fn workspace_apps_with_no_env_file_are_given_the_root_one() {
    let dir = workspace_with_root_env();
    // An app that has an env file of its own has said what it reads.
    std::fs::write(dir.path().join("apps/web/.env.example"), "X=1\n").unwrap();
    let signals = signals(dir.path());
    assert_eq!(
        signals.workspace_env_links,
        vec![("apps/api/.env".to_string(), ".env".to_string())]
    );
    let proposal = provision_proposal(&signals).unwrap();
    assert!(
        proposal.decided,
        "the developer's own file, under an ignored path"
    );
    assert_eq!(values(&proposal), vec![".env,apps/api/.env"]);
    let answer = &proposal.candidates[0];
    assert_eq!(
        answer.provision_from,
        BTreeMap::from([("apps/api/.env".to_string(), ".env".to_string())])
    );
    assert!(!answer.needs_a_human);
    assert!(answer.why.contains("apps/api"), "{}", answer.why);
}

#[test]
fn the_root_env_is_given_only_where_the_repository_ignores_the_path() {
    let dir = workspace_with_root_env();
    // Only the root `.env` is ignored: `apps/api/.env` would show as
    // untracked, so it is never offered.
    std::fs::write(dir.path().join(".gitignore"), "/.env\n").unwrap();
    assert!(signals(dir.path()).workspace_env_links.is_empty());

    // And with no root `.env` at all there is nothing to give.
    let dir = workspace_with_root_env();
    std::fs::remove_file(dir.path().join(".env")).unwrap();
    assert!(signals(dir.path()).workspace_env_links.is_empty());
}

// ---- app directories below a root that is not an app ------------------

/// A repository with `files` written at their paths, directories and all,
/// and every one git does not ignore committed: git only looks inside a
/// directory it tracks for the ignored files in it.
fn tree(files: &[(&str, &str)]) -> TempDir {
    let dir = tempdir().unwrap();
    crate::testutil::git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
    for (rel, contents) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
    crate::testutil::git(dir.path(), &["add", "-A"]);
    crate::testutil::git(dir.path(), &["commit", "--quiet", "-m", "fixture"]);
    dir
}

/// A Python API and a Nuxt frontend in sibling directories, each with its
/// own lockfile, a deployment compose file beside them, and nothing at the
/// root but a gitignore and what a test run left there.
fn polyglot_siblings() -> TempDir {
    tree(&[
        (".gitignore", ".env\n.coverage\n"),
        (".coverage", "sqlite"),
        (
            "backend/pyproject.toml",
            "[project]\nname = \"api\"\nrequires-python = \">=3.13\"\n",
        ),
        ("backend/uv.lock", "version = 1\n"),
        (
            "backend/.env.example",
            "DATABASE_URL=postgres://app@localhost:5432/app\nAPI_KEY=\n",
        ),
        (
            "backend/.env",
            "DATABASE_URL=postgres://me@localhost:5432/app\n",
        ),
        (
            "frontend/package.json",
            r#"{ "scripts": { "dev": "nuxt dev", "build": "nuxt build" } }"#,
        ),
        ("frontend/package-lock.json", "{}"),
        ("frontend/nuxt.config.ts", "export default {}\n"),
        ("frontend/.env", "NUXT_API_URL=http://localhost:8000\n"),
        (
            "docker/compose.yml",
            "services:\n  db:\n    image: postgres:16\n",
        ),
    ])
}

/// A Node API beside a mobile app under `apps/`, each with its own
/// package manager, and no `package.json` at the root.
fn api_and_mobile_app() -> TempDir {
    tree(&[
        (".gitignore", ".env\n"),
        (
            "backend/package.json",
            r#"{ "scripts": { "dev": "tsx watch src/index.ts" } }"#,
        ),
        ("backend/pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ("backend/.nvmrc", "22\n"),
        (
            "apps/mobile/package.json",
            r#"{ "main": "expo-router/entry", "scripts": { "start": "expo start" } }"#,
        ),
        ("apps/mobile/package-lock.json", "{}"),
    ])
}

// A root with nothing to build is read one level down: each directory
// with a manifest or a lockfile is an app, and one with neither — the
// deployment's compose directory — is not.
#[test]
fn a_root_with_no_manifest_is_read_one_level_down() {
    let dir = polyglot_siblings();
    let found = signals(dir.path());
    let dirs: Vec<&str> = found.app_dirs.iter().map(|a| a.dir.as_str()).collect();
    assert_eq!(dirs, ["backend", "frontend"]);
    assert_eq!(found.app_dirs[0].lockfiles, ["uv.lock"]);
    assert_eq!(found.app_dirs[0].markers, ["pyproject.toml"]);
    assert_eq!(found.app_dirs[1].scripts["dev"], "nuxt dev");
    assert!(
        found.lockfiles.is_empty(),
        "the root's own list stays the root's"
    );

    let dir = api_and_mobile_app();
    let dirs: Vec<String> = signals(dir.path())
        .app_dirs
        .into_iter()
        .map(|a| a.dir)
        .collect();
    assert_eq!(dirs, ["apps/mobile", "backend"]);
}

// Hidden directories, dependency trees and anything gitignored are never
// apps, whatever manifest they hold.
#[test]
fn a_hidden_ignored_or_vendored_directory_is_not_an_app() {
    let dir = tree(&[
        (".gitignore", "vendor/\n"),
        (".tools/package.json", "{}"),
        ("node_modules/package.json", "{}"),
        ("vendor/package.json", "{}"),
        ("packages/sdk/package.json", "{}"),
        ("api/go.mod", "module api\n"),
    ]);
    let dirs: Vec<String> = signals(dir.path())
        .app_dirs
        .into_iter()
        .map(|a| a.dir)
        .collect();
    assert_eq!(dirs, ["api", "packages/sdk"]);
}

// A root that is an app is read as one, as it always was: its
// subdirectories are its own business.
#[test]
fn a_root_with_a_manifest_reads_no_app_directories() {
    for marker in ["package.json", "uv.lock", "turbo.json", "pyproject.toml"] {
        let dir = tree(&[(marker, "{}"), ("backend/package.json", "{}")]);
        assert!(signals(dir.path()).app_dirs.is_empty(), "{marker}");
    }
}

// Each app installs from its own lockfile, in its own directory, the way
// it would at a root of its own.
#[test]
fn each_app_directory_installs_from_its_own_lockfile() {
    let dir = polyglot_siblings();
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(
        values(&proposal),
        ["(cd backend && uv sync --frozen) && (cd frontend && npm ci)"]
    );
    assert!(proposal.decided);
    assert_eq!(
        proposal.candidates[0].why,
        "backend: uv.lock; frontend: package-lock.json"
    );

    let dir = api_and_mobile_app();
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(
        values(&proposal),
        ["(cd apps/mobile && npm ci) && (cd backend && pnpm install --frozen-lockfile)"]
    );
}

// An app that gitignores its lockfile gets the plain install there, as a
// root would; an app with two lockfiles makes the whole answer a question.
#[test]
fn an_app_directorys_install_follows_the_root_rules() {
    let dir = tree(&[
        (".gitignore", "web/package-lock.json\n"),
        ("web/package.json", "{}"),
        ("web/package-lock.json", "{}"),
        ("api/package.json", "{}"),
        ("api/pnpm-lock.yaml", ""),
        ("api/yarn.lock", ""),
    ]);
    let proposal = install_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(
        values(&proposal),
        ["(cd api && pnpm install --frozen-lockfile) && (cd web && npm install)"]
    );
    assert!(!proposal.decided, "api has two lockfiles to choose between");
}

// The frontend's dev script is proposed as a process of its own, run in
// its directory by its own package manager. The Python API has no script
// to run, and a rule never invents one, so it is left for the developer:
// the option says so, and a flag may not take it.
#[test]
fn an_app_directory_with_a_dev_script_is_a_process_of_its_own() {
    let dir = polyglot_siblings();
    let proposal = processes_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert!(!proposal.decided, "the shape of the processes is asked");
    let [only] = proposal.candidates.as_slice() else {
        panic!("one form, and no root script beside it: {proposal:?}");
    };
    assert_eq!(only.value, "frontend: npm run dev in frontend");
    assert_eq!(
        only.why,
        format!("a dev script in frontend; backend has uv.lock but no dev script: {UNSTARTED}")
    );
    assert!(only.needs_a_human, "a check of it would never run backend");
    assert_eq!(
        crate::actions::question_for(&proposal, &[]).preselect,
        None,
        "nothing for --yes to take"
    );
    let processes = only.processes.as_ref().unwrap();
    assert_eq!(processes.keys().collect::<Vec<_>>(), ["frontend"]);
    let frontend = &processes["frontend"];
    assert_eq!(frontend.cwd.as_deref(), Some("frontend"));
    assert_eq!(
        frontend.env["PORT"], "{port:frontend}",
        "the Nuxt convention"
    );
    assert_eq!(
        frontend.ports,
        Some(PortsSpec::List(vec!["frontend".to_string()]))
    );
}

// Every app directory started is the first choice, as it always was: an
// api and an Expo app, each with its own script.
#[test]
fn a_process_list_that_starts_every_app_directory_is_preselected() {
    let dir = api_and_mobile_app();
    let proposal = processes_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert!(!proposal.candidates[0].needs_a_human, "{proposal:?}");
    assert!(!proposal.candidates[0].why.contains(UNSTARTED));
    assert_eq!(
        crate::actions::question_for(&proposal, &[]).preselect,
        Some(0)
    );
}

// A library under `packages/` is read, and never asked to be started:
// nothing runs one on its own. A directory whose dev script fans out
// over its own parts is, since the per-app form leaves it out.
#[test]
fn only_an_unstarted_app_outside_packages_holds_the_process_list_open() {
    let dir = tree(&[
        ("web/package.json", r#"{ "scripts": { "dev": "vite" } }"#),
        ("web/package-lock.json", "{}"),
        ("packages/ui/package.json", r#"{ "name": "ui" }"#),
    ]);
    let found = signals(dir.path());
    assert_eq!(found.app_dirs.len(), 2, "{:?}", found.app_dirs);
    let proposal = processes_proposal(dir.path(), &found).unwrap();
    assert!(!proposal.candidates[0].needs_a_human, "{proposal:?}");

    let dir = tree(&[
        ("web/package.json", r#"{ "scripts": { "dev": "vite" } }"#),
        ("web/package-lock.json", "{}"),
        (
            "admin/package.json",
            r#"{ "scripts": { "dev": "concurrently \"vite\" \"tsc -w\"" } }"#,
        ),
    ]);
    let proposal = processes_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert!(proposal.candidates[0].needs_a_human, "{proposal:?}");
    assert!(
        proposal.candidates[0].why.ends_with(&format!(
            "; admin's dev script starts several things at once: {UNSTARTED}"
        )),
        "{proposal:?}"
    );
}

// Each app runs under its own package manager: pnpm's `pnpm dev` in one,
// npm's `npm run dev` in the other, whatever the other one uses.
#[test]
fn each_app_directory_runs_under_its_own_package_manager() {
    let dir = tree(&[
        (
            "api/package.json",
            r#"{ "scripts": { "dev": "tsx watch src/index.ts" } }"#,
        ),
        ("api/pnpm-lock.yaml", ""),
        ("web/package.json", r#"{ "scripts": { "dev": "vite" } }"#),
        ("web/package-lock.json", "{}"),
        ("web/vite.config.ts", "export default {}\n"),
    ]);
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    let cmds: Vec<(&str, &str)> = apps
        .iter()
        .map(|app| (app.dir.as_str(), app.cmd.as_str()))
        .collect();
    assert_eq!(
        cmds,
        [
            ("api", "pnpm dev"),
            ("web", "npm run dev -- --port {port:web}")
        ]
    );
    let proposal = processes_proposal(dir.path(), &signals(dir.path())).unwrap();
    assert_eq!(proposal.candidates[0].why, "a dev script in api and web");
}

// The files a worktree is missing are the apps' own `.env`s, and the
// coverage database the root holds is not one of them.
#[test]
fn an_app_directorys_local_env_file_is_a_provision_file() {
    let dir = polyglot_siblings();
    let found = signals(dir.path());
    assert_eq!(found.ignored_present, ["backend/.env", "frontend/.env"]);
    let proposal = provision_proposal(&found).unwrap();
    assert_eq!(values(&proposal), ["backend/.env,frontend/.env"]);
    assert!(proposal.decided);
    // And the backend's env example is read, so the database it names is
    // one a services proposal can see.
    assert!(
        found
            .env_example
            .iter()
            .any(|(key, _)| key == "DATABASE_URL"),
        "{:?}",
        found.env_example
    );
}

// A deployment's compose file one directory down is reported, and said
// to be only that: no services are proposed from it, and the evidence no
// longer claims the repository has no compose file.
#[test]
fn a_compose_file_below_the_root_is_reported_and_not_run_from() {
    let dir = polyglot_siblings();
    let found = signals(dir.path());
    assert_eq!(found.compose_files, ["docker/compose.yml"]);
    let proposal = services_proposal(dir.path(), &found, None, &MachineEvidence::unknown(), None);
    assert!(
        proposal
            .as_ref()
            .is_none_or(|p| p.candidates.iter().all(|c| c
                .service
                .as_ref()
                .and_then(ServiceHint::file)
                .is_none())),
        "{proposal:?}"
    );
    let choice = service_choice_for(
        dir.path(),
        &found,
        &MachineEvidence::unknown(),
        &Config::default(),
    );
    assert_eq!(
        choice.evidence[..2],
        [
            "this repository has no compose file at its root".to_string(),
            "services are proposed from a compose file at the root, not from \
             docker/compose.yml below it"
                .to_string(),
        ]
    );

    // A compose file at the root is the one, as it always was.
    let dir = tree(&[
        ("compose.yml", "services: {}\n"),
        ("docker/compose.yml", "services: {}\n"),
    ]);
    assert_eq!(signals(dir.path()).compose_files, ["compose.yml"]);
}

// An app directory's version file is proposed under its path, which is
// the form the runtime reads it back in.
#[test]
fn an_app_directorys_version_file_is_proposed_under_its_path() {
    let dir = api_and_mobile_app();
    let found = signals(dir.path());
    assert_eq!(found.version_files, ["backend/.nvmrc"]);
    let [node] = found.runtime_requirements.as_slice() else {
        panic!("{:?}", found.runtime_requirements);
    };
    assert_eq!(
        (node.source.as_str(), node.dir.as_deref()),
        ("backend/.nvmrc", Some("backend"))
    );
    let proposal = version_files_proposal(&found).unwrap();
    assert_eq!(values(&proposal), ["backend/.nvmrc"]);
}

// A fresh clone has no `.env` in an app directory either; its example is
// offered the way the root's is, under the app's path.
#[test]
fn an_app_directorys_env_example_seeds_its_missing_env_file() {
    let dir = tree(&[
        (".gitignore", ".env\n"),
        ("api/package.json", "{}"),
        ("api/.env.example", "PORT=4000\n"),
        ("web/package.json", "{}"),
        (
            "web/.env.example",
            "PORT=3000\nAPI_URL=http://localhost:4000\n",
        ),
    ]);
    let found = signals(dir.path());
    assert_eq!(
        found.provision_seeds,
        [
            ("api/.env".to_string(), "api/.env.example".to_string()),
            ("web/.env".to_string(), "web/.env.example".to_string()),
        ]
    );
    // One key, one value: the first app's spelling of it wins.
    let ports: Vec<&str> = found
        .env_example
        .iter()
        .filter(|(key, _)| key == "PORT")
        .map(|(_, value)| value.as_str())
        .collect();
    assert_eq!(ports, ["4000"]);
}

// ---- Expo ------------------------------------------------------------

/// An Expo app as its template makes one: `expo start` under `start`, no
/// `dev` script, and `expo` among its dependencies.
fn expo_app(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("package.json"),
        r#"{ "main": "expo-router/entry",
             "scripts": { "start": "expo start", "android": "expo run:android",
                          "web": "expo start --web", "lint": "expo lint" },
             "dependencies": { "expo": "~57.0.0", "expo-router": "~6.0.0" } }"#,
    )
    .unwrap();
    std::fs::write(dir.join("app.json"), r#"{ "expo": { "name": "mobile" } }"#).unwrap();
}

// The whole `[dev]` an Expo app at the root gets with nobody asked: its own
// `start` script, Metro's port variable and the longer wait a cold Metro
// needs. No `CI`: with no terminal Expo waits on no key already, and `CI`
// turns off Metro's reloads.
#[test]
fn an_expo_app_at_the_root_is_started_by_its_start_script() {
    let dir = tempdir().unwrap();
    expo_app(dir.path());
    std::fs::write(dir.path().join("package-lock.json"), "{}\n").unwrap();
    let signals = signals(dir.path());
    let rule = framework(dir.path(), &signals).map(|rule| rule.name);
    assert_eq!(rule, Some("Expo"));

    let dev = dev_in(dir.path(), &signals);
    assert!(dev.decided, "{dev:?}");
    assert_eq!(values(&dev), vec!["npm run start"]);
    let port = port_proposal(&signals, framework(dir.path(), &signals)).unwrap();
    assert!(port.decided);
    assert_eq!(values(&port), vec!["RCT_METRO_PORT"]);

    let mut config = Config::default();
    apply(Slot::DevCmd, &dev.candidates[0], &mut config);
    apply(Slot::PortEnv, &port.candidates[0], &mut config);
    let process = &config.processes[DEV];
    assert_eq!(process.cmd, "npm run start");
    assert_eq!(
        process.ports,
        Some(PortsSpec::Map(BTreeMap::from([(
            "RCT_METRO_PORT".to_string(),
            "metro".to_string()
        )]))),
        "Metro's is no page a browser opens, so its role is not `web`"
    );
    assert!(process.env.is_empty(), "{:?}", process.env);
    assert_eq!(process.ready.as_ref().and_then(|r| r.timeout_s), Some(90));
    assert_eq!(process.ready.as_ref().and_then(|r| r.role.clone()), None);

    // And written to the file as it was applied.
    let written = snippet(Slot::DevCmd, &[&dev.candidates[0]]);
    assert!(written.contains(r#"cmd = "npm run start""#), "{written}");
    assert!(!written.contains("CI"), "{written}");
    assert!(written.contains("ready = { timeout_s = 90 }"), "{written}");
}

#[test]
fn an_expo_app_with_no_script_is_started_by_the_expo_cli() {
    let dir = tempdir().unwrap();
    expo_app(dir.path());
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "dependencies": { "expo": "~57.0.0" } }"#,
    )
    .unwrap();
    let dev = dev_in(dir.path(), &signals(dir.path()));
    assert_eq!(values(&dev), vec!["npx expo start"]);
    let process = &dev.candidates[0].processes.as_ref().unwrap()[DEV];
    assert!(process.env.is_empty(), "{:?}", process.env);
    assert_eq!(process.ready.as_ref().and_then(|r| r.timeout_s), Some(90));
}

// `app.json` is also Heroku's: one with no `expo` in it, beside a manifest
// that does not depend on Expo, is some other app.
#[test]
fn an_app_json_that_is_not_expos_is_no_expo_app() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join("app.json"),
        r#"{ "name": "shop", "env": { "EXPORT_DIR": { "value": "out" } } }"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{ "exports": "./index.js", "scripts": { "dev": "node server.js" } }"#,
    )
    .unwrap();
    let signals = signals(dir.path());
    assert_eq!(
        framework(dir.path(), &signals).map(|rule| rule.name),
        Some("Node")
    );
    let dev = dev_in(dir.path(), &signals);
    assert_eq!(dev.candidates[0].processes, None, "Node proposes no env");
}

// A `[dev]` the developer wrote an `env` or a `ready` into keeps them: the
// rule's own would replace the whole table, not add a key to it.
#[test]
fn a_rules_env_and_wait_never_replace_the_developers_own() {
    let dir = tempdir().unwrap();
    expo_app(dir.path());
    let dev = dev_in(dir.path(), &signals(dir.path()));
    let mut config = Config::default();
    let written = config.processes.entry(DEV.to_string()).or_default();
    written.env = BTreeMap::from([("EXPO_OFFLINE".to_string(), "1".to_string())]);
    written.ready = Some(crate::config::ReadySpec {
        role: None,
        timeout_s: Some(300),
    });
    apply(Slot::DevCmd, &dev.candidates[0], &mut config);
    let process = &config.processes[DEV];
    assert_eq!(process.cmd, "npm run start");
    assert_eq!(
        process.env,
        BTreeMap::from([("EXPO_OFFLINE".to_string(), "1".to_string())])
    );
    assert_eq!(process.ready.as_ref().and_then(|r| r.timeout_s), Some(300));
}

/// An Expo app in `apps/mobile` beside a Fastify api in `apps/api`.
fn expo_workspace(dir: &Path) {
    std::fs::write(dir.join("package.json"), r#"{ "workspaces": ["apps/*"] }"#).unwrap();
    std::fs::write(dir.join("package-lock.json"), "{}\n").unwrap();
    expo_app(&dir.join("apps/mobile"));
    std::fs::create_dir_all(dir.join("apps/api")).unwrap();
    std::fs::write(
        dir.join("apps/api/package.json"),
        r#"{ "scripts": { "dev": "tsx watch src/index.ts" } }"#,
    )
    .unwrap();
}

#[test]
fn an_expo_app_in_a_workspace_is_a_process_of_its_own() {
    let dir = tempdir().unwrap();
    expo_workspace(dir.path());
    let proposal = proposed_processes(dir.path());
    let processes = proposal.candidates[0].processes.clone().unwrap();
    assert_eq!(
        processes.keys().cloned().collect::<Vec<_>>(),
        vec!["api", "mobile"]
    );

    let mobile = &processes["mobile"];
    assert_eq!(mobile.cwd.as_deref(), Some("apps/mobile"));
    assert_eq!(
        mobile.cmd, "npm run start",
        "its own script, by its own name"
    );
    assert_eq!(mobile.roles(), vec!["mobile"]);
    assert_eq!(mobile.env["RCT_METRO_PORT"], "{port:mobile}");
    assert!(!mobile.env.contains_key("PORT"), "Expo never reads PORT");
    assert!(
        !mobile.env.contains_key("CI"),
        "CI turns off Metro's reloads"
    );
    let ready = mobile.ready.clone().unwrap();
    assert_eq!(ready.role.as_deref(), Some("mobile"));
    assert_eq!(ready.timeout_s, Some(90));

    let api = &processes["api"];
    assert_eq!(api.cmd, "npm run dev");
    assert_eq!(api.env["PORT"], "{port:api}");
    assert_eq!(api.ready.clone().unwrap().timeout_s, None);
}

// Expo inlines `EXPO_PUBLIC_*` from Metro's environment into the bundle,
// so the api's worktree port has to reach Metro as a variable. The app's
// own env example says which variable, and only that app is told.
#[test]
fn an_apps_own_env_example_points_it_at_another_apps_port() {
    let dir = tempdir().unwrap();
    expo_workspace(dir.path());
    std::fs::write(
        dir.path().join("apps/mobile/.env.example"),
        "EXPO_PUBLIC_API_URL=http://127.0.0.1:3000/v1\n\
         EXPO_PUBLIC_SELF_URL=http://localhost:8081\n\
         EXPO_PUBLIC_SENTRY_DSN=https://key@sentry.example.com/1\n",
    )
    .unwrap();
    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    let mobile = &processes["mobile"];
    assert_eq!(
        mobile.env["EXPO_PUBLIC_API_URL"],
        "http://127.0.0.1:{port:api}/v1"
    );
    assert!(
        !mobile.env.contains_key("EXPO_PUBLIC_SELF_URL"),
        "an app is not told where it is itself: {:?}",
        mobile.env
    );
    assert!(!mobile.env.contains_key("EXPO_PUBLIC_SENTRY_DSN"));
    assert!(
        !processes["api"].env.contains_key("EXPO_PUBLIC_API_URL"),
        "one app's env example is that app's alone"
    );
}

// A `node server.js` backend listens where its own `.env` puts it, not on
// the Node default, so the mobile app's example URL to that port is the
// backend's, and Metro is told the worktree's. A URL to a port no app
// listens on is left as it is.
#[test]
fn an_apps_own_env_file_says_which_port_a_sibling_url_points_at() {
    let dir = tree(&[
        (".gitignore", ".env\n"),
        (
            "backend/package.json",
            r#"{ "scripts": { "dev": "node server.js" } }"#,
        ),
        ("backend/package-lock.json", "{}"),
        ("backend/.env", "PORT=8787\n"),
        (
            "apps/mobile/package.json",
            r#"{ "main": "expo-router/entry", "scripts": { "start": "expo start" },
                 "dependencies": { "expo": "~57.0.0" } }"#,
        ),
        ("apps/mobile/package-lock.json", "{}"),
        (
            "apps/mobile/.env.example",
            "EXPO_PUBLIC_API_BASE_URL=http://127.0.0.1:8787\n\
             EXPO_PUBLIC_OTHER_URL=http://127.0.0.1:9999\n",
        ),
    ]);
    assert!(
        dir.path().join("backend/.env").is_file(),
        "ignored, and there"
    );
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    let backend = apps.iter().find(|a| a.name == "backend").unwrap();
    assert_eq!(backend.default_port, Some(8787));

    let processes = proposed_processes(dir.path()).candidates[0]
        .processes
        .clone()
        .unwrap();
    let mobile = &processes["mobile"];
    assert_eq!(
        mobile.env["EXPO_PUBLIC_API_BASE_URL"],
        "http://127.0.0.1:{port:backend}"
    );
    assert!(
        !mobile.env.contains_key("EXPO_PUBLIC_OTHER_URL"),
        "{:?}",
        mobile.env
    );
    assert_eq!(processes["backend"].env["PORT"], "{port:backend}");
}

// Only a framework pando knows how to start is found under `start`: a
// `start: node server.js` is as often production as development.
#[test]
fn a_workspace_apps_plain_start_script_is_still_no_dev_script() {
    let dir = tempdir().unwrap();
    expo_workspace(dir.path());
    std::fs::create_dir_all(dir.path().join("apps/worker")).unwrap();
    std::fs::write(
        dir.path().join("apps/worker/package.json"),
        r#"{ "scripts": { "start": "node server.js" } }"#,
    )
    .unwrap();
    let apps = workspace_apps(dir.path(), &signals(dir.path()));
    assert_eq!(
        apps.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
        vec!["api", "mobile"]
    );
}
