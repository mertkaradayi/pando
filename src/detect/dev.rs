//! Per-slot proposals for running the app: install, version files, the dev
//! command, make targets, ports, and the files each worktree needs.

use std::collections::BTreeMap;
use std::path::Path;

use crate::catalog::frameworks::{FrameworkRule, Guard, PortMechanism};
use crate::catalog::package_managers::{self, Ecosystem};
use crate::config::{PortsSpec, ProcessConfig, ReadySpec};

use super::apply::{DEV, dedup_by_value};
use super::frameworks::{framework, go_commands, only_builds, runs, several_binaries};
use super::proposal::{Candidate, Proposal, Slot};
use super::signals::{AppDir, Signals};

/// Lockfile to frozen install command.
///
/// A lockfile the project gitignores is not frozen against anything: a new
/// worktree is checked out without it, so the frozen install fails there.
/// Such a project, and a JavaScript one with no lockfile, gets its
/// manager's plain install instead where the lockfiles it would write are
/// gitignored, as [`unlocked_command`] asks — the file it writes is one
/// git ignores, or none, so it cannot change the repository.
pub(super) fn install_proposal(root: &Path, signals: &Signals) -> Option<Proposal> {
    if !signals.app_dirs.is_empty() {
        return app_install_proposal(root, &signals.app_dirs);
    }
    let candidates = installs_in(root, &signals.lockfiles);
    if candidates.is_empty() {
        return None;
    }
    let decided = candidates.len() == 1;
    Some(Proposal::of(Slot::Install, candidates, decided))
}

/// The installs the lockfiles of one directory propose, in lockfile order.
fn installs_in(dir: &Path, lockfiles: &[String]) -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = Vec::new();
    for lock in lockfiles {
        let Some((cmd, why)) = package_managers::install_for(lock) else {
            continue;
        };
        let candidate = match unlocked_install(dir, package_managers::for_lockfile(lock)) {
            Some(unlocked) => unlocked,
            None => Candidate {
                value: cmd.to_string(),
                why: why.to_string(),
                ..Candidate::default()
            },
        };
        candidates.push(candidate);
    }
    if lockfiles.is_empty() && dir.join("package.json").is_file() {
        let manifest = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
        candidates.extend(unlocked_install(
            dir,
            Some(package_managers::declared_javascript(&manifest)),
        ));
    }
    dedup_by_value(&mut candidates);
    candidates
}

/// The install of a root that is not an app: each app directory's own,
/// run in that directory, one after the other in path order —
/// `(cd backend && uv sync --frozen) && (cd frontend && npm ci)`.
///
/// Each app is installed the way it would be at a root of its own, with
/// the same rules for a gitignored lockfile. One candidate, made of every
/// app's first; decided only when no app had a choice to make, because
/// an app with two lockfiles is the same question it is at the root.
fn app_install_proposal(root: &Path, apps: &[AppDir]) -> Option<Proposal> {
    let mut steps: Vec<String> = Vec::new();
    let mut why: Vec<String> = Vec::new();
    let mut sure = true;
    for app in apps {
        let candidates = installs_in(&root.join(&app.dir), &app.lockfiles);
        let Some(first) = candidates.first() else {
            continue;
        };
        sure &= candidates.len() == 1;
        steps.push(format!("(cd {} && {})", shell_word(&app.dir), first.value));
        why.push(format!("{}: {}", app.dir, first.why));
    }
    if steps.is_empty() {
        return None;
    }
    let candidate = Candidate {
        value: steps.join(" && "),
        why: why.join("; "),
        ..Candidate::default()
    };
    Some(Proposal::of(Slot::Install, vec![candidate], sure))
}

/// A relative path as one shell word: bare when it is plain, as nearly
/// every directory name is, and quoted otherwise.
fn shell_word(path: &str) -> String {
    let plain = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-'));
    match plain && !path.is_empty() {
        true => path.to_string(),
        false => crate::process::shell_quote(path),
    }
}

/// The unlocked install of `manager`, when its lockfiles are gitignored.
fn unlocked_install(
    root: &Path,
    manager: Option<&'static package_managers::PackageManager>,
) -> Option<Candidate> {
    let manager = manager?;
    let cmd = unlocked_command(root, manager)?;
    let names = lockfiles_written(root, manager);
    let ignored = format!("{} {} gitignored", listed(&names), is_or_are(&names));
    let why = match Some(cmd) == manager.unsaved_install {
        true => {
            let tracked: Vec<&'static str> = manager
                .lockfiles
                .iter()
                .copied()
                .filter(|lockfile| !super::signals::is_gitignored(root, lockfile))
                .collect();
            format!(
                "{ignored} and {} {} not, so {cmd}, which writes no lockfile, cannot change \
                 the repository",
                listed(&tracked),
                is_or_are(&tracked)
            )
        }
        false => format!("{ignored}, so {cmd} cannot change the repository"),
    };
    Some(Candidate {
        value: cmd.to_string(),
        why,
        ..Candidate::default()
    })
}

/// The verb for a list of `names`: "is" for one, "are" for more.
fn is_or_are(names: &[&str]) -> &'static str {
    match names.len() {
        1 => "is",
        _ => "are",
    }
}

/// The install pando proposes for `manager` in place of the frozen one,
/// where `lockfiles_ignored` says the lockfiles are gitignored.
///
/// The plain install, unless a name the manager writes is not gitignored:
/// a new worktree is checked out without the ignored lockfile present
/// here, so the plain install there writes whichever name this version of
/// the manager writes, and a bun from 1.2 on writes a `bun.lock` that a
/// gitignore naming only `bun.lockb` leaves untracked in the worktree. The
/// install that writes no lockfile is proposed there.
pub fn unlocked_command(
    root: &Path,
    manager: &package_managers::PackageManager,
) -> Option<&'static str> {
    let plain = manager.unlocked_install?;
    if !lockfiles_ignored(root, manager) {
        return None;
    }
    let every = manager
        .lockfiles
        .iter()
        .all(|lockfile| super::signals::is_gitignored(root, lockfile));
    match every {
        true => Some(plain),
        false => Some(manager.unsaved_install.unwrap_or(plain)),
    }
}

/// Whether the lockfiles `manager`'s plain install would rewrite are ones
/// the project gitignores.
///
/// The ones present are what it rewrites, so those are asked of: a
/// gitignored `bun.lockb` is enough where only `bun.lockb` is in the
/// gitignore, and a tracked `bun.lock` beside it is not. With none
/// present every name is, not the first: bun writes `bun.lock` or
/// `bun.lockb` depending on its version.
fn lockfiles_ignored(root: &Path, manager: &package_managers::PackageManager) -> bool {
    let names = lockfiles_written(root, manager);
    !names.is_empty()
        && names
            .iter()
            .all(|lockfile| super::signals::is_gitignored(root, lockfile))
}

/// The lockfiles of `manager` present at the root, else every name it
/// writes.
fn lockfiles_written(root: &Path, manager: &package_managers::PackageManager) -> Vec<&'static str> {
    let present: Vec<&'static str> = manager
        .lockfiles
        .iter()
        .copied()
        .filter(|lockfile| root.join(lockfile).exists())
        .collect();
    if present.is_empty() {
        return manager.lockfiles.to_vec();
    }
    present
}

pub(super) fn version_files_proposal(signals: &Signals) -> Option<Proposal> {
    if signals.version_files.is_empty() {
        return None;
    }
    // Informational only — every file that pins a version is worth showing,
    // so there is nothing to choose between.
    Some(Proposal::of(
        Slot::VersionFiles,
        vec![Candidate {
            value: signals.version_files.join(","),
            why: signals.version_files.join(", "),
            ..Candidate::default()
        }],
        true,
    ))
}

/// Scripts that are never a dev server, whatever they are called.
const EXCLUDED_SCRIPTS: [&str; 6] = ["build", "lint", "test", "preview", "typecheck", "format"];

/// Bodies that mean "serve the production build", not "develop".
const PRODUCTION_SHAPES: [&str; 6] = [
    "next start",
    "node dist",
    "serve out",
    "serve dist",
    "node build",
    "start:prod",
];

/// Bodies that run several processes at once. The command works, but it
/// gives one log and one readiness rule for two servers, so it is not
/// something to accept silently — the developer is asked.
const MULTIPLEXERS: [&str; 6] = [
    "concurrently",
    "npm-run-all",
    "run-p",
    "pnpm -r",
    "turbo run",
    "npm:",
];

pub(super) fn is_production(body: &str) -> bool {
    PRODUCTION_SHAPES.iter().any(|shape| body.contains(shape))
}

/// Whether a script serves the production build rather than develops.
///
/// Never one named `dev` or `dev:*`: a TypeScript dev loop compiles and
/// then runs its output, `tsc-watch --onSuccess "node dist/index.js"`, and
/// the name is the project saying which script it develops with.
fn serves_production(name: &str, body: &str) -> bool {
    let dev_by_name = name == "dev" || name.starts_with("dev:");
    !dev_by_name && is_production(body)
}

pub(super) fn is_multiplexer(body: &str) -> bool {
    MULTIPLEXERS.iter().any(|shape| body.contains(shape))
}

/// How this project runs a `package.json` script.
pub(super) fn script_runner(signals: &Signals) -> &'static str {
    package_managers::run_prefix(lockfiles(signals), Ecosystem::JavaScript).unwrap_or("npm run ")
}

/// What goes between a `package.json` script and the arguments handed on
/// to it, for the runner [`script_runner`] names: npm's `-- ` where that
/// is npm.
pub(super) fn script_args(signals: &Signals) -> &'static str {
    package_managers::script_args(lockfiles(signals), Ecosystem::JavaScript).unwrap_or("-- ")
}

/// How this project runs a Python command.
pub(super) fn python_runner(signals: &Signals) -> &'static str {
    package_managers::run_prefix(lockfiles(signals), Ecosystem::Python).unwrap_or("")
}

/// The port a script fixes on its own command line: `next dev -p 3001`,
/// `vite --port=5174`. A CLI takes its flag over the environment, so no
/// port pando hands the process can move it.
///
/// Only a number counts. `-p $PORT` reads the port it is given, which is
/// exactly the shape that should be given one.
pub(super) fn fixed_port(script: &str) -> Option<u16> {
    let mut words = script.split_whitespace();
    while let Some(word) = words.next() {
        let value = match word {
            "--port" | "-p" => words.next(),
            _ => word
                .strip_prefix("--port=")
                .or_else(|| word.strip_prefix("-p=")),
        };
        if let Some(port) = value.and_then(|v| v.trim_matches(['"', '\'']).parse().ok()) {
            return Some(port);
        }
    }
    None
}

/// The port a script fixes for itself, run under `rule`: on its command
/// line, or by assigning the variable its app reads the port from,
/// `PORT=4000 tsx watch src/index.ts` or `cross-env PORT=3001 next dev`.
/// The shell that runs the script sets that variable over the one pando
/// exported, so a role for it would be a port nothing binds.
///
/// Not the assignment for a framework told its port by a flag pando
/// appends: the flag wins over the variable, so pando's port still does.
/// As with the flag, only a number counts: `PORT=${PORT:-4000}` reads
/// the port it is given.
pub(super) fn own_port(script: &str, rule: Option<&FrameworkRule>) -> Option<u16> {
    let flagged =
        rule.is_some_and(|rule| rule.port == PortMechanism::InCommand && rule.port_flag.is_some());
    let variable = match rule.map(|rule| rule.port) {
        Some(PortMechanism::Env(name)) => name,
        _ => "PORT",
    };
    let assigned = || {
        script.split_whitespace().find_map(|word| {
            let value = word.strip_prefix(variable)?.strip_prefix('=')?;
            value.trim_matches(['"', '\'']).parse().ok()
        })
    };
    fixed_port(script).or_else(|| match flagged {
        true => None,
        false => assigned(),
    })
}

pub(super) fn lockfiles(signals: &Signals) -> impl Iterator<Item = &str> {
    signals.lockfiles.iter().map(String::as_str)
}

/// The script an app develops with, as `(name, body)`: its `dev`, or,
/// with none, a `start` or `serve` that runs its own framework's dev
/// server. Expo's template names it `start`, and `expo start` is that
/// server as surely as `next dev` is Next's.
///
/// Only a framework pando knows how to start counts. A `start: node
/// server.js` is as often the production entry as the dev one, and
/// nothing in it says which, so an app with only that is still no app.
pub(super) fn dev_script<'a>(root: &Path, signals: &'a Signals) -> Option<(&'a str, &'a str)> {
    if let Some(body) = signals.scripts.get("dev") {
        return Some(("dev", body));
    }
    let rule = framework(root, signals).filter(|rule| rule.command.is_some())?;
    ["start", "serve"].into_iter().find_map(|name| {
        let (name, body) = signals.scripts.get_key_value(name)?;
        (runs(rule, body) && !only_builds(rule, body) && !is_production(body))
            .then_some((name.as_str(), body.as_str()))
    })
}

/// The keys besides `cmd` and `ports` that the one `[dev]` process a
/// `dev_cmd` answer fills is proposed with, when it runs `rule`'s server:
/// the rule's readiness wait. `None` for a rule that brings none, which is
/// nearly every rule, so an answer writes nothing it does not need.
fn rule_process(rule: &FrameworkRule, cmd: &str) -> Option<BTreeMap<String, ProcessConfig>> {
    rule.ready_timeout_s?;
    Some(BTreeMap::from([(
        DEV.to_string(),
        ProcessConfig {
            cmd: cmd.to_string(),
            ready: rule.ready_timeout_s.map(|timeout| ReadySpec {
                role: None,
                timeout_s: Some(timeout),
            }),
            ..ProcessConfig::default()
        },
    )]))
}

/// Scripts that could be a dev server, best first.
///
/// Exact `dev` wins; then `serve`, then `start` unless its body is the
/// production one; then anything else dev-shaped, alphabetically. Build,
/// lint, test and preview never qualify.
fn ranked_scripts(signals: &Signals) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> = Vec::new();
    let mut rest: Vec<(String, String)> = Vec::new();
    for (name, body) in &signals.scripts {
        if EXCLUDED_SCRIPTS.contains(&name.as_str()) || serves_production(name, body) {
            continue;
        }
        let dev_shaped = name == "dev"
            || name == "serve"
            || name == "start"
            || name.starts_with("dev:")
            || name.starts_with("start:");
        if !dev_shaped {
            continue;
        }
        match name.as_str() {
            "dev" | "serve" | "start" => named.push((name.clone(), body.clone())),
            _ => rest.push((name.clone(), body.clone())),
        }
    }
    let rank = |name: &str| match name {
        "dev" => 0,
        "serve" => 1,
        _ => 2,
    };
    named.sort_by_key(|(name, _)| rank(name));
    named.extend(rest);
    named
}

pub(super) fn dev_cmd_proposal(
    root: &Path,
    signals: &Signals,
    rule: Option<&'static FrameworkRule>,
) -> Option<Proposal> {
    let scripts: Vec<Candidate> = ranked_scripts(signals)
        .into_iter()
        .map(|(name, body)| script_candidate(signals, rule, &name, &body))
        .collect();
    let targets = target_candidates(signals);
    // Where the scripts only build a backend's assets, they start the asset
    // server and not the app: the project's own targets lead, then the
    // framework's command, and the scripts come last.
    let assets_only = rule.is_some_and(|rule| rule.scripts_build_assets);
    let own_command = targets.is_empty() && (scripts.is_empty() || assets_only);
    let mut candidates: Vec<Candidate> = match assets_only {
        true => targets.into_iter().chain(scripts).collect(),
        false => scripts.into_iter().chain(targets).collect(),
    };

    // Whether the candidates are a Go module's commands under `cmd/`.
    let mut go_commands_offered = false;
    // A framework's own command, for a project with nothing else to run it.
    if own_command
        && let Some(rule) = rule
        && let Some(command) = rule.command
    {
        let value = command.replace("{runner}", python_runner(signals));
        let ports = command
            .contains("{port:")
            .then(|| PortsSpec::List(vec![crate::config::WEB_ROLE.to_string()]));
        let why = format!("the {} rule", rule.name);
        // A crate with several binaries and none run by default: cargo
        // refuses the bare command, so each binary is offered by name. A
        // Go module whose root is not a main package has nothing for
        // `go run .` to run, so each command under `cmd/` is offered by
        // its path: the rule's `.`, the root, becomes `./cmd/<name>`.
        let named: Vec<(String, String)> = match rule.guard {
            Guard::BinaryCrate => several_binaries(root)
                .into_iter()
                .map(|name| {
                    let why = format!("{why}, for the {name} binary");
                    (format!("{value} --bin {name}"), why)
                })
                .collect(),
            Guard::GoMain => go_commands(root)
                .into_iter()
                .map(|name| {
                    (
                        format!("{value}/cmd/{name}"),
                        format!("{why}, for cmd/{name}"),
                    )
                })
                .collect(),
            _ => Vec::new(),
        };
        go_commands_offered = rule.guard == Guard::GoMain && !named.is_empty();
        let own: Vec<Candidate> = match named.is_empty() {
            true => vec![Candidate {
                processes: rule_process(rule, &value),
                value,
                why,
                ports,
                ..Candidate::default()
            }],
            false => named
                .into_iter()
                .map(|(value, why)| Candidate {
                    processes: rule_process(rule, &value),
                    value,
                    why,
                    ports: ports.clone(),
                    ..Candidate::default()
                })
                .collect(),
        };
        candidates.splice(0..0, own);
    }

    dedup_by_value(&mut candidates);
    if candidates.is_empty() {
        // A framework pando recognises but has no command shape for: it
        // knows there is a server here and not how to start it, which is a
        // question, and the only slot in this phase that has one with no
        // options to offer.
        return rule.map(|_| Proposal::of(Slot::DevCmd, Vec::new(), false));
        // With no rule at all — a library, or a repository with nothing to
        // serve — there is no proposal. Asking about a dev server that does
        // not exist is worse than saying nothing.
    }
    // One candidate is certain. So is a script named exactly `dev` that is
    // really one dev server: a body that runs several at once is the
    // multi-process shape, which is a question, not an assumption. A Go
    // module's commands never are: `cmd/` holds tools as often as
    // servers, so which one serves is the developer's to say even where
    // there is one.
    let sure_script = signals
        .scripts
        .get("dev")
        .filter(|body| !is_multiplexer(body))
        .map(|body| script_candidate(signals, rule, "dev", body).value);
    let decided = !go_commands_offered
        && (candidates.len() == 1 || sure_script.is_some_and(|dev| candidates[0].value == dev));
    Some(Proposal::of(Slot::DevCmd, candidates, decided))
}

/// Running the project's own `name` script.
///
/// A framework that takes its port on the command line gets its flag
/// appended, as a workspace app does: `npm run dev -- --port {port:web}`
/// runs what the project already runs, on the port pando chose, and owns
/// the web role. Told nothing, Vite moves itself to the next free port and
/// pando holds no role for it, so the worktree has no URL to open or share.
/// Only a script whose last command, the one npm and pnpm hand the flag
/// to, runs the framework's own server, alone, on no port of its own: a
/// custom server or a fan-out would hand the flag to something else, a
/// build refuses it, and where the script names a port it would be said
/// twice.
fn script_candidate(
    signals: &Signals,
    rule: Option<&'static FrameworkRule>,
    name: &str,
    body: &str,
) -> Candidate {
    let value = format!("{}{name}", script_runner(signals));
    let why = format!("package.json scripts.{name}");
    // A script that runs the framework's own server gets what the rule
    // proposes for one, whatever the script is called.
    let serves = rule.filter(|rule| runs(rule, body) && !only_builds(rule, body));
    let flagged = rule.filter(|rule| {
        rule.port == PortMechanism::InCommand
            && !rule.scripts_build_assets
            && flag_reaches_server(rule, body)
            && !is_multiplexer(body)
            && fixed_port(body).is_none()
    });
    let Some((rule, flag)) = flagged.and_then(|rule| Some((rule, rule.port_flag?))) else {
        return Candidate {
            processes: serves.and_then(|rule| rule_process(rule, &value)),
            value,
            why,
            ..Candidate::default()
        };
    };
    let role = crate::config::WEB_ROLE;
    let value = format!(
        "{value} {}{}",
        script_args(signals),
        flag.replace("{port}", &format!("{{port:{role}}}"))
    );
    Candidate {
        processes: rule_process(rule, &value),
        value,
        why: format!("{why}, told its port with the {} flag", rule.name),
        ports: Some(PortsSpec::List(vec![role.to_string()])),
        ..Candidate::default()
    }
}

/// Whether a port flag appended to a script reaches the rule's server:
/// the script's last command, the one npm and pnpm hand it to, runs the
/// framework's server and not only its build.
pub(super) fn flag_reaches_server(rule: &FrameworkRule, body: &str) -> bool {
    let last = last_command(body);
    runs(rule, last) && !only_builds(rule, last)
}

/// The last command of a script body, after its final `&`, `&&`, `;`, `|`
/// or `||`: the one the arguments after a script's name are handed to, so
/// `vite & node api.js --port 1234` tells `node` the port and not Vite.
/// A redirect's `&`, `2>&1` or `&> log`, runs nothing and ends nothing.
fn last_command(body: &str) -> &str {
    let mut start = 0;
    for (at, c) in body.char_indices() {
        let redirect = c == '&' && (body[..at].ends_with('>') || body[at + 1..].starts_with('>'));
        if matches!(c, '&' | ';' | '|') && !redirect {
            start = at + 1;
        }
    }
    &body[start..]
}

/// Makefile or justfile targets that look like they start something.
///
/// `make <target>` is proposed, not the recipe. A recipe is a *make*
/// program rather than a shell script, and lifting a line out of one drops
/// three separate things: the prerequisites, which run first and are how
/// `run: build` says the binary has to exist; every line after the one that
/// was taken, and the first line is as often a guard — a `command -v … ||
/// exit 1` check — as it is the command; and `$(VAR)`, which is make's
/// expansion and expands to nothing at all in a plain shell. A first run on
/// a repository pando had not generated met all three at once: the opening
/// guard of a `dev` target, proposed on its own, spawned, exited 0 in a
/// millisecond and left an empty log.
///
/// The narrow exception is a target that *is* its recipe: no
/// prerequisites, exactly one command line, and nothing in that line the
/// runner would expand — any `$` for make, because every `$` in a makefile
/// is make's own, `$$` included, which is how a makefile escapes one *for*
/// the shell; `{{` for just, which passes `$VAR` through untouched. A `-`
/// or `+` line prefix disqualifies it too, because those say how the runner
/// should treat the command and vanish with the runner. So does a file
/// whose top level changes every recipe's environment — `include .env` and
/// `export`, just's `set dotenv-load` — because pando does not load that
/// file into the process, and the server would start without the variables
/// the runner hands it. There the original
/// reasoning still holds and is kept: the recipe is the command, and
/// leaving `make` in the middle only adds a process between pando and the
/// server, with its own output and its own view of a signal.
fn target_candidates(signals: &Signals) -> Vec<Candidate> {
    let mut out = Vec::new();
    for name in ["dev", "run", "serve", "start"] {
        if let Some(target) = signals.targets.get(name)
            && !target.recipe.iter().any(|line| is_production(line))
        {
            out.push(Candidate {
                value: target.command(name),
                why: format!("the {name} target"),
                ..Candidate::default()
            });
        }
    }
    out
}

/// The service families a `*PORT*` key can belong to, matched on the key's
/// stem — everything before the trailing `PORT`.
///
/// A key in one of these names a *service's* port, never one of the
/// application's own. It is the services slot that gives it a meaning: an
/// entry in `[[services]] env` pointing the key at the compose service it
/// addresses, which is where `DATABASE_PORT` and `REDIS_PORT` end up. Here
/// they only have to be kept out of the app's roles, because a role pando
/// allocated for `DATABASE_PORT` would hand the app a port with no database
/// behind it.
///
/// Matched as the whole stem (`DB_PORT`), as a prefix of it
/// (`DATABASE_REPLICA_PORT`), or as anything the stem *ends* with
/// (`READ_DB_PORT`, `INFLUXDB_PORT`, `COUCHDB_PORT`). The suffix arm is
/// deliberately not word-bounded: that is what the `ends_with` list this
/// replaced did, and requiring a boundary quietly turned every
/// `<something>DB_PORT` into one of the app's roles — a port allocated with
/// no database behind it. It is coarse at the far edge, where `GMAIL_PORT`
/// and `JPG_PORT` read as a mail and a Postgres port, and it was coarse
/// there before. The prefix arm *is* bounded, so `DBX_PORT` and
/// `PGADMIN_PORT` stay the application's own.
///
/// `REPLICA` earns its own entry for the bare `REPLICA_PORT` a project with
/// a read replica writes; the `<FAMILY>_REPLICA_PORT` spelling is already
/// caught by its family.
const SERVICE_PORT_FAMILIES: [&str; 13] = [
    "DATABASE",
    "DB",
    "POSTGRESQL",
    "POSTGRES",
    "PG",
    "MYSQL",
    "MARIADB",
    "REDIS",
    "MONGODB",
    "MONGO",
    "REPLICA",
    "SMTP",
    "MAIL",
];

/// Whether a key is port-shaped at all: exactly `PORT`, or `<SOMETHING>_PORT`.
///
/// A substring test also matches `SUPPORT_EMAIL`, `REPORT_URL`,
/// `IMPORT_PATH` and `PASSPORT_SECRET`, which is not only nonsense to offer:
/// it turns a slot that resolved silently into a question, and a
/// non-interactive `start` that exited 0 into an exit 3.
fn port_stem(key: &str) -> Option<&str> {
    if key == "PORT" {
        return Some("");
    }
    key.strip_suffix("_PORT")
}

pub(super) fn is_service_port(key: &str) -> bool {
    let Some(stem) = port_stem(key) else {
        return false;
    };
    SERVICE_PORT_FAMILIES.iter().any(|family| {
        stem.strip_prefix(family)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('_'))
            || stem.ends_with(family)
    })
}

/// The role a port variable owns. `WEB_PORT` owns `web`; a bare `PORT` owns
/// `web` too, which is the role `share` and the browser key default to.
///
/// `None` for a stem that would not survive as a template argument, since
/// the role has to come back out of `{port:<role>}`.
fn port_role(key: &str) -> Option<String> {
    let stem = port_stem(key)?;
    if stem.is_empty() {
        return Some(crate::config::WEB_ROLE.to_string());
    }
    let role = stem.to_ascii_lowercase();
    role.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        .then_some(role)
}

/// Several port variables, each owning the role its own name says:
/// `WEB_PORT` owns `web`, `API_PORT` owns `api`, a bare `PORT` owns `web`.
///
/// One function for the option a rule builds out of the env example and
/// for the same list typed as an answer, so `WEB_PORT, API_PORT` means one
/// map whichever way it arrived. `Err` names the first variable whose name
/// says no role.
pub(super) fn roles_named_by<'a>(vars: &[&'a str]) -> Result<BTreeMap<String, String>, &'a str> {
    vars.iter()
        .map(|key| Ok(((*key).to_string(), port_role(key).ok_or(*key)?)))
        .collect()
}

/// "has" for one of `count`, "have" for several, as a sentence that
/// [`listed`] them goes on.
pub fn has_or_have(count: usize) -> &'static str {
    match count {
        1 => "has",
        _ => "have",
    }
}

/// `a`, `a and b`, `a, b and c` — a list in a sentence.
pub fn listed<S: AsRef<str>>(names: &[S]) -> String {
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.as_ref().to_string(),
        Some((last, rest)) => format!(
            "{} and {}",
            rest.iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>()
                .join(", "),
            last.as_ref()
        ),
    }
}

pub(super) fn port_proposal(
    signals: &Signals,
    rule: Option<&'static FrameworkRule>,
) -> Option<Proposal> {
    // Not where the script that would run fixes its own port: the flag
    // wins over every variable, the framework's and the env example's
    // alike, an inline `PORT=4000` over the one pando exports, and a role
    // for it is a port nothing binds. A backend whose scripts only build
    // its assets is not run by them, so their port is the asset server's.
    if !rule.is_some_and(|rule| rule.scripts_build_assets)
        && ranked_scripts(signals)
            .first()
            .is_some_and(|(_, body)| own_port(body, rule).is_some())
    {
        return None;
    }
    let framework_env = rule.and_then(|r| match r.port {
        PortMechanism::Env(name) => Some((name, r.name)),
        _ => None,
    });

    // The project's own port variables, in file order: port-shaped keys of
    // the env example that are not a service's. The root's own example
    // alone: below a root that is not an app, an app directory's `PORT`
    // is that app's, which its own process is given, and no port of the
    // project's as a whole.
    let mut declared: Vec<&str> = Vec::new();
    for (key, _) in signals.root_env_example() {
        if port_stem(key).is_some() && !is_service_port(key) && !declared.contains(&key) {
            declared.push(key);
        }
    }

    let mut candidates: Vec<Candidate> = Vec::new();
    // The framework's own mechanism first: it is a rule, not a guess.
    if let Some((name, framework)) = framework_env {
        candidates.push(Candidate {
            value: name.to_string(),
            why: format!("the {framework} convention"),
            ..Candidate::default()
        });
    }
    // Then `PORT`, then anything else port-shaped that is not a service's.
    let mut others: Vec<&str> = Vec::new();
    for key in &declared {
        if *key == "PORT" {
            candidates.insert(
                framework_env.is_some().into(),
                Candidate {
                    value: (*key).to_string(),
                    why: "PORT in the env example".to_string(),
                    ..Candidate::default()
                },
            );
        } else {
            others.push(key);
        }
    }
    candidates.extend(others.into_iter().map(|key| Candidate {
        value: key.to_string(),
        why: format!("{key} in the env example"),
        ..Candidate::default()
    }));
    dedup_by_value(&mut candidates);
    // A project that names two or more ports by role, and no bare `PORT` to
    // say which of them is the web server's, has declared several roles
    // rather than offered several guesses at one. That reading leads,
    // because it is the project's own statement about itself and the
    // framework rule underneath it is a convention pando brought with it —
    // but the single keys stay on offer below, since pando cannot know that
    // one process really owns them all.
    if let Some(candidate) = roles_from_env(&declared, framework_env.map(|(_, name)| name)) {
        candidates.insert(0, candidate);
    }
    if candidates.is_empty() {
        return None;
    }
    let decided = candidates.len() == 1;
    Some(Proposal::of(Slot::PortEnv, candidates, decided))
}

/// The one candidate that answers the port slot with a whole `ports` map:
/// every port variable the project declared, each owning a role named after
/// it.
fn roles_from_env(declared: &[&str], framework: Option<&str>) -> Option<Candidate> {
    if declared.len() < 2 || declared.contains(&"PORT") {
        return None;
    }
    let map = roles_named_by(declared).ok()?;
    let names = listed(declared);
    Some(Candidate {
        value: declared.join(", "),
        why: match framework {
            Some(framework) => {
                format!("{names} in the env example, over the {framework} convention")
            }
            None => format!("{names} in the env example"),
        },
        ports: Some(PortsSpec::Map(map)),
        ..Candidate::default()
    })
}

/// Which local files each worktree needs a copy of.
///
/// Two shapes. A file that is gitignored and present is a file pando can
/// link, and there is nothing to decide about it. A file the repository
/// only ships an *example* of is a different offer: nothing is copied from
/// a tracked file into a worktree unless a developer says so, because the
/// example's defaults are not their local settings and pando has no way to
/// know whether that is close enough. So a proposal with a seed in it is
/// never decided, and the question below it is the plain answer: only what
/// is already here.
pub(super) fn provision_proposal(signals: &Signals) -> Option<Proposal> {
    let links = &signals.workspace_env_links;
    // The root's own files, then the same `.env` given to each app that
    // reads one from its own directory. Linked like the root file, not
    // seeded: it is the developer's local file, not a tracked example.
    let present: Vec<String> = signals
        .ignored_present
        .iter()
        .cloned()
        .chain(links.iter().map(|(destination, _)| destination.clone()))
        .collect();
    let present = &present;
    let seeds = &signals.provision_seeds;
    if present.is_empty() && seeds.is_empty() {
        return None;
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    // The files that are already here lead, and are what `--yes` takes.
    if !present.is_empty() {
        let mut why = "gitignored and present in the main checkout".to_string();
        if !links.is_empty() {
            why.push_str(&format!(
                "; the root .env also given to {}, which have no env file of their own",
                listed(
                    &links
                        .iter()
                        .map(|(destination, _)| {
                            destination.trim_end_matches("/.env").to_string()
                        })
                        .collect::<Vec<_>>()
                )
            ));
        }
        candidates.push(Candidate {
            value: present.join(","),
            why,
            provision_from: links.iter().cloned().collect(),
            ..Candidate::default()
        });
    }
    if !seeds.is_empty() {
        let mut paths = present.clone();
        for (destination, _) in seeds {
            if !paths.contains(destination) {
                paths.push(destination.clone());
            }
        }
        // Naming the source in the option text is the whole safeguard on
        // this path: a developer reading the prompt can open the file and
        // judge what is in it, which is the judgement pando cannot make.
        let seeded = listed(
            &seeds
                .iter()
                .map(|(destination, source)| format!("{destination} copied from {source}"))
                .collect::<Vec<_>>(),
        );
        candidates.push(Candidate {
            value: paths.join(","),
            why: match present.is_empty() {
                true => format!("{seeded} — this clone has none of its own"),
                false => format!("the same, plus {seeded}"),
            },
            provision_from: seeds.iter().chain(links.iter()).cloned().collect(),
            needs_a_human: true,
            ..Candidate::default()
        });
    }
    let decided = seeds.is_empty();
    Some(Proposal::of(Slot::Provision, candidates, decided))
}

/// Which dependency trees a new worktree clones from the main checkout:
/// every one the main checkout has, unless `install` deletes it before it
/// installs, as `npm ci` does `node_modules` — cloning that would be work
/// thrown away on every `new`.
///
/// Decided: nothing a branch has is lost by it. The install still runs on
/// the clone and rewrites what the branch changed, a filesystem that
/// cannot clone leaves the tree to the install, and `pando check` never
/// clones, so a check still proves the install from nothing.
pub fn clone_proposal(signals: &Signals, install: Option<&str>) -> Option<Proposal> {
    let cleared = install.and_then(crate::catalog::package_managers::cleared_by);
    let dirs: Vec<&str> = signals
        .dependency_dirs
        .iter()
        .map(String::as_str)
        .filter(|dir| cleared.is_none_or(|name| dir.rsplit('/').next() != Some(name)))
        .collect();
    if dirs.is_empty() {
        return None;
    }
    let candidate = Candidate {
        value: dirs.join(","),
        why: "gitignored dependency trees in the main checkout, cloned copy-on-write before \
              the install or build so a worktree shares their blocks"
            .to_string(),
        ..Candidate::default()
    };
    Some(Proposal::of(Slot::Clone, vec![candidate], true))
}
