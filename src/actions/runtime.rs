//! The runtime the project asks for.

use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{self, Config};
use crate::detect::{self, Slot};
use crate::paths::PandoPaths;
use crate::process as proc;

use super::questions::{
    Answer, Answerer, Ask, Pending, RefusedAnswer, answered_by, pending_decision, pick,
    question_for, slot_label,
};

/// What the runtime check needs from outside this process: a shell to ask,
/// the home directory version managers install themselves into, and the
/// places a runtime may sit that the shell does not put first.
///
/// Injected rather than read, so a test can answer for a machine it does
/// not have.
pub struct Machine<'a> {
    pub shell: crate::runtime::Shell<'a>,
    pub home: PathBuf,
    /// Where the absolute places pando knows about are read: a manager's
    /// system-wide marker, a language's install directories. `/` on a
    /// real machine, and a directory of the test's own in a test, so that
    /// what this laptop has in /opt/homebrew decides nothing there.
    pub system: PathBuf,
    /// The PATH pando was started with, which is the developer's own
    /// shell's: where the binary they get by hand is, learnt without
    /// reading their profile. Empty in a test.
    pub path: Vec<PathBuf>,
    /// The OS, and what pando found at run time: read once, so a section
    /// of doctor that asks never reads the machine again.
    pub host: crate::platform::Host,
}

impl<'a> Machine<'a> {
    /// This machine, asked through `shell`.
    ///
    /// Under `cargo test` it is one with nothing on it: the empty HOME
    /// every login shell under test gets, nothing under `/` and no PATH,
    /// so a test that reaches it through a command's own entry point
    /// reads nothing of the laptop it runs on.
    pub fn here(shell: crate::runtime::Shell<'a>) -> Machine<'a> {
        #[cfg(test)]
        {
            let empty = crate::testutil::shell_home().to_path_buf();
            Machine {
                shell,
                home: empty.clone(),
                system: empty,
                path: Vec::new(),
                host: crate::platform::Host::here().clone(),
            }
        }
        #[cfg(not(test))]
        Machine {
            shell,
            home: user_home(),
            system: PathBuf::from("/"),
            path: std::env::var_os("PATH")
                .map(|path| std::env::split_paths(&path).collect())
                .unwrap_or_default(),
            host: crate::platform::Host::here().clone(),
        }
    }

    /// A machine a test describes: its home, with the absolute places
    /// read under the same directory, and no PATH of its own.
    #[cfg(test)]
    pub fn at(shell: crate::runtime::Shell<'a>, home: PathBuf) -> Machine<'a> {
        Machine {
            shell,
            host: crate::platform::Host::at(&home),
            system: home.clone(),
            home,
            path: Vec::new(),
        }
    }
}

/// How long one probe gets. It is a login shell that may source a version
/// manager, so it is not instant — and it is cached against the
/// requirement, so it is not often either.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// A shell that runs what a spawn runs: `bash -lc`, in the main checkout.
/// Its output is captured rather than inherited, so nothing here can paint
/// over the TUI, and `run_captured` bounds the wait as well as the drain.
pub fn runtime_shell(cwd: &Path) -> impl Fn(&str) -> Option<String> {
    move |command: &str| {
        let captured = proc::run_captured(command, cwd, &[], PROBE_TIMEOUT).ok()?;
        Some(format!("{}\n{}", captured.stdout, captured.stderr))
    }
}

/// The developer's home, which is where version managers live.
///
/// Not pando's home: `~/.pando` is where pando writes, `~/.nvm` is where
/// nvm is.
pub fn user_home() -> PathBuf {
    crate::platform::dirs::home().unwrap_or_else(|| PathBuf::from("/"))
}

/// What the probe found, and what there is to do about it.
pub(super) enum RuntimeOutcome {
    /// Nothing to say: nothing pinned, a match, or something this build
    /// cannot judge. Silence is the common case and the right one.
    Fine,
    /// A mismatch with no prelude set: a question, with the report that
    /// makes it answerable. The lines that would fix it are tried on this
    /// machine, which costs a shell each, so [`prelude_proposal`] builds
    /// them only for a pass that is going to ask.
    Ask {
        check: Box<crate::runtime::Check>,
        requirements: Vec<crate::runtime::Requirement>,
        report: Vec<String>,
    },
    /// A mismatch with a prelude already set. The prelude is not working,
    /// and nothing but the developer can say what should replace it.
    Broken(String),
}

/// Asks the shell pando will spawn in what it resolves, and compares it to
/// what the repository asks for.
///
/// Three outcomes, and the one that matters is the second: a process
/// started under a runtime the project rejects dies of it, and the whole
/// point of asking first is not to start it.
pub(super) fn resolve_runtime(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
) -> Result<RuntimeOutcome> {
    let prelude = match config.runtime.prelude.as_deref() {
        // An answer, and the one that means "this machine needs nothing".
        // The same distinction `ports = []` makes: unset is nobody having
        // said, empty is somebody having said no.
        Some(prelude) if prelude.trim().is_empty() => return Ok(RuntimeOutcome::Fine),
        Some(prelude) => prelude.trim(),
        None => "",
    };
    let requirements =
        crate::runtime::requirements_for(paths.root(), &config.runtime.version_files);
    let walked = walk(paths, config, &requirements, prelude, machine.shell, true)?;
    let Some(check) = walked.mismatch else {
        return Ok(RuntimeOutcome::Fine);
    };
    if prelude.is_empty() {
        let report = runtime_report(&check, &requirements, prelude, machine, None);
        Ok(RuntimeOutcome::Ask {
            check: Box::new(check),
            requirements,
            report,
        })
    } else {
        let origin = config::prelude_origin(paths);
        let report = runtime_report(&check, &requirements, prelude, machine, origin);
        Ok(RuntimeOutcome::Broken(report.join("\n  ")))
    }
}

/// The requirement this machine does not meet with no prelude in front of
/// it, when nobody has answered the prelude question: what a start would
/// ask it for. `version_files` stands in for config's, so a caller can
/// ask about the ones `init --yes` would write.
///
/// Writes nothing, not even the probe cache: `init --agent` asks this,
/// and it writes nothing anywhere. A pass the cache already holds costs
/// no shell.
pub fn prelude_needed(
    paths: &PandoPaths,
    config: &Config,
    version_files: &[String],
    machine: &Machine<'_>,
) -> Option<crate::runtime::Check> {
    if config.runtime.prelude.is_some() {
        return None;
    }
    let requirements = crate::runtime::requirements_for(paths.root(), version_files);
    walk(paths, config, &requirements, "", machine.shell, false)
        .ok()?
        .mismatch
}

/// What one walk over the requirements found.
struct Walk {
    /// The first requirement this machine definitely does not meet.
    mismatch: Option<crate::runtime::Check>,
    /// Every requirement that passed, by its fingerprint, with the version
    /// that passed it: probed now, or remembered from before.
    satisfied: std::collections::BTreeMap<String, String>,
}

/// The first language whose requirement this machine definitely does not
/// meet, probing at most once per language and directory and, when
/// `remember` says so, remembering the ones that passed.
///
/// Only passes are remembered: a cached failure would go on reporting a
/// problem the developer has just fixed, and a failure stops the start
/// anyway, so there is no spawn to save by keeping it.
fn walk(
    paths: &PandoPaths,
    config: &Config,
    requirements: &[crate::runtime::Requirement],
    prelude: &str,
    shell: crate::runtime::Shell<'_>,
    remember: bool,
) -> Result<Walk> {
    let mut walked = Walk {
        mismatch: None,
        satisfied: Default::default(),
    };
    if requirements.is_empty() {
        return Ok(walked);
    }
    let cache_file = paths.runtime_cache_file();
    let mut cache = crate::runtime::load_cache(&cache_file);
    let mut learned = false;
    // The pin if the project has one, else whatever range it stated: that
    // is what `for_language` sorted to the front — at the root, and in each
    // app directory whose version file config names.
    for requirement in crate::runtime::to_compare(requirements) {
        let Some(language) = crate::runtime::language(&requirement.language) else {
            continue;
        };
        let fingerprint = crate::runtime::fingerprint(requirement, prelude);
        if let Some(version) = cache.satisfied.get(&fingerprint) {
            walked.satisfied.insert(fingerprint, version.clone());
            continue;
        }
        // Where its processes run, which is where a runner's lockfile is.
        let dir = match &requirement.dir {
            Some(dir) => paths.root().join(dir),
            None => paths.root().to_path_buf(),
        };
        if runs_through_runner(&dir, config, language, prelude, shell) {
            continue;
        }
        let check = crate::runtime::check(requirement, prelude, shell);
        match check.verdict {
            crate::runtime::Verdict::Satisfied => {
                let version = check.resolved.version.clone().unwrap_or_default();
                walked
                    .satisfied
                    .insert(fingerprint.clone(), version.clone());
                cache.remember(fingerprint, version);
                learned = true;
            }
            crate::runtime::Verdict::Mismatch => {
                walked.mismatch = Some(check);
                break;
            }
            // A spec this build cannot evaluate, a probe that could not
            // run, an output that was not a version: never a reason to
            // stop anything, and never cached either.
            crate::runtime::Verdict::Unknown => {}
        }
    }
    if learned && remember {
        // The home is created through the one function that makes it 0700
        // and refuses it inside the repository, rather than by a cache
        // write that would know neither rule.
        paths.ensure_home()?;
        // Best effort. The cache only saves the next start a probe, and
        // this start's check has already passed: a save that fails must
        // not fail the start that learnt what it would have saved.
        let _ = crate::runtime::save_cache(&cache_file, &cache);
    }
    Ok(walked)
}

/// Whether every command this project runs in `language` goes through a
/// runner that resolves the interpreter itself, and this shell has it.
///
/// `uv run` reads `.python-version` and finds or installs that Python, so
/// what `bash -lc` resolves on its own is beside the point — and asking
/// for a prelude line to fix it was a dead end on any machine with no
/// pyenv. Only when the project uses the runner (its lockfile is there),
/// every configured process and hook goes through it (nothing configured yet counts,
/// since detection proposes the runner's form), and the shell finds it.
///
/// Public because `doctor` reports a mismatch only where a start would act
/// on one, and a start skips the language this answers true for.
pub fn runs_through_runner(
    root: &Path,
    config: &Config,
    language: &crate::runtime::Language,
    prelude: &str,
    shell: crate::runtime::Shell<'_>,
) -> bool {
    let Some(runner) = language.runner(root) else {
        return false;
    };
    let Some(prefix) = runner.run_prefix.map(str::trim) else {
        return false;
    };
    // Hooks too, fallbacks included: a migration hook that runs `python`
    // bare gets whatever interpreter the shell resolves, exactly as a
    // process would, and it runs first.
    // Not one switched off: `on = "never"` is how the schema question's
    // "no" is written down, and a command that never runs needs no
    // interpreter.
    let hooks = config
        .hooks
        .iter()
        .filter(|hook| hook.on != Some(config::HookScope::Never))
        .flat_map(|hook| std::iter::once(hook.cmd.as_str()).chain(hook.fallback.as_deref()));
    let through = config
        .processes
        .values()
        .map(|process| process.cmd.as_str())
        .chain(hooks)
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
        .all(|cmd| cmd.starts_with(prefix));
    if !through {
        return false;
    }
    const OK: &str = "pando-runner-ok";
    let probe = format!("command -v {} >/dev/null && echo {OK}", runner.program);
    let probe = match prelude {
        "" => probe,
        prelude => format!("{prelude} && {probe}"),
    };
    shell(&probe).is_some_and(|out| out.lines().any(|line| line.trim() == OK))
}

/// The lines that make a mismatch answerable.
///
/// Deliberately concrete about the path: pando's shell is not the
/// developer's shell, and "node 24" is not a diagnosis when everything
/// works when they type it by hand. Where the binary came from is.
fn runtime_report(
    check: &crate::runtime::Check,
    requirements: &[crate::runtime::Requirement],
    prelude: &str,
    machine: &Machine<'_>,
    origin: Option<PathBuf>,
) -> Vec<String> {
    let requirement = &check.requirement;
    let language = &requirement.language;
    let mut lines: Vec<String> = Vec::new();

    if !prelude.is_empty() {
        lines.push(match &origin {
            Some(path) => format!("the prelude in {} is not working", path.display()),
            None => "that line does not work".to_string(),
        });
        lines.push(format!("prelude: {prelude}"));
    }
    lines.push(format!(
        "this project asks for {language} {} ({})",
        requirement.spec, requirement.source
    ));
    for other in requirements
        .iter()
        .filter(|r| r.language == *language && r.source != requirement.source)
    {
        lines.push(format!("{} also asks for {}", other.source, other.spec));
    }
    if !check.resolved.ran {
        lines.push(match &check.resolved.failure {
            Some(failure) => format!("the prelude itself failed: {failure}"),
            None => "the prelude itself failed".to_string(),
        });
    } else {
        lines.push(
            match (
                &check.resolved.version,
                &check.resolved.path,
                &check.resolved.failure,
            ) {
                (Some(version), Some(path), _) => {
                    format!("`bash -lc` here resolves {language} {version}, from {path}")
                }
                (None, Some(path), Some(failure)) => {
                    format!("`bash -lc` here finds {language} at {path}, and it fails: {failure}")
                }
                _ => format!("`bash -lc` here has no {language} at all"),
            },
        );
    }
    lines.push(
        "pando runs every command with `bash -lc`, which is not your interactive shell".to_string(),
    );

    let Some(entry) = crate::runtime::language(language) else {
        return lines;
    };
    let installed = crate::runtime::installed(entry, &machine.home, &machine.system);
    lines.push(match installed.as_slice() {
        [] => format!("no version manager pando knows about is installed for {language}"),
        managers => format!(
            "version managers installed here: {}",
            managers
                .iter()
                .map(|m| m.name)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    });
    // Printed, never run. Installing a toolchain is the developer's
    // decision to make on their own machine.
    if let Some(command) = installed
        .first()
        .and_then(|manager| manager.install_command(entry, &requirement.spec))
    {
        lines.push(format!(
            "if {language} {} is not installed yet: {command} — pando never installs one",
            requirement.spec
        ));
    }
    // The question is about this laptop, and the answer to it outlives
    // this project: say so before anybody picks a line.
    if prelude.is_empty() {
        lines.push(MACHINE_WIDE.to_string());
    }
    lines
}

/// What the prelude is, in one line, wherever it is asked for.
pub const MACHINE_WIDE: &str = "a prelude is this machine's, not this project's: pando runs it in \
                                front of every command, in every project on this machine";

/// How many directories outside the managers the prelude question tries.
/// Each costs a login shell, and past a handful the question is a list
/// nobody reads.
const MAX_PATH_OFFERS: usize = 6;

/// One line the prelude question offers, tried on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub line: String,
    pub why: String,
    /// Whether, with this line in front, `bash -lc` resolves every
    /// requirement the project states. Only such a line is one `--yes`
    /// may take.
    pub works: bool,
    /// Whether the line reorders PATH rather than asking a version
    /// manager. It works, but it also changes which `npm`, `pnpm` or
    /// `python` every project on the machine runs, so it is offered and
    /// never taken for the developer.
    pub reorders_path: bool,
}

impl Offer {
    /// Whether `--yes` may take this line: a manager's line that works.
    /// A PATH line works as well, but it reorders every project's tools,
    /// so the developer picks it.
    pub fn taken_by_yes(&self) -> bool {
        self.works && !self.reorders_path
    }
}

/// The lines that would make this machine resolve what `check` found it
/// does not, each tried on it.
///
/// First the installed managers' lines that work, in the table's order;
/// then the directories that hold a version the project accepts, one per
/// binary and narrowest first ([`binary_dirs`](crate::runtime::binary_dirs));
/// last the managers' lines that do not work yet,
/// each with the command that would install the version under it. A
/// directory whose line does not work is not offered at all: nothing but
/// that directory would make it.
///
/// A line works when the pin itself is met, not a range beside it: the
/// requirement compared is the pin whenever the project states one, so a
/// directory holding node 24 is never offered for an `.nvmrc` of 25 on
/// the strength of an `engines` range that 24 would satisfy.
///
/// `remember` is whether a line that works is written to the probe
/// cache, which is what makes checking it again, once it is picked, cost
/// nothing. `doctor` writes nothing, and passes false.
pub fn prelude_offers(
    paths: &PandoPaths,
    config: &Config,
    requirements: &[crate::runtime::Requirement],
    check: &crate::runtime::Check,
    machine: &Machine<'_>,
    remember: bool,
) -> Result<Vec<Offer>> {
    let requirement = &check.requirement;
    let Some(language) = crate::runtime::language(&requirement.language) else {
        return Ok(Vec::new());
    };
    let (name, spec) = (&requirement.language, &requirement.spec);
    // The version a line gives the requirement it is for, when it leaves
    // no requirement unmet.
    let tried = |line: &str| -> Result<Option<String>> {
        let walked = walk(paths, config, requirements, line, machine.shell, remember)?;
        if walked.mismatch.is_some() {
            return Ok(None);
        }
        Ok(walked
            .satisfied
            .get(&crate::runtime::fingerprint(requirement, line))
            .cloned())
    };
    let mut working: Vec<Offer> = Vec::new();
    let mut not_yet: Vec<Offer> = Vec::new();
    for fix in crate::runtime::fixes(language, &machine.home, &machine.system, requirement) {
        if tried(&fix.line)?.is_some() {
            working.push(Offer {
                line: fix.line,
                why: fix.why,
                works: true,
                reorders_path: false,
            });
            continue;
        }
        let install = language
            .managers
            .iter()
            .find(|manager| manager.name == fix.manager)
            .and_then(|manager| manager.install_command(language, spec));
        let why = match install {
            Some(command) => format!(
                "{}; it gives `bash -lc` no {name} {spec} yet: `{command}` first",
                fix.why
            ),
            None => format!("{}; it gives `bash -lc` no {name} {spec} yet", fix.why),
        };
        not_yet.push(Offer {
            line: fix.line,
            why,
            works: false,
            reorders_path: false,
        });
    }
    // Not the directory `bash -lc` already takes it from: that is the one
    // that gave the wrong answer.
    let resolved_dir = check
        .resolved
        .path
        .as_deref()
        .and_then(|path| Path::new(path).parent())
        .map(Path::to_path_buf);
    for dir in crate::runtime::binary_dirs(language, &machine.system, &machine.path, requirement)
        .into_iter()
        .filter(|dir| Some(dir) != resolved_dir.as_ref())
        // Before the login shell a probe costs: a binary that says it is
        // the wrong version is not worth one.
        .filter(|dir| !crate::runtime::rules_out(language, dir, spec))
        .take(MAX_PATH_OFFERS)
    {
        let Some(line) = crate::runtime::path_line(&dir) else {
            continue;
        };
        if working
            .iter()
            .chain(&not_yet)
            .any(|offer| offer.line == line)
        {
            continue;
        }
        if let Some(version) = tried(&line)? {
            // A directory every installer shares puts all of them first,
            // which is worth knowing before picking it.
            let what = match crate::runtime::is_shared_bin(&dir, &machine.system) {
                true => "it and everything else in that directory",
                false => "it",
            };
            let why = format!(
                "{name} {version} is in {}; this puts {what} first on PATH, in every project \
                 on this machine",
                dir.display()
            );
            working.push(Offer {
                line,
                why,
                works: true,
                reorders_path: true,
            });
        }
    }
    working.extend(not_yet);
    Ok(working)
}

/// The prelude question's options: every line [`prelude_offers`] tried,
/// and only the ones [`Offer::taken_by_yes`] are lines `--yes` may take.
pub(super) fn prelude_proposal(
    paths: &PandoPaths,
    config: &Config,
    requirements: &[crate::runtime::Requirement],
    check: &crate::runtime::Check,
    machine: &Machine<'_>,
) -> Result<detect::Proposal> {
    let candidates = prelude_offers(paths, config, requirements, check, machine, true)?
        .into_iter()
        .map(|offer| detect::Candidate {
            needs_a_human: !offer.taken_by_yes(),
            value: offer.line,
            why: offer.why,
            ..detect::Candidate::default()
        })
        .collect();
    // Never decided. What one machine needs is not something a rule gets to
    // settle on a developer's behalf, and the answer lands in a file every
    // project on that machine shares.
    Ok(detect::Proposal::of(Slot::Prelude, candidates, false))
}

/// Asks the prelude question, checks the answer, and writes it to the user
/// layer.
///
/// Checked *before* it is written, not after: this line applies to every
/// project on the machine, and `--yes` must not be able to persist one
/// that does not work into a file nobody looked at.
pub(super) fn answer_prelude(
    paths: &PandoPaths,
    config: &mut Config,
    proposal: &detect::Proposal,
    report: &[String],
    ask: Ask<'_>,
    machine: &Machine<'_>,
) -> Result<Option<Pending>> {
    let question = question_for(proposal, report).at(paths);
    let offered = question.options.len();
    let (answer, by) = answered_by(ask(&question)?);
    // Before the match takes it apart. The caller writes it down once the
    // line is on disk — and only if it gets there, since a prelude that
    // fails its own probe is refused below.
    let pending = pending_decision(&question, proposal, &answer, by);
    let (line, note) = match answer {
        Answer::Choice(index) => {
            let candidate = pick(proposal, index)?;
            let why = candidate.why.clone();
            (candidate.value, by.note(config::Note::Detected(why)))
        }
        Answer::Auto(index) => (
            pick(proposal, index)?.value,
            config::Note::TookFirst(offered),
        ),
        Answer::Custom(value) => (value.trim().to_string(), by.note(config::Note::Answered)),
        // "This machine needs nothing." Recorded as an empty prelude
        // rather than left unset, so it is never asked again — the shape
        // the port slot already uses for a process with no ports.
        Answer::None => (String::new(), by.note(config::Note::Answered)),
        Answer::Many(_) | Answer::Processes(_) | Answer::Namespaced(_) => bail!(
            "{} is one line, not several of them",
            slot_label(Slot::Prelude)
        ),
        Answer::Program(_) => unreachable!("answered_by peels every wrapper"),
    };
    if !line.is_empty() {
        let mut proposed = config.clone();
        proposed.runtime.prelude = Some(line.clone());
        if let RuntimeOutcome::Broken(report) = resolve_runtime(paths, &proposed, machine)? {
            // A program handed this line in, so it is the program's input
            // that is wrong: the usage-error shape, not a failure.
            if by == Answerer::Program {
                return Err(anyhow::Error::new(RefusedAnswer(report.to_string())));
            }
            bail!("{report}");
        }
    }
    let (table, key) = Slot::Prelude
        .key()
        .expect("the prelude slot writes one key");
    config::set_detected(paths, Slot::Prelude.layer(), table, key, line.clone(), note)?;
    config.runtime.prelude = Some(line);
    Ok(pending)
}

/// `[runtime].prelude`, when set, runs before every command pando starts, so
/// a version manager sourced from a login profile is in effect. The shell is
/// `bash -lc`, so `.bash_profile` is read and `.zshrc` is not.
pub fn with_prelude(config: &Config, cmd: &str) -> String {
    match config.runtime.prelude.as_deref().map(str::trim) {
        // Grouped, so the `&&` guards the whole command: `a; b` after a
        // failed prelude ran `b` anyway. The newline before `}` lets a
        // command end in a comment. A group, not a subshell, so `exec`
        // still replaces the shell pando records.
        Some(prelude) if !prelude.is_empty() => format!("{prelude} && {{\n{cmd}\n}}"),
        _ => cmd.to_string(),
    }
}
