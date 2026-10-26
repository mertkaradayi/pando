//! Validation of the merged config: provision, install, and templates.

use std::collections::BTreeMap;
use std::path::Path;

use crate::catalog::package_managers;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::{actions, detect, ports, template};

use super::report::{Finding, Section};

pub(super) fn validate_config(paths: &PandoPaths, config: &Config, findings: &mut Vec<Finding>) {
    check_provision(paths, config, findings);
    check_install(paths.root(), config, findings);
    check_templates(config, findings);
    check_disabling_env(paths, config, findings);
    check_something_to_run(paths, config, findings);
}

/// What to do about a project with no process: the file to write it in,
/// by its absolute path, and the two lines to write. Shared with the CLI,
/// which says the same thing when `start` finds nothing to start.
pub fn nothing_to_run_fix(config_file: &Path) -> String {
    format!(
        "add a dev command to {}:\n[dev]\ncmd = \"…\"   # the command that runs it, e.g. \"cargo run\"",
        config_file.display()
    )
}

/// A project with no process configured and none detected: `start` has
/// nothing to start, and nothing else in the report would say so — a
/// library is healthy, and "nothing to report" read as "ready to run".
fn check_something_to_run(paths: &PandoPaths, config: &Config, findings: &mut Vec<Finding>) {
    if !config.processes.is_empty() {
        return;
    }
    let signals = detect::signals(paths.root());
    let detected = detect::propose(paths.root(), &signals)
        .into_iter()
        .any(|p| {
            matches!(p.slot, detect::Slot::DevCmd | detect::Slot::Processes)
                && !p.candidates.is_empty()
        });
    if detected {
        // `start` asks, and the question is how it gets something to run.
        return;
    }
    findings.push(
        Finding::note(
            Section::Config,
            "nothing to run: no dev command detected or configured — `pando start` has \
             nothing to start",
        )
        .with_fix(nothing_to_run_fix(&paths.config_file())),
    );
}

/// Every path a worktree is given a copy of has to be gitignored in the
/// main checkout, or `new` refuses it. Checked with git itself, the way
/// `new` checks it.
fn check_provision(paths: &PandoPaths, config: &Config, findings: &mut Vec<Finding>) {
    for entry in config.project.provision_paths() {
        if detect::is_ignored_in(&paths.project, paths.root(), entry) {
            continue;
        }
        findings.push(Finding::problem(
            Section::Config,
            format!(
                "`project.provision` names {entry:?}, which this repository does not ignore — \
                 `pando new` refuses to write a file that would show up in `git status`"
            ),
            format!("add {entry} to the repository's .gitignore, or drop it from provision"),
        ));
    }
    // A statement about a file nobody asked for. Dangling, not unsafe:
    // `provision_from` only ever answers "where does this provisioned file
    // come from", and a source for a destination nothing provisions is
    // never read.
    for destination in config.project.provision_from.keys() {
        if config
            .project
            .provision_paths()
            .iter()
            .any(|p| p == destination)
        {
            continue;
        }
        findings.push(Finding::note(
            Section::Config,
            format!(
                "`project.provision_from` has a source for {destination:?}, which is not in \
                 `project.provision` — nothing reads it"
            ),
        ));
    }
}

fn check_install(root: &Path, config: &Config, findings: &mut Vec<Finding>) {
    let Some(install) = config.project.install.as_deref() else {
        return;
    };
    // Every step of a chained command, because `cd apps/web && pnpm
    // install` is one of the shapes a developer writes.
    for step in install.split("&&").flat_map(|s| s.split(';')) {
        let words: Vec<&str> = step.split_whitespace().collect();
        // Leading `KEY=value` is how a frozen bundle is spelled, so the
        // program is the first word that is not one.
        let program = words.iter().find(|w| !w.contains('='));
        let Some(program) = program else { continue };
        let index = words.iter().position(|w| w == program).unwrap_or(0);
        let sub = words.get(index + 1).copied().unwrap_or("");
        let Some(manager) = package_managers::for_program(program) else {
            continue;
        };
        let Some(shape) = manager
            .install_shape
            .filter(|shape| shape.verbs.contains(&sub))
        else {
            continue;
        };
        // The install pando proposes itself where the lockfiles are
        // gitignored: the file it writes is one git ignores, or none, so
        // it cannot change the repository. It is also the one to use
        // there, where a frozen install has nothing to be frozen against.
        let unlocked = crate::detect::unlocked_command(root, manager);
        if unlocked == Some(step.trim()) {
            continue;
        }
        let fix = unlocked.unwrap_or(shape.suggest);
        if shape
            .frozen_markers
            .iter()
            .any(|marker| step.contains(marker))
        {
            continue;
        }
        findings.push(Finding::problem(
            Section::Config,
            format!(
                "`project.install` runs {:?}, which can rewrite the project's lockfile — pando \
                 never runs a non-frozen install",
                step.trim()
            ),
            format!("use `{fix}`"),
        ));
    }
}

/// A process whose environment turns off what its framework's dev server
/// is for: `CI` on Expo's Metro, which then neither reloads nor watches a
/// file. pando's own Expo rule proposed it until 0.6.0, so a config
/// written then still says it.
fn check_disabling_env(paths: &PandoPaths, config: &Config, findings: &mut Vec<Finding>) {
    for (name, process) in &config.processes {
        let vars = process.port_vars();
        let vars: Vec<&str> = vars.keys().map(String::as_str).collect();
        let Some((device, _)) = crate::catalog::frameworks::device(&vars, &process.cmd) else {
            continue;
        };
        for (var, what) in device.disabled_by {
            if !process.env.contains_key(*var) {
                continue;
            }
            findings.push(
                Finding::note(
                    Section::Config,
                    format!("process {name:?} sets {var}, which turns off {what}"),
                )
                .with_fix(format!(
                    "remove {var} from the process's `env` in {} — with no terminal it waits \
                     on no keypress without it",
                    paths.config_file().display()
                )),
            );
        }
    }
}

/// Every `{…}` a process's command and environment carry, resolved against
/// the roles the config itself declares.
///
/// A `{port:<role>}` naming a role nothing owns is a start that fails after
/// the worktree exists and the install has run. Nothing before this said so
/// at rest.
fn check_templates(config: &Config, findings: &mut Vec<Finding>) {
    let roles = declared_roles(config);
    for (name, process) in &config.processes {
        // What a bare `{port}` means for this process, exactly as `start`
        // resolves it: the role it owns.
        let own = process.roles();
        let default_role = own.first().map(String::as_str);
        let mut texts: Vec<(String, String)> = vec![("cmd".to_string(), process.cmd.clone())];
        if let Some(cwd) = &process.cwd {
            texts.push(("cwd".to_string(), cwd.clone()));
        }
        for (key, value) in process.env.iter().chain(process.port_env().iter()) {
            texts.push((format!("env.{key}"), value.clone()));
        }
        for (what, text) in texts {
            check_template(
                &roles,
                default_role,
                &format!("process {name:?}"),
                &what,
                &text,
                &format!("name a role something owns, or give {name:?} that role in its `ports`"),
                findings,
            );
        }
    }
    // A hook owns no role, so a bare `{port}` in one is an error naming
    // the roles it could have used — which is what `run_hook` passes too.
    for hook in &config.hooks {
        let mut texts: Vec<(String, String)> = vec![("cmd".to_string(), hook.cmd.clone())];
        if let Some(cwd) = &hook.cwd {
            texts.push(("cwd".to_string(), cwd.clone()));
        }
        if let Some(fallback) = &hook.fallback {
            texts.push(("fallback".to_string(), fallback.clone()));
        }
        for (what, text) in texts {
            check_template(
                &roles,
                None,
                &format!("hook {:?}", hook.name),
                &what,
                &text,
                "name a role something owns — a hook owns none of its own, so `{port:<role>}` \
                 has to say which",
                findings,
            );
        }
    }
}

/// One template, rendered against a worktree that could exist.
///
/// The paths are stand-ins: what is being checked is whether every
/// placeholder *resolves*, and the only ones that can fail are the ones
/// naming a role.
#[allow(clippy::too_many_arguments)]
fn check_template(
    roles: &BTreeMap<String, u16>,
    default_role: Option<&str>,
    owner: &str,
    what: &str,
    text: &str,
    fix: &str,
    findings: &mut Vec<Finding>,
) {
    let ctx = template::Context {
        name: "a-worktree",
        branch: Some("a-branch"),
        worktree: Path::new("/worktree"),
        root: Path::new("/root"),
        project: "project",
        ports: roles,
        default_role,
        log: Some(Path::new("/log")),
    };
    if let Err(e) = template::render(text, &ctx) {
        findings.push(Finding::problem(
            Section::Config,
            format!("{owner}: {what} cannot be resolved — {e:#}"),
            fix,
        ));
    }
}

/// Every role this config declares, mapped to a number that is only there
/// so a template can render.
///
/// Both kinds: a process's own roles and a service's name, compose or
/// native, which is a role too — that is what lets a process be told the
/// port of the database beside it.
fn declared_roles(config: &Config) -> BTreeMap<String, u16> {
    let mut out = BTreeMap::new();
    let mut next = ports::PORT_MIN;
    let mut give = |role: String, out: &mut BTreeMap<String, u16>| {
        out.entry(role).or_insert_with(|| {
            next = next.saturating_add(1);
            next
        });
    };
    for process in config.processes.values() {
        for role in process.roles() {
            give(role, &mut out);
        }
    }
    // The start's own list, so the two cannot disagree about what a
    // service's role is.
    for role in actions::service_roles(config) {
        give(role, &mut out);
    }
    out
}
