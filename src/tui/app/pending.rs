//! Actions in flight on a worker thread, and what they report when done.

use anyhow::Result;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

use crate::actions;
use crate::actions::git::{GitAction, Ran};
use crate::cancel::Cancel;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::state::{self, Aggregate, Phase, WorktreeRecord};
use crate::worktree::PrInfo;

use super::App;
use super::background::{AppEvent, ask_through_ui, config_now};
use super::dialogs::Modal;

pub const SPINNER_FRAMES: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

/// What a start says when the config names no process. Matched loosely:
/// the wording after it may change, and a miss only means the TUI learns
/// it on the next start instead.
const NO_PROCESSES: &str = "no processes configured";

/// Whether a line an action said on its way is worth the header as well
/// as `m`: one that calls itself a warning, says something could not be
/// done, or says a public URL is closed — which a stop or a whole restart
/// does, and otherwise the row just loses the URL, and whoever it was
/// sent to finds out first. Matched loosely too: a miss still leaves the
/// line in `m`.
fn is_warning(line: &str) -> bool {
    line.starts_with("warning:")
        || line.contains("could not")
        || (line.contains("public URL") && line.contains("is closed"))
}

/// The lines an action said on its way that are worth the header, as one,
/// or nothing when none is.
fn warnings_in(said: &[String]) -> Option<String> {
    let warnings: Vec<&str> = said
        .iter()
        .map(String::as_str)
        .filter(|line| is_warning(line))
        .collect();
    (!warnings.is_empty()).then(|| warnings.join(" · "))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Create,
    Remove,
    Start,
    Stop,
    Restart,
    Share,
    Unshare,
    StopAll,
    /// A move from the git menu.
    Git(GitAction),
}

impl PendingKind {
    /// What the row and the status line say while it runs.
    pub fn verb(self) -> &'static str {
        match self {
            PendingKind::Create => "creating",
            PendingKind::Remove => "removing",
            PendingKind::Start => "starting",
            PendingKind::Stop => "stopping",
            PendingKind::Restart => "restarting",
            PendingKind::Share => "sharing",
            PendingKind::Unshare => "unsharing",
            PendingKind::StopAll => "stopping",
            PendingKind::Git(action) => action.verb(),
        }
    }

    /// What it was trying to do, for the line that says it did not.
    pub(super) fn noun(self) -> &'static str {
        match self {
            PendingKind::Create => "create",
            PendingKind::Remove => "remove",
            PendingKind::Start => "start",
            PendingKind::Stop => "stop",
            PendingKind::Restart => "restart",
            PendingKind::Share => "share",
            PendingKind::Unshare => "unshare",
            PendingKind::StopAll => "stop",
            PendingKind::Git(action) => action.word(),
        }
    }
}

pub enum PendingOutcome {
    Created(String),
    Removed(String),
    /// The worktree, its URL, and each process the call spawned with its
    /// pid.
    Started(String, Option<String>, Vec<(String, u32)>),
    Stopped(String),
    /// A stop that found nothing of the worktree up.
    NotRunning(String),
    /// The worktree, its public URL, and whether a proxy is injecting a
    /// header in front of it.
    Shared(String, String, bool),
    Unshared(String),
    /// Every worktree `X` stopped, and every one it left running because
    /// it came up after the confirmation was shown.
    StoppedAll(actions::StopAllReport),
    /// What a move from the git menu did to the worktree.
    Git(String, Ran),
    /// A start or a restart `x` cancelled, and whether anything of the
    /// worktree was up afterwards for the stop that followed it to take
    /// down.
    Cancelled {
        name: String,
        stopped: bool,
    },
}

impl PendingOutcome {
    /// What a start or a restart of `name` reports.
    pub(super) fn started(name: String, report: &actions::StartReport) -> Self {
        let spawned = report
            .started
            .iter()
            .map(|p| (p.process.clone(), p.record.pid))
            .collect();
        PendingOutcome::Started(name, report.url.clone(), spawned)
    }
}

/// A start that returned before what it spawned was ready.
#[derive(Debug, Clone)]
pub struct AwaitingReady {
    pub name: String,
    /// The URL the start reported, for the line that says it is ready.
    pub url: Option<String>,
    /// Each process the start spawned, and its pid.
    pub spawned: Vec<(String, u32)>,
    /// A read has shown one of them under its pid. That read was taken
    /// after the start, so one a later read is missing, its pid dead, is
    /// not still to come.
    pub seen: bool,
}

/// Where a start's processes are, as one read of the state has them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Readiness {
    /// Still starting, or not in a read that may be from before the start.
    Waiting,
    Ready,
    Failed,
    /// Nothing is left to wait for: the worktree was stopped, or what the
    /// start spawned was replaced.
    Gone,
}

impl AwaitingReady {
    /// What `record` says about the processes the start spawned, and
    /// `alive` about the pid of one it no longer shows.
    ///
    /// By pid, and only those: a read taken before the start still shows
    /// the run it replaced, every process running, and one taken between
    /// a restart's stop and its start shows only the siblings it left
    /// alone, or nothing. Neither says anything about the new processes,
    /// so both are waited through. Once a read has shown one of them, a
    /// process missing from a later read whose pid is not alive was
    /// stopped or replaced since — by `x`, or a `pando stop` or `pando
    /// restart` in another pane — and is never coming. One that died by
    /// itself is still in the read, failed. A start that spawned nothing,
    /// all of it already up, has the whole record's phase.
    pub(super) fn readiness(
        &mut self,
        record: Option<&WorktreeRecord>,
        alive: impl Fn(u32) -> bool,
    ) -> Readiness {
        if self.spawned.is_empty() {
            return match record.and_then(state::aggregate_phase) {
                Some(Aggregate::Running { .. }) => Readiness::Ready,
                Some(Aggregate::Starting { .. }) => Readiness::Waiting,
                Some(Aggregate::Failed { .. }) => Readiness::Failed,
                None => Readiness::Gone,
            };
        }
        let ours: Vec<Option<&Phase>> = self
            .spawned
            .iter()
            .map(|(process, pid)| {
                let p = record?.processes.get(process)?;
                (p.pid == *pid).then_some(&p.phase)
            })
            .collect();
        self.seen |= ours.iter().any(Option::is_some);
        let gone = || {
            self.spawned
                .iter()
                .zip(&ours)
                .any(|((_, pid), phase)| phase.is_none() && !alive(*pid))
        };
        if ours
            .iter()
            .flatten()
            .any(|phase| matches!(phase, Phase::Failed { .. }))
        {
            Readiness::Failed
        } else if self.seen && gone() {
            Readiness::Gone
        } else if ours
            .iter()
            .all(|phase| matches!(phase, Some(Phase::Running { .. })))
        {
            Readiness::Ready
        } else {
            Readiness::Waiting
        }
    }
}

pub struct PendingAction {
    pub name: String,
    pub kind: PendingKind,
    /// The worktrees it acts on: its own, or every one a stop-all listed.
    pub covers: Vec<String>,
    pub rx: Receiver<Result<PendingOutcome, String>>,
    pub started_at: Instant,
    pub spinner_frame: u8,
    pub progress_rx: Option<Receiver<String>>,
    pub stage: Option<String>,
    /// Every line the worker has said, in order. The spinner shows only
    /// the newest, and a line that arrives with the outcome is never on
    /// screen at all, so `m` gets them once the action is done.
    pub said: Vec<String>,
    /// What the messages call it: the branch, which is what a developer
    /// knows it by, rather than the directory name derived from it.
    pub label: String,
    /// A create whose worker has said [`actions::INSTALLING`]: every ref
    /// it writes is written, and what is left, the install, writes none.
    pub refs_written: bool,
    /// What `x` cancels it with: a start's or a restart's, `None` for an
    /// action nothing cancels.
    pub cancel: Option<Cancel>,
}

impl PendingAction {
    /// What the row, the spinner and a refusal say it is doing:
    /// `cancelling` once `x` has cancelled it, until its undo is done.
    pub fn doing(&self) -> &'static str {
        match self.cancelled() {
            true => "cancelling",
            false => self.kind.verb(),
        }
    }

    /// Whether `x` has cancelled it.
    pub fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(Cancel::is_cancelled)
    }

    /// Whether an action of `kind` on `covers` has to wait for this one.
    ///
    /// Two on one worktree always do. A stop-all runs alone: it stops
    /// what is live when it takes the lock, and a worktree halfway
    /// through an isolated start is not live yet, so its services would
    /// be stopped under the start. A create is the exception: it starts
    /// nothing, so neither waits for the other. And the actions that
    /// write the repository's own refs — a create, a git move — run one
    /// at a time: git refuses a ref another process holds the lock of
    /// rather than waiting for it.
    ///
    /// A create holds that turn only while git runs. Its install takes
    /// minutes and writes no ref, so once its worker says the install has
    /// begun, a git move or another create goes ahead beside it.
    ///
    /// A removal writes no ref: it keeps the branch, and `git worktree
    /// remove` deletes only that worktree's own entry under
    /// `.git/worktrees`. So removals run beside each other, and beside a
    /// create or a git move on another worktree, the state lock ordering
    /// their bookkeeping as it orders two `pando rm` in two panes.
    pub(super) fn conflicts(&self, kind: PendingKind, covers: &[String]) -> bool {
        let all = |k: PendingKind| k == PendingKind::StopAll;
        let create = |k: PendingKind| k == PendingKind::Create;
        let alone = (all(self.kind) && !create(kind)) || (all(kind) && !create(self.kind));
        let repo = |k: PendingKind| matches!(k, PendingKind::Create | PendingKind::Git(_));
        alone
            || (repo(self.kind) && !self.refs_written && repo(kind))
            || self.covers.iter().any(|n| covers.contains(n))
    }

    /// Takes whatever the worker has said since the last poll.
    fn take_progress(&mut self) {
        let Some(prx) = self.progress_rx.as_ref() else {
            return;
        };
        while let Ok(line) = prx.try_recv() {
            if self.kind == PendingKind::Create && line == actions::INSTALLING {
                self.refs_written = true;
            }
            self.stage = Some(line.clone());
            self.said.push(line);
        }
    }

    /// Everything it said, once the outcome is in: a line sent just before
    /// the outcome may have arrived after this poll's first read.
    fn all_it_said(&mut self) -> Vec<String> {
        self.take_progress();
        std::mem::take(&mut self.said)
    }
}

impl App {
    /// Returns whether the action started; the create modal only closes (and
    /// throws away what was typed) once it did.
    pub(super) fn spawn_create(&mut self, branch: String, base: Option<String>) -> bool {
        let (label, dir_name) = (branch.clone(), actions::sanitize_branch_to_dir(&branch));
        self.spawn_create_with(label, dir_name, move |paths, config, progress| {
            actions::new(paths, config, &branch, base.as_deref(), progress)
        })
    }

    /// A worktree for a pull request, `p` then ⏎: its branch, or for a
    /// fork's, one fetched from `origin`'s copy of its head.
    pub(super) fn spawn_create_pr(&mut self, pr: PrInfo) -> bool {
        let branch = pr.local_branch();
        let label = format!("#{} {branch}", pr.number);
        let dir_name = actions::sanitize_branch_to_dir(&branch);
        self.spawn_create_with(label, dir_name, move |paths, config, progress| {
            actions::new_for_pr(paths, config, &pr, progress)
        })
    }

    /// Settles what `new` needs to know, asking through the modal, then
    /// runs `create` with the config that settled.
    fn spawn_create_with<F>(&mut self, label: String, dir_name: String, create: F) -> bool
    where
        F: FnOnce(&PandoPaths, &Config, &dyn Fn(&str)) -> Result<String> + Send + 'static,
    {
        let paths = self.paths.clone();
        let tx = self.event_tx.clone();
        let (ptx, prx) = mpsc::channel::<String>();
        let started = self.spawn_pending(dir_name, PendingKind::Create, move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            // What `pando new` settles before it creates anything — the
            // install command, the files to provision, the version files —
            // settled here the same way, asking through the modal where
            // detection cannot decide. Without it a worktree made with `n`
            // has no dependencies installed and its first start fails.
            let ask = |question: &actions::Question| ask_through_ui(&tx, question);
            let config = config_now(&paths)?;
            let config = actions::resolve_for_new(&paths, &config, &ask, &progress)
                .map_err(|e| format!("{e:#}"))?;
            let _ = tx.send(AppEvent::ConfigResolved(Box::new(config.clone())));
            create(&paths, &config, &progress)
                .map(PendingOutcome::Created)
                .map_err(|e| {
                    // With where to fix it, as `pando new` says it.
                    let text = format!("{e:#}");
                    actions::install_remedy(&paths, &text).unwrap_or(text)
                })
        });
        if started && let Some(p) = self.pending.last_mut() {
            p.progress_rx = Some(prx);
            p.label = label;
        }
        started
    }

    pub(super) fn spawn_remove(&mut self, name: String, yes: bool, force: bool) -> bool {
        let paths = self.paths.clone();
        let worker_name = name.clone();
        let (ptx, prx) = mpsc::channel::<String>();
        let started = self.spawn_pending(name, PendingKind::Remove, move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            actions::rm(&paths, &worker_name, yes, force, &progress)
                .map(|()| PendingOutcome::Removed(worker_name))
                .map_err(|e| format!("{e:#}"))
        });
        if started && let Some(p) = self.pending.last_mut() {
            p.progress_rx = Some(prx);
        }
        started
    }

    /// Runs `work` on a worker thread and parks the receiver. One action
    /// per worktree: a second on a worktree that has one in flight is
    /// refused rather than queued, and one on any other worktree runs
    /// beside it, as two `pando` commands in two panes do — the state
    /// lock and the ports recorded under it are what make that safe.
    /// [`PendingAction::conflicts`] says which others wait.
    pub(super) fn spawn_pending<F>(&mut self, name: String, kind: PendingKind, work: F) -> bool
    where
        F: FnOnce() -> Result<PendingOutcome, String> + Send + 'static,
    {
        let covers = vec![name.clone()];
        self.spawn_pending_on(name, covers, kind, work)
    }

    /// [`spawn_pending`](Self::spawn_pending) for an action on `covers`
    /// rather than on `name` alone: a stop-all, on every worktree it lists.
    pub(super) fn spawn_pending_on<F>(
        &mut self,
        name: String,
        covers: Vec<String>,
        kind: PendingKind,
        work: F,
    ) -> bool
    where
        F: FnOnce() -> Result<PendingOutcome, String> + Send + 'static,
    {
        // By what the spinner calls it: a stop-all has no worktree's name,
        // and a create's name is the directory, not the branch typed.
        if let Some(p) = self.pending.iter().find(|p| p.conflicts(kind, &covers)) {
            let busy = format!("{} {}", p.doing(), p.label);
            self.set_error(format!("already busy {busy}"));
            return false;
        }
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(work());
        });
        let label = self.label_of(&name);
        self.set_progress(format!("{} {} {label}…", SPINNER_FRAMES[0], kind.verb()));
        self.pending.push(PendingAction {
            label,
            name,
            kind,
            covers,
            rx,
            started_at: Instant::now(),
            spinner_frame: 0,
            progress_rx: None,
            stage: None,
            said: Vec::new(),
            refs_written: false,
            cancel: None,
        });
        true
    }

    /// The action in flight on `name`, if there is one — so its row and
    /// its detail pane say `creating…` or `waiting for your answer` rather
    /// than `stopped` while it runs.
    ///
    /// A stop-all is on every worktree it listed that still has anything
    /// up.
    pub fn pending_on(&self, name: &str) -> Option<&PendingAction> {
        self.pending.iter().find(|p| {
            p.name == name
                || (p.kind == PendingKind::StopAll
                    && p.covers.iter().any(|n| n == name)
                    && self.is_live(name))
        })
    }

    /// Whether the action in flight is blocked on the question modal.
    pub fn awaiting_answer(&self) -> bool {
        !self.pending.is_empty()
            && (matches!(self.modal, Some(Modal::Question { .. }))
                || !self.queued_questions.is_empty())
    }

    /// What an action said on its way, as one entry in `m`, before the
    /// outcome: the CLI prints every line, and the spinner here showed
    /// each only until the next.
    fn keep_what_it_said(&mut self, doing: &str, said: &[String]) {
        if !said.is_empty() {
            self.set_status(format!("{doing}: {}", said.join(" · ")));
        }
    }

    pub fn poll_pending(&mut self) {
        let mut done = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            let pending = &mut self.pending[i];
            pending.take_progress();
            match pending.rx.try_recv() {
                Ok(result) => done.push((self.pending.remove(i), Some(result))),
                Err(mpsc::TryRecvError::Disconnected) => done.push((self.pending.remove(i), None)),
                Err(mpsc::TryRecvError::Empty) => {
                    pending.spinner_frame = pending.spinner_frame.wrapping_add(1);
                    i += 1;
                }
            }
        }
        for (pending, result) in done {
            self.finish_pending(pending, result);
        }
        // `q` twice cancelled the starts in flight; pando goes once each
        // has run its undo, and abandons whatever else is still running.
        if self.quitting && !self.pending.iter().any(|p| p.cancel.is_some()) {
            self.should_quit = true;
        }
        // After the outcomes: one that finished has said so on the header,
        // and the spinner of the ones still running waits its turn.
        if self.spinner_may_take_header()
            && let Some(message) = self.spinner_line()
        {
            self.set_progress(message);
        }
    }

    /// What the header says while actions run: the one in flight with its
    /// stage and its clock, or each of several by what it is doing.
    fn spinner_line(&self) -> Option<String> {
        let first = self.pending.first()?;
        // A worker blocked on the question modal is not doing anything
        // a spinner could stand for, and its clock is the reader's.
        if self.awaiting_answer() {
            return Some(match self.pending.as_slice() {
                [one] => format!("? {} is waiting for your answer", one.label),
                _ => "? a question is waiting for your answer".to_string(),
            });
        }
        let glyph = SPINNER_FRAMES[first.spinner_frame as usize % SPINNER_FRAMES.len()];
        let elapsed = first.started_at.elapsed().as_secs();
        let suffix = if elapsed >= 3 {
            format!(" {elapsed}s")
        } else {
            String::new()
        };
        if let [pending] = self.pending.as_slice() {
            let label = format!("{} {}", pending.doing(), pending.label);
            return Some(match &pending.stage {
                Some(stage) => format!("{glyph} {label} · {stage}{suffix}"),
                None => format!("{glyph} {label}…{suffix}"),
            });
        }
        let each: Vec<String> = self
            .pending
            .iter()
            .map(|p| format!("{} {}", p.doing(), p.label))
            .collect();
        Some(format!("{glyph} {}…{suffix}", each.join(" · ")))
    }

    /// What an action that finished says: its outcome, its error, or that
    /// its worker died without either.
    fn finish_pending(
        &mut self,
        mut pending: PendingAction,
        result: Option<Result<PendingOutcome, String>>,
    ) {
        let Some(result) = result else {
            self.set_error(format!(
                "{} worker stopped unexpectedly",
                pending.kind.verb()
            ));
            return;
        };
        match result {
            Ok(outcome) => {
                let label = pending.label.clone();
                let kind = pending.kind;
                let said = pending.all_it_said();
                let doing = format!("{} {label}", pending.kind.verb());
                self.keep_what_it_said(&doing, &said);
                match outcome {
                    PendingOutcome::Created(name) => {
                        // As the detail pane's status row says it.
                        self.set_success(format!("created {label} — s starts it · ⏎ picks a mode"));
                        // The cursor goes to it as soon as discovery lists
                        // it: the next key is almost always meant for it.
                        self.select_on_arrival = Some(name);
                        self.spawn_discovery();
                    }
                    // Behind a fresh error: the row going says it too, and
                    // what removing a set of marks kept, and why, is the
                    // line still worth reading while the others go.
                    PendingOutcome::Removed(_) => {
                        self.set_success_behind_error(format!("removed {label}"));
                        self.spawn_discovery();
                    }
                    PendingOutcome::Started(name, url, spawned) => {
                        // A start returns once the processes are spawned,
                        // which is not the same as ready: a `✓ started`
                        // beside a row that still says `starting` is a
                        // claim the row contradicts. Ready is announced by
                        // the refresh that sees what it spawned running.
                        let at = url
                            .as_ref()
                            .map(|url| format!(" — {url}"))
                            .unwrap_or_default();
                        let mut awaited = AwaitingReady {
                            name: name.clone(),
                            url,
                            spawned,
                            seen: false,
                        };
                        let now =
                            awaited.readiness(self.record_for(&name), crate::process::is_alive);
                        if now == Readiness::Ready {
                            self.set_success(format!("{label} is ready{at}"));
                        } else {
                            self.set_status(format!(
                                "started {label}, waiting for it to be ready{at}"
                            ));
                            self.await_ready(awaited);
                        }
                        // The log is new, so whatever was tailed for this
                        // worktree is about the run that just ended. The
                        // scroll belongs to the selected row's tail, which
                        // is another worktree's once the cursor moved on.
                        self.log_tails.forget_worktree(&name);
                        if self.selected_worktree().is_some_and(|w| w.name == name) {
                            self.tail_scroll = 0;
                        }
                        self.spawn_refresh();
                    }
                    PendingOutcome::Stopped(_) => {
                        self.set_success(format!("stopped {label}"));
                        self.spawn_refresh();
                    }
                    // Not a tick: nothing was stopped, and `pando stop`
                    // says the same.
                    PendingOutcome::NotRunning(_) => {
                        self.set_status(format!("{label} was not running"));
                        self.spawn_refresh();
                    }
                    PendingOutcome::Shared(_, url, pre_authed) => {
                        let how = if pre_authed { " (pre-authed)" } else { "" };
                        // Long enough to read out; `m` keeps it after that.
                        self.set_lasting(format!(
                            "{label} is public at {url}{how} — O opens it, C copies it"
                        ));
                        self.spawn_refresh();
                    }
                    PendingOutcome::StoppedAll(report) => {
                        // What came up after the confirmation was shown was
                        // left running, and the header names it by branch:
                        // said only to `m`, it left "nothing was running"
                        // on the header beside a row that ran.
                        let stopped = match report.stopped.as_slice() {
                            [] if report.kept.is_empty() => "nothing was running".to_string(),
                            [] => "nothing listed was still running".to_string(),
                            [name] => format!("stopped {}", self.label_of(name)),
                            names => format!("stopped {} worktrees", names.len()),
                        };
                        let kept: Vec<String> =
                            report.kept.iter().map(|name| self.label_of(name)).collect();
                        self.set_success(match kept.as_slice() {
                            [] => stopped,
                            [one] => {
                                format!("{stopped} · {one} came up since and was left running")
                            }
                            many => format!(
                                "{stopped} · {} came up since and were left running",
                                many.join(", ")
                            ),
                        });
                        self.spawn_refresh();
                    }
                    PendingOutcome::Cancelled { name, stopped } => {
                        self.awaiting_ready.retain(|a| a.name != name);
                        self.set_success(match stopped {
                            true => format!(
                                "cancelled the {} of {label}, and stopped what it had brought up",
                                kind.noun()
                            ),
                            false => format!("cancelled the {} of {label}", kind.noun()),
                        });
                        self.spawn_refresh();
                    }
                    PendingOutcome::Unshared(_) => {
                        self.set_success(format!("{label} is no longer public"));
                        self.spawn_refresh();
                    }
                    PendingOutcome::Git(name, ran) => {
                        let action = match kind {
                            PendingKind::Git(action) => action,
                            _ => GitAction::Fetch,
                        };
                        self.git_finished(&name, &label, action, Ok(ran));
                    }
                }
                // After the outcome, so the header has them: a hook that
                // changed the worktree is what Invariant 1 promises is
                // said, and the one command that removes the volumes an
                // `rm` could not is not left to be found in `m`.
                if let Some(warnings) = warnings_in(&said) {
                    self.set_error(warnings);
                }
            }
            Err(e) => {
                let (kind, label, name) =
                    (pending.kind, pending.label.clone(), pending.name.clone());
                let said = pending.all_it_said();
                self.keep_what_it_said(&format!("{} {label}", kind.verb()), &said);
                if let PendingKind::Git(action) = kind {
                    self.git_finished(&name, &label, action, Err(e));
                    return;
                }
                // And on the header after the error, as after an outcome:
                // a restart whose start half failed has already closed the
                // share, and the error alone never said which URL had gone.
                let also = warnings_in(&said)
                    .map(|warnings| format!(" · {warnings}"))
                    .unwrap_or_default();
                // The one start failure that is not about this worktree but
                // about the project: from now on `⏎` and `s` say so up
                // front.
                if matches!(kind, PendingKind::Start | PendingKind::Restart)
                    && e.contains(NO_PROCESSES)
                {
                    self.nothing_to_run = true;
                    self.set_error_about(&name, format!("{}{also}", self.nothing_to_run_line()));
                    return;
                }
                // A create whose install step failed kept its worktree, and
                // says so itself: "could not create" in front of it
                // contradicted it, and the cursor goes to it as to any
                // worktree `n` made. One that another command recorded
                // while git checked it out is kept the same way.
                if kind == PendingKind::Create
                    && (e.contains(actions::CREATED_BUT_INSTALL_FAILED)
                        || e.contains(actions::KEPT_OVER_RACED_RECORD))
                {
                    self.set_error_about(&name, format!("{e}{also}"));
                    self.select_on_arrival = Some(name);
                    self.spawn_discovery();
                    return;
                }
                self.set_error_about(
                    &name,
                    format!("could not {} {label}: {e}{also}", kind.noun()),
                );
            }
        }
    }
}
