//! The setup screen: what the first `pando` in a new project shows in
//! place of the dashboard, and the ready view it turns into.
//!
//! It follows pando's own files, never the agent: the four files whose
//! change can move the setup along are polled on the tick, by their
//! modification times, while the screen is up. A config file that
//! changed is re-read on a worker, as `config_now` lists the worktrees;
//! a check record that changed is re-read on the spot, being two small
//! files and a lock asked without waiting. Nothing here starts a check in
//! the TUI: `v` starts `pando check` as a detached child, and the screen
//! watches its record like any other check's.
//!
//! `⏎` with no settings lets pando try its own guess: the guess is worked
//! out on a worker, written by `actions`, and tested by the same detached
//! check, so the single action slot is never the setup's.

use chrono::{DateTime, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::actions::{self, OwnGuess};
use crate::config::Config;
use crate::detect;
use crate::doctor;
use crate::paths::PandoPaths;
use crate::setup::{self, CheckOutcome, FailureKind, RanBy, Setup, SetupMemory, SetupState};

use super::App;
use super::background::{AppEvent, config_now};
use super::dialogs::Modal;
use super::pending::SPINNER_FRAMES;

/// How long a `v` waits for the check it started to say anything before
/// the screen says it did not start.
pub(super) const CHECK_START_TIMEOUT: Duration = Duration::from_secs(10);

/// The setup screen's state, for as long as it is up.
pub struct SetupScreen {
    /// The config as its files say now: re-read when one changes, and
    /// adopted by the dashboard when the developer leaves.
    pub config: Config,
    /// The setup as last read: its state and the last check's record.
    pub setup: Setup,
    /// What pando's own detection proposed for the project, once it has
    /// read it; `None` while it reads.
    pub detected: Option<Vec<detect::Proposal>>,
    /// When the settings were last seen to change, for "saved just now".
    pub settings_seen_at: Option<Instant>,
    /// A `v` whose check has not yet said anything: when it was pressed,
    /// on both clocks, so the record it writes can be told from the last.
    pub check_requested: Option<(Instant, DateTime<Utc>)>,
    /// Where "let pando try on its own" stands, once `⏎` asked for it.
    pub trying: Option<Trying>,
}

/// The watch on the four files, one for the setup screen and the
/// dashboard's setup line alike: what was last seen of them, and the one
/// config re-read a worker may have in flight.
#[derive(Default)]
pub struct SetupWatch {
    /// The four files' modification times and sizes, as last seen.
    seen: Watched,
    /// A config re-read is on a worker; another change waits for it.
    reading: bool,
    read_again: bool,
}

/// What the screen's live line says, from what the files say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupLine {
    /// pando's own detection is reading the project.
    Reading,
    /// Nothing to show yet: the agent has not saved anything.
    Waiting,
    /// A `v` was pressed and its check has not written its record yet.
    Starting,
    /// Settings exist, untested.
    SettingsSaved,
    /// A check runs; its own progress lines so far.
    Testing(Vec<String>),
    /// The last check passed: the ready view.
    Passed,
    Failed {
        reason: String,
        /// Whose it is to fix: a settings failure is the agent's, a
        /// machine's or a base's the developer's.
        kind: FailureKind,
        /// A program ran the check, which is the agent the prompt went to.
        by_program: bool,
    },
    Interrupted,
    /// The check found a run question open.
    NotSetUp {
        slot: String,
    },
    /// pando's own guess, working or stopped.
    OwnGuess(Trying),
}

/// "Let pando try on its own", until the check it starts takes over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trying {
    /// The guess is being worked out, on a worker.
    Resolving,
    /// A question pando has no option for, or nothing to run after every
    /// answer: the agent's job. Nothing was written.
    CannotTell,
    /// The runtime needs a prelude line, which pando never takes on its
    /// own: doctor's line for it, and doctor's fix. Nothing was written.
    NeedsPrelude { line: String, fix: Option<String> },
}

/// What the worker trying pando's guess hands back.
pub enum Tried {
    /// The answers are on disk: the config as the files hold it now.
    Saved(Box<Config>),
    CannotTell,
    NeedsPrelude {
        line: String,
        fix: Option<String>,
    },
}

/// The four files that move a setup along: the project's own
/// `pando.toml`, the machine's `config.toml`, the committed `pando.toml`,
/// and the check's record.
type Watched = [Option<(SystemTime, u64)>; 4];

fn watched_files(paths: &PandoPaths) -> [PathBuf; 4] {
    [
        paths.config_file(),
        paths.user_config_file(),
        paths.root().join("pando.toml"),
        paths.check_file(),
    ]
}

pub(super) fn stat(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn look(paths: &PandoPaths) -> Watched {
    watched_files(paths).map(|path| stat(&path))
}

/// Whether there is anything a check could run.
pub fn has_settings(config: &Config) -> bool {
    config.runnable_processes().next().is_some()
}

impl SetupScreen {
    fn new(config: Config, setup: Setup) -> Self {
        Self {
            config,
            setup,
            detected: None,
            settings_seen_at: None,
            check_requested: None,
            trying: None,
        }
    }

    /// Whether the check has passed on today's settings: the ready view.
    pub fn is_ready(&self) -> bool {
        self.setup.state == SetupState::Ready
    }

    /// Whether `v` may start a check: there is something to run, and no
    /// check runs or is on its way.
    pub fn may_test(&self) -> bool {
        has_settings(&self.config)
            && self.setup.state != SetupState::Testing
            && self.check_requested.is_none()
            && self.trying != Some(Trying::Resolving)
    }

    /// Whether `⏎` lets pando try its own guess: nothing to run yet.
    pub fn may_try(&self) -> bool {
        !has_settings(&self.config)
    }

    /// The live line, from the state and the last record.
    pub fn line(&self) -> SetupLine {
        if let Some(trying) = self.guess_line() {
            return SetupLine::OwnGuess(trying);
        }
        let record = self.setup.last_check.as_ref();
        match self.setup.state {
            SetupState::Testing => {
                SetupLine::Testing(record.map(|r| r.progress.clone()).unwrap_or_default())
            }
            _ if self.check_requested.is_some() => SetupLine::Starting,
            SetupState::Ready => SetupLine::Passed,
            SetupState::Interrupted => SetupLine::Interrupted,
            SetupState::Failing => match record.map(|r| (&r.outcome, r.ran_by)) {
                Some((CheckOutcome::NotSetUp { slot }, _)) => {
                    SetupLine::NotSetUp { slot: slot.clone() }
                }
                Some((CheckOutcome::Failed { kind, reason }, ran_by)) => SetupLine::Failed {
                    reason: reason.clone(),
                    kind: *kind,
                    by_program: ran_by == RanBy::Program,
                },
                // Failing is decided from a finished, failed record; any
                // other reads as settings still to test.
                _ => SetupLine::SettingsSaved,
            },
            SetupState::Stale | SetupState::Untested => SetupLine::SettingsSaved,
            SetupState::New { .. } if self.detected.is_none() => SetupLine::Reading,
            SetupState::New { .. } => SetupLine::Waiting,
        }
    }

    /// pando's own guess, while it is worked out, and where it stopped
    /// while there are still no settings.
    fn guess_line(&self) -> Option<Trying> {
        match &self.trying {
            Some(Trying::Resolving) => Some(Trying::Resolving),
            Some(stopped) if self.may_try() => Some(stopped.clone()),
            _ => None,
        }
    }

    /// Whether the line has a spinner, which the tick turns.
    pub fn spinning(&self) -> bool {
        matches!(
            self.line(),
            SetupLine::Reading
                | SetupLine::Waiting
                | SetupLine::Starting
                | SetupLine::Testing(_)
                | SetupLine::OwnGuess(Trying::Resolving)
        )
    }
}

impl App {
    /// Opens the setup screen when the project is new and the developer
    /// has not skipped it, whatever git lists. Called once, as the TUI
    /// starts.
    pub fn open_setup_if_new(&mut self) {
        let setup = setup::read(&self.paths, &self.config);
        if setup.state == (SetupState::New { skipped: false }) {
            self.open_setup(setup);
        }
    }

    /// Puts the setup screen up over the dashboard, and starts pando's own
    /// detection reading the project behind it.
    pub fn open_setup(&mut self, setup: Setup) {
        self.setup_screen = Some(SetupScreen::new(self.config.clone(), setup));
        self.watch_setup_files();
        self.spawn_setup_detection();
    }

    /// What detection proposes for the project, read off the UI thread:
    /// it reads manifests, scripts and what git ignores.
    fn spawn_setup_detection(&self) {
        let project = self.paths.project.clone();
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let signals = detect::signals_of(&project);
            let proposals = detect::propose(&project.root, &signals);
            let _ = tx.send(AppEvent::SetupDetected(Box::new(proposals)));
        });
    }

    /// The spinner's frame on this tick.
    pub fn spinner(&self) -> &'static str {
        SPINNER_FRAMES[self.tick as usize % SPINNER_FRAMES.len()]
    }

    /// Starts the watch on the four files from what they are now.
    pub(super) fn watch_setup_files(&mut self) {
        self.setup_watch.seen = look(&self.paths);
    }

    /// The tick's look at the four files, for the setup screen while it
    /// is up and for the dashboard's setup line after. Returns whether to
    /// repaint.
    pub(super) fn poll_setup(&mut self) -> bool {
        if self.setup_screen.is_none() && self.setup_row.setup.is_none() {
            return false;
        }
        let now = look(&self.paths);
        let watch = &mut self.setup_watch;
        let config_changed = now[..3] != watch.seen[..3];
        let check_changed = now[3] != watch.seen[3];
        watch.seen = now;
        // The setup screen's grove quakes, so it is drawn again on every
        // tick while it is up, spinner or not.
        let mut repaint = self.setup_screen.is_some();
        if config_changed {
            self.spawn_setup_read();
        } else if self.setup_watch.reading {
            // The read in flight may have taken the record before this
            // change: the one after it takes it again.
            self.setup_watch.read_again |= check_changed;
        } else if check_changed || self.waiting_on_a_check() {
            // A running check finishes by writing its record and then
            // letting go of the lock, and a killed one only lets go: the
            // lock is asked on every tick while one runs, not only when
            // the record changes.
            let setup = setup::read(&self.paths, self.files_config());
            repaint |= self.adopt_read(setup);
        }
        repaint | self.expire_check_request()
    }

    /// Whether a check runs, or a `v` waits for one: the lock is asked
    /// on every tick until it is over.
    fn waiting_on_a_check(&self) -> bool {
        match &self.setup_screen {
            Some(screen) => {
                screen.setup.state == SetupState::Testing || screen.check_requested.is_some()
            }
            None => self.setup_row.waiting_on_a_check(),
        }
    }

    /// The config as its files said at the last read: what the setup is
    /// read against, which need not be the session's own until it is
    /// adopted.
    fn files_config(&self) -> &Config {
        match &self.setup_screen {
            Some(screen) => &screen.config,
            None => self.setup_row.files_config.as_ref().unwrap_or(&self.config),
        }
    }

    /// A fresh read goes to whoever shows it: the screen while it is up,
    /// the dashboard's line after.
    fn adopt_read(&mut self, setup: Setup) -> bool {
        if self.setup_screen.is_some() {
            self.adopt_setup(setup)
        } else {
            self.adopt_setup_row(setup)
        }
    }

    /// Re-reads the config, and the setup against it, on a worker:
    /// `config_now` lists the worktrees. One at a time.
    fn spawn_setup_read(&mut self) {
        let watch = &mut self.setup_watch;
        if watch.reading {
            watch.read_again = true;
            return;
        }
        watch.reading = true;
        let paths = self.paths.clone();
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let result = config_now(&paths).map(|config| {
                let setup = setup::read(&paths, &config);
                (config, setup)
            });
            let _ = tx.send(AppEvent::SetupRead(Box::new(result)));
        });
    }

    /// A re-read landed. A config that does not load is one somebody is
    /// writing: the last good one stays, and `m` has why. The dashboard
    /// keeps the config it read for its line only: the session starts
    /// worktrees on its own until it adopts one.
    pub(super) fn setup_read(&mut self, result: Result<(Config, Setup), String>) -> bool {
        self.setup_watch.reading = false;
        let again = std::mem::take(&mut self.setup_watch.read_again);
        let repaint = match result {
            Ok((config, setup)) => {
                match self.setup_screen.as_mut() {
                    Some(screen) => {
                        if config != screen.config {
                            screen.config = config;
                            screen.settings_seen_at = Some(Instant::now());
                        }
                    }
                    None => self.setup_row.files_config = Some(config),
                }
                self.adopt_read(setup);
                true
            }
            Err(e) => {
                self.set_error(format!("the settings do not read: {e}"));
                true
            }
        };
        if again {
            self.spawn_setup_read();
        }
        repaint
    }

    /// Takes a fresh read of the setup. A `v` stops waiting once its
    /// check is running or has written a record since it was pressed.
    fn adopt_setup(&mut self, setup: Setup) -> bool {
        let Some(screen) = self.setup_screen.as_mut() else {
            return false;
        };
        if let Some((_, pressed)) = screen.check_requested {
            let recorded = setup
                .last_check
                .as_ref()
                .is_some_and(|r| r.started_at >= pressed - chrono::Duration::seconds(1));
            if setup.state == SetupState::Testing || recorded {
                screen.check_requested = None;
            }
        }
        let changed = setup != screen.setup;
        screen.setup = setup;
        changed
    }

    /// A check that said nothing in time did not start: its log says why.
    fn expire_check_request(&mut self) -> bool {
        let Some(screen) = self.setup_screen.as_mut() else {
            return false;
        };
        if screen
            .check_requested
            .is_none_or(|(at, _)| at.elapsed() < CHECK_START_TIMEOUT)
        {
            return false;
        }
        screen.check_requested = None;
        let log = crate::tui::render::home_relative(&check_log_file(&self.paths));
        self.set_error(format!("the test did not start — {log} says why"));
        true
    }

    /// Detection has read the project.
    pub(super) fn setup_detected(&mut self, proposals: Vec<detect::Proposal>) -> bool {
        match self.setup_screen.as_mut() {
            Some(screen) => {
                screen.detected = Some(proposals);
                true
            }
            None => false,
        }
    }

    /// The setup screen's keys. Every key below is a row of
    /// `keymap::SETUP_KEYS`, which help prints and a test holds to this
    /// match.
    pub(super) fn handle_setup_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('a') => {
                self.copy_to_clipboard(setup::SETUP_PROMPT);
                self.set_success("copied the setup prompt — paste it into your coding agent");
            }
            KeyCode::Char('v') => self.test_setup(),
            KeyCode::Enter => self.setup_enter(),
            KeyCode::Esc => self.skip_setup(),
            KeyCode::Char('?') => {
                self.help_scroll = 0;
                self.modal = Some(Modal::Help);
            }
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
    }

    /// `v`: tests the settings with `pando check`, run apart from the TUI.
    fn test_setup(&mut self) {
        let Some(screen) = self.setup_screen.as_ref() else {
            return;
        };
        if !has_settings(&screen.config) {
            self.set_error("nothing to test yet: the settings come first — a copies the prompt");
            return;
        }
        if !screen.may_test() {
            self.set_status("a test is already running");
            return;
        }
        if let Err(e) = self.start_check() {
            self.set_error(format!("could not start the test: {e:#}"));
            return;
        }
        if let Some(screen) = self.setup_screen.as_mut() {
            screen.check_requested = Some((Instant::now(), Utc::now()));
        }
    }

    /// Starts `pando check` for the project, detached. Tests record the
    /// request instead: nothing under test runs a check it did not set up.
    pub(super) fn start_check(&mut self) -> anyhow::Result<()> {
        #[cfg(test)]
        {
            self.checks_started += 1;
            Ok(())
        }
        #[cfg(not(test))]
        spawn_check(&self.paths)
    }

    /// `⏎`: pando's own guess while there is nothing to run, and the
    /// dashboard once there is.
    fn setup_enter(&mut self) {
        let Some(screen) = self.setup_screen.as_ref() else {
            return;
        };
        if screen.trying == Some(Trying::Resolving) {
            self.set_status("pando is already trying its own guess");
        } else if screen.may_try() {
            self.try_on_its_own();
        } else {
            self.leave_setup();
        }
    }

    /// Lets pando try its own guess, on a worker: the resolve runs
    /// detection and git. What it answers is said where `m` keeps it.
    fn try_on_its_own(&mut self) {
        let Some(screen) = self.setup_screen.as_mut() else {
            return;
        };
        screen.trying = Some(Trying::Resolving);
        let paths = self.paths.clone();
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let notices = tx.clone();
            let progress = move |line: &str| {
                let _ = notices.send(AppEvent::Notice(line.to_string()));
            };
            let result = guess(&paths, &progress);
            let _ = tx.send(AppEvent::SetupTried(Box::new(result)));
        });
    }

    /// The guess is worked out. Saved, it is tested at once, by the same
    /// detached check `v` starts.
    pub(super) fn setup_tried(&mut self, result: Result<Tried, String>) -> bool {
        let Some(screen) = self.setup_screen.as_mut() else {
            return false;
        };
        screen.trying = None;
        match result {
            Ok(Tried::Saved(config)) => {
                if *config != screen.config {
                    screen.config = *config;
                    screen.settings_seen_at = Some(Instant::now());
                }
                self.test_setup();
            }
            Ok(Tried::CannotTell) => screen.trying = Some(Trying::CannotTell),
            Ok(Tried::NeedsPrelude { line, fix }) => {
                screen.trying = Some(Trying::NeedsPrelude { line, fix });
            }
            Err(e) => self.set_error(format!("pando could not try its own guess: {e}")),
        }
        true
    }

    /// `⏎`: the dashboard, with the settings the files hold now.
    fn leave_setup(&mut self) {
        if let Some(screen) = self.setup_screen.take() {
            self.adopt_config(screen.config);
        }
    }

    /// `esc`: the dashboard, and never this screen again for the project.
    /// A skip that cannot be saved still goes to the dashboard: the screen
    /// is never a gate.
    fn skip_setup(&mut self) {
        let mut memory = SetupMemory::load(&self.paths);
        memory.skipped_at = Some(Utc::now());
        if let Err(e) = memory.save(&self.paths) {
            self.set_error(format!("could not remember the skip: {e:#}"));
        }
        self.leave_setup();
    }
}

/// pando's own guess, from the settings as the files hold them now. A
/// runtime it will not take is said in doctor's own words.
fn guess(paths: &PandoPaths, progress: &dyn Fn(&str)) -> Result<Tried, String> {
    let config = config_now(paths)?;
    let guessed =
        actions::try_on_its_own(paths, &config, progress).map_err(|e| format!("{e:#}"))?;
    Ok(match guessed {
        OwnGuess::Saved(_) => Tried::Saved(Box::new(config_now(paths)?)),
        OwnGuess::NoOption(_) | OwnGuess::NothingToRun => Tried::CannotTell,
        OwnGuess::NeedsPrelude(report) => {
            let shell = actions::runtime_shell(paths.root());
            let machine = actions::Machine::here(&shell);
            match doctor::runtime_findings(paths, &config, &machine)
                .into_iter()
                .next()
            {
                Some(finding) => Tried::NeedsPrelude {
                    line: finding.message,
                    fix: finding.fix,
                },
                // Doctor sees it no longer: the lines the question had.
                None => Tried::NeedsPrelude {
                    line: report.first().cloned().unwrap_or_default(),
                    fix: Some(report.get(1..).unwrap_or_default().join("\n")),
                },
            }
        }
    })
}

/// Where a check the TUI started writes what it prints: under the
/// project's pando directory, never the repository.
pub(super) fn check_log_file(paths: &PandoPaths) -> PathBuf {
    paths.project_dir().join("tui-check.log")
}

/// Starts `pando check` in the project as a detached child: its own
/// session, so quitting the TUI or closing the terminal abandons nothing,
/// its output to a log under the pando home, and nothing waiting on it
/// but a thread that reaps it.
#[cfg(not(test))]
fn spawn_check(paths: &PandoPaths) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe().context("find pando's own binary")?;
    let log_path = check_log_file(paths);
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    }
    let log = std::fs::File::create(&log_path)
        .with_context(|| format!("create {}", log_path.display()))?;
    let mut command = Command::new(exe);
    command
        .arg("check")
        .current_dir(paths.root())
        // The home this TUI uses, whatever it was resolved from.
        .env("PANDO_HOME", &paths.home)
        .env(crate::actions::CHECK_RAN_BY_ENV, "tui")
        .stdin(Stdio::null())
        .stdout(log.try_clone().context("share the check's log")?)
        .stderr(log);
    let (mut child, _) =
        crate::platform::process::spawn_group(&mut command).context("start pando check")?;
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
