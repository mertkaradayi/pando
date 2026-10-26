//! `signals --json`: everything detection read and proposes, for an
//! agent to decide from.

use super::JSON_VERSION;
use super::ls::ProjectOut;
use crate::actions;
use crate::config::Config;
use crate::paths::PandoPaths;
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;

// ---- signals --------------------------------------------------------------

/// Everything tier 1 found, and everything the rules made of it.
///
/// The input an agent reads before it decides anything, so completeness
/// beats brevity: if a rule considered something, it is in here. Nothing
/// in this path spawns a process or writes a byte — no probe of the
/// machine's runtime, no `docker compose config` — which is what makes two
/// runs identical.
#[derive(Serialize)]
struct SignalsOutput {
    version: u32,
    project: ProjectOut,
    signals: crate::detect::Signals,
    /// Every compose file at the root, as this build's own reader sees it
    /// — including the keys it does not follow, which is why a services
    /// proposal may be missing or unticked.
    compose: Vec<ComposeOut>,
    /// One entry per question pando can ask, in the order it asks them.
    slots: Vec<SlotOut>,
    /// What a namespaced start would do with each service: a database or
    /// a slot of the worktree's own, a prefix its app is told, or the main
    /// checkout's data and why — what the `namespaced` answer changes.
    namespaced: Vec<actions::NamespacedService>,
}

#[derive(Serialize)]
struct ComposeOut {
    file: String,
    /// The services this reader could see, in file order.
    services: Vec<String>,
    /// Services carrying `extends:`, whose real definition is in another
    /// file this reader does not follow.
    extends: Vec<String>,
    /// Whether the file has a top-level `include:`, which brings in
    /// services this list does not have.
    include: bool,
    /// Why the file could not be read at all, when it could not.
    error: Option<String>,
}

#[derive(Serialize)]
struct SlotOut {
    slot: crate::detect::Slot,
    /// The question, as it would be asked.
    prompt: &'static str,
    /// Whether config already answers this, from any layer. An answers
    /// file for an answered slot is refused unless it says `--replace`.
    answered: bool,
    /// `null` when the rules had nothing to say about this slot at all —
    /// which is different from a proposal with no candidates, and the
    /// difference matters: the second is a rule deciding the answer is
    /// "none".
    proposal: Option<ProposalOut>,
}

#[derive(Serialize)]
struct ProposalOut {
    /// Whether the rules are sure enough to take this without asking. A
    /// decided slot is never a question, so an answers file's value for it
    /// is not used.
    decided: bool,
    /// The option a question would preselect, and the one `--yes` takes.
    /// `null` when there is nothing here a flag may take on a developer's
    /// behalf.
    preferred: Option<usize>,
    /// For the question whose answer is a set: the options that start
    /// ticked.
    checked: Vec<usize>,
    /// Whether the answer is a set of the options rather than one of them.
    multi: bool,
    /// Whether a command or path of your own is an answer here.
    allow_custom: bool,
    /// Whether "none of them" is an answer here — `null` in an answers
    /// file.
    allow_none: bool,
    /// The compose file a services proposal is about, which outlives
    /// having any candidates.
    file: Option<String>,
    /// Why there is nothing to choose from, when a rule settled the slot
    /// with the empty answer instead of finding candidates for it.
    none_because: Option<String>,
    candidates: Vec<CandidateOut>,
}

#[derive(Serialize)]
struct CandidateOut {
    /// What an answers file names to pick this option.
    value: String,
    /// The signal that made it a candidate, written into the config as
    /// `# detected: <why>` when it is taken.
    why: String,
    /// Whether a set question starts with this one ticked.
    preselected: bool,
    /// Whether only a person may take this: an option that makes pando
    /// create a file out of contents it did not write is never one a flag
    /// accepts. An answers file naming it explicitly still may.
    needs_a_human: bool,
    /// Roles this option brings with it, when it carries its own ports.
    ports: Option<crate::config::PortsSpec>,
    /// Whole process tables, for the answer that is several of them.
    processes: Option<BTreeMap<String, crate::config::ProcessConfig>>,
    /// For a compose service: the file it is declared in and the env key
    /// that points at it.
    service: Option<ServiceHintOut>,
    /// For the schema slot: the whole `[[hooks]]` entry this option is.
    hook: Option<crate::config::HookConfig>,
    /// For the provision slot: which of the paths in `value` would be
    /// copied from an example rather than linked from a file that is
    /// already here.
    provision_from: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct ServiceHintOut {
    /// The compose file that declares it. Empty for a native service,
    /// which no file in the repository declares — `recipe` names what
    /// would run it instead. Kept rather than made optional so a program
    /// reading this since Phase 5 still finds the key it knows.
    file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    recipe: Option<String>,
    env_key: Option<String>,
}

pub fn signals_json<W: Write>(paths: &PandoPaths, config: &Config, out: &mut W) -> Result<()> {
    let root = paths.root();
    let signals = crate::detect::signals(root);
    // No resolver: asking `docker compose config` would spawn a process and
    // make the answer depend on this laptop. What this build's own reader
    // could not follow is published instead, under `compose`.
    let proposals = crate::detect::propose(root, &signals);
    let output = SignalsOutput {
        version: JSON_VERSION,
        project: ProjectOut::of(&paths.project),
        compose: signals
            .compose_files
            .iter()
            .map(|file| match crate::compose::read(&root.join(file)) {
                Ok(parsed) => ComposeOut {
                    file: file.clone(),
                    services: parsed.services.keys().cloned().collect(),
                    extends: parsed.unresolved.extends.clone(),
                    include: parsed.unresolved.include,
                    error: None,
                },
                Err(e) => ComposeOut {
                    file: file.clone(),
                    services: Vec::new(),
                    extends: Vec::new(),
                    include: false,
                    error: Some(format!("{e:#}")),
                },
            })
            .collect(),
        slots: actions::ALL_SLOTS
            .iter()
            .map(|slot| SlotOut {
                slot: *slot,
                prompt: slot.prompt(),
                answered: actions::settled(*slot, config),
                proposal: proposals.iter().find(|p| p.slot == *slot).map(proposal_out),
            })
            .collect(),
        namespaced: actions::namespaced_report(paths, config, &signals.compose_files),
        signals,
    };
    writeln!(out, "{}", serde_json::to_string_pretty(&output)?)?;
    Ok(())
}

fn proposal_out(proposal: &crate::detect::Proposal) -> ProposalOut {
    // Through the same function the resolver asks with, so what is
    // published is the question itself rather than a second description of
    // one.
    let question = actions::question_for(proposal, &[]);
    ProposalOut {
        decided: proposal.decided,
        preferred: question.preselect,
        checked: question.checked.clone(),
        multi: question.multi,
        allow_custom: question.allow_custom,
        allow_none: question.allow_none,
        file: proposal.service_file().map(str::to_string),
        none_because: proposal.none_because.clone(),
        candidates: proposal
            .candidates
            .iter()
            .map(|candidate| CandidateOut {
                value: candidate.value.clone(),
                why: candidate.why.clone(),
                preselected: candidate.preselected,
                needs_a_human: candidate.needs_a_human,
                ports: candidate.ports.clone(),
                processes: candidate.processes.clone(),
                service: candidate.service.as_ref().map(|hint| ServiceHintOut {
                    file: hint.file().unwrap_or_default().to_string(),
                    recipe: hint.recipe().map(str::to_string),
                    env_key: hint.env_key.clone(),
                }),
                hook: candidate.hook.clone(),
                provision_from: candidate.provision_from.clone(),
            })
            .collect(),
    }
}
