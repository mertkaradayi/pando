//! The check: settle the settings, choose where its data runs, make a
//! throwaway worktree of the commit a new one would fork from, install
//! and start it — on the shared services, or in namespaces of its own
//! where the schema step can be proved — wait until every process is
//! ready and the browser's app serves a page, take it all down, and
//! record what happened.

use anyhow::{Result, bail};
use chrono::Utc;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::{self, Config, HookPoint};
use crate::detect::Slot;
use crate::paths::{CHECK_WORKTREE, PandoPaths};
use crate::ports::{self, PageAnswer};
use crate::setup::{
    CheckMode, CheckOutcome, CheckRecord, FailureKind, ProcessResult, RanBy, fingerprint,
    redact_line,
};
use crate::state::{self, WorktreeRecord};
use crate::worktree;

use super::super::hooks::{failed_words, hook_scope};
use super::super::lifecycle::start_for_check;
use super::super::namespaced::{check_stays_shared, prepare};
use super::super::questions::{
    Answer, Answering, NeedsAnswer, Question, resolve_for_new, resolve_process, resolve_silencing,
};
use super::super::readiness::{ReadyVerdict, ready_limit, ready_line, ready_verdict};
use super::super::refresh::{failure_tail, refresh};
use super::super::services::{observed_port_for_role, url_role};
use super::super::worktree::{
    CREATED_BUT_INSTALL_FAILED, new_detached, ref_exists, resolve_create_base,
};
use super::super::{INSTALL_HOOK, Mode};
use super::base::{Standing, on_the_base};
use super::interrupt::interrupted;
use super::logs;
use super::machine::first_down;
use super::teardown::{sweep_leftover_check, tear_down};

/// The environment variable that says who started a check, when stderr
/// cannot: the TUI sets it to `tui` on the `pando check` it spawns.
pub const CHECK_RAN_BY_ENV: &str = "PANDO_CHECK_RAN_BY";

/// How often a wait looks at the processes again: the cadence `start
/// --wait` and `logs -f` use.
const POLL: Duration = Duration::from_millis(250);

/// How long a dev server may sit on its first request before the check
/// says why it is waiting.
const SLOW_PAGE: Duration = Duration::from_secs(3);

/// Who ran this check, from [`CHECK_RAN_BY_ENV`]'s value and whether
/// stderr is a terminal: the TUI when it says so, a person at a terminal,
/// or else a program.
pub fn ran_by(env: Option<&str>, stderr_is_terminal: bool) -> RanBy {
    match (env, stderr_is_terminal) {
        (Some("tui"), _) => RanBy::Tui,
        (_, true) => RanBy::Terminal,
        (_, false) => RanBy::Program,
    }
}

/// Where a check says what it is doing.
pub struct Narration<'a> {
    /// A step, in the words the record keeps for the screen that watches
    /// it: "installing", "starting web, api".
    pub step: &'a dyn Fn(&str),
    /// What the commands under it say along the way — the checkout, the
    /// files linked in, the install's command line.
    pub detail: &'a dyn Fn(&str),
}

/// What a check ended with.
#[derive(Debug)]
pub struct Checked {
    /// As it was saved to `check.json`, or, for a probe, as it would have
    /// been.
    pub record: CheckRecord,
    /// The question a run slot still has open, when the check could not
    /// start for want of an answer.
    pub unanswered: Option<NeedsAnswer>,
    /// The base `--base` named, when the run was a probe of it: a base
    /// the settings do not choose. Its record was not saved, so the last
    /// check at the project's own base still says where the setup stands.
    pub probe: Option<String>,
}

/// Runs `pando check`, each of its steps said through `say.step` and kept
/// in `check.json` as it happens, so a screen watching that file sees the
/// check move.
///
/// Refuses, with nothing recorded, only when another check holds the lock
/// or pando's own files cannot be used. Everything else — a question still
/// open, a stopped server, a process that dies, a page that fails — is a
/// result, recorded and returned. Once the throwaway worktree exists, every
/// way out goes through its teardown.
pub fn check(
    paths: &PandoPaths,
    config: &Config,
    ran_by: RanBy,
    say: &Narration<'_>,
) -> Result<Checked> {
    check_at(paths, config, None, ran_by, say)
}

/// [`check`], at `base` for this run when one is given, as `new --base`
/// forks one worktree from it: a branch, or any ref, looked up the way
/// `new` looks a base up. A base the repository does not have is refused
/// before anything is made or recorded — tested at origin/HEAD instead,
/// the run would say it tested what was asked when it had not.
///
/// A base that is not the one the settings choose makes the run a probe:
/// its result is returned and printed, never saved, because `new` forks
/// from another commit, and the setup is what the last check at the
/// settings' own base says it is. A `--base` naming the settings' own is
/// an ordinary check.
pub fn check_at(
    paths: &PandoPaths,
    config: &Config,
    base: Option<&str>,
    ran_by: RanBy,
    say: &Narration<'_>,
) -> Result<Checked> {
    if let Some(base) = base
        && !ref_exists(paths.root(), &resolve_create_base(paths.root(), base))
    {
        bail!("base {base:?} does not exist in this repository — nothing was tested");
    }
    let mut probe = probe_of(paths.root(), config, base);
    paths.ensure_home()?;
    // One at a time, for the whole run.
    let Some(_lock) = state::try_lock(&paths.check_lock_file())? else {
        bail!(
            "a check is already running for this project — its progress is in {}; wait for it \
             to finish",
            paths.check_file().display()
        );
    };
    // What a killed check left, before anything else is made.
    sweep_leftover_check(paths, config, say)?;
    // And the logs of a probe killed before it put the last check's back.
    logs::settle(paths);

    // The settings a start would run on, with no question put to
    // anybody: a slot the rules cannot settle alone is an answer the
    // check does not have.
    let unasked = |question: &Question| -> Result<Answer> {
        Err(anyhow::Error::new(NeedsAnswer {
            question: question.clone(),
        }))
    };
    let resolved = resolve_for_new(paths, config, &unasked, say.detail)
        .and_then(|config| resolve_process(paths, &config, Mode::Shared, &unasked, say.detail));
    let config = match resolved {
        Ok(config) => config,
        Err(e) => {
            let needs = e.downcast::<NeedsAnswer>()?;
            return Ok(not_set_up(paths, config, ran_by, needs, probe));
        }
    };
    // The base, last, as `init` asks it: open only where origin/HEAD is
    // far behind the main checkout's branch, and only while the settings
    // name none. Tested at origin/HEAD then, the check would test the
    // commit the question doubts. A `--base` answers it for this run,
    // which makes the run a probe: the settings still choose no base.
    if config.project.base.is_none() && crate::worktree::base_drift(paths.root()).is_some() {
        let silent = [Slot::Services];
        let asked = resolve_silencing(
            paths,
            &config,
            &[Slot::Base],
            &silent,
            &Answering::asking(&unasked),
            say.detail,
        );
        if let Err(e) = asked {
            let needs = e.downcast::<NeedsAnswer>()?;
            match base {
                Some(given) => probe = Some(given.to_string()),
                None => return Ok(not_set_up(paths, &config, ran_by, needs, None)),
            }
        }
    }

    let mut run = Run::begin(paths, &config, ran_by, say, probe);
    if config.runnable_processes().next().is_none() {
        return Ok(run.end(
            failed(
                FailureKind::Settings,
                "nothing to run: this project configures no process, and pando found no dev \
                 command to offer — answer `dev_cmd` with `pando init --answers -`",
            ),
            None,
        ));
    }
    // The commit a new worktree would fork from, or the one asked for.
    let Some((commit, base_ref)) = commit_to_test(paths.root(), &config, base) else {
        return Ok(run.end(
            failed(
                FailureKind::Settings,
                "no commit to test yet: this repository has no commit for a worktree to check \
                 out",
            ),
            None,
        ));
    };
    let short: String = commit.chars().take(7).collect();
    run.record.commit = Some(commit.clone());
    run.record.base_ref = base_ref.clone();
    // Where the data runs. The hooks after the services — the schema
    // step, a seed — are the one part of a setup a shared check cannot
    // run, because there they would run against the developer's own
    // data. So when there are some to prove, and a namespaced start of
    // the check's worktree could happen without a question, the check is
    // that start: they run in a database of the check's own, which goes
    // with its worktree. Otherwise it is shared, and says what that left
    // untested, and why.
    let after = hooks_after_services(&config);
    let proving = !to_prove(&config).is_empty();
    let shared_because = proving
        .then(|| check_stays_shared(paths, &config))
        .flatten();
    let namespaced = proving && shared_because.is_none();
    if namespaced {
        run.record.mode = CheckMode::Namespaced;
    }
    run.step(&format!(
        "testing {}'s setup at {short} ({}), in a throwaway worktree (no branch){}",
        paths.project.display_name,
        base_ref.as_deref().unwrap_or("HEAD"),
        if namespaced { ", namespaced" } else { "" }
    ));
    // A base given for this run that the settings do not choose is one
    // `new` does not fork from: said now, so a pass is not read as the
    // setup's.
    if let Some(given) = run.probe.clone() {
        let default = commit_to_test(paths.root(), &config, None)
            .and_then(|(_, base_ref)| base_ref)
            .unwrap_or_else(|| "HEAD".to_string());
        // A base already answered is one only `--replace` changes.
        let answer = match config.project.base {
            Some(_) => "pando init --answers - --replace",
            None => "pando init --answers -",
        };
        let line = format!(
            "testing {given} for this run only, because --base named it, and keeping the last \
             check's result: `pando new` still forks from {default}; to make {given} the base, \
             answer `base` with it through `{answer}`"
        );
        run.record.notes.push(line.clone());
        run.step(&line);
    }
    // The machine, before anything is made.
    if let Some(down) = first_down(paths, &config, say.detail) {
        return Ok(run.end(failed(FailureKind::Machine, &down.reason), None));
    }
    if interrupted() {
        return Ok(run.end(CheckOutcome::Interrupted, None));
    }
    // What the check does with the hooks after the services, said before
    // it runs anything.
    if namespaced {
        let line = format!(
            "running the hooks after services ({}) in a database of the check's own, dropped \
             with its worktree",
            after.join(", ")
        );
        run.record.notes.push(line.clone());
        run.step(&line);
    } else if !after.is_empty() {
        let mut line = format!(
            "skipped the hooks that run after services ({}): on the shared services they would \
             run against your own data",
            after.join(", ")
        );
        if let Some(why) = &shared_because {
            line.push_str(&format!(", so the schema step was not tested — {why}"));
        }
        run.record.notes.push(line.clone());
        run.step(&line);
    }

    // From here the worktree may exist, so every way out tears it down.
    // The worktrees directory goes too when this check made it, empty.
    let worktrees_dir = config.worktrees_dir(paths);
    let made_dir = !worktrees_dir.exists();
    // The last check's logs are kept until this one needs the place, and
    // a probe's never take it.
    logs::claim(paths, run.probe.is_some())?;
    let outcome = run.test(&config, &commit, namespaced);
    // A step that failed for want of a file the tested commit never had
    // is the base's, not the settings'.
    let install = (run.record.failed_process.as_deref() == Some(INSTALL_HOOK))
        .then_some(config.project.install.as_deref())
        .flatten();
    let outcome = on_the_base(
        paths.root(),
        outcome,
        &commit,
        base_ref.as_deref(),
        &run.record.failed_tail,
        install,
        run.probe
            .as_ref()
            .and(config.project.base.as_deref())
            .map(|base| Standing {
                base,
                checked: CheckRecord::load(paths).is_some(),
            }),
    );
    let outcome = match tear_down(paths, &config, say.detail) {
        Ok(left) => {
            if made_dir {
                let _ = std::fs::remove_dir(&worktrees_dir);
            }
            match left.is_empty() {
                true => run.step("removed the test worktree; nothing left behind"),
                false => {
                    run.step("removed the test worktree, but not everything it made");
                    run.record.notes.extend(left);
                }
            }
            outcome
        }
        Err(e) => {
            let line = format!(
                "the test worktree could not all be removed: {e:#} — the next `pando check` \
                 sweeps it, and `pando doctor` reports it until then"
            );
            run.record.notes.push(line.clone());
            (say.detail)(&line);
            outcome
        }
    };
    logs::settle(paths);
    let outcome = match run.probe {
        Some(_) => logs::pointed_at_probe(paths, outcome),
        None => outcome,
    };
    let after = config::load(paths)
        .map(|loaded| fingerprint(&loaded.config))
        .unwrap_or_else(|e| format!("unreadable: {e:#}"));
    Ok(run.end(outcome, Some(after)))
}

/// The run settings' fingerprint as the files say them now — the same
/// read the setup state compares with — or, when they cannot be read,
/// the settings this check runs on.
fn settings_fingerprint(paths: &PandoPaths, config: &Config) -> String {
    config::load(paths)
        .map(|loaded| fingerprint(&loaded.config))
        .unwrap_or_else(|_| fingerprint(config))
}

/// A check that stopped before it made anything, for want of an answer:
/// recorded with the slot that is open, unless the run was a probe, and
/// the question handed back for the front end to put.
fn not_set_up(
    paths: &PandoPaths,
    config: &Config,
    ran_by: RanBy,
    needs: NeedsAnswer,
    probe: Option<String>,
) -> Checked {
    // What the resolve pass wrote before it reached the open slot is on
    // disk now; the record is of the settings as they stand.
    let mut record = CheckRecord::begin(settings_fingerprint(paths, config), ran_by);
    record.settings_base = Some(config.project.base.clone());
    record.outcome = CheckOutcome::NotSetUp {
        slot: slot_name(needs.question.slot),
    };
    record.fingerprint_after = Some(record.fingerprint_before.clone());
    record.finished_at = Some(Utc::now());
    if probe.is_none() {
        let _ = record.save(paths);
    }
    Checked {
        record,
        unanswered: Some(needs),
        probe,
    }
}

/// The base `given` names, when a run at it is a probe: the ref it
/// resolves to is not the one the settings choose. A `--base` naming the
/// settings' own, however it is spelled, is no probe.
fn probe_of(root: &Path, config: &Config, given: Option<&str>) -> Option<String> {
    let given = given?;
    let tested = commit_to_test(root, config, Some(given)).and_then(|(_, base_ref)| base_ref);
    let chosen = commit_to_test(root, config, None).and_then(|(_, base_ref)| base_ref);
    (tested != chosen).then(|| given.to_string())
}

/// A slot's published name, as `pando signals` and the answers file spell
/// it.
fn slot_name(slot: Slot) -> String {
    serde_json::to_value(slot)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{slot:?}"))
}

fn failed(kind: FailureKind, reason: &str) -> CheckOutcome {
    CheckOutcome::Failed {
        kind,
        reason: reason.to_string(),
    }
}

/// The commit `new` would fork a branch from, and the ref it was read
/// from: `given` for this run, else the project's `base`, else the
/// repository's default branch, else whatever HEAD is. `None` in a
/// repository with no commit at all.
pub(super) fn commit_to_test(
    root: &Path,
    config: &Config,
    given: Option<&str>,
) -> Option<(String, Option<String>)> {
    let base = given
        .or(config.project.base.as_deref())
        .map(|base| resolve_create_base(root, base))
        .filter(|base| ref_exists(root, base))
        .or_else(|| worktree::resolve_base_branch(root));
    base.into_iter().map(Some).chain([None]).find_map(|base| {
        let refname = base.as_deref().unwrap_or("HEAD");
        let out = crate::project::git(
            root,
            [
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{refname}^{{commit}}"),
            ],
        )
        .ok()
        .filter(|out| out.status.success())?;
        let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!sha.is_empty()).then_some((sha, base))
    })
}

/// The names of the hooks a shared check leaves out: every one after
/// `services` and after `dev`.
fn hooks_after_services(config: &Config) -> Vec<String> {
    config
        .hooks
        .iter()
        .filter(|hook| matches!(hook.after, HookPoint::Services | HookPoint::Dev))
        .map(|hook| hook.name.clone())
        .collect()
}

/// Of those, the ones a start on data of its own would run: what a
/// namespaced check is there to prove. One set to `on = "never"` runs
/// nowhere, and is nothing to prove.
fn to_prove(config: &Config) -> Vec<&str> {
    config
        .hooks
        .iter()
        .filter(|hook| matches!(hook.after, HookPoint::Services | HookPoint::Dev))
        .filter(|hook| hook_scope(config, hook) != config::HookScope::Never)
        .map(|hook| hook.name.as_str())
        .collect()
}

/// A check under way, and its record, saved at every step — unless it
/// is a probe, whose record is never saved.
struct Run<'a> {
    paths: &'a PandoPaths,
    say: &'a Narration<'a>,
    record: CheckRecord,
    /// The base a probe tests; see [`Checked::probe`].
    probe: Option<String>,
    /// Whether a save has failed already, so it is said once.
    save_failed: std::cell::Cell<bool>,
}

/// How the processes' wait ended.
enum Waited {
    Ready(Box<WorktreeRecord>),
    Failed {
        process: String,
        reason: String,
        log: std::path::PathBuf,
    },
    Interrupted,
}

impl<'a> Run<'a> {
    fn begin(
        paths: &'a PandoPaths,
        config: &Config,
        ran_by: RanBy,
        say: &'a Narration<'a>,
        probe: Option<String>,
    ) -> Run<'a> {
        let mut record = CheckRecord::begin(settings_fingerprint(paths, config), ran_by);
        record.settings_base = Some(config.project.base.clone());
        let run = Run {
            paths,
            say,
            record,
            probe,
            save_failed: std::cell::Cell::new(false),
        };
        run.save();
        run
    }

    fn save(&self) {
        if self.probe.is_some() {
            return;
        }
        if let Err(e) = self.record.save(self.paths)
            && !self.save_failed.replace(true)
        {
            (self.say.detail)(&format!("could not record the check's progress: {e:#}"));
        }
    }

    /// A step said, and kept in the record for whoever watches it.
    fn step(&mut self, line: &str) {
        (self.say.step)(line);
        self.record.progress.push(line.to_string());
        self.save();
    }

    /// The check's result, saved; `after` is the settings' fingerprint
    /// taken again at the end, when the check got that far.
    fn end(mut self, outcome: CheckOutcome, after: Option<String>) -> Checked {
        self.record.outcome = outcome;
        self.record.fingerprint_after =
            Some(after.unwrap_or_else(|| self.record.fingerprint_before.clone()));
        self.record.finished_at = Some(Utc::now());
        self.save();
        Checked {
            record: self.record,
            unanswered: None,
            probe: self.probe,
        }
    }

    /// The test itself: make the worktree, install, make its namespaces
    /// when it runs namespaced, start, wait, and ask for a page. Whatever
    /// it returns, the caller tears the worktree down, and its namespaces
    /// with it.
    fn test(&mut self, config: &Config, commit: &str, namespaced: bool) -> CheckOutcome {
        let installs = config
            .project
            .install
            .as_deref()
            .is_some_and(|install| !install.trim().is_empty())
            || config
                .hooks
                .iter()
                .any(|hook| hook.after == HookPoint::Create);
        if installs {
            self.step("installing");
        }
        let began = Instant::now();
        if let Err(e) = new_detached(self.paths, config, commit, self.say.detail) {
            // A Ctrl-C reaches the install too, which then fails: that is
            // the interruption, not the settings.
            if interrupted() {
                return CheckOutcome::Interrupted;
            }
            let text = format!("{e:#}");
            if text.contains(CREATED_BUT_INSTALL_FAILED) {
                self.failed_in(
                    INSTALL_HOOK,
                    &self.paths.log_file(CHECK_WORKTREE, INSTALL_HOOK),
                );
                return failed(FailureKind::Settings, &install_failed(&text));
            }
            return failed(
                FailureKind::Settings,
                &format!("the test worktree could not be made: {text}"),
            );
        }
        let installed = began.elapsed();
        if interrupted() {
            return CheckOutcome::Interrupted;
        }

        // The check's own database, and slot, in the main checkout's
        // servers, made before anything starts, as a namespaced start makes
        // them. Nothing here is the settings' doing: a server that refuses
        // the login, or a login with no grant to make a database, is the
        // machine's to fix, and the error says how.
        let namespaces = match namespaced {
            false => None,
            true => {
                self.step("making the check's own database in your servers");
                match prepare(self.paths, config, CHECK_WORKTREE, self.say.detail) {
                    Ok(ready) => Some(ready),
                    Err(_) if interrupted() => return CheckOutcome::Interrupted,
                    Err(e) => return failed(FailureKind::Machine, &format!("{e:#}")),
                }
            }
        };
        if interrupted() {
            return CheckOutcome::Interrupted;
        }

        let names: Vec<&str> = config
            .runnable_processes()
            .map(|(name, _)| name.as_str())
            .collect();
        self.step(&format!("starting {}", names.join(", ")));
        let started = Instant::now();
        if let Err(e) = start_for_check(
            self.paths,
            config,
            CHECK_WORKTREE,
            namespaces,
            self.say.detail,
        ) {
            if interrupted() {
                return CheckOutcome::Interrupted;
            }
            let text = format!("{e:#}");
            // A hook after the services that failed is the settings' — the
            // schema step `init --answers -` sets — and its log says why.
            if let Some(hook) = config.hooks.iter().find(|hook| {
                matches!(hook.after, HookPoint::Services | HookPoint::Dev)
                    && text.contains(&failed_words(&hook.name))
            }) {
                let log = self.paths.log_file(CHECK_WORKTREE, &hook.name);
                self.failed_in(&hook.name, &log);
            }
            return failed(FailureKind::Settings, &text);
        }
        // Every process ready, judged as `start --wait` judges it.
        self.step(&format!("waiting for {} to be ready", names.join(", ")));
        let mut ready_at: BTreeMap<String, f64> = BTreeMap::new();
        let record = match self.wait_ready(started, &mut ready_at) {
            Waited::Ready(record) => record,
            Waited::Interrupted => return CheckOutcome::Interrupted,
            Waited::Failed {
                process,
                reason,
                log,
            } => {
                self.failed_in(&process, &log);
                return failed(FailureKind::Settings, &format!("{process} {reason}"));
            }
        };
        // The page the browser would get.
        let page = self.ask_for_page(&record);
        if let Some(outcome) = page {
            return outcome;
        }
        let mut summary: Vec<String> = Vec::new();
        if installs {
            summary.push(format!("installed {:.1}s", installed.as_secs_f64()));
        }
        for p in &self.record.processes {
            let mut line = format!("{} ready", p.name);
            if let Some(port) = p.port {
                line.push_str(&format!(" :{port}"));
            }
            if let Some(status) = p.http_status {
                line.push_str(&format!(" (HTTP {status})"));
            }
            summary.push(line);
        }
        self.step(&summary.join(" · "));
        CheckOutcome::Passed
    }

    /// The failed process's closing lines, redacted, and its name.
    fn failed_in(&mut self, process: &str, log: &Path) {
        self.record.failed_process = Some(process.to_string());
        self.record.failed_tail = failure_tail(log).iter().map(|l| redact_line(l)).collect();
    }

    /// Waits until every process of the check's worktree is ready, one of
    /// them fails, or the wait is stopped: by a signal, or by a `stop` of
    /// the worktree from elsewhere, which leaves nothing to wait on.
    fn wait_ready(&mut self, began: Instant, ready_at: &mut BTreeMap<String, f64>) -> Waited {
        loop {
            if interrupted() {
                return Waited::Interrupted;
            }
            let refreshed = refresh(self.paths);
            let Some(record) = refreshed.state.worktrees.get(CHECK_WORKTREE) else {
                return Waited::Interrupted;
            };
            let now = Utc::now();
            let elapsed = began.elapsed().as_secs_f64();
            for (process, p) in &record.processes {
                if ready_line(process, p, now, began.elapsed()).is_some() {
                    ready_at.entry(process.clone()).or_insert(elapsed);
                }
            }
            // Each process as it stands: ready, and when; or not yet, and
            // for how long — which is how long a failed one lasted.
            self.record.processes = record
                .processes
                .iter()
                .map(|(name, p)| ProcessResult {
                    name: name.clone(),
                    ready: ready_at.contains_key(name),
                    port: p.ready_port,
                    http_status: None,
                    secs: ready_at.get(name).copied().unwrap_or(elapsed),
                })
                .collect();
            match ready_verdict(record, None, now) {
                ReadyVerdict::Ready => return Waited::Ready(Box::new(record.clone())),
                ReadyVerdict::Gone => return Waited::Interrupted,
                ReadyVerdict::Failed {
                    process,
                    reason,
                    log,
                } => {
                    return Waited::Failed {
                        process,
                        reason: format!("failed: {reason}"),
                        log,
                    };
                }
                ReadyVerdict::Waiting => {}
            }
            if began.elapsed() > ready_limit(record, None) {
                let (process, p) = record
                    .processes
                    .iter()
                    .find(|(name, _)| !ready_at.contains_key(*name))
                    .or_else(|| record.processes.iter().next())
                    .expect("a record with no processes is Gone above");
                return Waited::Failed {
                    process: process.clone(),
                    reason: format!("was still starting after {}s", began.elapsed().as_secs()),
                    log: p.log_path.clone(),
                };
            }
            std::thread::sleep(POLL);
        }
    }

    /// Asks the process that owns the worktree's URL for its first page.
    /// `None` when the page passes; the outcome otherwise.
    fn ask_for_page(&mut self, record: &WorktreeRecord) -> Option<CheckOutcome> {
        let Some(role) = url_role(record) else {
            let note = "no process has a port, so there was no page to ask for".to_string();
            self.record.notes.push(note);
            return None;
        };
        let owner = record
            .roles
            .iter()
            .find(|(_, roles)| roles.contains(&role))
            .map(|(process, _)| process.clone())?;
        let port =
            observed_port_for_role(record, &role).or_else(|| record.ports.get(&role).copied())?;
        self.step(&format!("asking {owner} for its first page on :{port}"));
        let wait = ports::page_wait();
        let paths = self.paths;
        let mut said_slow = false;
        let mut looks = 0u32;
        let mut stopped: Option<Waited> = None;
        let say = self.say;
        let answer = ports::ask_for_page(port, wait, &mut |elapsed| {
            if interrupted() {
                stopped = Some(Waited::Interrupted);
                return false;
            }
            if !said_slow && elapsed >= SLOW_PAGE {
                said_slow = true;
                (say.step)(&format!(
                    "{owner} has not answered yet — a dev server may compile a page on its first \
                     request; waiting up to {}s",
                    wait.as_secs()
                ));
            }
            // Once a second, whether the owner is still up to answer.
            looks += 1;
            if !looks.is_multiple_of(4) {
                return true;
            }
            let refreshed = refresh(paths);
            let Some(now) = refreshed.state.worktrees.get(CHECK_WORKTREE) else {
                stopped = Some(Waited::Interrupted);
                return false;
            };
            match ready_verdict(now, Some(&owner), Utc::now()) {
                ReadyVerdict::Gone => {
                    stopped = Some(Waited::Interrupted);
                    false
                }
                ReadyVerdict::Failed {
                    process,
                    reason,
                    log,
                } => {
                    stopped = Some(Waited::Failed {
                        process,
                        reason: format!("failed: {reason}"),
                        log,
                    });
                    false
                }
                _ => true,
            }
        });
        let log = self.paths.log_file(CHECK_WORKTREE, &owner);
        match stopped {
            Some(Waited::Interrupted) => return Some(CheckOutcome::Interrupted),
            Some(Waited::Failed {
                process,
                reason,
                log,
            }) => {
                self.failed_in(&process, &log);
                return Some(failed(
                    FailureKind::Settings,
                    &format!("{process} {reason}"),
                ));
            }
            _ => {}
        }
        let (outcome, status) = match answer {
            PageAnswer::Status(status) if status >= 500 => (
                Some(failed(
                    FailureKind::Settings,
                    &format!("{owner} answered its first page with HTTP {status}"),
                )),
                Some(status),
            ),
            PageAnswer::Status(status) => (None, Some(status)),
            PageAnswer::NotHttp => {
                self.record.notes.push(format!(
                    "{owner} answers on :{port}, but not in plain HTTP (TLS, or another \
                     protocol), so its page was not checked"
                ));
                (None, None)
            }
            PageAnswer::Refused => (
                Some(failed(
                    FailureKind::Settings,
                    &format!(
                        "nothing answered on :{port}, where {owner} was ready — it may listen on \
                         another address than localhost"
                    ),
                )),
                None,
            ),
            PageAnswer::Silent => (
                Some(failed(
                    FailureKind::Settings,
                    &format!(
                        "{owner} did not answer its first page within {}s",
                        wait.as_secs()
                    ),
                )),
                None,
            ),
        };
        // `ready` stays what the wait found: the port answered. A page
        // that failed is the outcome's to say, with its status here.
        if let Some(result) = self.record.processes.iter_mut().find(|p| p.name == owner) {
            result.http_status = status;
        }
        if outcome.is_some() {
            self.failed_in(&owner, &log);
        }
        outcome
    }
}

/// An install failure as `new` words it, said once: less the part that
/// says the worktree was kept, which the check removes, and less the
/// hook's own name for the step, with its exit status beside the words —
/// "the install step failed (exit 2): …" where `new` has "was created,
/// but its install step failed: the install hook failed: exited 2: …".
pub(super) fn install_failed(text: &str) -> String {
    let rest = match text.split_once(CREATED_BUT_INSTALL_FAILED) {
        Some((_, rest)) => rest.trim_start_matches([':', ' ']),
        None => text,
    };
    let hook = format!("{}: ", failed_words(INSTALL_HOOK));
    let rest = rest.strip_prefix(&hook).unwrap_or(rest);
    let status = rest.strip_prefix("exited ").and_then(|after| {
        let digits = after
            .find(|c: char| !c.is_ascii_digit() && c != '-')
            .unwrap_or(after.len());
        (digits > 0).then(|| after.split_at(digits))
    });
    match status {
        Some((code, after)) => format!("the install step failed (exit {code}){after}"),
        None => format!("the install step failed: {rest}"),
    }
}
