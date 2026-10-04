//! `init`: every slot resolved in one pass, and the machine evidence
//! detection reads.

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{self, Config};
use crate::detect::{self, Slot};
use crate::paths::PandoPaths;
use crate::process as proc;

use super::questions::{Answering, RefusedAnswer, resolve_for_init, settled, slot_label};
use super::services::service_roles;
// Only for the intra-doc links above `ALL_SLOTS` and `init_dry_run`.
#[cfg(doc)]
use super::questions::resolve;

/// Every slot `init` fills: everything `new` needs and everything `start`
/// needs, in the order the config file reads.
///
/// Deliberately the same slots, asked through the same [`resolve`] those
/// two commands call. `init` is the batch form of the just-in-time
/// questions, not a second set of them: one implementation of each
/// question, or the two drift and a developer gets a different config
/// depending on which command reached the slot first.
pub const ALL_SLOTS: [Slot; 10] = [
    Slot::Install,
    Slot::VersionFiles,
    Slot::Prelude,
    // Before the slots that fill a single process, exactly as
    // `START_SLOTS` orders them: this one decides whether there is one.
    Slot::Processes,
    Slot::DevCmd,
    Slot::PortEnv,
    // Asked here, unlike on a plain `start`, which silences it: a start
    // that is not isolating has no business asking about a mode it is not
    // in, and `init` is the pass where every question is on the table.
    Slot::Services,
    Slot::SchemaHook,
    Slot::Provision,
    // Last: which commit the rest is tested on. A question only where
    // origin/HEAD is far behind the main checkout, and asked here and by
    // `check`, which would otherwise test the commit it doubts: with no
    // base named, `new` forks from origin/HEAD as it always has.
    Slot::Base,
];

/// Whether this project would run nothing: no process that config
/// configures, and no rule with a process to offer at `processes` or
/// `dev_cmd`.
///
/// The state `init` asks the dev command in with no options, and the one
/// `init --agent` lists that question open for — so the job and the run
/// agree about whether there is a question.
pub fn runs_nothing(config: &Config, proposals: &[detect::Proposal]) -> bool {
    config.runnable_processes().next().is_none()
        && !proposals
            .iter()
            .any(|p| matches!(p.slot, Slot::Processes | Slot::DevCmd) && !p.candidates.is_empty())
}

/// One slot, after `init` has been through it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotSummary {
    pub slot: Slot,
    /// The slot in a sentence: "install command", "dev command".
    pub label: &'static str,
    /// What config says now, short enough for one line. `None` when
    /// nothing says anything: a slot no rule found a candidate for and
    /// nobody answered.
    pub value: Option<String>,
    /// Whether this run is what answered it.
    pub answered_now: bool,
}

/// What `init` did, and what config says afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitReport {
    /// The project layer: the file every answer about this project is in.
    pub config_file: PathBuf,
    /// The machine-wide file, named only when this run put an answer
    /// there — the prelude is the one slot that belongs to the laptop.
    pub user_file: Option<PathBuf>,
    /// One entry per slot, in the order they were asked.
    pub slots: Vec<SlotSummary>,
    /// What loading the written config back had to say.
    pub warnings: Vec<String>,
}

impl InitReport {
    /// Whether this run answered anything at all. A second `init` answers
    /// nothing, which is the point of writing the first one down.
    pub fn answered_anything(&self) -> bool {
        self.slots.iter().any(|s| s.answered_now)
    }
}

/// Asks every unanswered question in one pass, writes every answer, and
/// reports what config holds afterwards.
///
/// Starts nothing: it is [`resolve`] over [`ALL_SLOTS`] and a summary. Like
/// every other answer path it writes inside pando's home and nowhere else.
pub fn init(
    paths: &PandoPaths,
    config: &Config,
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
) -> Result<InitReport> {
    refuse_unreplaceable(paths, config, answers)?;
    init_slots(paths, config, &ALL_SLOTS, answers, progress)
}

/// What `--replace` may not change, refused before anything is written.
///
/// The prelude, once it has an answer: it is about this machine, and only
/// a person changes it. Unanswered, a program's answer to it is a first
/// answer like any other, verified before it is written.
///
/// And a slot whose answer is whole tables that a file beneath pando's own
/// declares — the `pando.toml` a team committed, or the machine-wide
/// config. pando's own layer is written over the others, and an array of
/// tables there hides theirs entirely while a process table merges with
/// theirs: either way the result would not be the answer given. pando
/// never writes those files, so the answer there is a person's to change.
fn refuse_unreplaceable(
    paths: &PandoPaths,
    config: &Config,
    answers: &Answering<'_>,
) -> Result<()> {
    for slot in answers.named_for_replacing() {
        if slot.layer() == config::Layer::User {
            if settled(*slot, config) {
                let file =
                    config::prelude_origin(paths).unwrap_or_else(|| paths.user_config_file());
                return Err(anyhow::Error::new(RefusedAnswer(format!(
                    "the {} is already answered, and --replace never changes it: it is about \
                     this machine, and only a person changes it, in {} — leave it out of the \
                     answers file",
                    slot_label(*slot),
                    file.display()
                ))));
            }
            continue;
        }
        for table in slot.answer_tables() {
            if let Some(file) = config::declared_below(paths, table) {
                return Err(anyhow::Error::new(RefusedAnswer(format!(
                    "the {} comes from {}, which pando never writes, and --replace changes only \
                     pando's own config, {} — nothing was written",
                    slot_label(*slot),
                    file.display(),
                    paths.config_file().display()
                ))));
            }
        }
    }
    Ok(())
}

/// [`init`] over some of the slots: the dry run leaves out the ones
/// nobody could answer.
fn init_slots(
    paths: &PandoPaths,
    config: &Config,
    slots: &[Slot],
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
) -> Result<InitReport> {
    let before: Vec<bool> = ALL_SLOTS
        .iter()
        .map(|slot| settled(*slot, config))
        .collect();
    resolve_for_init(paths, config, slots, answers, progress)?;
    // Read back from disk rather than reported from memory. The summary is
    // then a statement about the file that exists, and a file pando cannot
    // read again is a failure worth having at the end of `init` rather
    // than at the start of whatever the developer runs next.
    let loaded = config::load(paths)?;
    Ok(init_report(paths, &loaded, &before, answers))
}

/// [`init`], against a copy of the files it would write.
///
/// Returns the report and the files as they would be, each with the path
/// it would really land at. The same pass, the same resolver and the same
/// renderer: a preview that re-implemented the writing would be a preview
/// of something else. The one difference is a namespace login's password,
/// which reads as `(hidden)`: the files are printed, and a login is never
/// a line of output.
///
/// The copies live in a scratch directory under pando's **own home**,
/// which is one of the three places Invariant 1 names — a preview is not
/// a reason to add a fourth — and it is removed before this returns.
pub fn init_dry_run(
    paths: &PandoPaths,
    config: &Config,
    answers: &Answering<'_>,
    progress: &dyn Fn(&str),
) -> Result<(InitReport, Vec<(PathBuf, String)>)> {
    // Of the real files: the copies are of pando's own two, and the
    // committed one is not copied at all.
    refuse_unreplaceable(paths, config, answers)?;
    let scratch = Scratch::new(paths)?;
    let previewed = PandoPaths::new(scratch.dir.clone(), paths.project.clone());
    // What pando has already written for this project and this machine, so
    // the preview edits the real file rather than one that starts empty:
    // the answer it adds lands among the developer's own keys and
    // comments, exactly where it would.
    let files = [
        (paths.config_file(), previewed.config_file()),
        (paths.user_config_file(), previewed.user_config_file()),
    ];
    // A question nobody here can answer — no terminal, no `--yes` that
    // may take it — is not a reason for a preview to fail. It is part of
    // the answer to "what would you write": that slot, unanswered. So the
    // pass is run again without it, and the preview says so.
    let mut slots: Vec<Slot> = ALL_SLOTS.to_vec();
    let mut open: Vec<super::questions::Question> = Vec::new();
    let mut waiting = false;
    // A pass run again says what the first one said; once is enough.
    let said = std::cell::RefCell::new(std::collections::HashSet::<String>::new());
    let progress = |line: &str| {
        if said.borrow_mut().insert(line.to_string()) {
            progress(line);
        }
    };
    let progress: &dyn Fn(&str) = &progress;
    let mut report = loop {
        // Every pass from the files as they really are. The pass before
        // this one wrote into the copies, and `config` does not know it:
        // run again over them, it wrote every answer a second time, and a
        // `[[services]]` or `[[hooks]]` entry is appended, not replaced.
        seed_scratch(&files, &previewed)?;
        match init_slots(&previewed, config, &slots, answers, progress) {
            Ok(report) => break report,
            Err(e) => {
                let Some(needs) = e.downcast_ref::<super::questions::NeedsAnswer>() else {
                    return Err(e);
                };
                let slot = needs.question.slot;
                if !slots.contains(&slot) {
                    return Err(e);
                }
                slots.retain(|s| *s != slot);
                open.push(needs.question.clone());
                // The single-process slots wait on the shape: filling the
                // `[dev]` fallback here would preview an answer nobody
                // gave.
                if slot == Slot::Processes {
                    slots.retain(|s| !matches!(s, Slot::DevCmd | Slot::PortEnv));
                    waiting = true;
                }
            }
        }
    };
    for question in &open {
        if let Some(summary) = report.slots.iter_mut().find(|s| s.slot == question.slot) {
            summary.value = Some(format!("(unanswered) {}", question.prompt));
            summary.answered_now = false;
        }
    }
    if waiting {
        for summary in report
            .slots
            .iter_mut()
            .filter(|s| matches!(s.slot, Slot::DevCmd | Slot::PortEnv) && s.value.is_none())
        {
            summary.value = Some("(unanswered) waits on the process list".to_string());
        }
    }
    // Only the files this pass would *change*. A machine-wide config the
    // run never touched is not part of the answer to "what would you
    // write", and printing it back is noise in front of the thing that is.
    let rendered: Vec<(PathBuf, String)> = files
        .iter()
        .filter_map(|(real, preview)| {
            let body = std::fs::read_to_string(preview).ok()?;
            let before = std::fs::read_to_string(real).ok();
            (before.as_deref() != Some(body.as_str()))
                .then(|| (real.clone(), config::hide_passwords(&body)))
        })
        .collect();
    Ok((
        InitReport {
            // Named as the files it would really write, not the copies it
            // wrote instead.
            config_file: paths.config_file(),
            user_file: report.user_file.map(|_| paths.user_config_file()),
            ..report
        },
        rendered,
    ))
}

/// Puts the scratch copies back to what the real files say: each one
/// copied over, or removed where the real one does not exist, and the
/// decisions log a pass recorded into removed.
pub(super) fn seed_scratch(files: &[(PathBuf, PathBuf)], previewed: &PandoPaths) -> Result<()> {
    for (from, to) in files {
        let Ok(text) = std::fs::read_to_string(from) else {
            remove_if_there(to)?;
            continue;
        };
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(to, text).with_context(|| format!("write {}", to.display()))?;
    }
    remove_if_there(&previewed.decisions_file())
}

fn remove_if_there(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("remove {}", path.display()))
        }
        _ => Ok(()),
    }
}

/// A throwaway pando home, removed when it goes out of scope — including
/// when the pass it was made for failed partway through.
///
/// It sees what the pass reads from the real home and never writes there:
/// the developer's own recipes, and the shims in its `bin` that the
/// machine probe puts first on PATH. Without them the preview proposed
/// services from the built-in recipes alone, which is not what the real
/// run proposes. They are links, not copies, and removing the scratch
/// removes the links and leaves what they point at.
pub(super) struct Scratch {
    pub(super) dir: PathBuf,
    /// The directories this preview had to create to hold the scratch
    /// copy, innermost first, so they go with it. A dry run promises to
    /// write nothing, and an empty project directory left in the home is
    /// a write all the same.
    created: Vec<PathBuf>,
}

impl Scratch {
    pub(super) fn new(paths: &PandoPaths) -> Result<Scratch> {
        let project_dir = paths.project_dir();
        let created: Vec<PathBuf> = [
            Some(project_dir.clone()),
            project_dir.parent().map(Path::to_path_buf),
            Some(paths.home.clone()),
        ]
        .into_iter()
        .flatten()
        .filter(|dir| !dir.exists())
        .collect();
        // Through the one function that makes the home 0700 and refuses it
        // inside the repository, rather than beside it with a `create_dir`
        // that knows neither rule.
        paths.ensure_home()?;
        let dir = paths.home.join("preview").join(format!(
            "{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        // Made before the links, so a link that fails still has the
        // scratch removed behind it.
        let scratch = Scratch { dir, created };
        for real in [paths.recipes_dir(), paths.home.join("bin")] {
            let (Ok(relative), Ok(target)) = (real.strip_prefix(&paths.home), real.canonicalize())
            else {
                continue;
            };
            if !target.is_dir() {
                continue;
            }
            let link = scratch.dir.join(relative);
            std::os::unix::fs::symlink(&target, &link)
                .with_context(|| format!("link {} → {}", link.display(), target.display()))?;
        }
        Ok(scratch)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
        // And the directory they all share, once the last one has gone:
        // `remove_dir` only succeeds on an empty one, so a preview running
        // beside this one keeps it.
        if let Some(parent) = self.dir.parent() {
            let _ = std::fs::remove_dir(parent);
        }
        // Then whatever the home did not have before this preview. Only
        // ever an empty directory: a concurrent real run that wrote into
        // one of them keeps it.
        for dir in &self.created {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

fn init_report(
    paths: &PandoPaths,
    loaded: &config::Loaded,
    before: &[bool],
    answers: &Answering<'_>,
) -> InitReport {
    let slots: Vec<SlotSummary> = ALL_SLOTS
        .iter()
        .enumerate()
        .map(|(i, slot)| SlotSummary {
            slot: *slot,
            label: slot_label(*slot),
            value: slot_value(&loaded.config, *slot),
            // A replaced slot was settled before the run too, and this run
            // is what answered it all the same.
            answered_now: (!before[i] && settled(*slot, &loaded.config)) || answers.replaced(*slot),
        })
        .collect();
    let user_file = slots
        .iter()
        .any(|s| s.answered_now && s.slot.layer() == config::Layer::User)
        .then(|| paths.user_config_file());
    InitReport {
        config_file: paths.config_file(),
        user_file,
        slots,
        warnings: loaded.warnings.clone(),
    }
}

/// What config says about one slot, in the fewest words that are still
/// true. A line for a human to read; nothing parses it back.
///
/// Public because `init --agent` says what is already set in the same
/// words `init` reports it with.
pub fn slot_value(config: &Config, slot: Slot) -> Option<String> {
    match slot {
        Slot::Install => config.project.install.clone(),
        Slot::VersionFiles => (!config.runtime.version_files.is_empty())
            .then(|| config.runtime.version_files.join(", ")),
        // The empty prelude is an answer — "this machine needs nothing" —
        // and an empty cell would read as no answer at all.
        Slot::Prelude => config
            .runtime
            .prelude
            .as_ref()
            .map(|line| match line.trim().is_empty() {
                true => "nothing in front of this project's commands".to_string(),
                false => line.clone(),
            }),
        Slot::Processes => (!config.processes.is_empty()).then(|| {
            config
                .processes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        }),
        Slot::DevCmd => config
            .processes
            .get(detect::DEV)
            .map(|process| process.cmd.clone())
            .filter(|cmd| !cmd.trim().is_empty()),
        Slot::PortEnv => config
            .processes
            .get(detect::DEV)
            .and_then(|process| process.ports.as_ref())
            .map(ports_summary),
        Slot::Services => services_summary(config),
        // The command, with the name of the hook that runs it: the label
        // says "schema command", and a bare `migrate` is the tab it logs
        // to rather than the thing it does.
        Slot::SchemaHook => (!config.hooks.is_empty()).then(|| {
            config
                .hooks
                .iter()
                .map(|hook| match hook.scope(!service_roles(config).is_empty()) {
                    config::HookScope::Always => format!("{}: {}", hook.name, hook.cmd),
                    scope => format!("{}: {} (on {})", hook.name, hook.cmd, scope.as_str()),
                })
                .collect::<Vec<_>>()
                .join("; ")
        }),
        // `[]` is an answer here too: no worktree needs a local file of
        // this developer's.
        Slot::Provision => config
            .project
            .provision
            .as_ref()
            .map(|paths| match paths.is_empty() {
                true => "none".to_string(),
                false => paths.join(", "),
            }),
        Slot::Base => config.project.base.clone(),
        // Nothing config holds.
        Slot::FreeSlot => None,
        // Which services have one, and never what it is.
        Slot::Login => {
            let services: Vec<&str> = config
                .namespaced
                .iter()
                .filter(|(_, login)| login.has_login())
                .map(|(service, _)| service.as_str())
                .collect();
            (!services.is_empty()).then(|| format!("a login for {}", services.join(", ")))
        }
    }
}

fn ports_summary(ports: &config::PortsSpec) -> String {
    match ports {
        config::PortsSpec::Map(map) if !map.is_empty() => map
            .iter()
            .map(|(var, role)| format!("{var} = {role}"))
            .collect::<Vec<_>>()
            .join(", "),
        config::PortsSpec::List(roles) if !roles.is_empty() => roles.join(", "),
        // Both empty forms mean the same thing, and it is an answer.
        _ => "no ports".to_string(),
    }
}

/// The services a worktree would run private copies of. `none` when an
/// entry exists and includes nothing, or when `[isolation] none` says so,
/// which are the two written-down answers "none of them"; `None` when
/// nothing says anything at all.
fn services_summary(config: &Config) -> Option<String> {
    if config.services.is_empty() {
        return config.isolation.none.then(|| "none".to_string());
    }
    let mut names: Vec<String> = Vec::new();
    for service in &config.services {
        match service {
            config::ServiceConfig::Compose { include, .. } => names.extend(include.iter().cloned()),
            config::ServiceConfig::Native { name, .. } => names.push(name.clone()),
        }
    }
    Some(match names.is_empty() {
        true => "none".to_string(),
        false => names.join(", "),
    })
}

/// What this machine can run, probed through the shell a real spawn uses.
///
/// One `bash -lc` for every engine and for docker at once, and only when
/// the services slot is still unanswered — a project that has already
/// said what it runs is not asking this question, and a start is not the
/// place to pay for an answer nobody wanted.
///
/// A probe that cannot run leaves the evidence `unknown`, which is not
/// the same as "nothing is installed": a decision made from a failed
/// probe would be a guess wearing evidence's clothes.
pub fn machine_evidence(
    paths: &PandoPaths,
    recipes: &crate::recipes::Recipes,
) -> detect::MachineEvidence {
    let (script, names) = machine_evidence_script(paths, recipes);
    let root = paths.root();
    let Ok(captured) = proc::run_captured(&script, root, &[], EVIDENCE_TIMEOUT) else {
        return detect::MachineEvidence::unknown();
    };
    machine_evidence_from(&captured.stdout, &names)
}

/// The one script both probes run, and the recipe names it answers for.
///
/// Shared so that what `doctor` reports and what the start path decides
/// from cannot drift: `doctor` asks the shell it was given, the start
/// path asks the shell a real spawn uses, and both read the same lines.
pub fn machine_evidence_script(
    paths: &PandoPaths,
    recipes: &crate::recipes::Recipes,
) -> (String, Vec<String>) {
    use std::fmt::Write as _;
    let mut script = String::new();
    // The same PATH a recipe command gets, and for the same reason: a
    // shim in pando's own `bin` is what the adapter will run, so it is
    // what the evidence has to be about. Without this the report and the
    // start path could disagree about whether an engine is there at all.
    let _ = writeln!(
        script,
        "export PATH={}:\"$PATH\"",
        proc::shell_quote(&paths.home.join("bin").display().to_string())
    );
    let _ = writeln!(script, "command -v docker >/dev/null 2>&1 && echo docker");
    let mut names: Vec<String> = Vec::new();
    for (name, loaded) in recipes.entries() {
        let Some(_) = loaded.recipe.service() else {
            continue;
        };
        if loaded.recipe.binaries.is_empty() {
            continue;
        }
        let checks: Vec<String> = loaded
            .recipe
            .binaries
            .iter()
            .map(|b| format!("command -v {} >/dev/null 2>&1", proc::shell_quote(b)))
            .collect();
        let _ = writeln!(script, "{} && echo {name}", checks.join(" && "));
        names.push(name.to_string());
    }
    (script, names)
}

/// What the probe's output means.
pub fn machine_evidence_from(stdout: &str, names: &[String]) -> detect::MachineEvidence {
    let found: Vec<&str> = stdout.lines().map(str::trim).collect();
    detect::MachineEvidence {
        probed: true,
        docker: found.contains(&"docker"),
        engines: names
            .iter()
            .map(|name| (name.clone(), found.contains(&name.as_str())))
            .collect(),
    }
}

/// How long the machine probe gets. It is one login shell asking
/// `command -v` a dozen times, which is fast; the budget is for a shell
/// whose profile is slow, not for the lookups.
const EVIDENCE_TIMEOUT: Duration = Duration::from_secs(20);
