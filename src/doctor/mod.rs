//! `doctor`: what pando found, from where, and what is wrong.
//!
//! Read-only. It is the command a developer runs on a machine nobody else
//! can see, so it is written to be read by a stranger: every fact names the
//! file or the path it came from, and every problem says what to do about
//! it. It exits 0 when nothing found will break a command and 1 when
//! something will, and it never fails the shell for a reason it has not
//! printed.
//!
//! **Where this sits.** Above `actions`, not beneath it:
//! `paths → … → actions → doctor → cli · tui`. doctor reports what the rest
//! of pando already knows — the slots the resolver would ask about, the
//! services a record holds, the shell the start path probes — so a module
//! below `actions` would have to keep a second copy of all of it.
//!
//! **What it must never do.** Write. Not a config, not a cache, not a state
//! file, not pando's home. `actions::refresh` is therefore out of bounds
//! here: it takes the lock, advances phases and saves. doctor loads state,
//! advances a *copy* in memory, and reports the difference.
//!
//! **Where things are.** `report` holds the types the report is made of
//! (`Finding`, `Section`, `Severity`, and one `*Report` per section) and
//! `render` turns them into text. Each section is built in its own file:
//! `config` (the project and every config layer), `runtime`, `tools`,
//! `worktrees` with `provision` (worktrees pando did not create that lack
//! a file `provision` names), `services` with `workers` (queue workers that share one
//! Redis across worktrees), `namespaces` (databases a namespaced worktree
//! left behind), `hooks`, and `adopt` (project folders left behind by a
//! moved repository, and `--adopt` itself). `stale` compares
//! detected values with what detection would write now, `validate`
//! checks the merged config, and `portless` names a process that runs a
//! framework's server with no port of its own. This file only gathers
//! them: [`run`] and [`run_on`].

use crate::actions;
use crate::actions::Machine;
use crate::paths::PandoPaths;

mod adopt;
mod config;
mod hooks;
mod namespaces;
mod portless;
mod provision;
mod render;
mod report;
mod runtime;
mod services;
mod stale;
#[cfg(test)]
mod tests;
mod tools;
mod validate;
mod workers;
mod worktrees;

use adopt::adoptable;
pub use adopt::{AdoptPlan, Adoptable, Adoption, adopt};
use config::{config_report, project_report};
use hooks::hooks_report;
pub use portless::{Portless, portless_processes};
pub use report::{
    ComposeEntryReport, ConfigReport, EngineBinary, Finding, HookReport, HookRunReport,
    IncludedService, IsolationReport, KeyReport, LanguageReport, LayerReport, NativeInstance,
    NativeServiceReport, ProcessReport, ProjectReport, Report, RuntimeReport, Section,
    ServicesReport, Severity, ToolReport, Unhealthy, WorktreeReport, WorktreeServiceReport,
};
use runtime::runtime_report;
use services::services_report;
pub use stale::answers_command;
use stale::stale_detection_findings;
use tools::tools_report;
pub use validate::nothing_to_run_fix;
use validate::validate_config;
use worktrees::worktrees_report;

/// Everything doctor has to say about this project, gathered without
/// writing anything anywhere.
///
/// The shell is the one a real spawn uses — `bash -lc`, in the main
/// checkout — built here the way `actions::resolve_silencing` builds it,
/// so what doctor reports about this machine is what the start path would
/// have found.
pub fn run(paths: &PandoPaths) -> Report {
    let shell = actions::runtime_shell(paths.root());
    let machine = Machine::here(&shell);
    run_on(paths, &machine)
}

/// [`run`] with the machine injected, so a test can report on a laptop it
/// does not have.
pub fn run_on(paths: &PandoPaths, machine: &Machine<'_>) -> Report {
    let mut findings: Vec<Finding> = Vec::new();

    // Its own load, not the one `main` did: `main` hands every command the
    // merged config and throws away the error, and the error is the fact
    // doctor exists to report. `Command::needs_config` is false for
    // `Doctor` for the same reason — a project layer pando cannot read is
    // exactly when this command is worth running.
    let (config, error, warnings) = match crate::config::load(paths) {
        Ok(loaded) => (loaded.config, None, loaded.warnings),
        Err(e) => {
            let fallback = crate::config::load_without_home(paths);
            (fallback.config, Some(format!("{e:#}")), fallback.warnings)
        }
    };

    let config_report = config_report(paths, error, warnings, &mut findings);
    validate_config(paths, &config, &mut findings);
    portless::portless_findings(paths, &config, &mut findings);
    stale_detection_findings(paths, &config, &config_report, &mut findings);
    let project = project_report(paths, &config, machine, &mut findings);
    let runtime = runtime_report(paths, &config, machine, &mut findings);
    let tools = tools_report(paths, &config, machine, &mut findings);
    // Once, and shared: every group's listening sockets are scanned to
    // advance a phase, and that is a real cost to pay twice.
    let view = actions::inspect(paths);
    let worktrees = worktrees_report(paths, &config, &view, &mut findings);
    provision::unprovisioned_findings(paths, &config, &view, &mut findings);
    let services = services_report(paths, &config, machine, &mut findings);
    namespaces::leftover_findings(paths, &config, &mut findings);
    let hooks = hooks_report(paths, &config, &view, &worktrees, &mut findings);
    let adoption = adoption_report(paths, &mut findings);
    // Lines composed below doctor name the machine-wide config by its
    // default path; the fix has to name the one this run reads.
    for finding in &mut findings {
        finding.message = config::with_real_user_config(paths, &finding.message);
        if let Some(fix) = &mut finding.fix {
            *fix = config::with_real_user_config(paths, fix);
        }
    }

    Report {
        project,
        config: config_report,
        runtime,
        tools,
        worktrees,
        services,
        hooks,
        adoption,
        findings,
    }
}

/// Doctor's runtime findings alone, worded as `doctor` words them: for
/// the setup screen, which never takes a runtime prelude on its own and
/// shows doctor's line for it instead.
pub fn runtime_findings(
    paths: &PandoPaths,
    config: &crate::config::Config,
    machine: &Machine<'_>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    runtime_report(paths, config, machine, &mut findings);
    for finding in &mut findings {
        finding.message = config::with_real_user_config(paths, &finding.message);
        if let Some(fix) = &mut finding.fix {
            *fix = config::with_real_user_config(paths, fix);
        }
    }
    findings
}

fn adoption_report(paths: &PandoPaths, findings: &mut Vec<Finding>) -> Vec<Adoptable> {
    let found = adoptable(paths);
    for entry in &found {
        let finding = match &entry.old_root {
            Some(root) => Finding::note(
                Section::Adoption,
                format!(
                    "{} holds a project folder for a repository called {:?} that is no longer \
                     where it was ({root}) — the id is a hash of the path, so this repository \
                     moving gave it a new one and left that folder behind",
                    entry.id, paths.project.display_name,
                ),
            )
            .with_fix(format!(
                "`pando doctor --adopt {}` moves it under this repository's id, with its \
                 config, its state and its worktrees",
                entry.id
            )),
            // Nothing says where its repository is, so nothing says it
            // moved: another checkout by the same name may be using it.
            None => Finding::note(
                Section::Adoption,
                format!(
                    "{} holds a project folder for a repository called {:?}, and pando cannot \
                     tell which repository it belonged to — another checkout by that name may \
                     still be using it",
                    entry.id, paths.project.display_name,
                ),
            )
            .with_fix(format!(
                "if it was this repository's before a move, `pando doctor --adopt {}` moves it \
                 under this repository's id, with its config, its state and its worktrees — \
                 and a checkout that still uses it loses them",
                entry.id
            )),
        };
        findings.push(finding);
    }
    found
}
