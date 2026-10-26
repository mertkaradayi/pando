//! `init --agent`: the setup job for the developer's own coding agent, for
//! this project and this pando, and `--reference`, the brief and the
//! contract it points at, or the block the agent saves, on the
//! developer's yes, to remember how to run the project.
//!
//! The setup prompt names this command and nothing else, so what an agent
//! learns about the job it learns here: pando's view of the project, the
//! questions still open, what each command it will run writes, and the
//! brief's first-run section — the rules and the steps, which live in the
//! brief and are printed from it rather than written twice. It ends with
//! that block, [`crate::setup::memory_block`].
//!
//! Kept under about 10 KB, which the tests hold it to: agents' shells cut
//! long output — Claude Code's at about 30,000 characters — and the brief
//! alone is past that, which is why the whole of it is behind
//! `--reference`.
//!
//! Read-only, like `signals`: it writes nothing, not even pando's home,
//! asks nothing, and exits 0. A config pando cannot read is a fact in the
//! job, not a refusal — the agent is the one who can tell the developer.

use std::fmt::Write as _;
use std::io::Write;

use anyhow::Result;

use crate::actions;
use crate::config::Config;
use crate::detect::{self, Slot};
use crate::paths::PandoPaths;
use crate::project::ProjectRef;

use super::answers::slot_name;

/// The procedure, as this binary was built with it. `--reference brief`
/// prints it whole, and the job prints its first-run section.
pub(super) const BRIEF: &str = include_str!("../../agent/brief.md");

/// The contract for every JSON shape, as this binary was built with it.
pub(super) const JSON_CONTRACT: &str = include_str!("../../agent/json.md");

/// The heading the job's rules and steps are found under in [`BRIEF`].
const FIRST_RUN_HEADING: &str = "## First run";

/// What `--reference` prints.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    /// The whole procedure: agent/brief.md.
    Brief,
    /// Every JSON shape pando publishes: agent/json.md.
    Json,
    /// Not a document: how to run this project with pando, made for it, for
    /// an agent to save in its own memory once the developer says yes.
    Memory,
}

/// `init --agent`, and `init --agent --reference`.
pub fn agent<W: Write>(
    paths: &PandoPaths,
    reference: Option<Reference>,
    out: &mut W,
) -> Result<()> {
    let text = match reference {
        Some(Reference::Brief) => BRIEF.to_string(),
        Some(Reference::Json) => JSON_CONTRACT.to_string(),
        Some(Reference::Memory) => {
            let (config, _) = load(paths);
            let proposals = proposals(paths);
            crate::setup::memory_block(paths, crate::setup::device_note(&config, &proposals))
        }
        None => job(paths),
    };
    out.write_all(text.as_bytes())?;
    Ok(())
}

/// The job, for this project as pando sees it now.
pub(super) fn job(paths: &PandoPaths) -> String {
    let shell = actions::runtime_shell(paths.root());
    job_on(paths, &actions::Machine::here(&shell))
}

/// [`job`] on a machine a test describes.
pub(super) fn job_on(paths: &PandoPaths, machine: &actions::Machine<'_>) -> String {
    let project = &paths.project.display_name;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Set up pando for {project} (pando {})\n",
        env!("CARGO_PKG_VERSION")
    );
    let what = match paths.project.is_multi_repo() {
        true => {
            "pando runs each git branch of these repositories as one worktree set, with its \
                 own\nports, logs and services."
        }
        false => {
            "pando runs each git branch of this repository in its own worktree, with its \
                  own\nports, logs and services."
        }
    };
    let _ = writeln!(
        out,
        "{what} You are done when `pando check` passes and you have told\n\
         the developer so, as the steps below say.\n"
    );
    if let Some(folder) = crate::project::folder_of(&paths.project) {
        out.push_str(&beside_others(&paths.project, &folder));
    }

    let (config, error) = load(paths);
    if let Some(error) = &error {
        let _ = writeln!(
            out,
            "**pando cannot read its settings for this project:** {error}\n\n\
             Tell the developer this first. `pando doctor` names the file and the fix, and\n\
             `pando init --answers -` refuses until it is fixed. Do not edit the file yourself.\n"
        );
    }
    let setup = crate::setup::read(paths, &config);
    if let Some(lead) = last_check(&setup) {
        out.push_str(&lead);
    }
    // Read once: the proposals come from these, and the services section
    // reads the app directories they list.
    let signals = detect::signals_of(&paths.project);
    let proposals = proposals_from(paths, &signals);
    let device_note = crate::setup::device_note(&config, &proposals);
    // The lead has said there is nothing left to set up; a line telling
    // the agent to save choices and check again contradicted it.
    let passing = setup.state == crate::setup::SetupState::Ready && setup.last_check.is_some();
    out.push_str(&project_now(
        paths,
        &config,
        &signals,
        &proposals,
        device_note,
        passing,
        machine,
    ));
    out.push_str(&writes(paths));
    out.push_str(first_run_section().trim_end());
    out.push_str(
        "\n\nFor the full procedure: `pando init --agent --reference brief`.\n\
         For every JSON shape: `pando init --agent --reference json`.\n",
    );
    out.push_str(&remember(paths, device_note));
    out
}

/// Config as the job reads it, with the error that kept a layer out.
///
/// Its own load, as doctor does: `main` keeps the error to itself and
/// hands a command whatever layers are left, and the error is the fact
/// worth putting first.
fn load(paths: &PandoPaths) -> (Config, Option<String>) {
    match crate::config::load(paths) {
        Ok(loaded) => (loaded.config, None),
        Err(e) => (
            crate::config::load_without_home(paths).config,
            Some(format!("{e:#}")),
        ),
    }
}

/// What the rules propose for the project, from the same places `signals`
/// reads, with no process spawned.
fn proposals(paths: &PandoPaths) -> Vec<detect::Proposal> {
    proposals_from(paths, &detect::signals_of(&paths.project))
}

fn proposals_from(paths: &PandoPaths, signals: &detect::Signals) -> Vec<detect::Proposal> {
    detect::propose(paths.root(), signals)
}

/// The job's last section: the block an agent saves once the developer
/// tells it to, fenced, so what it saves is exactly what `--reference
/// memory` prints.
fn remember(paths: &PandoPaths, device_note: Option<&str>) -> String {
    format!(
        "\n## Remember how to run {project}\n\n\
         Once the check passes, say in your report that you can save this in your own \
         memory, asking nothing, and save it only when the developer tells you to, as \
         \"Remember how to run it\" says:\n\n\
         ```markdown\n{block}```\n",
        project = paths.project.display_name,
        block = crate::setup::memory_block(paths, device_note),
    )
}

/// What the last check says, when it says something the agent has to act
/// on before anything else: a failure on today's settings first, then a
/// check that is running, never finished, or tested other settings.
///
/// From `setup::read`, the same state the TUI shows, so the job and the
/// screen never disagree about where the setup stands. Nothing for a
/// project never checked: the job below is the whole story then.
pub(super) fn last_check(setup: &crate::setup::Setup) -> Option<String> {
    use crate::setup::{CheckOutcome, FailureKind, SetupState};
    let record = setup.last_check.as_ref();
    match (setup.state, record.map(|r| &r.outcome)) {
        (SetupState::Failing, Some(CheckOutcome::Failed { kind, reason })) => {
            let whose = match kind {
                FailureKind::Settings => {
                    "pando's settings for this project are wrong, which is yours to fix: correct \
                     them with `pando init --answers - --replace`, then run `pando check` again."
                }
                FailureKind::Machine => {
                    "This machine is not ready (`kind: \"machine\"`), which is the developer's: \
                     tell them the command pando printed, and change no setting."
                }
                // The trap is loosening the install until it passes: on
                // the wrong commit, with a lockfile it wrote itself.
                FailureKind::Base => {
                    "The commit the check tested lacks a file the setup needs, which the main \
                     checkout's branch has (`kind: \"base\"`): the settings may well be right. \
                     Change no setting to get past it, and never drop a frozen install's flag. \
                     Which branch work starts from is the developer's: tell them both refs from \
                     the reason, and once they name one, answer `base` with it through \
                     `pando init --answers -`, with `--replace` if `base` is already answered."
                }
            };
            let tail = record.map(|r| r.failed_tail.as_slice()).unwrap_or_default();
            Some(failure(reason, tail, whose))
        }
        (SetupState::Failing, Some(CheckOutcome::NotSetUp { slot })) => Some(format!(
            "## The last test found a question open\n\n\
             `{slot}` has no answer, so there was nothing to test. Answer it with \
             `pando init --answers -`, then run `pando check` again.\n\n"
        )),
        (SetupState::Testing, _) => Some(
            "A test is running now: wait for it to finish rather than starting another \
             `pando check`.\n\n"
                .to_string(),
        ),
        (SetupState::Interrupted, _) => Some(
            "The last test never finished: it was stopped, or pando was. Run `pando check` \
             again; it clears away what that one left.\n\n"
                .to_string(),
        ),
        (SetupState::Stale, _) => Some(
            "The settings changed since the last test, so it no longer speaks for them: run \
             `pando check` once they are right.\n\n"
                .to_string(),
        ),
        (SetupState::Ready, Some(_)) => Some(format!(
            "The last test passed, on these settings, with pando {}: there is nothing left to \
             set up. Tell the developer they are ready, and if they have not said yes or no to \
             the block at the end of this job, offer it as \"Remember how to run it\" says: \
             save it only on a yes.\n\n",
            record.map_or("", |r| r.pando_version.as_str())
        )),
        _ => None,
    }
}

/// The failure, first: after a failed check the same prompt brings it to
/// the agent, so nothing long and nothing secret goes through a clipboard.
///
/// The last lines are redacted again as they are printed. The check
/// redacted them before it saved them; this is the one place they leave
/// pando's home, so it is the second guard, and a record an older pando
/// wrote is covered too.
pub(super) fn failure(reason: &str, tail: &[String], whose: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## The last test failed\n\n{reason}\n");
    if !tail.is_empty() {
        out.push_str("Its last lines:\n\n");
        for line in tail {
            let _ = writeln!(out, "    {}", crate::setup::redact_line(line));
        }
        out.push('\n');
    }
    let _ = writeln!(
        out,
        "{whose} Fix it before you run `pando check` again, and never rerun it unchanged.\n"
    );
    out
}

/// Where the project is, what each question stands at, the services the
/// check will use, and the questions still open with pando's options.
///
/// From the same places `signals` reads: detection's proposals, and
/// `settled` for whether config already answers a slot. Two things are
/// added: a connect to each shared service's port, bounded as every
/// readiness probe is, and, while nobody has answered the prelude, the
/// runtime probe `init` makes — a login shell per pinned language, none
/// for one the probe cache already holds as a pass.
///
/// `passing` is a last check that passed on these settings: with nothing
/// open, the line that sends an agent to `init --yes` and a check is left
/// out then.
fn project_now(
    paths: &PandoPaths,
    config: &Config,
    signals: &detect::Signals,
    proposals: &[detect::Proposal],
    device_note: Option<&str>,
    passing: bool,
    machine: &actions::Machine<'_>,
) -> String {
    let mut out = String::from("## This project, as pando sees it now\n\n");
    let root = paths.root();
    let main = crate::worktree::discover_all(&paths.project)
        .ok()
        .map(|found| found.main);
    match paths.project.is_multi_repo() {
        // The paths an agent writes are not this root's but a
        // repository's, which nothing else in the job says.
        true => {
            let repos: Vec<String> = main
                .iter()
                .flat_map(|main| &main.repos)
                .map(|repo| match &repo.branch {
                    Some(branch) => format!("{} (on {branch})", repo.name),
                    None => repo.name.clone(),
                })
                .collect();
            let _ = writeln!(
                out,
                "- root: {}, a folder of {} git repositories: {}. Each worktree is a set \
                 holding a checkout of every one at `<set>/<repo>`, so a path in the settings \
                 — a process's `cwd`, `provision`, `clone` — starts with the repository it is \
                 in: `<repo>/.env`. A base one repository needs of its own is `repos.base` in \
                 the answers",
                root.display(),
                repos.len(),
                repos.join(", "),
            );
        }
        false => {
            let branch = main
                .and_then(|main| main.branch)
                .map(|branch| format!(", on branch {branch}"))
                .unwrap_or_default();
            let _ = writeln!(out, "- root: {}{branch}", root.display());
        }
    }
    let settings = paths.config_file();
    let _ = writeln!(
        out,
        "- settings: {} ({})",
        settings.display(),
        match settings.is_file() {
            true => "written",
            false => "none yet",
        }
    );
    let runs: Vec<String> = config
        .processes
        .iter()
        .filter(|(_, process)| !process.cmd.trim().is_empty())
        .map(|(name, process)| format!("{name}: `{}`", process.cmd))
        .collect();
    if !runs.is_empty() {
        let _ = writeln!(out, "- runs: {}", runs.join(" · "));
    }
    // A passing setup never meets a device, so nothing else would say it.
    if let Some(note) = device_note {
        let _ = writeln!(
            out,
            "- {note}. Tell the developer when you are done, and never guess the address; \
             on the simulator, `pando open <name>` opens the app; `pando status <name>` gives \
             the commands."
        );
    }

    let proposal = |slot: Slot| proposals.iter().find(|p| p.slot == slot);

    out.push_str("\nEach question, as it stands (`pando signals` has the evidence):\n\n");
    let mut open: Vec<&detect::Proposal> = Vec::new();
    // A project that would run nothing has one question no rule raised:
    // `init` asks the dev command with no options, and "Open questions:
    // none" above a setup that starts nothing sent an agent to a check
    // that could only fail.
    let nothing_to_run = actions::runs_nothing(config, proposals);
    let dev_cmd = detect::Proposal::of(Slot::DevCmd, Vec::new(), false);
    // Processes that run a framework's server with no port: settled, and
    // still every worktree's server on the framework's own port. "Covered
    // by processes" there sent an agent past the one thing to fix.
    let portless = crate::doctor::portless_processes(paths, config);
    // The prelude is raised by a probe, not proposed, so it is never in
    // `open`: "none" beside an open prelude line sent an agent to a
    // `--yes` that then stopped on it.
    let mut prelude_open = false;
    for slot in actions::ALL_SLOTS {
        let name = slot_name(slot);
        let line = match (actions::settled(slot, config), proposal(slot)) {
            (false, None) if slot == Slot::DevCmd && nothing_to_run => {
                open.push(&dev_cmd);
                "open, below: nothing would run".to_string()
            }
            (true, _) if slot == Slot::PortEnv && !portless.is_empty() => portless
                .iter()
                .map(|p| format!("not given one: {} — fix: {}", p.message, p.fix))
                .collect::<Vec<_>>()
                .join("; "),
            (true, _) => match actions::slot_value(config, slot) {
                Some(value) => format!("set: `{value}`"),
                // The dev command and its ports, settled by process
                // tables that name no `dev`: "set" sent an agent looking
                // for keys pando.toml does not have.
                None if !config.processes.is_empty()
                    && matches!(slot, Slot::DevCmd | Slot::PortEnv) =>
                {
                    "covered by processes".to_string()
                }
                None => "set".to_string(),
            },
            (false, Some(p)) if !p.decided => {
                open.push(p);
                "open, below".to_string()
            }
            (false, Some(p)) => match p.preferred_set().as_slice() {
                [] => format!(
                    "decided: none{}",
                    p.none_because
                        .as_deref()
                        .map(|why| format!(" ({why})"))
                        .unwrap_or_default()
                ),
                chosen => format!("pando's guess: {}", candidates(chosen)),
            },
            (false, None) if slot == Slot::Prelude => {
                let line = prelude_line(paths, config, proposal(Slot::VersionFiles), machine);
                prelude_open = line.starts_with("open:");
                line
            }
            // No proposal here is no gap: origin/HEAD is close enough to
            // the main checkout's branch to be where work starts.
            (false, None) if slot == Slot::Base && paths.project.is_multi_repo() => {
                "each repository's own default branch".to_string()
            }
            (false, None) if slot == Slot::Base => "origin/HEAD".to_string(),
            // Never proposed, and what it is about is only known once
            // pando's choices are saved: which services stay on main's.
            (false, None) if slot == Slot::Namespaced => {
                "answer after `init --yes`, if it names one".to_string()
            }
            (false, None) => "nothing proposed".to_string(),
        };
        let _ = writeln!(out, "- {name}: {line}");
    }

    // Where the services' ports are read: the root, then each directory a
    // process runs in, then each app directory detection read, so a
    // project whose env files sit beside its apps is probed before any
    // process is configured.
    let mut dirs = actions::env_dirs(&paths.project, config);
    for app in &signals.app_dirs {
        if !dirs.contains(&app.dir) {
            dirs.push(app.dir.clone());
        }
    }
    let services = services_now(paths, config, &dirs, proposal(Slot::Services));
    if !services.is_empty() {
        out.push_str(
            "\nServices, the main checkout's own, which `pando check` uses (the port from its \
             env files, never a value):\n\n",
        );
        for line in services {
            let _ = writeln!(out, "- {line}");
        }
    }

    match open.is_empty() {
        true if passing => out.push('\n'),
        true if prelude_open => out.push_str(
            "\nOpen questions: only the prelude above. `pando init --yes` saves pando's \
             choices and stops on it; answer it, then `pando check`.\n\n",
        ),
        true => out.push_str(
            "\nOpen questions: none. `pando init --yes` saves pando's choices; then \
             `pando check`.\n\n",
        ),
        false => {
            out.push_str(
                "\nOpen questions, with pando's options. The marked one is what `pando init \
                 --yes` takes: take it, and ask the developer nothing. Answer by value only one \
                 that has no option.\n\n",
            );
            for p in open {
                out.push_str(&open_question(paths, p));
            }
            out.push('\n');
        }
    }
    out
}

/// The prelude's line in the job, while nobody has answered it: whether
/// `init` will ask it on this machine, which is whether `bash -lc` here
/// resolves what the project pins — in the version files config names,
/// or, while those are open, the ones `init --yes` would write.
fn prelude_line(
    paths: &PandoPaths,
    config: &Config,
    version_files: Option<&detect::Proposal>,
    machine: &actions::Machine<'_>,
) -> String {
    let mut after_init = config.clone();
    if !actions::settled(Slot::VersionFiles, config)
        && let Some(candidate) =
            version_files.and_then(|p| p.candidates.iter().find(|c| !c.needs_a_human))
    {
        detect::apply(Slot::VersionFiles, candidate, &mut after_init);
    }
    let files = after_init.runtime.version_files.clone();
    if crate::runtime::requirements_for(paths.root(), &files).is_empty() {
        return "not needed: the project pins no runtime".to_string();
    }
    let Some(check) = actions::prelude_needed(paths, &after_init, &files, machine) else {
        return "not asked on this machine: `bash -lc` here resolves what the project pins"
            .to_string();
    };
    let requirement = &check.requirement;
    let language = &requirement.language;
    let resolved = match (&check.resolved.version, &check.resolved.path) {
        (Some(version), _) => format!("`bash -lc` resolves {version}"),
        (None, Some(path)) => format!("`bash -lc` finds {path}, and it fails"),
        (None, None) => format!("`bash -lc` has no {language}"),
    };
    let asked = format!(
        "open: {language} {} asked ({}), {resolved}",
        requirement.spec, requirement.source
    );
    // What `--yes` would really do, from the lines tried on this machine:
    // a manager's line that works, or nothing at all, since a PATH line is
    // never taken for the developer. Nothing is written by trying them.
    let requirements = crate::runtime::requirements_for(paths.root(), &files);
    let offers = actions::prelude_offers(paths, &after_init, &requirements, &check, machine, false)
        .unwrap_or_default();
    match offers.iter().find(|offer| offer.taken_by_yes()) {
        Some(offer) => format!(
            "{asked}. `pando init --yes` takes `{}`; it runs in every project on this \
             machine, so name it in your report",
            offer.line
        ),
        None => format!(
            "{asked}. Answer it with the first line `pando doctor` lists, or `null` to run on \
             what this machine has when it lists none or pando refuses the line: it runs in \
             every project on this machine, so name it in your report"
        ),
    }
}

/// Candidates as a list of their values, each with the signal that found
/// it.
fn candidates(chosen: &[&detect::Candidate]) -> String {
    chosen
        .iter()
        .map(|c| match c.why.is_empty() {
            true => format!("`{}`", c.value),
            false => format!("`{}` ({})", c.value, c.why),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One open question: its prompt and its options, the one pando takes on
/// its own marked — the option `init --yes` takes, or the ticked ones of a
/// set question. An option `--yes` may not take is never marked, even
/// where `new` and `start` would take it: the job says the marked one is
/// what `--yes` takes.
fn open_question(paths: &PandoPaths, proposal: &detect::Proposal) -> String {
    let question = actions::question_for(proposal, &[]).at(paths);
    let taken: Vec<usize> = match question.multi {
        true => question.checked.clone(),
        false => question.preselect.into_iter().collect(),
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        "- {}: {}{}",
        slot_name(proposal.slot),
        question.prompt,
        match question.multi {
            true => " (a list of the options)",
            false => "",
        }
    );
    for (index, (value, why)) in question.options.iter().enumerate() {
        let why = match why.is_empty() {
            true => String::new(),
            false => format!(" — {why}"),
        };
        let mark = match taken.contains(&index) {
            true => " ← pando's choice",
            false => "",
        };
        let _ = writeln!(out, "  {}. `{value}`{why}{mark}", index + 1);
    }
    if question.options.is_empty() {
        let _ = writeln!(
            out,
            "  pando found nothing to offer, and `pando init --yes` exits 3 here. Answer it with \
             a command of your own from the project's docs, or, for several processes, \
             `processes` with an object of process tables (`--reference json`, \"The answers \
             file\")."
        );
    }
    // A process list that leaves an app directory unstarted: its evidence
    // above says which, and taken, it passes a check with that app never
    // run.
    if proposal.slot == Slot::Processes && question.preselect.is_none() {
        let _ = writeln!(
            out,
            "  None is marked: only the project's docs or the developer know how each directory \
             named above runs. Answer `processes` with an object of process tables covering \
             every process."
        );
    }
    out
}

/// Each shared service and whether it answers: the ones config names, or,
/// before the services question is answered, the ones detection proposes,
/// found through the same env key a shared start reads, each with the env
/// file its port came from.
fn services_now(
    paths: &PandoPaths,
    config: &Config,
    dirs: &[String],
    proposal: Option<&detect::Proposal>,
) -> Vec<String> {
    let statuses = match config.services.is_empty() {
        false => actions::shared_service_statuses(paths, config, dirs),
        true => {
            let mut statuses = Vec::new();
            for candidate in proposal
                .map(|p| p.candidates.as_slice())
                .unwrap_or_default()
            {
                match candidate
                    .service
                    .as_ref()
                    .and_then(|hint| hint.env_key.as_deref())
                {
                    Some(key) => statuses.push(actions::shared_service_status(
                        paths,
                        dirs,
                        key,
                        &candidate.value,
                    )),
                    None => statuses.push(actions::ServiceStatus {
                        name: candidate.value.clone(),
                        port: None,
                        up: false,
                        logging: false,
                        env_file: None,
                    }),
                }
            }
            statuses
        }
    };
    statuses
        .iter()
        .map(|status| match status.port {
            Some(port) => format!(
                "{} :{port}{} ({})",
                status.name,
                status
                    .env_file
                    .as_deref()
                    .map(|file| format!(" in {file}"))
                    .unwrap_or_default(),
                match status.up {
                    true => "running",
                    false => "not running",
                }
            ),
            None => format!(
                "{}: no port in the main checkout's env files, so not probed",
                status.name
            ),
        })
        .collect()
}

/// What the job says first for a repository that sits in a folder beside
/// other repositories: pando can run that folder as one project, and
/// which of the two to set up is the developer's shape, not pando's — so
/// the agent is told how to tell, and to say which it chose.
fn beside_others(project: &ProjectRef, folder: &ProjectRef) -> String {
    let others: Vec<&str> = folder
        .repos
        .iter()
        .map(String::as_str)
        .filter(|repo| Some(*repo) != project.root.file_name().and_then(|n| n.to_str()))
        .collect();
    format!(
        "**This repository sits in {} beside {}.** pando runs such a folder as one \
         project, a worktree of every repository on one branch, when it is set up from \
         there. If this app needs theirs to run — a web app and its API — set pando up \
         from the folder instead: run `pando init --agent` in {} and follow that job, not \
         this one. Say in your report which you chose and why.\n\n",
        folder.root.display(),
        others.join(", "),
        folder.root.display()
    )
}

/// What each command the job names writes, and what it needs, so an agent
/// in a sandbox knows which two to hand to the developer.
fn writes(paths: &PandoPaths) -> String {
    format!(
        "## What each command writes and needs\n\n\
         - `pando signals`, `pando doctor --json`: read only.\n\
         - `pando init --answers -`: writes under {project} (this project's settings and its \
         decisions log), and {user} only for a runtime prelude.\n\
         - `pando check`: writes under {home} and inside {git} (a throwaway worktree it \
         removes); \
         runs the project's install, which may need the network.\n\n\
         In a sandbox that blocks either (Codex's workspace-write blocks both), end your \
         report on these two commands, for the developer to run outside it.\n\n",
        project = paths.project_dir().display(),
        user = paths.user_config_file().display(),
        home = paths.home.display(),
        git = match paths.project.is_multi_repo() {
            true => "each repository's .git",
            false => ".git",
        },
    )
}

/// The brief's first-run section: from its heading to the next heading of
/// the same level or higher, or the rule that ends the brief's opening.
pub(super) fn first_run_section() -> &'static str {
    let start = BRIEF
        .find(&format!("\n{FIRST_RUN_HEADING}"))
        .map(|at| at + 1)
        .expect("agent/brief.md has a first-run section");
    let rest = &BRIEF[start..];
    let body = rest.find('\n').map_or(rest.len(), |at| at + 1);
    let end = ["\n## ", "\n# ", "\n---"]
        .iter()
        .filter_map(|marker| rest[body..].find(marker).map(|at| body + at + 1))
        .min()
        .unwrap_or(rest.len());
    &rest[..end]
}
