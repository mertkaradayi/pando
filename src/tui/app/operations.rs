//! What a key does to the selected worktree: start, stop, restart, share,
//! open its URL, copy its path.

use std::sync::mpsc;
#[cfg(not(test))]
use std::thread;

use crate::actions;
use crate::state::{Aggregate, Phase, ServiceMode};
use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::background::{AppEvent, ask_through_ui, config_now};
use super::dialogs::Modal;
use super::pending::{PendingKind, PendingOutcome};
use super::{ARM_TTL, App, Armed};

/// What `o` and `c` say of a URL whose own process is stopped, in the
/// words `pando share` uses for the same state.
fn owner_not_running(label: &str, owner: &str) -> String {
    format!("{label} is not running {owner}, the process its URL points at — s starts it")
}

/// Standard base64, for the OSC 52 payload. A dependency would be a lot of
/// machinery for one escape sequence.
/// Why the main checkout runs only one way: its services are the
/// project's own, which hold its data, and the other modes exist to give a
/// worktree data apart from main's.
pub(super) const MAIN_ONLY_SHARED: &str = "the main checkout runs on the project's own services, \
     which hold its data — isolated and namespaced are for worktrees";

pub(super) fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

impl App {
    /// The URL a worktree serves on.
    ///
    /// The one shared rule, so the row, the detail pane and `o` open the
    /// address `pando status` prints — including when a framework ignored
    /// the port it was given.
    pub fn url_of(&self, name: &str) -> Option<String> {
        actions::worktree_url(self.record_for(name)?)
    }

    /// The process a worktree's URL points at, when that one is stopped
    /// and a sibling runs: nothing answers the URL then, so the row, the
    /// detail pane, `o` and `c` treat it as a stopped worktree's.
    pub fn url_owner_not_running(&self, name: &str) -> Option<String> {
        actions::url_owner_not_running(self.record_for(name)?).map(str::to_string)
    }

    pub(super) fn selected_name(&mut self) -> Option<String> {
        match self.selected_worktree() {
            Some(wt) => Some(wt.name.clone()),
            None => {
                self.set_error("nothing selected");
                None
            }
        }
    }

    pub(super) fn start_selected(&mut self) {
        self.start_selected_with(actions::Mode::Remembered)
    }

    /// Start with private copies of the project's services. The same path
    /// as `start_selected`; the flag reaches detection too, because the
    /// services question is only worth asking when it is being answered.
    pub(super) fn start_selected_isolated(&mut self) {
        if let Some(name) = self.selected_worktree().map(|wt| wt.name.clone())
            && self.is_main(&name)
        {
            return self.set_error(MAIN_ONLY_SHARED);
        }
        self.start_selected_in(ServiceMode::Isolated, 'i')
    }

    /// Back to the project's own services, the private copies stopped:
    /// `start --shared`.
    pub(super) fn start_selected_shared(&mut self) {
        self.start_selected_in(ServiceMode::Shared, 'S')
    }

    /// A mode key on a worktree that is already up is a restart in that
    /// mode: a start would find its processes running and leave them on
    /// the services they were started against.
    ///
    /// Pressed on one that runs in another mode, it asks in a dialog,
    /// because that restart swaps the services under every process. In
    /// the mode it already runs in it is a plain restart, and asks for the
    /// key twice like `r` does.
    fn start_selected_in(&mut self, to: ServiceMode, key: char) {
        let mode = actions::Mode::from(to);
        let Some(name) = self.selected_worktree().map(|wt| wt.name.clone()) else {
            return self.start_selected_with(mode);
        };
        if self.phase_of(&name).is_none() {
            return self.start_selected_with(mode);
        }
        let runs = self.record_for(&name).map(|r| r.mode()).unwrap_or_default();
        if self.is_up(&name) && to != runs {
            self.modal = Some(Modal::SwitchMode { name, to });
            return;
        }
        let label = self.label_of(&name);
        if self.pressed_twice(key, &name, &format!("restart {label} {}", to.word())) {
            self.restart_named(name, mode, None)
        }
    }

    /// Whether anything of a worktree is up — starting or running, not
    /// only failed. Only then is there something a key would interrupt.
    pub fn is_up(&self, name: &str) -> bool {
        self.record_for(name).is_some_and(|record| {
            record
                .processes
                .values()
                .any(|p| !matches!(p.phase, crate::state::Phase::Failed { .. }))
        })
    }

    /// Whether a key that interrupts a worktree goes ahead now. On one with
    /// nothing up it always does. On one that runs, the first press only
    /// says what the second would do, and the second — the same key, the
    /// same worktree, within `ARM_TTL` — does it. A dialog for these
    /// would be read once and then answered without reading; a second
    /// press is cheap on purpose and still catches a stray one.
    pub(super) fn pressed_twice(&mut self, key: char, name: &str, what: &str) -> bool {
        !self.is_up(name) || self.pressed_again(key, name, what)
    }

    /// The second press itself, whatever it is on: whether this is the
    /// same key on the same `name` within `ARM_TTL`, and if not, the
    /// prompt that arms it.
    pub(super) fn pressed_again(&mut self, key: char, name: &str, what: &str) -> bool {
        if let Some(armed) = self.armed.take()
            && armed.key == key
            && armed.name == name
            && armed.at.elapsed() < ARM_TTL
        {
            return true;
        }
        self.armed = Some(Armed {
            key,
            name: name.to_string(),
            at: std::time::Instant::now(),
        });
        self.set_prompt(format!("{what}? {key} again to confirm · esc cancels"));
        false
    }

    /// Enter: the log of a worktree that runs, or has failed — the log is
    /// what says why — and a start for one that is stopped.
    /// ⏎ on any worktree: the mode chooser (decision 6). What it would
    /// keep is under the cursor — the mode it runs in, or last ran in, or
    /// shared for one never started — so ⏎ ⏎ is still one quick start.
    /// The logs are `l`'s.
    pub(super) fn enter_selected(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        if self.refuses_nothing_to_run() {
            return;
        }
        // The main checkout has one mode, so there is nothing to choose:
        // ⏎ starts it, on the project's own services, and says what it
        // would have switched to were it a worktree.
        if self.is_main(&name) {
            let label = self.label_of(&name);
            return match self.is_up(&name) {
                true => self.set_status(format!(
                    "{label} is the main checkout, on the project's own services — r restarts it"
                )),
                false => self.start_named(name, actions::Mode::Shared),
            };
        }
        let current = self
            .record_for(&name)
            .and_then(|record| record.mode)
            .unwrap_or_default();
        let selected = ServiceMode::ALL
            .iter()
            .position(|mode| *mode == current)
            .unwrap_or(0);
        self.modal = Some(Modal::Mode { name, selected });
    }

    /// A key in the mode chooser: move, choose, or leave it.
    pub(super) fn handle_mode_key(&mut self, key: KeyEvent, name: String, selected: usize) {
        let last = ServiceMode::ALL.len() - 1;
        let selected = match key.code {
            KeyCode::Down | KeyCode::Char('j') => (selected + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => selected.saturating_sub(1),
            KeyCode::Enter => return self.choose_mode(&name, ServiceMode::ALL[selected]),
            KeyCode::Esc | KeyCode::Char('q') => return,
            _ => selected,
        };
        self.modal = Some(Modal::Mode { name, selected });
    }

    /// The chooser's answer. A stopped worktree starts in it; a running one
    /// switches to it — every process restarts on the other services — or,
    /// in the mode it already runs in, stays exactly as it is.
    ///
    /// On the worktree the chooser names, not the row under the cursor: a
    /// discovery moves the cursor while the chooser is open, to a worktree
    /// `n` just made or to the neighbour of one that went away.
    fn choose_mode(&mut self, name: &str, chosen: ServiceMode) {
        if self.gone_from_under_dialog(name) {
            return;
        }
        let label = self.label_of(name);
        let runs = self.record_for(name).map(|r| r.mode()).unwrap_or_default();
        match self.is_up(name) {
            true if runs == chosen => self.set_status(format!(
                "{label} already runs {} — r restarts it",
                chosen.word()
            )),
            true => self.restart_named(name.to_string(), actions::Mode::from(chosen), None),
            false => self.start_named(name.to_string(), actions::Mode::from(chosen)),
        }
    }

    /// The switch-mode dialog's `y`: a restart in `to` of the worktree the
    /// dialog names, for the same reason the chooser acts on its own.
    pub(super) fn switch_mode(&mut self, name: String, to: ServiceMode) {
        if self.gone_from_under_dialog(&name) {
            return;
        }
        self.restart_named(name, actions::Mode::from(to), None)
    }

    /// Whether the project has nothing to run, said on the status line if
    /// so. The config is read again first — a `[dev]` added while the TUI
    /// is open is what somebody reading that line goes and does — so the
    /// next key after the edit starts it.
    fn refuses_nothing_to_run(&mut self) -> bool {
        if !self.nothing_to_run {
            return false;
        }
        if let Ok(loaded) = crate::config::load(&self.paths)
            && !loaded.config.processes.is_empty()
        {
            self.config = loaded.config;
            self.namespace_shared = std::cell::OnceCell::new();
            self.app_manifests = Default::default();
            self.app_manifests = Default::default();
            self.nothing_to_run = false;
            return false;
        }
        self.set_status(self.nothing_to_run_line());
        true
    }

    fn start_selected_with(&mut self, mode: actions::Mode) {
        if let Some(name) = self.selected_name() {
            self.start_named(name, mode)
        }
    }

    fn start_named(&mut self, name: String, mode: actions::Mode) {
        if self.refuses_nothing_to_run() {
            return;
        }
        let paths = self.paths.clone();
        let worker_name = name.clone();
        let tx = self.event_tx.clone();
        let (ptx, prx) = mpsc::channel::<String>();
        let started = self.spawn_pending(name, PendingKind::Start, move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            // Detection may have a question; it goes back to the UI thread
            // and this worker waits for the answer.
            let ask = |question: &actions::Question| ask_through_ui(&tx, question);
            let config = config_now(&paths)?;
            let config =
                actions::resolve_for_start(&paths, &config, &worker_name, mode, &ask, &progress)
                    .map_err(|e| format!("{e:#}"))?;
            // Back to the UI thread at once: an answer written to
            // `pando.toml` that this session's own copy does not have is
            // one the next keypress asks all over again.
            let _ = tx.send(AppEvent::ConfigResolved(Box::new(config.clone())));
            actions::start(&paths, &config, &worker_name, None, mode, &progress)
                .map(|report| PendingOutcome::started(worker_name.clone(), &report))
                .map_err(|e| format!("{e:#}"))
        });
        if started && let Some(p) = self.pending.as_mut() {
            p.progress_rx = Some(prx);
        }
    }

    pub(super) fn stop_selected(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let label = self.label_of(&name);
        if !self.pressed_twice('x', &name, &format!("stop {label}")) {
            return;
        }
        let paths = self.paths.clone();
        let worker_name = name.clone();
        // A stop reaches the sweep that closes a *sibling's* half-dead
        // share, and that notice is the only warning its public URL has
        // gone — so `stop` narrates like `start` does.
        let (ptx, prx) = mpsc::channel::<String>();
        let started = self.spawn_pending(name, PendingKind::Stop, move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            actions::stop(&paths, &worker_name, None, &progress)
                .map(|outcome| match outcome {
                    actions::StopOutcome::Stopped(_) => PendingOutcome::Stopped(worker_name),
                    actions::StopOutcome::NotRunning => PendingOutcome::NotRunning(worker_name),
                })
                .map_err(|e| format!("{e:#}"))
        });
        if started && let Some(p) = self.pending.as_mut() {
            p.progress_rx = Some(prx);
        }
    }

    /// The public URL of a worktree, when it has one.
    pub fn public_url_of(&self, name: &str) -> Option<String> {
        Some(self.record_for(name)?.share.as_ref()?.public_url.clone())
    }

    pub(super) fn open_selected_public_url(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let Some(url) = self.public_url_of(&name) else {
            self.set_error(format!("{name} is not shared — t shares it"));
            return;
        };
        self.open_url(&url);
        self.set_success(format!("opened {url}"));
    }

    /// One key for both directions, and both ask first: sharing puts the
    /// dev server on the internet, and unsharing takes a URL away from
    /// somebody who may be looking at it right now.
    pub(super) fn toggle_share(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        self.modal = Some(match self.public_url_of(&name) {
            Some(url) => Modal::Unshare { name, url },
            None => Modal::Share { name },
        });
    }

    pub(super) fn share_selected(&mut self, name: String) {
        let paths = self.paths.clone();
        let worker_name = name.clone();
        let (ptx, prx) = mpsc::channel::<String>();
        // A tunnel takes up to thirty seconds to publish, and an auth
        // command as long as it takes: all of it on the worker, with the
        // sub-steps coming back as progress.
        let started = self.spawn_pending(name, PendingKind::Share, move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            // The file as it is now, as `pando share` reads it: an
            // `auth_cmd` deleted since the TUI opened must not still mint
            // a session for everyone with the URL.
            let config = config_now(&paths)?;
            actions::share(&paths, &config, &worker_name, &progress)
                .map(|outcome| {
                    PendingOutcome::Shared(outcome.name, outcome.public_url, outcome.pre_authed)
                })
                .map_err(|e| format!("{e:#}"))
        });
        if started && let Some(p) = self.pending.as_mut() {
            p.progress_rx = Some(prx);
        }
    }

    pub(super) fn unshare_selected(&mut self, name: String) {
        let paths = self.paths.clone();
        let worker_name = name.clone();
        self.spawn_pending(name, PendingKind::Unshare, move || {
            actions::unshare(&paths, &worker_name)
                .map(|()| PendingOutcome::Unshared(worker_name))
                .map_err(|e| format!("{e:#}"))
        });
    }

    /// `r`. On a worktree that is not running there is nothing to
    /// restart, and what somebody pressing it wants is for it to run.
    pub(super) fn restart_selected(&mut self) {
        let stopped = self
            .selected_worktree()
            .is_some_and(|wt| self.phase_of(&wt.name).is_none());
        if stopped {
            return self.start_selected();
        }
        let Some(name) = self.selected_name() else {
            return;
        };
        let label = self.label_of(&name);
        if self.pressed_twice('r', &name, &format!("restart {label}")) {
            self.restart_named(name, actions::Mode::Remembered, None)
        }
    }

    /// `P`: only the process the detail pane's `▸` marks — `restart
    /// --only`. The others keep running, and so do their logs.
    ///
    /// On a worktree with one process it is `r`, a whole restart, and so
    /// unlike `restart --only` it closes a public URL — and says which.
    pub(super) fn restart_selected_process(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let processes = self.processes_of(&name);
        let label = self.label_of(&name);
        match processes.len() {
            0 => {
                self.set_error(format!("{label} is running nothing — s starts it"));
            }
            1 => {
                if self.pressed_twice('P', &name, &format!("restart {label}")) {
                    self.restart_named(name, actions::Mode::Remembered, None)
                }
            }
            n => {
                let process = processes[self.tail_index.min(n - 1)].0.clone();
                if self.pressed_twice('P', &name, &format!("restart {process} of {label}")) {
                    self.restart_named(name, actions::Mode::Remembered, Some(process));
                }
            }
        }
    }

    fn restart_named(&mut self, name: String, mode: actions::Mode, only: Option<String>) {
        if self.phase_of(&name).is_none() && self.refuses_nothing_to_run() {
            return;
        }
        let label = only
            .as_ref()
            .map(|process| format!("{process} of {}", self.label_of(&name)));
        let paths = self.paths.clone();
        let worker_name = name.clone();
        let tx = self.event_tx.clone();
        let (ptx, prx) = mpsc::channel::<String>();
        let started = self.spawn_pending(name, PendingKind::Restart, move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            let ask = |question: &actions::Question| ask_through_ui(&tx, question);
            let config = config_now(&paths)?;
            let config =
                actions::resolve_for_start(&paths, &config, &worker_name, mode, &ask, &progress)
                    .map_err(|e| format!("{e:#}"))?;
            let _ = tx.send(AppEvent::ConfigResolved(Box::new(config.clone())));
            actions::restart(
                &paths,
                &config,
                &worker_name,
                only.as_deref(),
                mode,
                &progress,
            )
            .map(|report| PendingOutcome::started(worker_name.clone(), &report))
            .map_err(|e| format!("{e:#}"))
        });
        if started && let Some(p) = self.pending.as_mut() {
            p.progress_rx = Some(prx);
            if let Some(label) = label {
                p.label = label;
            }
        }
    }

    /// Whether anything of a worktree's is up, as
    /// [`WorktreeRecord::is_live`](crate::state::WorktreeRecord::is_live)
    /// says. `X` lists these, and stops no other one that is up.
    pub fn is_live(&self, name: &str) -> bool {
        self.record_for(name)
            .is_some_and(crate::state::WorktreeRecord::is_live)
    }

    /// The worktrees `X` would stop: every one with something up.
    pub fn stop_all_targets(&self) -> Vec<String> {
        self.state
            .worktrees
            .keys()
            .filter(|name| self.is_live(name))
            .cloned()
            .collect()
    }

    /// `X`: asks first, listing what goes down. With nothing up there is
    /// nothing to confirm.
    pub(super) fn confirm_stop_all(&mut self) {
        let names = self.stop_all_targets();
        if names.is_empty() {
            self.set_status("nothing is running");
            return;
        }
        self.modal = Some(Modal::StopAll { names });
    }

    /// The confirmation's `y`: stops what it showed, `listed`, and leaves
    /// up anything that came up after it was shown.
    pub(super) fn stop_everything(&mut self, listed: Vec<String>) {
        let paths = self.paths.clone();
        let (ptx, prx) = mpsc::channel::<String>();
        // No worktree's name: the rows it covers are found through
        // `pending_on`, and an empty name matches no row by itself.
        let started = self.spawn_pending(String::new(), PendingKind::StopAll, move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            actions::stop_all_listed(&paths, &listed, &progress)
                .map(PendingOutcome::StoppedAll)
                .map_err(|e| format!("{e:#}"))
        });
        if started && let Some(p) = self.pending.as_mut() {
            p.progress_rx = Some(prx);
            p.label = "everything".to_string();
            self.set_progress("stopping everything…");
        }
    }

    /// `o`: only while the worktree runs. A stop keeps its port
    /// assignment, and nothing serves it then; the row and the detail pane
    /// stop showing the URL, and `pando open` refuses it, failed or not
    /// running alike. The phase is asked first, as `pando open` asks it: a
    /// running worktree whose processes hold no port has no URL, and
    /// starting it is no remedy for that.
    pub(super) fn open_selected_url(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let label = self.label_of(&name);
        match self.phase_of(&name) {
            None => self.set_error(format!("{label} is not running — s starts it")),
            Some(Aggregate::Failed { .. }) => self.set_error(format!(
                "{label} has failed — l shows the log, r restarts it"
            )),
            Some(_) => match self.url_of(&name) {
                // Metro's root in a browser shows nothing anybody wants:
                // its app is opened where it runs, as `pando open` does.
                None if self
                    .record_for(&name)
                    .is_some_and(|record| !record.pageless.is_empty()) =>
                {
                    self.open_device_apps(&name, &label)
                }
                None => self.set_error(format!(
                    "{label} is running and holds no port, so it has no URL to open"
                )),
                Some(url) => match self.url_owner_not_running(&name) {
                    Some(owner) => self.set_error(owner_not_running(&label, &owner)),
                    None => {
                        self.open_url(&url);
                        self.set_success(format!("opened {url}"));
                    }
                },
            },
        }
    }

    /// Opens the app of each running process of a worktree with no page
    /// whose app a device runs: on the booted simulator, a connected
    /// Android device, or a simulator it starts, as `pando open` does.
    fn open_device_apps(&mut self, name: &str, label: &str) {
        let Some(record) = self.record_for(name).cloned() else {
            return;
        };
        let record = &record;
        let read = |device: &crate::catalog::frameworks::Device, dir: &std::path::Path| {
            self.app_manifests
                .borrow_mut()
                .entry(dir.to_path_buf())
                .or_insert_with(|| actions::read_manifest(device, dir))
                .clone()
        };
        let mut apps = Vec::new();
        let mut starting = None;
        for (process, links) in actions::app_links_with(&self.config, record, &read) {
            if !record.pageless.contains(&process) {
                continue;
            }
            match record.processes.get(&process).map(|p| &p.phase) {
                Some(Phase::Running { .. }) => apps.push((process, links)),
                Some(Phase::Starting { .. }) => starting = starting.or(Some(process)),
                _ => {}
            }
        }
        if apps.is_empty() {
            return self.set_error(match starting {
                Some(process) => {
                    format!("{label}'s {process} is still starting — o opens its app once it runs")
                }
                None => format!(
                    "{label} serves no page to open in a browser, and runs no app a simulator or \
                     a device opens"
                ),
            });
        }
        // One opening at a time: a second would start the simulator and
        // wait for its boot all over again.
        if self.app_opening {
            return self.set_status("an app is being opened already — its message says when");
        }
        self.app_opening = true;
        self.set_progress(format!("opening {label}'s app"));
        let record = record.clone();
        self.spawn_app_open(label, record, apps);
    }

    /// Opens each app on a worker thread: finding a simulator, starting
    /// one and waiting for it to boot take up to minutes. Each wait is
    /// said as progress, and the end as a success or an error. The
    /// simulators' installed builds are read there too, as `pando open`
    /// reads them: one fills a scheme the app's config computes, and one
    /// made for another SDK is not opened.
    #[cfg(not(test))]
    fn spawn_app_open(
        &mut self,
        label: &str,
        record: crate::state::WorktreeRecord,
        apps: Vec<(String, crate::catalog::frameworks::AppLinks)>,
    ) {
        let paths = self.paths.clone();
        let config = self.config.clone();
        let tx = self.event_tx.clone();
        let label = label.to_string();
        thread::spawn(move || {
            let (apps, refused) = actions::openable_apps(&paths, &config, &record, apps);
            if let Some(why) = refused.first() {
                let _ = tx.send(AppEvent::AppOpened(Err(format!("{label}'s {why}"))));
                return;
            }
            let run = |command: &str| actions::run_command(&paths, command);
            let opener = actions::Opener::new(&run);
            let say = |line: &str| {
                let _ = tx.send(AppEvent::AppOpening(line.to_string()));
            };
            let mut opened = Vec::new();
            for (process, links) in &apps {
                let result = match actions::open_app(links, &opener, &say) {
                    Ok(on) => {
                        opened.push(format!(
                            "opened {label}'s {process} in {} on {on}",
                            links.client
                        ));
                        continue;
                    }
                    Err(actions::NotOpened::Nowhere(why)) => {
                        format!("{label}: {why} — its app rows give the commands that open it")
                    }
                    Err(actions::NotOpened::Unknown(why)) => format!(
                        "{label}'s {process}: {why} — its app rows give the commands, to fill in"
                    ),
                    Err(failed) => format!("{label}'s {process}: {failed}"),
                };
                let _ = tx.send(AppEvent::AppOpened(Err(result)));
                return;
            }
            let _ = tx.send(AppEvent::AppOpened(Ok(opened.join(" · "))));
        });
    }

    /// Tests open no app: nothing under test may reach a simulator.
    #[cfg(test)]
    fn spawn_app_open(
        &mut self,
        _label: &str,
        _record: crate::state::WorktreeRecord,
        apps: Vec<(String, crate::catalog::frameworks::AppLinks)>,
    ) {
        self.opened_apps = apps.into_iter().map(|(_, links)| links.url).collect();
    }

    /// Hands a URL to the browser — `$BROWSER` when it is set, as
    /// `pando open` reads it, the desktop's opener otherwise — on a worker
    /// thread with every stream captured: a browser launcher that writes
    /// to the terminal would paint over the alternate screen. One that
    /// fails says so, rather than leaving `opened …` on screen alone.
    #[cfg(not(test))]
    fn open_url(&mut self, url: &str) {
        let commands =
            crate::env_command::browser_commands(self.launch_env.browser.as_deref(), url);
        let url = url.to_string();
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let mut failure = String::new();
            for command in commands {
                let Some((program, args)) = command.split_first() else {
                    continue;
                };
                let ran = std::process::Command::new(program)
                    .args(args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .output();
                failure = match ran {
                    Ok(out) if out.status.success() => return,
                    Ok(out) => format!("{program} could not open {url} ({})", out.status),
                    Err(e) => format!("could not run {program} to open {url}: {e}"),
                };
            }
            let _ = tx.send(AppEvent::LaunchFailed(failure));
        });
    }

    #[cfg(test)]
    fn open_url(&mut self, url: &str) {
        self.opened = Some(url.to_string());
    }

    pub(super) fn copy_selected_path(&mut self) {
        let Some(wt) = self.selected_worktree() else {
            self.set_error("nothing selected");
            return;
        };
        let path = wt.path.display().to_string();
        self.copy_to_clipboard(&path);
        self.set_success(format!("copied {path}"));
    }

    /// `c`: the local URL, shared or not. One key that copied the public
    /// URL whenever there was one pasted a tunnel address where a
    /// localhost one was wanted; each URL has its own key instead.
    ///
    /// Not once it has stopped, when the port it keeps serves nothing, nor
    /// while the process it points at is stopped and a sibling runs. A
    /// failed one's still copies: another of its processes may answer it,
    /// as the detail pane says. The phase is asked first, as `o` asks it,
    /// and a worktree that holds no port is told so in its row's own word:
    /// a failed one is not called running.
    pub(super) fn copy_selected_url(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let label = self.label_of(&name);
        let Some(phase) = self.phase_of(&name) else {
            self.set_error(format!("{label} is not running — s starts it"));
            return;
        };
        let Some(url) = self.url_of(&name) else {
            let state = match phase {
                Aggregate::Failed { .. } => "has failed",
                Aggregate::Starting { .. } | Aggregate::Running { .. } => "is running",
            };
            self.set_error(format!(
                "{label} {state} and holds no port, so it has no URL to copy"
            ));
            return;
        };
        if let Some(owner) = self.url_owner_not_running(&name) {
            self.set_error(owner_not_running(&label, &owner));
            return;
        }
        self.copy_to_clipboard(&url);
        self.set_success(format!("copied {url}"));
    }

    /// `C`: the public URL, the one handed to somebody else.
    pub(super) fn copy_selected_public_url(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let Some(url) = self.public_url_of(&name) else {
            let label = self.label_of(&name);
            self.set_error(format!("{label} is not shared — t shares it"));
            return;
        };
        self.copy_to_clipboard(&url);
        self.set_success(format!("copied {url}"));
    }

    #[cfg(test)]
    pub(super) fn copy_to_clipboard(&mut self, text: &str) {
        self.clipboard = Some(text.to_string());
    }

    /// OSC 52 first: it works on Linux and macOS, in Windows Terminal, and
    /// through tmux with `set-clipboard on`. [`clipboard_program`] is the
    /// fallback for terminals that ignore the sequence; it is spawned with
    /// every stdio redirected, never inheriting the alternate screen.
    ///
    /// The escape sequence is written from here — it is one `write` to the
    /// terminal pando already owns — but the child is not. `y` is a key
    /// handler, and waiting for `pbcopy` to drain its stdin there is a
    /// blocking wait on a child inside the frame, so it goes to a detached
    /// thread exactly as the browser opener does.
    #[cfg(not(test))]
    pub(super) fn copy_to_clipboard(&mut self, text: &str) {
        use std::io::Write as _;
        let mut stdout = std::io::stdout();
        let _ = write!(stdout, "\x1b]52;c;{}\x07", base64(text.as_bytes()));
        let _ = stdout.flush();

        let Some(program) = clipboard_program(text, crate::wsl::Wsl::here().is_some()) else {
            return;
        };
        let text = text.to_string();
        thread::spawn(move || {
            let Ok(mut child) = std::process::Command::new(program)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                return;
            };
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            // Taking the handle above closed pando's end of the pipe, so
            // the program sees EOF; this reaps it rather than leaving a
            // zombie behind every yank. It blocks a worker thread, which
            // is what worker threads are for.
            let _ = child.wait_with_output();
        });
    }
}

/// What sets the clipboard for a terminal that ignores OSC 52: `pbcopy` on
/// macOS, and Windows' `clip.exe` under WSL, for ASCII text only. clip.exe
/// reads its input in the console's code page rather than as UTF-8, so a
/// path with a non-ASCII name in it would land mangled, over the copy OSC
/// 52 had already made right in a terminal that reads the sequence.
pub(super) fn clipboard_program(text: &str, wsl: bool) -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        return Some("pbcopy");
    }
    (wsl && text.is_ascii()).then_some("clip.exe")
}
