//! Questions: what pando asks when it cannot work something out, and how
//! the answer is written to `pando.toml`.

use anyhow::{Context, Result, bail};
use std::path::Path;

use crate::config::{self, Config};
use crate::decisions;
use crate::detect::{self, Slot};
use crate::paths::PandoPaths;
use crate::services;

use super::init::{machine_evidence, slot_value};
use super::lifecycle::{
    Mode, refuse_losing_namespaces, refuse_main_mode, target_of, worktree_roles,
};
use super::namespaced::{ask_for_logins, free_slots_if_full};
use super::runtime::{
    Machine, RuntimeOutcome, answer_prelude, prelude_proposal, resolve_runtime, runtime_shell,
};
use super::services::{backends_reachable, placeholder_ports, service_roles};
use super::worktree::{find_checkout, ref_exists, refuse_a_gone_directory, resolve_create_base};
use crate::state::ServiceMode;

/// Something pando needs to know and cannot work out on its own.
///
/// Asked at the moment the answer is needed, answered once, and written to
/// `pando.toml` so it is never asked again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub slot: Slot,
    pub prompt: String,
    /// Each option as its value and the signal that found it, in rule order.
    pub options: Vec<(String, String)>,
    /// The option the rules put first. `None` when they found nothing, in
    /// which case only a typed answer will do.
    pub preselect: Option<usize>,
    /// Whether a command typed by hand is acceptable. Always true in this
    /// phase: every slot accepts a shell command, so there is no dead end.
    pub allow_custom: bool,
    /// Whether "this process has none" is an answer. True for the port
    /// question: a worker or a watcher really has no port, and that has to
    /// be sayable, or the question comes back on every start. True for the
    /// services question too, where it means "none of them".
    pub allow_none: bool,
    /// Whether the answer is a *set* of the options rather than one of
    /// them: which services this project runs private copies of.
    pub multi: bool,
    /// For a multi-select question, the options that start ticked.
    pub checked: Vec<usize>,
    /// The report a question needs to be answerable: what the project
    /// asks for, what this machine answered, and where from. Printed
    /// above the options by every front end, and carried on the question
    /// rather than narrated separately so the exit-3 render has it too.
    pub details: Vec<String>,
    /// The file this answer is written to, absolute — the project's
    /// `pando.toml` under whatever home `PANDO_HOME` names, or the
    /// machine-wide config for the prelude. `None` for a question built
    /// with no project in hand, such as the one `signals` publishes.
    pub answer_file: Option<std::path::PathBuf>,
    /// What the first option would be in that file, ready to paste: the
    /// same edits an answer writes. For a question with no options, the
    /// key with a placeholder.
    pub snippet: String,
}

impl Question {
    /// The same question, told where its answer goes.
    pub fn at(mut self, paths: &PandoPaths) -> Question {
        self.answer_file = Some(match self.slot.layer() {
            config::Layer::User => paths.user_config_file(),
            _ => paths.config_file(),
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Choice(usize),
    /// The first option, taken because `--yes` was passed rather than
    /// because anyone chose it. Written down as exactly that: a config that
    /// claims a rule decided something a flag decided is a config nobody
    /// can review.
    Auto(usize),
    Custom(String),
    /// Several of the options, for a question whose answer is a set: which
    /// of the compose file's services this project runs private copies of.
    Many(Vec<usize>),
    /// "This process has none of those." Only offered where a question has
    /// an empty answer that means something: the port, and the empty set
    /// at the services question.
    None,
    /// Whole process tables of the answerer's own, at the one question
    /// whose answer is several processes: what `[processes.<name>]` says,
    /// named as it would be there.
    ///
    /// The typed answer a multi-process project needs. A command of one's
    /// own is one process, and a project of three is not one command
    /// without losing each process's log, readiness and port.
    Processes(std::collections::BTreeMap<String, config::ProcessConfig>),
    /// One of the shapes above, from a program rather than a person:
    /// `init --answers`.
    ///
    /// A wrapper rather than four more variants, so the shapes stay four
    /// and the one thing that differs — the note written beside the key —
    /// is decided in one place. [`answered_by`] peels it before anything
    /// matches on what is inside.
    Program(Box<Answer>),
}

/// What an everyday command takes instead of asking — `new`, `start` and
/// `restart` on a terminal, and the TUI: the option pando's rules put
/// first, with the line that says so and where to change it.
///
/// A developer's first `start` should run, not interview them; the file
/// the answer lands in is theirs to edit, and `pando init` is where
/// somebody who wants every choice put to them goes. Taken as a
/// [`Answer::Choice`], because that is what it is — one of pando's own
/// options, written down with the evidence that found it.
///
/// Somebody is there to read the line, so the first option is taken even
/// where `--yes` may not take it — a local file seeded from the project's
/// example — and the line carries the option's own reason, which names the
/// file it copies from.
///
/// `None` for a question with no options, which is still asked, for a
/// set question — which of the services to run private copies of — whose
/// answer is not one option but a selection, and for one only a person
/// may answer, because its answer empties somebody's data.
pub fn recommended(question: &Question) -> Option<(Answer, String)> {
    if question.multi || question.options.is_empty() || question.slot.takes_a_person() {
        return None;
    }
    // A machine-wide answer is never a guess: with nothing preselected,
    // every option there is one the developer has to pick, and what it
    // would change reaches every project on the machine.
    if question.preselect.is_none() && question.slot.layer() == crate::config::Layer::User {
        return None;
    }
    let index = question.preselect.unwrap_or(0);
    let (value, why) = question.options.get(index)?;
    let why = match why.is_empty() {
        true => String::new(),
        false => format!(" ({why})"),
    };
    let alternatives = match question.options.len() - 1 {
        0 => String::new(),
        1 => ", over 1 other option".to_string(),
        others => format!(", over {others} other options"),
    };
    let change = question
        .answer_file
        .as_ref()
        .map(|file| format!(" — change it in {}", file.display()))
        .unwrap_or_default();
    Some((
        Answer::Choice(index),
        format!(
            "{}: using {value:?}{why}, pando's first choice{alternatives}{change}",
            slot_label(question.slot)
        ),
    ))
}

/// How a front end asks. The CLI prompts on a terminal and refuses
/// elsewhere; the TUI opens a modal; a test hands back a scripted answer.
pub type Ask<'a> = &'a dyn Fn(&Question) -> Result<Answer>;

/// An answer a caller already has in hand, for a question nobody is going
/// to be asked.
///
/// Deliberately not an [`Ask`]. A question is something a front end *puts*
/// to somebody, and there is nobody to put this one to: the rules proposed
/// nothing, so there are no options and no preselection, and a terminal
/// that prompted here would be asking a developer to invent a command out
/// of nothing on every project pando has no guess for. A program reading
/// its own file is the one answerer that can have something to say.
pub type Volunteered<'a> = &'a dyn Fn(&Question) -> Option<Result<Answer>>;

/// Where one resolution pass gets its answers.
pub struct Answering<'a> {
    /// What a front end puts to somebody.
    pub ask: Ask<'a>,
    /// What a program supplies for a slot no rule proposed anything for —
    /// and, by its presence, the fact that a program rather than a person
    /// is driving this pass at all.
    pub program: Option<Volunteered<'a>>,
    /// The slots whose answer the program replaces: `init --answers
    /// --replace`. Empty on every other pass.
    replace: &'a [Slot],
    /// The slots the program has an answer for: its question is put to it
    /// even where a rule decided the slot, because the program's answer is
    /// the one it gave and the rule's is a guess. Empty on every other
    /// pass.
    answered: &'a [Slot],
    /// Which of them this pass wrote, so `init` can say so: a replaced
    /// slot was settled before the run and is settled after it.
    replaced: std::cell::RefCell<Vec<Slot>>,
}

impl<'a> Answering<'a> {
    /// A pass with a person behind it: nothing is volunteered, and a slot
    /// the rules are silent about stays silent.
    pub fn asking(ask: Ask<'a>) -> Answering<'a> {
        Answering {
            ask,
            program: None,
            replace: &[],
            answered: &[],
            replaced: Default::default(),
        }
    }

    /// A pass a program is driving, with its own answers to fall back on.
    pub fn by_program(ask: Ask<'a>, program: Volunteered<'a>) -> Answering<'a> {
        Answering {
            ask,
            program: Some(program),
            replace: &[],
            answered: &[],
            replaced: Default::default(),
        }
    }

    /// The same pass, with the program's answers taking the place of
    /// what config already says about these slots, and of what a rule
    /// would decide for them.
    ///
    /// Only for a pass whose `ask` puts these slots to the program: the
    /// answer that replaces is always a program's, and written down as
    /// one. Only project-layer slots are ever replaced — the prelude is
    /// about the machine, and only a person changes it, so it is
    /// answered here only when nothing has answered it yet, and `init`
    /// refuses the rest.
    pub fn replacing(self, slots: &'a [Slot]) -> Answering<'a> {
        Answering {
            replace: slots,
            ..self
        }
    }

    /// The same pass, with the program holding answers for these slots:
    /// a rule's decision does not stand in for any of them. Only for a
    /// pass whose `ask` puts these slots to the program, as `replacing`.
    pub fn answered(self, slots: &'a [Slot]) -> Answering<'a> {
        Answering {
            answered: slots,
            ..self
        }
    }

    /// Whether a rule's decision for `slot` is taken without asking: not
    /// when the pass replaces it, nor when the program has its own answer.
    fn takes_decision(&self, slot: Slot) -> bool {
        !self.replaces(slot) && !self.answered.contains(&slot)
    }

    /// Whether this pass replaces the answer to `slot`.
    pub(super) fn replaces(&self, slot: Slot) -> bool {
        slot.layer() == config::Layer::Project && self.replace.contains(&slot)
    }

    /// The slots [`replacing`](Self::replacing) named, whatever their
    /// layer.
    pub(super) fn named_for_replacing(&self) -> &[Slot] {
        self.replace
    }

    /// Whether this pass wrote a replacement for `slot`.
    pub(super) fn replaced(&self, slot: Slot) -> bool {
        self.replaced.borrow().contains(&slot)
    }

    fn wrote_replacement(&self, slot: Slot) {
        self.replaced.borrow_mut().push(slot);
    }
}

/// The error every front end turns into exit code 3.
///
/// A question is not a failure, and an agent has to be able to tell them
/// apart without reading English.
#[derive(Debug, Clone)]
pub struct NeedsAnswer {
    pub question: Question,
}

impl std::fmt::Display for NeedsAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.question.prompt)
    }
}

impl std::error::Error for NeedsAnswer {}

/// A value a *program* supplied that pando checked and refused — a
/// prelude that fails its own probe, say.
///
/// The same class of mistake as an answers file naming a question pando
/// does not ask: something wrong with what was handed in, not a failure
/// of what pando tried to do. Front ends give it the usage-error code, so
/// a program can tell "my answer was bad" from "the command broke"
/// without reading English. A person typing the same value at a prompt
/// gets an ordinary error, because for them there is no file to fix.
#[derive(Debug, Clone)]
pub struct RefusedAnswer(pub String);

impl std::fmt::Display for RefusedAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RefusedAnswer {}

/// Who answered, for the comment written beside the key it fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Answerer {
    Human,
    Program,
}

impl Answerer {
    /// The note for this answer: what a person choosing or typing it
    /// earns, or the one that says a program did.
    pub(super) fn note(self, human: config::Note) -> config::Note {
        match self {
            Answerer::Human => human,
            Answerer::Program => config::Note::Program,
        }
    }

    /// An answer this answerer gave that pando refuses: the usage-error
    /// shape for a program, whose file is what has to change, and an
    /// ordinary error for a person, who has no file to fix.
    pub(super) fn refuse(self, message: String) -> anyhow::Error {
        match self {
            Answerer::Program => anyhow::Error::new(RefusedAnswer(message)),
            Answerer::Human => anyhow::anyhow!(message),
        }
    }
}

/// A typed answer, as the candidate it becomes — or refused, with nothing
/// written, when it cannot be an answer to this slot at all.
///
/// The one door both callers go through, the question loop and
/// [`volunteer`], so a value is held to the same rules whether a rule had
/// options for its slot or not.
fn typed(slot: Slot, value: &str, by: Answerer) -> Result<detect::Candidate> {
    let value = value.trim();
    if slot == Slot::PortEnv
        && let Err(why) = detect::typed_ports(value)
    {
        return Err(by.refuse(format!(
            "{value:?} cannot be this project's {}: {why} — nothing was written",
            slot_label(slot)
        )));
    }
    Ok(detect::custom(slot, value))
}

/// Peels [`Answer::Program`] off the shape underneath it, so one match
/// handles the four shapes and the provenance is decided once.
/// A program's answer, held until the answer is actually on disk.
///
/// The record says what config says *afterwards*, so it cannot be written
/// at the moment the answer arrives: an answer that is refused a moment
/// later by [`refuse_unloadable`] never happened, and a log that claims
/// otherwise is worse than no log.
pub(super) struct Pending {
    slot: Slot,
    answer: serde_json::Value,
    shape: decisions::Shape,
    evidence: decisions::Evidence,
    /// Whether this answer takes the place of one config already has:
    /// the old one's tables are removed when this one is written, and
    /// not a moment before.
    replaces: bool,
}

/// What a program answered, in the shape an answers file would have sent.
///
/// `None` for every other answerer. A person's answer is not this file's
/// business — the rules are not being asked to learn from it — and
/// neither is `--yes`, which took what the rules already preferred.
pub(super) fn pending_decision(
    question: &Question,
    proposal: &detect::Proposal,
    answer: &Answer,
    by: Answerer,
) -> Option<Pending> {
    if by != Answerer::Program {
        return None;
    }
    // By value, exactly as the answers file names it, so the log can be
    // turned back into one.
    let option = |index: usize| {
        question
            .options
            .get(index)
            .map(|(value, _)| value.clone())
            .unwrap_or_default()
    };
    let (answer, shape) = match answer {
        Answer::Choice(index) => (
            serde_json::Value::String(option(*index)),
            decisions::Shape::Choice,
        ),
        Answer::Custom(text) => (
            serde_json::Value::String(text.trim().to_string()),
            decisions::Shape::Custom,
        ),
        Answer::Many(indexes) => (
            serde_json::Value::Array(
                indexes
                    .iter()
                    .map(|index| serde_json::Value::String(option(*index)))
                    .collect(),
            ),
            decisions::Shape::Set,
        ),
        Answer::None => (serde_json::Value::Null, decisions::Shape::None),
        // The object the answers file sent, so the line replays as one.
        Answer::Processes(tables) => (
            serde_json::to_value(tables).unwrap_or_default(),
            decisions::Shape::Custom,
        ),
        // `--yes` never reaches here, and every wrapper is peeled before
        // this is called.
        Answer::Auto(_) | Answer::Program(_) => return None,
    };
    Some(Pending {
        slot: question.slot,
        answer,
        shape,
        evidence: decisions::Evidence {
            prompt: question.prompt.clone(),
            details: question.details.clone(),
            mechanism: proposal.mechanism.map(str::to_string),
            weighed: proposal.evidence.clone(),
            preferred: question.preselect,
            // The candidates rather than the question's options, because
            // the two are the same list and only one of them carries why
            // an option was preselected and whether a flag was allowed to
            // take it.
            options: proposal
                .candidates
                .iter()
                .map(|c| decisions::Opt {
                    value: c.value.clone(),
                    why: c.why.clone(),
                    preselected: c.preselected,
                    needs_a_human: c.needs_a_human,
                })
                .collect(),
        },
        replaces: false,
    })
}

/// Writes one pending decision down, now that its answer is on disk.
///
/// `wrote` is read back through [`config::load`] rather than taken from
/// the config in hand: a later run compares against what it computes from
/// the file, and recording anything else is a false override waiting to
/// happen.
///
/// Never fatal. A corpus that costs somebody their `init` is one nobody
/// will leave switched on.
fn record_decision(paths: &PandoPaths, pending: Option<Pending>, progress: &dyn Fn(&str)) {
    let Some(pending) = pending else { return };
    let wrote = config::load(paths)
        .ok()
        .and_then(|loaded| slot_value(&loaded.config, pending.slot));
    let entry = decisions::Entry::answered(
        pending.slot,
        pending.answer,
        pending.shape,
        wrote,
        pending.evidence,
    );
    if let Err(e) = decisions::append(paths, &entry) {
        progress(&format!(
            "could not record the answer to the {} question: {e:#}",
            slot_label(pending.slot)
        ));
    }
}

pub(super) fn answered_by(answer: Answer) -> (Answer, Answerer) {
    let mut answer = answer;
    let mut by = Answerer::Human;
    while let Answer::Program(inner) = answer {
        by = Answerer::Program;
        answer = *inner;
    }
    (answer, by)
}

/// The slots `new` fills: what to install, what pins the runtime, and which
/// local files a worktree needs a copy of.
pub const NEW_SLOTS: [Slot; 3] = [Slot::Install, Slot::VersionFiles, Slot::Provision];

/// The slots `start` fills: how many processes there are, then the dev
/// process and how it takes its port, then the schema step. `Processes`
/// first, because the answer to it decides whether the others have
/// anything left to ask.
///
/// `Services` is in the list on every start, because a project whose
/// services a rule resolved outright should have them in config — that is
/// what the shared-mode health chips read. It is in [`SILENT_UNLESS_ISOLATED`]
/// too, so on a plain start it is only ever *taken*, never *asked*:
/// "which services do you want private copies of?" is a question about a
/// mode this start is not in.
pub const START_SLOTS: [Slot; 6] = [
    // First, and before anything is spawned: a process started under the
    // wrong runtime dies of it, and the point of asking is to not start
    // it.
    Slot::Prelude,
    Slot::Processes,
    Slot::DevCmd,
    Slot::PortEnv,
    Slot::Services,
    Slot::SchemaHook,
];

/// Slots a start that is not isolating may accept but must not ask about.
///
/// The schema step too: it runs after the private services are up, and a
/// start that is not isolating has none — asking whether to migrate a
/// database this start does not have is a question about a mode it is
/// not in. Its proposal is never decided, so such a start neither asks it
/// nor takes it.
pub(super) const SILENT_UNLESS_ISOLATED: [Slot; 2] = [Slot::Services, Slot::SchemaHook];

/// Fills the dev process from detection when config has none.
pub fn resolve_process(
    paths: &PandoPaths,
    config: &Config,
    mode: Mode,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    // Only a start onto data of its own may *ask* which services there are
    // and what fills a fresh database. `--shared` is a start that is
    // putting them away, which is no more a reason to ask than a plain
    // one. With no worktree to read, the flag is all there is;
    // [`resolve_for_start`] knows more.
    resolve_onto(
        paths,
        config,
        mode == Mode::Isolated,
        mode == Mode::Namespaced,
        ask,
        progress,
    )
}

/// [`resolve_process`] for a start of one worktree: what `start` and
/// `restart` call.
///
/// Two things only the worktree can say. Whether this start runs on data
/// of its own is not only the flag: a plain start of a worktree that is
/// already isolated or namespaced keeps its own, so the schema and
/// services questions are about the mode it is in — and a namespaced one
/// asks for the login its namespaces are made with, when nothing gives one. And a start that isolates with Docker not
/// answering cannot happen at all, so that is found out before anything
/// is asked — never after the developer has answered a question for it.
pub fn resolve_for_start(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    mode: Mode,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    // A worktree whose directory is gone is refused before anything is
    // asked: a login given, or a slot freed, for a start that cannot run
    // there is a cost with nothing for it. A name git does not list is
    // `start`'s to refuse.
    let checkout = find_checkout(paths, name).ok();
    if let Some(checkout) = &checkout {
        refuse_a_gone_directory(&checkout.worktree)?;
        // Nor for the main checkout in a mode it never runs in.
        if checkout.main {
            refuse_main_mode(name, mode)?;
        }
    }
    let worktree = checkout.map(|checkout| checkout.worktree);
    // Nor is anything asked for a start of a namespaced worktree that can
    // no longer have its namespaces: that start is refused.
    refuse_losing_namespaces(paths, config, name, mode)?;
    let target = target_of(paths, config, name, mode);
    let isolating = mode == Mode::Isolated || target == ServiceMode::Isolated;
    if isolating
        && !service_roles(config).is_empty()
        && let Some(worktree) = &worktree
    {
        let canonical =
            std::fs::canonicalize(&worktree.path).unwrap_or_else(|_| worktree.path.clone());
        let ports = placeholder_ports(&worktree_roles(config, true));
        backends_reachable(paths, config, name, &canonical, &ports)?;
    }
    let namespacing = mode == Mode::Namespaced || target == ServiceMode::Namespaced;
    let mut config = resolve_onto(paths, config, isolating, namespacing, ask, progress)?;
    // A namespaced start's login, when nothing gives one, is asked now
    // with the rest — before anything runs, and of the config the answers
    // above may just have given its services.
    if namespacing {
        ask_for_logins(paths, &mut config, ask, progress)?;
        free_slots_if_full(paths, &config, name, ask, progress)?;
    }
    Ok(config)
}

/// The questions of a start that isolates, namespaces, or does neither.
///
/// A namespaced start is onto data of its own only where its plan gives it
/// some: the schema step runs only on a database of the worktree's own
/// with nothing left on the main checkout's data, so it is asked about
/// only then. The plan is of the config the other answers give, because
/// which services there are is one of them.
fn resolve_onto(
    paths: &PandoPaths,
    config: &Config,
    isolating: bool,
    namespacing: bool,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    if isolating || !namespacing {
        return resolve_starting(paths, config, isolating, ask, progress);
    }
    let answering = Answering::asking(ask);
    let config = resolve_silencing(
        paths,
        config,
        &START_SLOTS,
        &[Slot::SchemaHook],
        &answering,
        progress,
    )?;
    if super::namespaced::plan(paths, &config)
        .not_own_data()
        .is_some()
    {
        return Ok(config);
    }
    // The services are settled by now; silenced so a slot nothing
    // proposed pays no probe for a question this pass cannot ask.
    resolve_silencing(
        paths,
        &config,
        &[Slot::SchemaHook],
        &[Slot::Services],
        &answering,
        progress,
    )
}

/// The questions of a start that is, or is not, onto data of its own.
fn resolve_starting(
    paths: &PandoPaths,
    config: &Config,
    isolating: bool,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    let silent: &[Slot] = if isolating {
        &[]
    } else {
        &SILENT_UNLESS_ISOLATED
    };
    resolve_silencing(
        paths,
        config,
        &START_SLOTS,
        silent,
        &Answering::asking(ask),
        progress,
    )
}

/// Fills what `new` needs before it creates anything.
pub fn resolve_for_new(
    paths: &PandoPaths,
    config: &Config,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    let shell = runtime_shell(paths.root());
    let machine = Machine::here(&shell);
    resolve_for_new_on(paths, config, ask, progress, &machine)
}

/// [`resolve_for_new`] with the machine injected.
pub(super) fn resolve_for_new_on(
    paths: &PandoPaths,
    config: &Config,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
    machine: &Machine<'_>,
) -> Result<Config> {
    let answering = Answering::asking(ask);
    let config = resolve_on(
        paths,
        config,
        &NEW_SLOTS,
        &[],
        &answering,
        progress,
        machine,
    )?;
    // `new` spawns one thing, the install step — and an install run under
    // the wrong runtime builds native modules for the wrong ABI, which
    // `start` only finds out about afterwards. So when there is an install
    // to run, the runtime question comes first; when there is none, `new`
    // spawns nothing and has no business probing the machine.
    let installs = config
        .project
        .install
        .as_deref()
        .is_some_and(|install| !install.trim().is_empty());
    if !installs {
        return Ok(config);
    }
    resolve_on(
        paths,
        &config,
        &[Slot::Prelude],
        &[],
        &answering,
        progress,
        machine,
    )
}

/// Detects, asks where it has to, and writes every answer to `pando.toml`.
///
/// Returns the config with the answers applied, so the caller does not have
/// to re-read the file it just wrote.
pub fn resolve(
    paths: &PandoPaths,
    config: &Config,
    slots: &[Slot],
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    resolve_silencing(paths, config, slots, &[], &Answering::asking(ask), progress)
}

/// [`resolve`], with slots that may be *taken* when the rules decided them
/// and must never be *asked* about.
///
/// One case so far: the services question is about isolation, and a start
/// that is not isolating has no business asking it — but a project whose
/// services the rules resolved outright should still get them written
/// down, because that is what shared mode reads to show whether the
/// global database is up.
pub fn resolve_silencing(
    paths: &PandoPaths,
    config: &Config,
    slots: &[Slot],
    silent: &[Slot],
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    let shell = runtime_shell(paths.root());
    let machine = Machine::here(&shell);
    resolve_on(paths, config, slots, silent, answers, progress, &machine)
}

/// [`resolve_silencing`] with the machine injected, so a test can answer
/// for a laptop it does not have.
pub fn resolve_on(
    paths: &PandoPaths,
    config: &Config,
    slots: &[Slot],
    silent: &[Slot],
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
    machine: &Machine<'_>,
) -> Result<Config> {
    resolve_pass(
        paths, config, slots, silent, answers, progress, machine, false,
    )
}

/// `init`'s pass: every slot, and one question no other command asks.
///
/// A project where nothing configures a process and no rule proposes one
/// has the dev command put to whoever is answering, with no options: a
/// command of their own on a terminal or in an answers file, exit 3
/// everywhere else. `init` is the wizard, and a setup that runs nothing is
/// not one it may call finished. `new` and `start` never ask it — a
/// question nobody can pick an option at is the one thing they do not
/// put to a developer who only wanted to start something.
pub(super) fn resolve_for_init(
    paths: &PandoPaths,
    config: &Config,
    slots: &[Slot],
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
) -> Result<Config> {
    let shell = runtime_shell(paths.root());
    let machine = Machine::here(&shell);
    resolve_pass(paths, config, slots, &[], answers, progress, &machine, true)
}

/// The prelude question, when this machine needs one: the report that
/// makes it answerable, and the lines tried on it. Refused outright when
/// a prelude is set and still does not work, since only the developer
/// can say what should replace it; nothing is spawned, which is the
/// point.
fn raise_prelude(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
) -> Result<(Vec<String>, Option<detect::Proposal>)> {
    match resolve_runtime(paths, config, machine)? {
        RuntimeOutcome::Broken(report) => bail!("{report}"),
        RuntimeOutcome::Ask {
            check,
            requirements,
            report,
        } => {
            let proposal = prelude_proposal(paths, config, &requirements, &check, machine)?;
            Ok((report, Some(proposal)))
        }
        RuntimeOutcome::Fine => Ok((Vec::new(), None)),
    }
}

/// One resolution pass. `needs_a_process` is [`resolve_for_init`]'s.
#[allow(clippy::too_many_arguments)]
fn resolve_pass(
    paths: &PandoPaths,
    config: &Config,
    slots: &[Slot],
    silent: &[Slot],
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
    machine: &Machine<'_>,
    needs_a_process: bool,
) -> Result<Config> {
    let ask = answers.ask;
    let mut config = config.clone();
    // First, and before the early return below: the commonest shape for a
    // developer changing an answer a program wrote is a project where
    // every slot is answered already, and nothing else in a run like that
    // looks at config twice.
    for slot in decisions::note_overrides(paths, &|slot| slot_value(&config, slot), progress) {
        progress(&format!(
            "{}: this is not what a program answered any more — recorded in {}",
            slot_label(slot),
            paths.decisions_file().display()
        ));
    }
    // Before the early return, and not inside the slot loop: a prelude
    // that is already set reads as an answered slot, and "the prelude is
    // set and still does not work" is exactly the case worth reporting.
    // It is also read-only unless it has something to say, so a project
    // that pins nothing pays one directory read for it.
    let (mut prelude_details, prelude_proposal) = match slots.contains(&Slot::Prelude) {
        true => raise_prelude(paths, &config, machine)?,
        false => (Vec::new(), None),
    };
    // Which version files the runtime was checked against. `init` answers
    // them earlier in this same pass, and a project whose only pin is in
    // an app directory — `backend/.nvmrc` — has nothing to check until
    // they are written.
    let checked_with = config.runtime.version_files.clone();
    // Of the config as loaded, so [`settled`]'s answer holds: nothing in
    // this run has changed it yet.
    if prelude_proposal.is_none()
        && slots
            .iter()
            .all(|slot| settled(*slot, &config) && !answers.replaces(*slot))
    {
        return Ok(config);
    }
    let signals = detect::signals(paths.root());
    // Only ever called for a compose file pando's own reader could not
    // follow — `extends:`, a top-level `include:`, a YAML alias — so a
    // plain project costs no process spawn. `config` prints; it creates
    // nothing.
    let program = services::docker_program(paths);
    let resolve = |file: &Path| -> Option<crate::compose::ComposeFile> {
        let dir = file.parent()?;
        services::Compose::new(
            &program,
            crate::compose::project_name(paths.project_id(), "detect"),
            vec![file.to_path_buf()],
            dir,
        )
        .config()
        .ok()
    };
    // Only when something is still unanswered about the services: the
    // probe is a login shell, and a project that has already said what it
    // runs is not asking this question.
    let recipes = crate::recipes::Recipes::load(&paths.recipes_dir());
    // …and only for a start that could act on the answer. A plain
    // `start` never asks which services to run private copies of — see
    // `SILENT_UNLESS_ISOLATED` — so paying for a login shell to find out
    // what this machine has would be a cost with no question behind it.
    let asking_about_services = (!already_answered(Slot::Services, &config)
        || answers.replaces(Slot::Services))
        && !silent.contains(&Slot::Services);
    let evidence = match asking_about_services {
        true => machine_evidence(paths, &recipes),
        false => detect::MachineEvidence::unknown(),
    };
    let mut proposals = detect::propose_with(
        paths.root(),
        &signals,
        Some(&resolve),
        &evidence,
        config.isolation.preferred(),
    );
    // The one proposal that is not tier 1: it took a probe to find, and it
    // is about this machine rather than this repository. It goes through
    // the same loop as every other slot from here on.
    proposals.extend(prelude_proposal);
    // Asked of the config as it was loaded: once detection has written
    // `[dev].cmd`, the file is indistinguishable from one a developer
    // wrote by hand, and a `[dev]` they wrote is an answer about its ports
    // too. The only thing that changes it mid-run is an answer to the
    // shape question itself.
    let mut may_fill_dev = detect::may_fill_dev(&config);
    // Answers that decide the shape of the dev process, held in memory
    // until the port question after them is settled too. Written one by
    // one, an interrupted pass — a start with no terminal that stops at
    // the port question — left `[dev].cmd` on disk, and a `[dev]` on disk
    // reads as a developer's own, which answers the port question for
    // good: no ports, no URL, and a start that "succeeds". Held back, the
    // interrupted pass writes nothing about the process, and the next one
    // asks both questions again.
    let defer_until_ports = slots.contains(&Slot::PortEnv);
    let mut deferred: Vec<Deferred> = Vec::new();

    for slot in slots {
        // Anything past the process-shape slots flushes what they held:
        // the port question has been asked, skipped or answered by now.
        if !matches!(slot, Slot::Processes | Slot::DevCmd | Slot::PortEnv) {
            flush(paths, &mut deferred, progress)?;
        }
        // The runtime again, against the version files an earlier slot of
        // this pass wrote: checked only against the ones loaded, `init
        // --yes` passed a machine that `check` then stopped on.
        if *slot == Slot::Prelude
            && config.runtime.version_files != checked_with
            && !proposals.iter().any(|p| p.slot == Slot::Prelude)
        {
            let (details, proposal) = raise_prelude(paths, &config, machine)?;
            prelude_details = details;
            proposals.extend(proposal);
        }
        // A replacement is a program saying the answer config has is
        // wrong, so the answer being there is no reason to skip it.
        let replacing = answers.replaces(*slot);
        // …and a lone `dev` process is one whose command and ports it may
        // replace, whoever wrote them. Any other shape has no single
        // process for either slot to be about.
        let fills_dev = may_fill_dev || (replacing && detect::fills_one_dev_process(&config));
        if matches!(slot, Slot::DevCmd | Slot::PortEnv) && !fills_dev {
            continue;
        }
        // Just in time, and once: a slot the developer has already filled
        // in, by hand or by answering before, is never asked about again —
        // and a config that declares its processes has answered every
        // question about them, including the ones detection could only
        // write into `[dev]`.
        //
        // Re-read from `config` on every pass, because an earlier slot in
        // this same run may have answered a later one: taking the
        // per-app form settles the dev command and its ports with it.
        if !replacing && (already_answered(*slot, &config) || !detect::still_needed(*slot, &config))
        {
            continue;
        }
        // The old answer is set aside in memory now, so the new one is
        // checked against a config without it. On disk it goes only
        // when the new one is written.
        if replacing {
            unanswer(*slot, &mut config);
        }
        // Declared out here so the question below can borrow it like any
        // proposal the rules made.
        let nothing_proposed;
        let proposal = match proposals.iter().find(|p| p.slot == *slot) {
            Some(proposal) => proposal,
            None => {
                // No rule had anything to say. There is nobody to ask — but
                // a program driving this pass may still have an answer for
                // it.
                if volunteer(paths, &mut config, *slot, &proposals, answers, progress)? {
                    if replacing {
                        answers.wrote_replacement(*slot);
                    }
                    if *slot == Slot::Processes {
                        may_fill_dev = detect::fills_one_dev_process(&config);
                    }
                    continue;
                }
                // …except where the pass is `init`'s and nothing would run:
                // then the dev command is a question with no options, and
                // not asking it is a green `init` followed by a `check`
                // that finds nothing to start.
                if !(needs_a_process
                    && *slot == Slot::DevCmd
                    && config.runnable_processes().next().is_none())
                {
                    continue;
                }
                nothing_proposed = detect::Proposal::of(*slot, Vec::new(), false);
                &nothing_proposed
            }
        };
        if !proposal.decided && silent.contains(slot) {
            continue;
        }
        // The one slot whose answer is about the machine. It is written
        // to a different file, and it is verified before it is written,
        // which is two reasons not to run it through the generic path.
        if *slot == Slot::Prelude {
            let pending =
                answer_prelude(paths, &mut config, proposal, &prelude_details, ask, machine)?;
            record_decision(paths, pending, progress);
            continue;
        }
        // What a program answered, if it was a program: filled where the
        // question is asked, written down where the answer lands on disk.
        let mut pending: Option<Pending> = None;
        // The one slot whose answer is a set. It never takes the
        // single-candidate path below, because "these three" is not one of
        // the options — it is a subset of them.
        if slot.is_multi() {
            let (chosen, note): (Vec<detect::Candidate>, config::Note) = if proposal.decided
                && answers.takes_decision(*slot)
            {
                let taken: Vec<detect::Candidate> =
                    proposal.preferred_set().into_iter().cloned().collect();
                // With nothing taken the reason is the proposal's own: a
                // rule settled the slot with the empty answer, and there is
                // no candidate left to read a `why` off.
                let why = taken
                    .first()
                    .map(|c| c.why.clone())
                    .or_else(|| proposal.none_because.clone())
                    .unwrap_or_default();
                if taken.is_empty() {
                    // Every guess is visible, the empty one included: this
                    // is pando saying it looked at the compose file and
                    // found nothing in it to run a private copy of.
                    progress(&format!(
                        "this project has no services to run private copies of (detected: {why})"
                    ));
                } else {
                    // Not "running private copies of": a start that is not
                    // isolating runs none, and this line is written on
                    // every start that fills the slot.
                    // Each service's own evidence, once: two found from two
                    // variables are two reasons, not the first one twice.
                    let mut whys: Vec<&str> = Vec::new();
                    for c in &taken {
                        if !c.why.is_empty() && !whys.contains(&c.why.as_str()) {
                            whys.push(&c.why);
                        }
                    }
                    let whys = whys.join(" · ");
                    progress(&format!(
                        "using {} as this project's services (detected: {whys})",
                        taken
                            .iter()
                            .map(|c| c.value.as_str())
                            .collect::<Vec<_>>()
                            .join(", "),
                    ));
                }
                (taken, config::Note::Detected(why))
            } else {
                let question = question_for(proposal, &[]).at(paths);
                let offered = question.options.len();
                let (answer, by) = answered_by(ask(&question)?);
                pending = pending_decision(&question, proposal, &answer, by);
                match answer {
                    Answer::Many(indexes) => (
                        indexes
                            .iter()
                            .map(|index| pick(proposal, *index))
                            .collect::<Result<Vec<_>>>()?,
                        by.note(config::Note::Answered),
                    ),
                    // `--yes`. Written down as what it is: a flag took the
                    // options the rules had resolved. A config that claims
                    // a human chose them is one nobody can review.
                    Answer::Auto(_) => {
                        let taken: Vec<detect::Candidate> =
                            proposal.preferred_set().into_iter().cloned().collect();
                        let note = config::Note::TookRuled {
                            taken: taken.len(),
                            offered,
                        };
                        (taken, note)
                    }
                    Answer::None => (Vec::new(), by.note(config::Note::Answered)),
                    _ => bail!(
                        "{} is answered with a set of the {offered} options",
                        slot_label(*slot)
                    ),
                }
            };
            apply_service_answer(paths, &mut config, proposal, &chosen, note, replacing)?;
            record_decision(paths, pending, progress);
            if replacing {
                answers.wrote_replacement(*slot);
            }
            continue;
        }
        let candidate = if proposal.decided && answers.takes_decision(*slot) {
            let candidate = proposal
                .preferred()
                .expect("a decided proposal has a candidate")
                .clone();
            // Every guess is visible: a one-line notice now, and a comment
            // in the file afterwards.
            progress(&format!(
                "using {:?} for {} (detected: {})",
                candidate.value,
                slot_label(*slot),
                candidate.why
            ));
            let why = candidate.why.clone();
            (candidate, config::Note::Detected(why))
        } else {
            let question = question_for(proposal, &[]).at(paths);
            let offered = question.options.len();
            let (answer, by) = answered_by(ask(&question)?);
            pending = pending_decision(&question, proposal, &answer, by);
            if let Some(pending) = pending.as_mut() {
                pending.replaces = replacing;
            }
            match answer {
                Answer::Choice(index) => {
                    let candidate = pick(proposal, index)?;
                    let why = candidate.why.clone();
                    (candidate, by.note(config::Note::Detected(why)))
                }
                Answer::Auto(index) => (pick(proposal, index)?, config::Note::TookFirst(offered)),
                Answer::Custom(value) => {
                    (typed(*slot, &value, by)?, by.note(config::Note::Answered))
                }
                Answer::Processes(tables) if *slot == Slot::Processes => (
                    process_tables(paths, &config, &proposals, tables, by)?,
                    by.note(config::Note::Answered),
                ),
                Answer::Processes(_) => bail!(
                    "{} is not answered with process tables — only the process list is",
                    slot_label(*slot)
                ),
                // Only the multi-select slot has a set for an answer, and
                // it never reaches here.
                Answer::Many(_) => bail!(
                    "{} takes one of its {offered} options, not several",
                    slot_label(*slot)
                ),
                // Written down as an empty list rather than left out:
                // "this process has no ports" and "no worktree needs a
                // local file of mine" both have to be tellable from
                // "nobody has said yet", or the question returns on every
                // run with nowhere to put the answer but the TOML by hand.
                Answer::None if matches!(*slot, Slot::PortEnv | Slot::Provision) => {
                    flush(paths, &mut deferred, progress)?;
                    write_empty_answer(
                        paths,
                        &mut config,
                        *slot,
                        by.note(config::Note::Answered),
                        pending,
                        progress,
                    )?;
                    if replacing {
                        answers.wrote_replacement(*slot);
                    }
                    continue;
                }
                // "No" to the schema step, recorded as the step pando found
                // switched off: the command stays visible, and flipping
                // `on` is the whole of changing one's mind.
                Answer::None if *slot == Slot::SchemaHook => {
                    let mut declined = proposal
                        .preferred()
                        .context("the schema question has an option to decline")?
                        .clone();
                    if let Some(hook) = declined.hook.as_mut() {
                        hook.on = Some(config::HookScope::Never);
                    }
                    (declined, by.note(config::Note::Answered))
                }
                Answer::None => bail!("{} has no \"none\" answer", slot_label(*slot)),
                Answer::Program(_) => unreachable!("answered_by peels every wrapper"),
            }
        };
        let (candidate, note) = candidate;
        if defer_until_ports && matches!(slot, Slot::Processes | Slot::DevCmd) {
            // Checked now, written later: the answer is refused while
            // nothing is on disk, exactly as `write_answer` would.
            let mut proposed = config.clone();
            detect::apply(*slot, &candidate, &mut proposed);
            refuse_unloadable(paths, *slot, &candidate.value, &proposed, &note)?;
            config = proposed;
            deferred.push(Deferred {
                slot: *slot,
                candidate,
                note,
                pending,
            });
        } else {
            flush(paths, &mut deferred, progress)?;
            write_answer(
                paths,
                &mut config,
                *slot,
                &candidate,
                note,
                pending,
                progress,
            )?;
        }
        if replacing {
            answers.wrote_replacement(*slot);
        }
        if *slot == Slot::Processes {
            // The answer decided the shape. The per-app form leaves the
            // single-process slots nothing to fill; the root-script form
            // leaves them the port.
            may_fill_dev = detect::fills_one_dev_process(&config);
        }
    }
    flush(paths, &mut deferred, progress)?;
    Ok(config)
}

/// A single-value answer chosen in this pass and not yet on disk.
struct Deferred {
    slot: Slot,
    candidate: detect::Candidate,
    note: config::Note,
    pending: Option<Pending>,
}

/// Writes every held answer, in the order it was given.
///
/// The in-memory config already carries them, so each is written to a
/// scratch copy; only the file and the decisions log change here.
fn flush(paths: &PandoPaths, deferred: &mut Vec<Deferred>, progress: &dyn Fn(&str)) -> Result<()> {
    for held in deferred.drain(..) {
        let mut scratch = config::load(paths)
            .map(|loaded| loaded.config)
            .unwrap_or_default();
        write_answer(
            paths,
            &mut scratch,
            held.slot,
            &held.candidate,
            held.note,
            held.pending,
            progress,
        )?;
    }
    Ok(())
}

/// Writes one single-value answer: checks it loads, patches its keys, and
/// records what a program decided.
///
/// One implementation, because there are two callers — the question loop
/// and [`volunteer`] — and an answer written twice in two ways is an
/// answer that means two things.
fn write_answer(
    paths: &PandoPaths,
    config: &mut Config,
    slot: Slot,
    candidate: &detect::Candidate,
    note: config::Note,
    pending: Option<Pending>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    if candidate.value.trim().is_empty() {
        bail!("an empty answer is not a {}", slot_label(slot));
    }
    if slot == Slot::Base {
        refuse_missing_base(paths, &candidate.value, pending.is_some())?;
    }
    let replaces = pending.as_ref().is_some_and(|pending| pending.replaces);
    // A config read back from disk still has the answer being replaced.
    if replaces {
        unanswer(slot, config);
    }
    // Before a single key is written: an answer that would make the
    // merged config refuse to load is refused now, while nothing is on
    // disk, instead of at the next load with half a project broken.
    let mut proposed = config.clone();
    detect::apply(slot, candidate, &mut proposed);
    refuse_unloadable(paths, slot, &candidate.value, &proposed, &note)?;
    // Checked, so the old answer can go: now, and only now.
    if replaces {
        forget_on_disk(paths, slot)?;
    }
    // A slot whose answer is a whole `[[table]]` entry: appended, with
    // the note on the entry's own header rather than on each key.
    if let Some((array, entries)) = detect::array_edits(slot, &[candidate]) {
        config::set_detected_array_entry(paths, slot.layer(), array, entries, note)?;
        detect::apply(slot, candidate, config);
        record_decision(paths, pending, progress);
        return Ok(());
    }
    let mut edits = detect::edits(slot, candidate);
    // A `ports` the developer wrote is an answer, and `apply` keeps it: a
    // command carrying `{port:web}` fills in `cmd` beside their map. The
    // edits are what every rule would write, so the same guard goes here,
    // or the file said `ports = ["web"]` from the next load on.
    if slot == Slot::DevCmd
        && config
            .processes
            .get(detect::DEV)
            .is_some_and(|process| process.ports.is_some())
    {
        edits.retain(|edit| edit.key != "ports");
    }
    // And for the same reason an `env` or a `ready` they wrote: a rule's
    // own written whole over theirs would lose every key of it.
    if slot == Slot::DevCmd
        && let Some(process) = config.processes.get(detect::DEV)
    {
        edits.retain(|edit| {
            !(edit.key == "env" && !process.env.is_empty()
                || edit.key == "ready" && process.ready.is_some())
        });
    }
    if slot == Slot::Processes {
        // A whole process table is one answer to one question, so the
        // note goes on the table's header rather than on each of its
        // five keys.
        for (table, entries) in group_by_table(edits) {
            let table: Vec<&str> = table.iter().map(String::as_str).collect();
            config::set_detected_table(paths, slot.layer(), &table, entries, note.clone())?;
        }
    } else {
        for edit in edits {
            let table: Vec<&str> = edit.table.iter().map(String::as_str).collect();
            config::set_detected(
                paths,
                slot.layer(),
                &table,
                &edit.key,
                edit.value,
                note.clone(),
            )?;
        }
    }
    detect::apply(slot, candidate, config);
    record_decision(paths, pending, progress);
    Ok(())
}

/// A base this repository has no branch for is not an answer: written, it
/// would have `new` refuse every branch it forks, and the check test
/// origin/HEAD without a word, which is what the answer was given to
/// stop. Looked up as `new` looks a base up, so a branch only on origin
/// counts. From a program it is a refused answer, exit 2 like any other
/// bad value it sent.
fn refuse_missing_base(paths: &PandoPaths, base: &str, by_program: bool) -> Result<()> {
    let root = paths.root();
    if ref_exists(root, &resolve_create_base(root, base)) {
        return Ok(());
    }
    let message = format!(
        "{base:?} cannot be this project's {}: this repository has no such branch, here or on \
         origin — nothing was written",
        slot_label(Slot::Base)
    );
    match by_program {
        true => Err(anyhow::Error::new(RefusedAnswer(message))),
        false => bail!(message),
    }
}

/// Writes "none of them" as the empty form of the slot's own value.
///
/// The same two callers, and the same reason: `ports = []` and
/// `provision = []` are answers, and an answer nothing records is asked
/// again on every run with nowhere to put it but the TOML by hand.
fn write_empty_answer(
    paths: &PandoPaths,
    config: &mut Config,
    slot: Slot,
    note: config::Note,
    pending: Option<Pending>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    // Only these two have an empty form: one list, under one key.
    let (Slot::PortEnv | Slot::Provision, Some((table, key))) = (slot, slot.key()) else {
        bail!("{} has no \"none\" answer to write", slot_label(slot));
    };
    // Through the same gate every other answer goes through. Nothing
    // `validate` knows about refuses an empty list today; the guarantee
    // is that no answer is written without being read back, and an
    // exception to it is one the next rule walks into.
    let mut proposed = config.clone();
    apply_empty(slot, &mut proposed);
    refuse_unloadable(paths, slot, "none of them", &proposed, &note)?;
    config::set_detected(
        paths,
        slot.layer(),
        table,
        key,
        toml_edit::Value::Array(toml_edit::Array::new()),
        note,
    )?;
    apply_empty(slot, config);
    record_decision(paths, pending, progress);
    Ok(())
}

/// Fills a slot no rule proposed anything for, from a program's own
/// answer.
///
/// "Every question has a custom answer" has to hold where pando had no
/// guess at all, or it quietly becomes "except the ones we had nothing to
/// offer for" — and those are the projects that need a caller's help
/// most. A workspace with no lockfile is the plain case: pando will not
/// propose a non-frozen install, so it proposes nothing, and the one who
/// knows what installs this project is whoever is driving.
///
/// It is not a question. Nothing is prompted and nothing exits 3: a
/// caller with an answer in hand supplies it, and a caller without one
/// leaves the slot exactly as silent as it was.
///
/// Two slots are deliberately out:
///
/// - **`services`** takes a set of the options, and with no proposal there
///   are no options. A service pando did not find is not one it can run.
/// - **`prelude`** is verified against this machine before it is written,
///   and that verification only exists behind the proposal that raised the
///   question. A prelude nothing checked, written machine-wide by a
///   program, can break every command pando spawns; an unused answer
///   cannot.
fn volunteer(
    paths: &PandoPaths,
    config: &mut Config,
    slot: Slot,
    proposals: &[detect::Proposal],
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
) -> Result<bool> {
    let Some(program) = answers.program else {
        return Ok(false);
    };
    if slot == Slot::Prelude || !slot.allows_custom() {
        return Ok(false);
    }
    // An empty proposal, so the question a program answers here is built
    // by the same function every other question is — and so the decisions
    // log records, honestly, that the rules offered nothing.
    let proposal = detect::Proposal::of(slot, Vec::new(), false);
    let question = question_for(&proposal, &[]).at(paths);
    let Some(answer) = program(&question) else {
        return Ok(false);
    };
    let (answer, by) = answered_by(answer?);
    let replacing = answers.replaces(slot);
    let mut pending = pending_decision(&question, &proposal, &answer, by);
    if let Some(pending) = pending.as_mut() {
        pending.replaces = replacing;
    }
    match answer {
        Answer::Custom(value) => {
            let candidate = typed(slot, &value, by)?;
            let note = by.note(config::Note::Answered);
            write_answer(paths, config, slot, &candidate, note, pending, progress)?;
        }
        Answer::Processes(tables) if slot == Slot::Processes => {
            let candidate = process_tables(paths, config, proposals, tables, by)?;
            let note = by.note(config::Note::Answered);
            write_answer(paths, config, slot, &candidate, note, pending, progress)?;
        }
        Answer::None if matches!(slot, Slot::PortEnv | Slot::Provision) => {
            write_empty_answer(
                paths,
                config,
                slot,
                by.note(config::Note::Answered),
                pending,
                progress,
            )?;
        }
        // "No", replacing a schema step config has: with no step the
        // rules found to write down as switched off, the answer is the
        // step taken away.
        Answer::None if slot == Slot::SchemaHook && replacing => {
            forget_on_disk(paths, slot)?;
            record_decision(paths, pending, progress);
            progress(&format!(
                "{}: the rules found no schema step, so the one config had was removed",
                slot_label(slot)
            ));
            return Ok(true);
        }
        // "No" to a schema step the rules never found is already true:
        // there is no command to write down as switched off, and an
        // answers file that says so has said nothing wrong.
        Answer::None if slot == Slot::SchemaHook => {
            progress(&format!(
                "{}: the rules found no schema step, so there is nothing to switch off — \
                 nothing was written",
                slot_label(slot)
            ));
            return Ok(false);
        }
        // Nothing was on offer, so there was nothing to choose: only a
        // typed answer, or the empty one where the slot has it.
        _ => bail!(
            "{} has no options here — the rules found nothing to offer, so only a command of \
             your own is an answer",
            slot_label(slot)
        ),
    }
    progress(&format!(
        "{}: nothing was proposed here, and the answer supplied was taken",
        slot_label(slot)
    ));
    Ok(true)
}

/// Writes the answer to the one multi-select slot: a `[[services]]` entry
/// listing the services chosen and the env keys that point at them.
///
/// An empty set is a real answer — "none of them" — and it is written down
/// as an entry with an empty `include`. An answer nothing records is asked
/// again on every start: a prompt the developer already declined, and exit
/// 3 for a script, with nowhere to put the answer but the TOML by hand.
/// `Slot::PortEnv` writes `ports = []` for exactly this reason.
///
/// `replaces` when the answer takes the place of one config has: the old
/// entries are removed once the new ones have been checked.
fn apply_service_answer(
    paths: &PandoPaths,
    config: &mut Config,
    proposal: &detect::Proposal,
    chosen: &[detect::Candidate],
    note: config::Note,
    replaces: bool,
) -> Result<()> {
    let refs: Vec<&detect::Candidate> = chosen.iter().collect();
    if proposal.mechanism == Some("native") {
        return apply_native_service_answer(paths, config, &refs, note, replaces);
    }
    // The compose file the question was about. Without one there is
    // nothing to write an entry for, and nothing was asked either.
    let Some(file) = proposal.service_file().map(str::to_string) else {
        return Ok(());
    };
    let mut proposed = config.clone();
    detect::apply_services(&file, &refs, &mut proposed);
    let names: Vec<&str> = refs.iter().map(|c| c.value.as_str()).collect();
    refuse_unloadable(paths, Slot::Services, &names.join(", "), &proposed, &note)?;
    if replaces {
        forget_on_disk(paths, Slot::Services)?;
    }
    let (array, entries) = detect::service_entry(&file, &refs);
    config::set_detected_array_entry(paths, Slot::Services.layer(), array, entries, note)?;
    detect::apply_services(&file, &refs, config);
    Ok(())
}

/// Writes the answer to the native half of the services question: one
/// `[[services]] kind = "native"` entry per service chosen.
///
/// The negative is the interesting half. A compose entry records "none of
/// them" as an empty `include`; a native entry is one service and has
/// nowhere to put it, so the answer goes in `[isolation] none` instead —
/// a project fact, in the project layer, beside the machine-wide
/// `prefer`. Without somewhere to record it the question would return on
/// every isolated start, with nowhere to answer it but the TOML by hand.
pub(super) fn apply_native_service_answer(
    paths: &PandoPaths,
    config: &mut Config,
    chosen: &[&detect::Candidate],
    note: config::Note,
    replaces: bool,
) -> Result<()> {
    if chosen.is_empty() {
        let mut proposed = config.clone();
        proposed.isolation.none = true;
        refuse_unloadable(paths, Slot::Services, "none of them", &proposed, &note)?;
        if replaces {
            forget_on_disk(paths, Slot::Services)?;
        }
        config::set_detected(
            paths,
            config::Layer::Project,
            &["isolation"],
            "none",
            toml_edit::Value::from(true),
            note,
        )?;
        config.isolation.none = true;
        return Ok(());
    }
    // Every entry checked against the loader before any of them is
    // written: two services that are legal apart and not together — a
    // name a process already owns as a role — must not leave the file
    // half answered.
    let mut proposed = config.clone();
    detect::apply_native_services(chosen, &mut proposed);
    let names: Vec<&str> = chosen.iter().map(|c| c.value.as_str()).collect();
    refuse_unloadable(paths, Slot::Services, &names.join(", "), &proposed, &note)?;
    if replaces {
        forget_on_disk(paths, Slot::Services)?;
    }
    for candidate in chosen {
        let (array, entries) = detect::native_entry(candidate);
        // One entry per service, so each cites its own evidence: a redis
        // found from `CACHE_URL` is not "detected: DATABASE_URL".
        let note = match &note {
            config::Note::Detected(_) if !candidate.why.is_empty() => {
                config::Note::Detected(candidate.why.clone())
            }
            other => other.clone(),
        };
        config::set_detected_array_entry(paths, Slot::Services.layer(), array, entries, note)?;
    }
    detect::apply_native_services(chosen, config);
    Ok(())
}

/// Sets aside what config says about a slot whose answer is being
/// replaced, for the slots whose answer [`detect::apply`] adds to rather
/// than writes over: the processes, the services, the schema step.
///
/// Only ever of a config whose value came from pando's own layer — `init`
/// refuses a replacement anything beneath it declares —
/// so what this leaves is what the file will say once
/// [`forget_on_disk`] has run.
fn unanswer(slot: Slot, config: &mut Config) {
    match slot {
        Slot::Processes => config.processes.clear(),
        Slot::Services => {
            config.services.clear();
            config.isolation.none = false;
        }
        Slot::SchemaHook => config.hooks.clear(),
        _ => {}
    }
}

/// [`unanswer`], in the file: the old answer's tables, removed from
/// pando's own layer once the answer taking their place has been checked.
fn forget_on_disk(paths: &PandoPaths, slot: Slot) -> Result<()> {
    config::remove_keys(paths, slot.layer(), slot.answer_tables())
}

/// "None of them", written as the empty form of the slot's own value.
///
/// The shape that makes a negative answer recordable: `ports = []` is a
/// process with no ports, `provision = []` is a worktree that needs no
/// local file of anyone's, and either is tellable from "nobody has said".
fn apply_empty(slot: Slot, config: &mut Config) {
    match slot {
        Slot::PortEnv => {
            config
                .processes
                .entry(detect::DEV.to_string())
                .or_default()
                .ports = Some(config::PortsSpec::List(Vec::new()));
        }
        Slot::Provision => config.project.provision = Some(Vec::new()),
        // Nothing else has an empty form; `write_empty_answer` refuses
        // every other slot before it gets here.
        _ => {}
    }
}

/// An answer pando could not load back is not an answer.
///
/// The one choke point every answer passes through between being chosen
/// and being written — every slot, and every front end: a prompt, `--yes`,
/// an answers file, the TUI modal. It applies the answer to a *copy* and
/// asks pando's own loader whether the result is legal; a copy that is not
/// never reaches disk.
///
/// This exists because two answers could each be reasonable and the pair
/// illegal. An env example naming `API_PORT` gives the application a role
/// called `api`; a compose file with a service called `api` gives it that
/// name too — and a role is one port and belongs to one thing, so
/// `validate_services` refuses the pair. Written, it refused at the *next*
/// load, in the project layer, which fails hard: `start`, `new`, `restart`
/// and the TUI all stopped until a human edited the file pando had just
/// written.
///
/// Generic rather than a check for that one pair, deliberately. Every
/// cross-slot rule `validate` knows about is covered, present and future,
/// and the message a developer sees is the loader's own — the same
/// sentence they would have read later, minus the broken file.
///
/// A program's answer refused here is the program's input that is wrong,
/// so it gets the usage-error shape: `note` says whose answer it was.
fn refuse_unloadable(
    paths: &PandoPaths,
    slot: Slot,
    answer: &str,
    proposed: &Config,
    note: &config::Note,
) -> Result<()> {
    let Err(e) = config::validate(proposed, &paths.project) else {
        return Ok(());
    };
    let by = match note {
        config::Note::Program => Answerer::Program,
        _ => Answerer::Human,
    };
    Err(by.refuse(format!(
        "{answer:?} cannot be this project's {}: {e:#} — nothing was written",
        slot_label(slot)
    )))
}

/// Process tables a program wrote, as the candidate they become — held
/// first to what the rules' own per-app option is true of by
/// construction.
///
/// The loader's rules — names, roles, `ready.role`, a `cwd` inside the
/// worktree — are [`refuse_unloadable`]'s, which every answer passes on
/// its way to disk. What is left is what only a table somebody wrote can
/// get wrong: a `cwd` that is not a directory of this repository, and a
/// `{…}` in a command or an environment value that names nothing a
/// worktree will have. Either one is a start that fails after the install
/// has run, for a reason the answer could have been told.
fn process_tables(
    paths: &PandoPaths,
    config: &Config,
    proposals: &[detect::Proposal],
    tables: std::collections::BTreeMap<String, config::ProcessConfig>,
    by: Answerer,
) -> Result<detect::Candidate> {
    let refuse = |message: String| by.refuse(format!("{message} — nothing was written"));
    for (name, process) in &tables {
        // One that leaves the worktree is the loader's to refuse, in its
        // own words.
        if let Some(cwd) = process.cwd.as_deref()
            && Path::new(cwd).components().all(|c| {
                matches!(
                    c,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            })
            && !paths.root().join(cwd).is_dir()
        {
            return Err(refuse(format!(
                "processes.{name}.cwd is {cwd:?}, which is not a directory of this repository"
            )));
        }
    }
    // Every role a placeholder may name: the answer's own processes', a
    // service config already has, and a service the rules offer. The
    // services are asked after the processes, so a process told where
    // its database is names a role no config holds yet.
    let offered = proposals
        .iter()
        .filter(|p| p.slot == Slot::Services)
        .flat_map(|p| p.candidates.iter().map(|c| c.value.clone()));
    let mut roles = std::collections::BTreeMap::new();
    for role in tables
        .values()
        .flat_map(config::ProcessConfig::roles)
        .chain(service_roles(config))
        .chain(offered)
    {
        // Only there so a template can render: which number is not the
        // question, whether there is one is.
        roles.entry(role).or_insert(crate::ports::PORT_MIN);
    }
    for (name, process) in &tables {
        let own = process.roles();
        let ctx = crate::template::Context {
            name: "a-worktree",
            branch: Some("a-branch"),
            worktree: Path::new("/worktree"),
            root: Path::new("/root"),
            project: "project",
            ports: &roles,
            default_role: own.first().map(String::as_str),
            log: Some(Path::new("/log")),
        };
        let texts = std::iter::once(("cmd".to_string(), &process.cmd)).chain(
            process
                .env
                .iter()
                .map(|(key, value)| (format!("env.{key}"), value)),
        );
        for (what, text) in texts {
            if let Err(e) = crate::template::render(text, &ctx) {
                return Err(refuse(format!(
                    "processes.{name}.{what} cannot be resolved: {e:#} — a role is one a \
                     process owns in its `ports`, or a service's name"
                )));
            }
        }
    }
    // What the question and the refusals show, in the form the rules' own
    // per-app option is shown.
    let value = tables
        .iter()
        .map(|(name, process)| match &process.cwd {
            Some(cwd) => format!("{name}: {} in {cwd}", process.cmd),
            None => format!("{name}: {}", process.cmd),
        })
        .collect::<Vec<_>>()
        .join("; ");
    Ok(detect::Candidate {
        value,
        processes: Some(tables),
        ..detect::Candidate::default()
    })
}

/// Edits gathered per table, keeping the order they were produced in.
///
/// The keys of one table are written together so the note explaining them
/// can sit on the table rather than on every key.
type TableEdits = Vec<(Vec<String>, Vec<(String, toml_edit::Value)>)>;

fn group_by_table(edits: Vec<detect::Edit>) -> TableEdits {
    let mut out: TableEdits = Vec::new();
    for edit in edits {
        match out.iter_mut().find(|(table, _)| *table == edit.table) {
            Some((_, entries)) => entries.push((edit.key, edit.value)),
            None => out.push((edit.table, vec![(edit.key, edit.value)])),
        }
    }
    out
}

/// The candidate an answer chose, by index.
pub(super) fn pick(proposal: &detect::Proposal, index: usize) -> Result<detect::Candidate> {
    proposal
        .candidates
        .get(index)
        .with_context(|| format!("option {index} is not on offer"))
        .cloned()
}

/// The question a proposal becomes: the options, which one a flag may
/// take, and which shapes are answers here.
///
/// Public because `signals` publishes exactly this — an agent reading it
/// is looking at the question it would be asked, not at a second
/// description of one.
pub fn question_for(proposal: &detect::Proposal, details: &[String]) -> Question {
    Question {
        slot: proposal.slot,
        prompt: proposal.slot.prompt().to_string(),
        options: proposal
            .candidates
            .iter()
            .map(|c| (c.value.clone(), c.why.clone()))
            .collect(),
        // The first option a flag may take on a developer's behalf, which
        // is the first one for every slot but provisioning: `--yes` takes
        // the preselection, and an option that copies a file pando did not
        // write, or a process list that leaves an app unstarted, is not
        // one it may accept unattended. `None` means there is
        // nothing here `--yes` can take, and the question is printed
        // instead — "agents never hang" is about failing loudly, not about
        // accepting anything rather than stopping.
        preselect: proposal.candidates.iter().position(|c| !c.needs_a_human),
        // Both off the slot itself, so an answers file can check the
        // shape of what a program sent the moment the file is read —
        // before a question exists to check it against.
        allow_custom: proposal.slot.allows_custom(),
        allow_none: proposal.slot.allows_none(),
        multi: proposal.slot.is_multi(),
        checked: proposal.preselected(),
        details: details.to_vec(),
        answer_file: None,
        snippet: detect::snippet(proposal.slot, &proposal.preferred_set()),
    }
}

pub(super) fn slot_label(slot: Slot) -> &'static str {
    match slot {
        Slot::Install => "install command",
        Slot::VersionFiles => "runtime version file",
        Slot::Prelude => "runtime prelude",
        Slot::Processes => "process list",
        Slot::DevCmd => "dev command",
        Slot::PortEnv => "port variable",
        Slot::Services => "service list",
        Slot::SchemaHook => "schema command",
        Slot::Provision => "provision list",
        Slot::Base => "base branch",
        Slot::Login => "namespace login",
        Slot::FreeSlot => "slot to free",
    }
}

/// Whether nothing about this slot is left to ask, of the config as it
/// was loaded.
///
/// Public because it is the one answer to "is there still a question
/// here": `init` reports it, `signals` publishes it, an answers file's
/// unused values are explained by it, and a second implementation of it
/// would be a second opinion.
///
/// [`already_answered`], plus the one thing that function cannot say: a
/// config whose processes are not the lone `[dev]` detection may fill has
/// answered the dev command and its ports by declaring them, which is why
/// the resolver never asks either. Not for use mid-run — once a deferred
/// answer puts `[dev].cmd` in memory, the same test would skip the port
/// question that answer is waiting on.
pub fn settled(slot: Slot, config: &Config) -> bool {
    already_answered(slot, config)
        || (matches!(slot, Slot::DevCmd | Slot::PortEnv) && !detect::may_fill_dev(config))
}

/// Whether config already says what this slot needs, from any layer.
///
/// Re-read by the resolver on every slot, of a config earlier answers in
/// the same run have changed; what a reader outside the run wants is
/// [`settled`].
pub(super) fn already_answered(slot: Slot, config: &Config) -> bool {
    match slot {
        Slot::Install => config.project.install.is_some(),
        Slot::VersionFiles => !config.runtime.version_files.is_empty(),
        // Set to anything at all, the empty string included: `prelude =
        // ""` is "this machine needs nothing", and a developer who has
        // said that is not asked again.
        Slot::Prelude => config.runtime.prelude.is_some(),
        // Unset, not empty: `provision = []` is a developer saying no
        // worktree needs a local file of theirs, and an answer nothing
        // records is asked again on every `new`.
        Slot::Provision => config.project.provision.is_some(),
        Slot::Base => config.project.base.is_some(),
        // `[isolation] none` included: it is how the native half records
        // "none of them", and a reader that missed it published the slot
        // open for good and probed the machine on every isolated start.
        Slot::Services => !detect::still_needed(slot, config),
        Slot::SchemaHook => !config.hooks.is_empty(),
        // Both of these now live in one place, because they are the same
        // question asked twice: has anything already said what this
        // project's processes are?
        Slot::Processes | Slot::DevCmd | Slot::PortEnv => !detect::still_needed(slot, config),
        // Per service, so "any" is the most this can say; the question
        // itself asks about one service and checks that one.
        Slot::Login => config.namespaced.values().any(|login| login.has_login()),
        // Asked of a server, never of config.
        Slot::FreeSlot => false,
    }
}
