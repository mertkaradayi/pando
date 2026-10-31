//! TUI state and the key handlers.
//!
//! Two rules from the origin tool are carried unchanged, because breaking
//! either one is immediately visible to the user:
//!
//! - Nothing reachable from here calls a blocking `.status()` or a bare
//!   `.spawn()` that inherits the terminal. A child that writes to the
//!   terminal paints over the alternate screen.
//! - No git or network call runs inline in a key handler. They all run on a
//!   worker thread and report back as an `AppEvent`, or the frame freezes.
//!
//! `App` and its core key dispatch live here; the rest of its methods are
//! split by concern across the submodules, each adding its own `impl App`.

mod ages;
mod background;
mod dialogs;
mod git_menu;
mod keymap;
mod launch;
mod log_keys;
mod log_view;
mod marks;
mod merged;
mod mouse;
mod operations;
mod pending;
mod remedies;
mod removing;
mod setup;
mod setup_row;
mod sort;
mod tails;
mod themes;

pub use ages::{compact_age, parse_git_relative};
pub use background::{AppEvent, Snapshot, snapshot};
pub use dialogs::{
    BranchLoadState, CreateRow, Modal, RemoveBlocker, base_choices, create_rows, pr_rows,
};
pub use git_menu::{BY_HAND_KEY, GitStage};
pub use keymap::{
    INSPECT_LEGEND, KeyHelp, LEADER_KEYS, LIST_KEYS, LIST_LEGEND, LIST_MOUSE, LOG_KEYS, LOG_MOUSE,
    MOUSE_NOTE, MouseHelp, OVERLAY_KEYS, SETUP_KEYS, SETUP_LEGEND,
};
pub use launch::{
    Launch, LaunchEnv, LaunchRequest, is_terminal_editor, plan_editor, plan_shell, said_after,
};
pub use log_view::{LOG_VIEWER_CAPACITY, LineInspect, LogFilter, LogView, SearchMode, SearchState};
pub use marks::MarkedRemoval;
pub use merged::{ALL_SOURCE, MergedTail, SOURCE_SEPARATOR, ViewTail, strip_source};
pub use mouse::{Clicked, Hits};
pub use pending::{AwaitingReady, PendingAction, PendingKind, PendingOutcome, SPINNER_FRAMES};
pub use remedies::as_tui_remedy;
pub use setup::{SetupLine, SetupScreen, SetupWatch, Trying, has_settings};
pub use setup_row::{SetupHint, SetupRow};
pub use sort::ListSort;
pub use tails::LogTails;
pub use themes::ThemeState;

use anyhow::Result;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::ListState;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use crate::actions;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::state::{self, Aggregate, ProcessRecord, State, WorktreeRecord};
use crate::worktree::{self, GhAccount, PrInfo, Worktree};
use pending::Readiness;

/// How long a status message stays on the header before the counts return.
/// An error stays longer than a confirmation: it is the one that has to be
/// read to the end, and acted on.
const STATUS_TTL: Duration = Duration::from_secs(6);

const ERROR_TTL: Duration = Duration::from_secs(15);

/// A result somebody may want to read twice — a public URL — stays longer
/// again, and `m` has it after that.
const LASTING_TTL: Duration = Duration::from_secs(30);

/// What the running `pando check` is called wherever the TUI names it:
/// the same words the CLI uses.
pub use crate::worktree::CHECK_LABEL;

/// Messages `m` keeps, newest last.
const MESSAGE_HISTORY: usize = 50;

/// How long a first press of a key that interrupts a running worktree
/// waits for the second one. Long enough to read the prompt, short enough
/// that a stray press later does not find it still waiting.
pub const ARM_TTL: Duration = Duration::from_secs(3);

/// Ticks between repaints that nothing asked for, so the uptimes on screen
/// keep counting. A paint is a millisecond or two; a clock that says
/// `up 16s` for a minute is a bug report.
const CLOCK_EVERY: u32 = 4;

/// Ticks between porcelain re-discoveries. The fs watcher only sees pando's
/// own worktrees directory, so adopted worktrees elsewhere arrive here.
pub const SLOW_TICK_EVERY: u32 = 20;

/// Ticks between re-reads of the selected worktree's git state — whether
/// it has uncommitted changes, how far it has drifted. Somebody editing a
/// file in another pane expects the `*` to appear while they look.
pub const GIT_SELECTED_EVERY: u32 = SLOW_TICK_EVERY;

/// Ticks between re-reads of every worktree's git state. A `git status`
/// apiece, so far rarer than the selected one's.
pub const GIT_ALL_EVERY: u32 = SLOW_TICK_EVERY * 6;

/// Ticks between asks of GitHub for the project's pull requests, about a
/// minute: a pull request merged or closed on GitHub turns its row's chip
/// while the TUI is open, rather than only after it is opened again. Two
/// `gh` calls apiece, so far rarer than anything local.
pub const PR_EVERY: u32 = SLOW_TICK_EVERY * 12;

/// The least time between two refreshes the terminal's focus asks for:
/// coming back to the pane is when a merge done elsewhere is looked for,
/// and switching back and forth must not ask GitHub every time.
const FOCUS_REFRESH_GAP: Duration = Duration::from_secs(10);

/// Ticks between process-state refreshes. Much faster than discovery,
/// because a dev server that just died should turn red while the developer
/// is still looking at it — and far cheaper, because it reads one state file
/// rather than forking git.
pub const REFRESH_EVERY: u32 = 4;

/// A process that died: its worktree, its name, why, and when.
type Death = (String, String, String, chrono::DateTime<chrono::Utc>);

/// A question a worker is blocked on, and where its answer goes.
pub type QueuedQuestion = Box<(actions::Question, Sender<Result<actions::Answer, String>>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
}

/// What the whole screen is showing: the list with its detail pane, or one
/// worktree's log full screen.
pub enum View {
    List,
    Log(Box<LogView>),
}

/// What the last probe said about the project's services: the shared ones
/// the header chips show, and the private ones of the worktree that was
/// selected, the only ones on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceHealth {
    pub shared: Vec<actions::ServiceStatus>,
    pub worktrees: BTreeMap<String, Vec<actions::ServiceStatus>>,
}

/// A key pressed once on a worktree that runs, waiting to be pressed again
/// on the same worktree: `r` `x` `P`, and `i` `S` in the mode it already
/// runs in. Any other key disarms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Armed {
    pub key: char,
    pub name: String,
    pub at: Instant,
}

/// What a status message is, which decides its mark, its colour and how
/// long it stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    /// A plain note: `refreshing…`, `log: api`.
    Info,
    /// Something finished and worked.
    Success,
    Error,
    /// An action in flight. The spinner is in the message itself, and the
    /// tick rewrites it until the action is done.
    Progress,
}

#[derive(Debug, Clone)]
pub struct Status {
    pub message: String,
    pub kind: StatusKind,
    pub at: Instant,
    pub ttl: Duration,
    /// The worktree it is about, when it is about one: the log viewer of
    /// another worktree does not show it.
    pub about: Option<String>,
}

impl Status {
    pub fn is_error(&self) -> bool {
        self.kind == StatusKind::Error
    }

    fn fresh(&self) -> bool {
        self.at.elapsed() < self.ttl
    }
}

pub struct App {
    pub paths: PandoPaths,
    pub config: Config,
    /// What a namespaced start does with each service it makes nothing in
    /// — a prefix of the worktree's own, or main's data and why: read from
    /// the env files once per config, not on every paint. Emptied wherever
    /// `config` is replaced.
    pub namespace_plan: std::cell::OnceCell<Vec<crate::actions::PlanRow>>,
    /// What each device app's manifests say about opening it, by its
    /// directory: read once per config, not on every paint. Emptied
    /// wherever `config` is replaced.
    pub app_manifests:
        std::cell::RefCell<BTreeMap<std::path::PathBuf, crate::catalog::frameworks::AppManifest>>,
    pub main: Option<Worktree>,
    pub default_base: Option<String>,
    pub worktrees: Vec<Worktree>,
    pub created_by_pando: BTreeMap<String, bool>,
    /// Process state: what is running, on which ports, and why anything
    /// failed. Replaced wholesale by each refresh.
    pub state: State,
    /// One refresh at a time. A scan of listening sockets can take a moment,
    /// and stacking them up behind a slow one helps nobody.
    pub refreshing: bool,
    /// One discovery at a time: each is a `git worktree list` and a full
    /// refresh behind the state lock. One asked for while another runs is
    /// remembered, and runs when it lands, so its reason is still read.
    pub discovering: bool,
    /// A discovery was asked for while one ran.
    pub discover_again: bool,
    /// The next discovery resolves the base branch again rather than
    /// reusing the last answer: the slow git tick asked for it, perhaps
    /// while another discovery ran.
    pub resolve_base: bool,
    /// Whether the project's services are answering. Computed on the
    /// refresh worker, never in a paint or a key handler.
    pub service_health: ServiceHealth,
    /// Which options of an open multi-select question are ticked. Beside
    /// the modal rather than inside it, so the modal stays the plain
    /// `Clone` value the renderer takes.
    pub question_checked: Vec<usize>,
    /// Open log tails, keyed by worktree, for the detail pane.
    pub log_tails: LogTails,
    /// Which of the selected worktree's processes the tail is showing, by
    /// index into its process list. Clamped on every read, because the
    /// list changes under it as processes start and stop.
    pub tail_index: usize,
    /// Lines scrolled back from the end of the tail.
    pub tail_scroll: usize,
    /// How many rows the detail pane last gave the tail, so PgUp/PgDn move
    /// by a screenful of whatever is actually on screen.
    pub tail_rows: usize,
    /// The list, or one worktree's log full screen.
    pub view: View,
    /// The pretty-print overlay for one line of the open log (`J`).
    pub inspect: Option<LineInspect>,
    /// Rows the viewer's body last had. Set by the paint, read by the key
    /// handler, so a half-page is half of what is really on screen.
    pub viewer_height: usize,
    /// The last ownership warning shown, so a standing one is reported when
    /// it appears or changes rather than on every refresh.
    pub state_warning: Option<String>,
    pub prs: HashMap<String, PrInfo>,
    /// Every pull request the last fetch listed, in `gh`'s order: `prs`
    /// is keyed by branch, and two forks' `main` are one key.
    pub pr_list: Vec<PrInfo>,
    /// A fetch is on its way; the picker says so rather than "none".
    pub pr_fetching: bool,
    /// A fetch was asked for while one ran — `R` after a merge, while the
    /// periodic one was still out — and runs once that one lands, so what
    /// it asked about is read.
    pub pr_fetch_again: bool,
    /// When the terminal's focus last brought a refresh, so coming back
    /// and forth asks GitHub once in [`FOCUS_REFRESH_GAP`].
    pub focus_refreshed: Option<Instant>,
    /// A re-read of git state asked for while another ran, kept until it
    /// lands rather than dropped: `None` inside for every worktree, or
    /// the names asked for.
    pub git_again: Option<Option<Vec<String>>>,
    /// An app is being opened on a simulator or a device: a second `o`
    /// waits for it rather than starting the simulator twice.
    pub app_opening: bool,
    /// Why the last fetch failed, for the picker to say.
    pub pr_error: Option<String>,
    /// Which GitHub account `gh` acts as for this project; `None` while
    /// the first answer is on its way.
    pub gh_account: Option<GhAccount>,
    pub list_state: ListState,
    /// The order of the list's rows: `[ui] sort`, then whatever `,` chose.
    pub sort: ListSort,
    pub filter: String,
    pub filtered_indices: Vec<usize>,
    /// The worktrees `+` marked, by name: `s` `x` `r` and `d` act on all
    /// of them while there are any. Dropped when a worktree goes.
    pub marks: BTreeSet<String>,
    pub mode: Mode,
    pub modal: Option<Modal>,
    /// The first press of a key that asks for a second.
    pub armed: Option<Armed>,
    /// `space` was pressed: the next key is one of `keymap::LEADER_KEYS`,
    /// as after neovim's leader.
    pub leader: bool,
    pub help_scroll: usize,
    /// The furthest help or messages can scroll on the screen it was last
    /// painted on. Written by the paint, read by the scroll keys.
    pub help_scroll_max: usize,
    pub status: Option<Status>,
    /// Every message the header has shown, newest last, for `m`: a
    /// one-row header cuts a long error off, and a flash that has expired
    /// is otherwise gone.
    pub messages: VecDeque<Status>,
    /// A worktree to put the cursor on as soon as a discovery lists it —
    /// the one `n` just created.
    pub select_on_arrival: Option<String>,
    /// Workers' questions that arrived while somebody was typing, or
    /// while another question was open, held in order until the keyboard
    /// is free so keystrokes meant for something else do not answer them.
    pub queued_questions: VecDeque<QueuedQuestion>,
    /// The actions in flight, oldest first, at most one per worktree.
    pub pending: Vec<PendingAction>,
    /// Where each removal the last refresh found marked stands — `pando
    /// rm` in another pane, or one a quit interrupted — read on the
    /// refresh worker, never in a paint.
    pub removals: BTreeMap<String, actions::RemovalState>,
    /// Announced reads of git state in flight: the first, and one for the
    /// worktrees each discovery finds, which can overlap. The header says
    /// `reading git` while there is one.
    pub enriching: usize,
    /// A quiet re-read of git state is in flight: the periodic one, which
    /// the header does not announce the way it does the first. There is at
    /// most one, as none starts while any read is under way.
    pub git_refreshing: bool,
    /// What each worktree's commit age was when enrichment reported it, and
    /// when that was, so the age on screen keeps counting: git's `%cr` is a
    /// string, and a string does not age.
    pub commit_seen: HashMap<String, (String, i64, Instant)>,
    /// The project has nothing to run: a start already failed because the
    /// config names no process. Never concluded from the config alone —
    /// `new` writes a `pando.toml` with no process in it, and the start
    /// is what asks for one. `⏎` then says where to add one instead of
    /// pretending to start.
    pub nothing_to_run: bool,
    /// Each worktree whose start returned before it was ready, the URL
    /// the start reported, and what it spawned: the refresh that finds
    /// those processes running says so.
    pub awaiting_ready: Vec<AwaitingReady>,
    /// The failure last announced for each worktree's process, by when it
    /// failed, so the same death is never announced twice.
    pub deaths_told: HashMap<(String, String), chrono::DateTime<chrono::Utc>>,
    /// The silent ports already announced, per worktree and port, so a
    /// dead half is said once and not on every refresh.
    pub silence_told: std::collections::HashSet<(String, u16)>,
    pub should_quit: bool,
    /// `q` twice cancelled the starts in flight: pando quits as soon as
    /// each has run its undo.
    pub quitting: bool,
    pub tick: u32,
    /// Where the last frame put what the mouse acts on. Written by the
    /// paint, read by the mouse.
    pub hits: Hits,
    /// The last press on a worktree or on a picker's option, and which,
    /// while the next press could still make it a double click.
    pub last_click: Option<(Instant, Clicked)>,
    /// The drag under way has been told how to select text: once a drag,
    /// until the button is pressed or let go again.
    pub drag_told: bool,
    pub event_tx: Sender<AppEvent>,
    pub event_rx: Option<Receiver<AppEvent>>,
    /// How `!` and `e` leave pando: inside tmux or not, which shell, which
    /// editor. Read once at startup.
    pub launch_env: LaunchEnv,
    /// A shell or editor `!` or `e` asked for, waiting for the event loop,
    /// which owns the terminal, to run it.
    pub launch: Option<LaunchRequest>,
    /// The colour theme in force, every theme there is, and what chose it.
    pub theme: ThemeState,
    /// The setup screen, while it is up: from the first `pando` in a new
    /// project until the developer leaves it, whatever the setup does
    /// meanwhile.
    pub setup_screen: Option<SetupScreen>,
    /// Where the setup stands, for the dashboard's header line once the
    /// setup screen is gone or was never up.
    pub setup_row: SetupRow,
    /// The four files both of those follow, polled on the tick.
    pub setup_watch: SetupWatch,
    /// The checks `v` asked for, counted instead of started when tests
    /// drive the app.
    #[cfg(test)]
    pub checks_started: usize,
    /// Set instead of touching the terminal when tests drive the app.
    #[cfg(test)]
    pub clipboard: Option<String>,
    /// The URL a test asked to open, instead of handing it to the desktop.
    #[cfg(test)]
    pub opened: Option<String>,
    /// The app URLs a test asked to open on a simulator or a device,
    /// instead of reaching one.
    #[cfg(test)]
    pub opened_apps: Vec<String>,
}

impl App {
    pub fn new(paths: PandoPaths, config: Config) -> Result<Self> {
        let (event_tx, event_rx) = mpsc::channel();
        let theme_settings = config.ui.theme_settings();
        let sort = ListSort::from_config(&config);
        let themes_dir = paths.themes_dir();
        let mut app = Self {
            paths,
            config,
            namespace_plan: std::cell::OnceCell::new(),
            app_manifests: Default::default(),
            main: None,
            default_base: None,
            worktrees: Vec::new(),
            created_by_pando: BTreeMap::new(),
            state: State::new(),
            service_health: ServiceHealth::default(),
            question_checked: Vec::new(),
            refreshing: false,
            discovering: false,
            discover_again: false,
            resolve_base: false,
            log_tails: LogTails::default(),
            tail_index: 0,
            tail_scroll: 0,
            tail_rows: 0,
            view: View::List,
            inspect: None,
            viewer_height: 0,
            state_warning: None,
            prs: HashMap::new(),
            pr_list: Vec::new(),
            pr_fetching: false,
            pr_fetch_again: false,
            focus_refreshed: None,
            git_again: None,
            app_opening: false,
            pr_error: None,
            gh_account: None,
            list_state: ListState::default(),
            sort,
            filter: String::new(),
            filtered_indices: Vec::new(),
            marks: BTreeSet::new(),
            mode: Mode::Normal,
            modal: None,
            armed: None,
            leader: false,
            help_scroll: 0,
            help_scroll_max: usize::MAX,
            status: None,
            messages: VecDeque::new(),
            select_on_arrival: None,
            queued_questions: VecDeque::new(),
            pending: Vec::new(),
            removals: BTreeMap::new(),
            enriching: 0,
            git_refreshing: false,
            commit_seen: HashMap::new(),
            nothing_to_run: false,
            awaiting_ready: Vec::new(),
            deaths_told: HashMap::new(),
            silence_told: std::collections::HashSet::new(),
            should_quit: false,
            quitting: false,
            tick: 0,
            hits: Hits::default(),
            last_click: None,
            drag_told: false,
            event_tx,
            event_rx: Some(event_rx),
            launch_env: LaunchEnv::from_env(),
            launch: None,
            theme: ThemeState::new(theme_settings, &themes_dir),
            setup_screen: None,
            setup_row: SetupRow::default(),
            setup_watch: SetupWatch::default(),
            #[cfg(test)]
            checks_started: 0,
            #[cfg(test)]
            clipboard: None,
            #[cfg(test)]
            opened: None,
            #[cfg(test)]
            opened_apps: Vec::new(),
        };
        // The pull requests the last fetch found, so the first frame has
        // their chips and is already in their order; the fetch below
        // replaces them.
        app.prs = crate::cache::load_prs(&app.paths.pr_cache_file())
            .prs
            .into_iter()
            .collect();
        // One synchronous read so the first frame has rows, hydrated from the
        // disk cache so those rows already carry sha, age, and dirty state.
        app.apply_snapshot(snapshot(&app.paths, None, false)?);
        app.hydrate_from_cache();
        // Whatever git lists: a new project is new with worktrees too.
        app.open_setup_if_new();
        app.read_setup_row();
        app.watch_setup_files();
        app.spawn_enrichment(None, false);
        app.spawn_pr_fetch();
        app.spawn_gh_account_check();
        Ok(app)
    }

    /// Returns whether the frame needs repainting.
    pub fn handle_event(&mut self, ev: AppEvent) -> bool {
        let repaint = self.dispatch_event(ev);
        self.open_queued_question() || repaint
    }

    fn dispatch_event(&mut self, ev: AppEvent) -> bool {
        match ev {
            AppEvent::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                self.handle_key(key);
                true
            }
            AppEvent::Input(Event::Resize(..)) => true,
            AppEvent::Input(Event::Mouse(mouse)) => self.handle_mouse(mouse),
            AppEvent::Input(Event::FocusGained) => {
                self.focus_gained();
                false
            }
            AppEvent::Input(_) => false,
            AppEvent::Tick => {
                self.tick = self.tick.wrapping_add(1);
                let spinning = !self.pending.is_empty();
                let setup = self.poll_setup() | self.poll_setup_row();
                self.poll_pending();
                let grew = self.poll_logs();
                if self.tick.is_multiple_of(SLOW_TICK_EVERY) {
                    self.spawn_discovery();
                } else if self.tick.is_multiple_of(REFRESH_EVERY) || self.selection_unprobed() {
                    self.spawn_refresh();
                }
                if self.tick.is_multiple_of(PR_EVERY) && self.gh_may_answer() {
                    self.spawn_pr_fetch();
                }
                if self.tick.is_multiple_of(GIT_ALL_EVERY) {
                    self.refresh_git(None);
                } else if self.tick.is_multiple_of(GIT_SELECTED_EVERY) {
                    let selected = self.selected_worktree().map(|w| w.name.clone());
                    if let Some(name) = selected {
                        self.refresh_git(Some(vec![name]));
                    }
                }
                let clock =
                    self.tick.is_multiple_of(CLOCK_EVERY) && matches!(self.view, View::List);
                spinning || setup || grew || self.expire_status() || clock
            }
            AppEvent::FsChange => {
                self.spawn_discovery();
                false
            }
            AppEvent::Discovered(result) => {
                self.discovering = false;
                match *result {
                    Ok(snapshot) => {
                        let fresh = self.apply_snapshot(snapshot);
                        if !fresh.is_empty() {
                            self.hydrate_from_cache();
                            self.spawn_enrichment(Some(fresh), false);
                        }
                    }
                    Err(e) => self.set_error(format!("refresh failed: {e}")),
                }
                if std::mem::take(&mut self.discover_again) {
                    self.spawn_discovery();
                }
                true
            }
            AppEvent::Enrich(update) => {
                self.note_commit_age(&update.name, update.head_age.as_deref());
                let changed = match self.worktrees.iter_mut().find(|w| w.name == update.name) {
                    Some(wt) => {
                        let before = (wt.dirty, wt.ahead_behind, wt.head_age.clone());
                        worktree::apply_update(wt, update);
                        before != (wt.dirty, wt.ahead_behind, wt.head_age.clone())
                    }
                    None => false,
                };
                // A quiet re-read that found nothing new costs no paint.
                changed || self.enriching > 0
            }
            // Reads overlap — discovery reads what it finds whatever else
            // is under way — so the one that finished ends only itself.
            AppEvent::EnrichDone { quiet } => {
                if quiet {
                    self.git_refreshing = false;
                } else {
                    self.enriching = self.enriching.saturating_sub(1);
                }
                self.save_enrich_cache();
                if let Some(only) = self.git_again.take() {
                    self.refresh_git(only);
                }
                true
            }
            AppEvent::BranchesReady(branches) => {
                if let Some(Modal::Create {
                    branches: state, ..
                }) = self.modal.as_mut()
                {
                    *state = BranchLoadState::Ready(branches);
                    return true;
                }
                false
            }
            AppEvent::PrsReady(Ok(prs)) => {
                self.pr_fetching = false;
                self.pr_error = None;
                self.fetch_prs_again();
                self.prs = worktree::prs_by_branch(&prs).into_iter().collect();
                self.replace_pr_list(prs);
                self.save_pr_cache();
                if self.sort == ListSort::Pr {
                    self.refilter();
                }
                true
            }
            // A missing or unauthenticated `gh` just means no chips — and,
            // in the picker, the reason there is no list.
            AppEvent::PrsReady(Err(e)) => {
                self.pr_fetching = false;
                self.pr_error = Some(e);
                self.fetch_prs_again();
                matches!(self.modal, Some(Modal::PullRequests { .. }))
            }
            AppEvent::GhAccountReady(account) => {
                let changed = self.gh_account.as_ref() != Some(&account);
                self.gh_account = Some(account);
                changed
            }
            AppEvent::Refreshed(quick) => {
                self.refreshing = false;
                let background::QuickRefresh {
                    refreshed:
                        actions::Refreshed {
                            state,
                            warning,
                            notices,
                            ..
                        },
                    ran,
                } = *quick;
                // Adopted with a warning too, as a discovery adopts it: a
                // save that failed still has the phases right in memory.
                let changed = self.adopt_state(state);
                // Only a refresh that ran has a say about the warning. The
                // read taken instead when nothing can change saves nothing,
                // and its clearing a save that keeps failing had every
                // discovery say that again.
                let warning = if ran {
                    warning
                } else {
                    self.state_warning.clone()
                };
                self.report_refresh(warning, notices) || changed
            }
            AppEvent::ServiceHealth(health) => {
                let changed = *health != self.service_health;
                self.service_health = *health;
                changed
            }
            AppEvent::LaunchFailed(message) => {
                self.set_error(message);
                true
            }
            AppEvent::AppOpening(message) => {
                self.set_progress(message);
                true
            }
            AppEvent::AppOpened(result) => {
                self.app_opening = false;
                match result {
                    Ok(message) => self.set_success(message),
                    Err(message) => self.set_error(message),
                }
                true
            }
            AppEvent::Notice(message) => {
                self.set_status(message);
                true
            }
            AppEvent::Theme(resolved) => {
                self.adopt_theme(*resolved);
                true
            }
            AppEvent::ConfigResolved(config) => {
                // Without it the modal reopens on the next `s`, with the
                // rule's first candidate preselected rather than the answer
                // just given.
                self.adopt_config(*config);
                false
            }
            AppEvent::Removals(removals) => {
                let changed = *removals != self.removals;
                self.removals = *removals;
                changed
            }
            AppEvent::GitRead(read) => {
                let (name, read) = *read;
                self.git_read_arrived(&name, read)
            }
            AppEvent::SetupDetected(proposals) => self.setup_detected(*proposals),
            AppEvent::SetupRead(result) => self.setup_read(*result),
            AppEvent::SetupTried(result) => self.setup_tried(*result),
            AppEvent::AskQuestion(boxed) => {
                // Somebody typing — a branch name, a filter, a search —
                // would otherwise have their next keystrokes answer it:
                // `n` says "none", enter takes the highlighted option.
                // It waits until they are done; the status line says a
                // question is waiting meanwhile.
                if self.busy_typing() {
                    self.queued_questions.push_back(boxed);
                    return true;
                }
                self.open_question(*boxed);
                true
            }
        }
    }

    /// Replaces the session's config: a worker resolved something, or the
    /// setup screen hands over what its files say now. The two places the
    /// session's config changes while it runs.
    pub(super) fn adopt_config(&mut self, config: Config) {
        self.config = config;
        self.namespace_plan = std::cell::OnceCell::new();
        self.app_manifests = Default::default();
        // Only ever cleared here, never concluded: a config file with no
        // process in it is what `new` writes, and a start is what asks for
        // the command. `nothing_to_run` is set by a start that failed for
        // want of one, and nothing else.
        if !self.config.processes.is_empty() {
            self.nothing_to_run = false;
        }
        // The fingerprint is the config's: a setup read against the old
        // one may be stale against this.
        if self.setup_row.setup.is_some() {
            self.read_setup_row();
        }
    }

    /// Whether the keyboard belongs to something being typed or confirmed,
    /// which a question arriving now would take it from.
    ///
    /// Any modal: each one answers the keys pressed in it, and a question
    /// that replaced one would take the key meant for it — the `⏎` that
    /// confirms the mode chooser, the key that closes help or `m`.
    fn busy_typing(&self) -> bool {
        let searching = self
            .log_view()
            .is_some_and(|view| view.search_mode == SearchMode::Typing);
        self.modal.is_some() || self.typing_filter() || searching
    }

    /// Whether the list's filter line has the keyboard.
    fn typing_filter(&self) -> bool {
        self.mode == Mode::Filter && matches!(self.view, View::List)
    }

    /// A question that arrived while somebody was typing, opened once they
    /// are not.
    fn open_queued_question(&mut self) -> bool {
        if self.queued_questions.is_empty() || self.busy_typing() {
            return false;
        }
        match self.queued_questions.pop_front() {
            Some(boxed) => {
                self.open_question(*boxed);
                true
            }
            None => false,
        }
    }

    fn open_question(
        &mut self,
        (question, reply): (actions::Question, Sender<Result<actions::Answer, String>>),
    ) {
        let selected = question.preselect.unwrap_or(0);
        // The rules' own answer, ticked, so enter accepts it.
        self.question_checked = question.checked.clone();
        // No candidates to choose between means the answer can only be
        // typed, so the input line opens straight away. Never for a set
        // question, which has nothing to type, nor for one that may be
        // answered "none": the input line would take the `n` that says so.
        let custom = (question.options.is_empty() && !question.multi && !question.allow_none)
            .then(String::new);
        self.modal = Some(Modal::Question {
            question,
            selected,
            custom,
            reply,
        });
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match self.modal.take() {
            Some(modal @ (Modal::Help | Modal::Messages)) => {
                // Any key dismisses either, except scrolling it. The paint
                // clamps the scroll to what there is.
                //
                // `g` and `G` go to the top and the bottom. Every key here is
                // a row of `keymap::OVERLAY_KEYS`, held to it by a test. The bottom is
                // the furthest the last paint could scroll, so a `k` after
                // `G` moves at once rather than working off an overshoot.
                let bottom = self.help_scroll_max;
                let scroll = match key.code {
                    KeyCode::Down | KeyCode::Char('j') => Some(1),
                    KeyCode::Up | KeyCode::Char('k') => Some(-1),
                    KeyCode::PageDown | KeyCode::Char(' ') => Some(10),
                    KeyCode::PageUp => Some(-10),
                    KeyCode::Char('g') | KeyCode::Home => Some(isize::MIN),
                    KeyCode::Char('G') | KeyCode::End => Some(isize::MAX),
                    _ => None,
                };
                match scroll {
                    Some(delta) => {
                        self.help_scroll = self
                            .help_scroll
                            .min(bottom)
                            .saturating_add_signed(delta)
                            .min(bottom);
                        self.modal = Some(modal);
                    }
                    None => self.help_scroll = 0,
                }
                return;
            }
            Some(Modal::Remove {
                name,
                created_by_pando,
            }) => {
                self.handle_remove_key(key, name, created_by_pando);
                return;
            }
            Some(Modal::RemoveMarked { names }) => {
                self.handle_remove_marked_key(key, names);
                return;
            }
            Some(Modal::Create {
                input,
                branches,
                selected,
                base,
            }) => {
                self.handle_create_key(key, input, branches, selected, base);
                return;
            }
            Some(Modal::PullRequests { input, selected }) => {
                self.handle_pull_request_key(key, input, selected);
                return;
            }
            Some(Modal::StopAll { names }) => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => self.stop_everything(names),
                    KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {}
                    _ => self.modal = Some(Modal::StopAll { names }),
                }
                return;
            }
            Some(Modal::Unshare { name, url }) => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => self.unshare_selected(name),
                    // Anything else leaves it up, and the modal with it
                    // closed — the same shape the remove confirmation has.
                    KeyCode::Esc | KeyCode::Char('n') => {}
                    _ => self.modal = Some(Modal::Unshare { name, url }),
                }
                return;
            }
            Some(Modal::Share { name }) => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => self.share_selected(name),
                    KeyCode::Esc | KeyCode::Char('n') => {}
                    _ => self.modal = Some(Modal::Share { name }),
                }
                return;
            }
            Some(Modal::Theme { selected, before }) => {
                self.handle_theme_key(key, selected, before);
                return;
            }
            Some(Modal::Mode { name, selected }) => {
                self.handle_mode_key(key, name, selected);
                return;
            }
            Some(Modal::SwitchMode { name, to }) => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Enter => self.switch_mode(name, to),
                    KeyCode::Esc | KeyCode::Char('n') => {}
                    _ => self.modal = Some(Modal::SwitchMode { name, to }),
                }
                return;
            }
            Some(Modal::Question {
                question,
                selected,
                custom,
                reply,
            }) => {
                self.handle_question_key(key, question, selected, custom, reply);
                return;
            }
            Some(Modal::Git { name, stage }) => {
                self.handle_git_key(key, name, stage);
                return;
            }
            None => {}
        }
        // The setup screen owns the whole screen until it is left, and
        // only its own keys mean anything on it.
        if self.setup_screen.is_some() {
            self.handle_setup_key(key);
            return;
        }
        // The viewer owns the whole screen and the whole keyboard, so it
        // comes before the list's keys — but after the modals, because a
        // worker can still raise a question while it is open.
        if matches!(self.view, View::Log(_)) {
            self.handle_log_key(key);
            return;
        }
        if self.mode == Mode::Filter {
            self.handle_filter_key(key);
            return;
        }
        // After the leader, a key of `keymap::LEADER_KEYS`, which the footer
        // lists while it waits; anything else takes it back and says so.
        if std::mem::take(&mut self.leader) {
            match key.code {
                KeyCode::Char('g') => self.open_git_menu(),
                // The row's pull request on GitHub, where `o` is its page
                // on this machine.
                KeyCode::Char('p') => self.open_selected_pr(),
                KeyCode::Esc => self.set_status("cancelled"),
                _ => self.set_status("space, then g: the git menu, p: open its PR in the browser"),
            }
            return;
        }
        // A key waiting for its second press: esc takes it back and does
        // nothing else, where it would otherwise clear a filter or quit.
        if key.code == KeyCode::Esc && self.armed.take().is_some() {
            self.set_status("cancelled");
            return;
        }
        // Every key below is a row of `keymap::LIST_KEYS`, which help
        // prints and a test holds to this match.
        match key.code {
            // Esc unwinds a kept filter before it quits: the filter line
            // says `esc clears`, and quitting instead loses the session.
            // The marks next, so an esc meant for the filter keeps them.
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.refilter();
            }
            KeyCode::Esc if !self.marks.is_empty() => self.clear_marks(),
            KeyCode::Char('q') | KeyCode::Esc => self.quit(key.code),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('g') | KeyCode::Home => self.select_index(0),
            KeyCode::Char('G') | KeyCode::End => {
                self.select_index(self.filtered_indices.len().saturating_sub(1))
            }
            KeyCode::Char('/') => self.mode = Mode::Filter,
            KeyCode::Char('n') => self.open_create(),
            KeyCode::Char('p') => self.open_pull_requests(),
            KeyCode::Char('d') => self.open_remove(),
            KeyCode::Char('+') => self.toggle_mark(),
            // The leader, as in neovim: everything that moves the branch
            // is `space g`, and the menu shows the commands before any run.
            KeyCode::Char(' ') => self.leader = true,
            KeyCode::Char('y') => self.copy_selected_path(),
            KeyCode::Char('a') => self.copy_setup_prompt(),
            KeyCode::Char('v') => self.test_setup_from_list(),
            // The local URL, always: what gets pasted into a browser tab,
            // a curl, a chat. `C` is the public one, and never stands in.
            KeyCode::Char('c') => self.copy_selected_url(),
            KeyCode::Char('C') => self.copy_selected_public_url(),
            // With rows marked, the keys that make sense on several at
            // once act on every marked one; the rest stay the cursor's.
            KeyCode::Char('s') if !self.marks.is_empty() => self.start_marked(),
            KeyCode::Char('x') if !self.marks.is_empty() => self.stop_marked(),
            KeyCode::Char('r') if !self.marks.is_empty() => self.restart_marked(),
            KeyCode::Char('s') => self.start_selected(),
            // The key pressed on a row without thinking: which services
            // it runs on, with what it already does under the cursor.
            KeyCode::Enter => self.enter_selected(),
            // The isolated start. Its own key rather than a mode, because
            // isolation is remembered on the worktree: pressing it once is
            // what turns it on, and `s` from then on keeps it. `S` is the
            // way back.
            KeyCode::Char('i') => self.start_selected_isolated(),
            KeyCode::Char('S') => self.start_selected_shared(),
            KeyCode::Char('x') => self.stop_selected(),
            KeyCode::Char('r') => self.restart_selected(),
            // The one process the detail pane's `▸` marks, which tab moves.
            KeyCode::Char('P') => self.restart_selected_process(),
            KeyCode::Char('X') => self.confirm_stop_all(),
            KeyCode::Char('!') => self.open_shell(),
            KeyCode::Char('e') => self.open_editor(),
            KeyCode::Char('o') => self.open_selected_url(),
            // Shift-O, because the public one is the URL you give away and
            // `o` is the one you open fifty times a day.
            KeyCode::Char('O') => self.open_selected_public_url(),
            KeyCode::Char('t') => self.toggle_share(),
            KeyCode::Char('l') | KeyCode::Char('L') => self.open_log_viewer(),
            // PgUp is older, PgDn is newer — j/k stay on the list.
            KeyCode::PageUp => self.scroll_tail(-(self.tail_rows.max(1) as isize)),
            KeyCode::PageDown => self.scroll_tail(self.tail_rows.max(1) as isize),
            KeyCode::Tab => self.cycle_tail(),
            KeyCode::Char('T') => self.open_theme_picker(),
            KeyCode::Char(',') => self.cycle_sort(),
            KeyCode::Char('R') => {
                self.spawn_discovery();
                // A fetch or a commit in another pane moves the drift and
                // the `*`, which a discovery keeps from the last read
                // while the head is the same.
                self.resolve_base = true;
                self.refresh_git(None);
                // An account switched in another terminal shows here too,
                // and so does a pull request merged, closed or opened
                // since the last fetch.
                self.spawn_gh_account_check();
                self.spawn_pr_fetch();
                self.set_status("refreshing…");
            }
            KeyCode::Char('?') => {
                self.help_scroll = 0;
                self.modal = Some(Modal::Help);
            }
            KeyCode::Char('m') => {
                self.help_scroll = 0;
                self.modal = Some(Modal::Messages);
            }
            _ => {}
        }
        // Any key but the one that armed it takes the arming back, so a
        // second `r` after a `j` is a first press again.
        if self
            .armed
            .as_ref()
            .is_some_and(|armed| key.code != KeyCode::Char(armed.key))
        {
            self.armed = None;
        }
    }

    /// `q` and esc on the list. Quitting ends an action in flight wherever
    /// its worker is — halfway through an install, or through a switch
    /// whose undo then never runs — so with one in flight `q` asks for a
    /// second press, and esc, which cancels everywhere else, only says so.
    /// ctrl-c still quits at once, whatever is in flight.
    ///
    /// A start or a restart in flight is cancelled by the second `q`
    /// rather than abandoned, and pando quits once each has run its undo:
    /// abandoned, a switch to isolated left containers up that its record
    /// does not say are isolated. A cancel lands at the start's next safe
    /// point, so a hook it is running finishes first; a `q` meanwhile
    /// quits at once. Anything else in flight is abandoned, as before.
    fn quit(&mut self, key: KeyCode) {
        if self.quitting && key == KeyCode::Char('q') {
            self.should_quit = true;
            return;
        }
        let Some(first) = self.pending.first() else {
            self.should_quit = true;
            return;
        };
        let name = first.name.clone();
        let listed = |cancels: bool| {
            self.pending
                .iter()
                .filter(|p| p.cancel.is_some() == cancels)
                .map(|p| match cancels {
                    true => format!("the {} of {}", p.kind.noun(), p.label),
                    false => format!("{} {}", p.doing(), p.label),
                })
                .collect::<Vec<_>>()
                .join(" and ")
        };
        let (cancels, abandons) = (listed(true), listed(false));
        let what = match (cancels.is_empty(), abandons.is_empty()) {
            (true, _) => format!("abandon {abandons} and quit"),
            (false, true) => format!("cancel {cancels} and quit once it is undone"),
            (false, false) => {
                format!("cancel {cancels}, abandon {abandons}, and quit once it is undone")
            }
        };
        if key == KeyCode::Esc {
            self.set_prompt(match cancels.is_empty() {
                true => format!("{abandons} is in flight — q twice abandons it and quits"),
                false => format!("q twice would {what}"),
            });
            return;
        }
        if !self.pressed_again('q', &name, &what) {
            return;
        }
        if cancels.is_empty() {
            self.should_quit = true;
            return;
        }
        for pending in &self.pending {
            if let Some(cancel) = &pending.cancel {
                cancel.cancel();
            }
        }
        self.quitting = true;
        self.set_status(format!(
            "cancelling {cancels} — pando quits once it is undone · q quits now"
        ));
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.end_filter();
            }
            KeyCode::Enter => {
                // Enter settles on the row the filter found. A worktree
                // `n` made, listed yet or not, does not take the cursor
                // from it, then or on a later discovery: `s` next started
                // that one, not the row just picked. A filter that left
                // no row to settle on still gives the cursor to it.
                if self.selected_worktree().is_some() {
                    self.select_on_arrival = None;
                }
                self.end_filter();
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.refilter();
            }
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.refilter();
            }
            _ => {}
        }
    }

    /// Leaves the filter line, for a worktree `n` made while it was being
    /// typed if one arrived and is still waiting, and for the row it was
    /// on otherwise.
    fn end_filter(&mut self) {
        self.mode = Mode::Normal;
        let keep = self
            .take_arrival()
            .or_else(|| self.selected_worktree().map(|w| w.name.clone()));
        self.refilter_keeping(keep, 0);
    }

    // ---- processes -------------------------------------------------------

    /// The state record for a worktree, when there is one.
    pub fn record_for(&self, name: &str) -> Option<&WorktreeRecord> {
        self.state.worktrees.get(name)
    }

    /// The phase of a whole worktree: failed if any of its processes is,
    /// then starting, then running. One row, one answer, however many
    /// processes it runs.
    pub fn phase_of(&self, name: &str) -> Option<Aggregate> {
        state::aggregate_phase(self.record_for(name)?)
    }

    /// The selected worktree's processes, in config order, with the log
    /// each one writes.
    pub fn processes_of(&self, name: &str) -> Vec<(String, ProcessRecord)> {
        self.record_for(name)
            .map(|record| {
                record
                    .processes
                    .iter()
                    .map(|(process, p)| (process.clone(), p.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Since when a worktree has been up: its *oldest* running process.
    /// Restarting one process of several does not make the worktree new —
    /// the rest of it has been serving all along.
    pub fn up_since(&self, name: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        self.record_for(name)?
            .processes
            .values()
            .filter_map(|p| match p.phase {
                state::Phase::Running { since } => Some(since),
                _ => None,
            })
            .min()
    }

    /// Whether the selected worktree has private services the last probe
    /// did not ask about, because the cursor moved onto it since. Only the
    /// selected worktree's are probed, so a move asks on the next tick
    /// rather than leaving its rows out until the next refresh.
    fn selection_unprobed(&self) -> bool {
        self.selected_worktree().is_some_and(|w| {
            !self.service_health.worktrees.contains_key(&w.name)
                && self
                    .record_for(&w.name)
                    .is_some_and(|r| !r.services.is_empty())
        })
    }

    /// One worktree's private services as the last probe found them.
    ///
    /// From the probe rather than from the record, because "is it up" is
    /// the one thing the record cannot say — and asking here would put a
    /// TCP connect inside a paint.
    pub fn services_of(&self, name: &str) -> Vec<actions::ServiceStatus> {
        self.service_health
            .worktrees
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// What a worktree is called on screen: its branch, which is what a
    /// developer knows it by. The directory name is derived from it
    /// (`feat/x` lives in `feat+x`) and only shows where the branch is
    /// missing — a detached checkout — or where a path needs it.
    ///
    /// `pando check`'s throwaway worktree has no row and no branch, and
    /// its directory name is nobody's word for it: it is named for what
    /// it is, in the stop-everything confirmation and what follows it.
    pub fn label_of(&self, name: &str) -> String {
        if worktree::is_check(name) {
            return CHECK_LABEL.to_string();
        }
        self.worktrees
            .iter()
            .find(|w| w.name == name)
            .and_then(|w| w.branch.clone())
            .unwrap_or_else(|| name.to_string())
    }

    /// Whether `name` is the main checkout's row.
    pub fn is_main(&self, name: &str) -> bool {
        self.main.as_ref().is_some_and(|main| main.name == name)
    }

    /// The repository of a multi-repo project that `process` runs in: the
    /// first directory of its `cwd`, when that is one of the project's
    /// repositories. `None` for a project that is one repository, where
    /// there is only one to run in, and for a process that runs at the
    /// set's own root.
    pub fn process_repo(&self, process: &str) -> Option<&str> {
        let cwd = self.config.processes.get(process)?.cwd.as_deref()?;
        self.paths.project.repo_of(cwd)
    }

    /// The repositories of a multi-repo project that a set has no checkout
    /// of: one its directory never got, or one removed from under it.
    /// What it lacks is what `pando new <branch>` adds back. Empty for a
    /// worktree of one repository.
    pub fn missing_checkouts<'a>(&'a self, wt: &Worktree) -> Vec<&'a str> {
        if !wt.is_set() {
            return Vec::new();
        }
        wt.absent_from(&self.paths.project.repos)
    }

    /// How many worktrees the list has, the main checkout not among them:
    /// it is not a worktree, and the header and the list's title count
    /// worktrees.
    pub fn linked_count(&self) -> usize {
        self.worktrees
            .iter()
            .filter(|w| !self.is_main(&w.name))
            .count()
    }

    /// Whether the main checkout has a row: once the project has a
    /// worktree, or pando has anything recorded for main. A project with
    /// neither is a first run, and the welcome it gets is about making a
    /// worktree — a main checkout pando never ran does not change that.
    pub fn main_row_shown(&self) -> bool {
        self.linked_count() > 0
            || self
                .main
                .as_ref()
                .is_some_and(|main| self.state.worktrees.contains_key(&main.name))
    }

    pub fn selected_worktree(&self) -> Option<&Worktree> {
        let row = self.list_state.selected()?;
        let idx = *self.filtered_indices.get(row)?;
        self.worktrees.get(idx)
    }

    fn move_cursor(&mut self, delta: isize) {
        // The tail belongs to a row, so moving off it starts at the end of
        // the next one's log rather than wherever the last one was
        // scrolled — and at that worktree's first process, not at whatever
        // index the last one happened to be showing.
        self.tail_scroll = 0;
        self.tail_index = 0;
        if self.filtered_indices.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as isize;
        let last = self.filtered_indices.len() as isize - 1;
        self.select_index((current + delta).clamp(0, last) as usize);
    }

    fn select_index(&mut self, index: usize) {
        if self.filtered_indices.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state
                .select(Some(index.min(self.filtered_indices.len() - 1)));
        }
        self.drop_stale_flash();
    }

    /// An error about one worktree leaves the header once the cursor is on
    /// another: read beside a different row it reads as that row's. `m`
    /// still has it.
    fn drop_stale_flash(&mut self) {
        let selected = self.selected_worktree().map(|w| w.name.clone());
        let stale = self.status.as_ref().is_some_and(|s| {
            s.is_error()
                && s.about
                    .as_deref()
                    .is_some_and(|about| Some(about) != selected.as_deref())
        });
        if stale {
            self.status = None;
        }
    }

    fn refilter(&mut self) {
        let keep = self.selected_worktree().map(|w| w.name.clone());
        self.refilter_keeping(keep, 0);
    }

    /// `keep` is the worktree the cursor was on, passed in rather than read
    /// here: `apply_snapshot` has already replaced the list by the time it
    /// refilters, and resolving the old row against the new list would pick
    /// whichever worktree now happens to sit at that index.
    ///
    /// `fallback` is the row the cursor goes to when `keep` is gone: the
    /// first match while a filter is typed, and for a worktree that went
    /// away, the row it was on — its neighbour, not the top of the list.
    fn refilter_keeping(&mut self, keep: Option<String>, fallback: usize) {
        let needle = self.filter.to_lowercase();
        let keep_name = keep;
        let main_shown = self.main_row_shown();
        self.filtered_indices = self
            .worktrees
            .iter()
            .enumerate()
            .filter(|(_, w)| main_shown || !self.is_main(&w.name))
            .filter(|(_, w)| {
                needle.is_empty()
                    || w.name.to_lowercase().contains(&needle)
                    || w.branch
                        .as_deref()
                        .is_some_and(|b| b.to_lowercase().contains(&needle))
            })
            .map(|(i, _)| i)
            .collect();
        // The order `,` chose, below the main checkout and what is up. A
        // row moves only when it starts or stops, or when `run` sees it
        // started; the cursor stays on its worktree either way.
        let mut rows = std::mem::take(&mut self.filtered_indices);
        self.sort_rows(&mut rows);
        self.filtered_indices = rows;
        // Keep the cursor on the same worktree when it survives the filter.
        let row = keep_name.and_then(|name| {
            self.filtered_indices
                .iter()
                .position(|&i| self.worktrees[i].name == name)
        });
        match row {
            Some(r) => {
                self.list_state.select(Some(r));
                self.drop_stale_flash();
            }
            None => self.select_index(fallback),
        }
    }

    pub fn set_status(&mut self, message: impl Into<String>) {
        self.post(message.into(), StatusKind::Info, STATUS_TTL);
    }

    pub fn set_success(&mut self, message: impl Into<String>) {
        self.post(message.into(), StatusKind::Success, STATUS_TTL);
    }

    /// A success that goes to `m` and leaves an error still fresh on the
    /// header where it is. For the line that says a start is ready, which
    /// the row says too: the error it would replace — the public URL a
    /// restart closed, a warning from a hook — the row does not.
    fn set_success_behind_error(&mut self, message: impl Into<String>) {
        let error = self.flash().filter(|s| s.is_error()).cloned();
        self.set_success(message);
        if error.is_some() {
            self.status = error;
        }
    }

    /// A success worth more than a glance: a public URL somebody is about
    /// to read out or type.
    pub fn set_lasting(&mut self, message: impl Into<String>) {
        self.post(message.into(), StatusKind::Success, LASTING_TTL);
    }

    /// An error, with any CLI flag it recommends said as the key that
    /// does the same here.
    pub fn set_error(&mut self, message: impl Into<String>) {
        let message = as_tui_remedy(&message.into());
        self.post(message, StatusKind::Error, ERROR_TTL);
    }

    /// An error about one worktree: the log viewer of another one does not
    /// show it.
    pub fn set_error_about(&mut self, name: &str, message: impl Into<String>) {
        self.set_error(message);
        let about = Some(name.to_string());
        if let Some(status) = self.status.as_mut() {
            status.about = about.clone();
        }
        if let Some(last) = self.messages.back_mut() {
            last.about = about;
        }
    }

    /// The spinner line of an action in flight. Rewritten every tick, so
    /// it stays out of the history `m` shows.
    pub fn set_progress(&mut self, message: impl Into<String>) {
        self.status = Some(Status {
            message: message.into(),
            kind: StatusKind::Progress,
            at: Instant::now(),
            ttl: STATUS_TTL,
            about: None,
        });
    }

    /// Whether the spinner may have the header: nothing else is on it, or
    /// what is on it has had its time. An error, a confirmation or a prompt
    /// posted while an action is in flight stays for as long as any other;
    /// the row and the detail pane still say what is in flight.
    pub(super) fn spinner_may_take_header(&self) -> bool {
        self.status
            .as_ref()
            .is_none_or(|s| s.kind == StatusKind::Progress || !s.fresh())
    }

    /// What a second press would do, for as long as it would. Not kept
    /// in the history: once it has expired it is no longer true.
    pub fn set_prompt(&mut self, message: impl Into<String>) {
        self.status = Some(Status {
            message: message.into(),
            kind: StatusKind::Info,
            at: Instant::now(),
            ttl: ARM_TTL,
            about: None,
        });
    }

    fn post(&mut self, message: String, kind: StatusKind, ttl: Duration) {
        let status = Status {
            message,
            kind,
            at: Instant::now(),
            ttl,
            about: None,
        };
        if self.messages.len() >= MESSAGE_HISTORY {
            self.messages.pop_front();
        }
        self.messages.push_back(status.clone());
        self.status = Some(status);
    }

    /// The status message while it is still fresh.
    pub fn active_status(&self) -> Option<(&str, bool)> {
        self.flash().map(|s| (s.message.as_str(), s.is_error()))
    }

    /// The same, with its kind, for the paint.
    pub fn flash(&self) -> Option<&Status> {
        self.status.as_ref().filter(|s| s.fresh())
    }

    /// The flash as a view of one worktree shows it: nothing that is about
    /// a different worktree. `m` still has it.
    pub fn flash_for(&self, name: &str) -> Option<&Status> {
        self.flash()
            .filter(|s| s.about.as_deref().is_none_or(|about| about == name))
    }

    // ---- git state -------------------------------------------------------

    /// Re-reads git state off the UI thread: every worktree, or `only`
    /// these. Quiet — the header's `reading git` is for the first read —
    /// and never two at once.
    ///
    /// One asked for while another runs is kept, merged with any other
    /// kept, and runs when that one lands: a git move's re-read, or the
    /// one after a shell, was dropped behind the periodic read of the
    /// selected row and the row kept its old drift for half a minute.
    pub fn refresh_git(&mut self, only: Option<Vec<String>>) {
        if self.enriching > 0 || self.git_refreshing {
            self.git_again = Some(match (self.git_again.take(), only) {
                (Some(None), _) | (_, None) => None,
                (None, Some(names)) => Some(names),
                (Some(Some(mut kept)), Some(names)) => {
                    for name in names {
                        if !kept.contains(&name) {
                            kept.push(name);
                        }
                    }
                    Some(kept)
                }
            });
            return;
        }
        self.spawn_enrichment(only, true);
    }

    /// The terminal has the focus again — the pane switched back to, the
    /// window raised. What changed while it was away, a pull request merged
    /// on GitHub above all, is looked for now rather than on the next
    /// tick that would, at most once in [`FOCUS_REFRESH_GAP`].
    pub(super) fn focus_gained(&mut self) {
        if self
            .focus_refreshed
            .is_some_and(|at| at.elapsed() < FOCUS_REFRESH_GAP)
        {
            return;
        }
        self.focus_refreshed = Some(Instant::now());
        self.came_back();
    }

    /// What changed while the developer was somewhere else — a shell `!`
    /// opened, another pane, a browser: the worktrees, their git state
    /// and their pull requests, read again.
    pub fn came_back(&mut self) {
        self.spawn_discovery();
        self.refresh_git(None);
        if self.gh_may_answer() {
            self.spawn_pr_fetch();
        }
    }

    /// Whether asking `gh` could tell anything: not when it is missing or
    /// signed out, which only `R` asks about again.
    fn gh_may_answer(&self) -> bool {
        !matches!(
            self.gh_account,
            Some(worktree::GhAccount::Missing | worktree::GhAccount::SignedOut)
        )
    }

    /// Remembers what enrichment said a commit's age was, and when.
    fn note_commit_age(&mut self, name: &str, age: Option<&str>) {
        match age.and_then(|text| Some((text, parse_git_relative(text)?))) {
            Some((text, secs)) => {
                self.commit_seen
                    .insert(name.to_string(), (text.to_string(), secs, Instant::now()));
            }
            None => {
                self.commit_seen.remove(name);
            }
        }
    }

    /// How old the checked-out commit is now, compact: `82 seconds ago`
    /// reported a minute back reads `2m ago`. Git's own words when they
    /// cannot be read.
    pub fn commit_age(&self, wt: &Worktree) -> Option<String> {
        let text = wt.head_age.as_deref()?;
        let secs = match self.commit_seen.get(&wt.name) {
            Some((seen, secs, at)) if seen == text => {
                secs.saturating_add(at.elapsed().as_secs() as i64)
            }
            _ => match parse_git_relative(text) {
                Some(secs) => secs,
                None => return Some(text.to_string()),
            },
        };
        Some(format!("{} ago", compact_age(secs)))
    }

    // ---- nothing to run --------------------------------------------------

    /// The one line that says so, with the file to add it to.
    pub fn nothing_to_run_line(&self) -> String {
        format!(
            "nothing to run: add a [dev] command in {}",
            self.paths.config_file().display()
        )
    }

    /// Takes a fresh read of process state — from the one-second refresh
    /// or from a discovery, which runs the full refresh itself — and says
    /// what it changed. Both paths, because either can be the one that
    /// first sees a process dead: a death only the discovery saw was
    /// never announced, and on one tick in five that is the discovery.
    /// Returns whether anything changed.
    pub(super) fn adopt_state(&mut self, state: State) -> bool {
        let changed = state != self.state;
        let shown = self.main_row_shown();
        let before = std::mem::replace(&mut self.state, state);
        // A main checkout started from a shell while the welcome is up gets
        // its row with the state that records it, not a discovery later;
        // a start or a stop moves its row to or from the top, and a start
        // reorders a list sorted by what ran last.
        let ran = |state: &State| -> Vec<_> {
            state
                .worktrees
                .iter()
                .map(|(name, record)| (name.clone(), record.last_run()))
                .collect()
        };
        let live = |state: &State| -> Vec<_> {
            state
                .worktrees
                .iter()
                .filter(|(_, record)| record.is_live())
                .map(|(name, _)| name.clone())
                .collect()
        };
        let reordered = live(&before) != live(&self.state)
            || self.sort == ListSort::Run && ran(&before) != ran(&self.state);
        if self.main_row_shown() != shown || reordered {
            self.refilter();
        }
        let died = self.deaths_since(&before);
        self.announce_ready(&died);
        self.announce_deaths(died);
        self.announce_silence();
        changed
    }

    /// Says once when a running worktree has a port nothing listens on —
    /// see `state::silent_ports`. Forgotten when the port answers again or
    /// the worktree stops, so a second death is said too.
    fn announce_silence(&mut self) {
        let now = chrono::Utc::now();
        let silent: Vec<(String, String, u16)> = self
            .state
            .worktrees
            .iter()
            .flat_map(|(name, record)| {
                state::silent_ports(record, now)
                    .into_iter()
                    .map(|(role, port)| (name.clone(), role, port))
            })
            .collect();
        self.silence_told
            .retain(|(name, port)| silent.iter().any(|(n, _, p)| n == name && p == port));
        for (name, role, port) in silent {
            if self.silence_told.insert((name.clone(), port)) {
                let label = self.label_of(&name);
                self.set_error_about(
                    &name,
                    format!("{label}: nothing listens on {role}'s port {port} — l shows the log"),
                );
            }
        }
    }

    /// Every process that was starting or running before this refresh and
    /// has failed since: the worktree, the process, and why.
    ///
    /// A failure already announced is not announced again: a discovery
    /// that read state before the last refresh can arrive after it, and
    /// the older read it brings shows the process still up.
    fn deaths_since(&self, before: &State) -> Vec<Death> {
        let mut died = Vec::new();
        for (name, record) in &self.state.worktrees {
            for (process, now) in &record.processes {
                let state::Phase::Failed { reason, at } = &now.phase else {
                    continue;
                };
                let was_up = before
                    .worktrees
                    .get(name)
                    .and_then(|r| r.processes.get(process))
                    .is_some_and(|p| !matches!(p.phase, state::Phase::Failed { .. }));
                let told = self
                    .deaths_told
                    .get(&(name.clone(), process.clone()))
                    .is_some_and(|told| told == at);
                if was_up && !told {
                    died.push((name.clone(), process.clone(), reason.clone(), *at));
                }
            }
        }
        died
    }

    /// Says so for each process that died, whenever it did: after a `P`, or
    /// an hour into a run. Otherwise the last word on screen is the
    /// `ready` that preceded it.
    fn announce_deaths(&mut self, died: Vec<Death>) {
        for (name, process, reason, at) in died {
            self.deaths_told.insert((name.clone(), process.clone()), at);
            let label = self.label_of(&name);
            let reason = reason.trim();
            let message = if reason.is_empty() {
                format!("{process} of {label} exited")
            } else {
                format!("{process} of {label} exited — {reason}")
            };
            self.set_error_about(&name, message);
        }
    }

    /// A start that returned before its worktree was ready says so once
    /// the refresh finds what it spawned running — or stops waiting when
    /// that failed or the worktree went away. A failure that `died`
    /// already names is left to it.
    fn announce_ready(&mut self, died: &[Death]) {
        for mut awaited in std::mem::take(&mut self.awaiting_ready) {
            let name = awaited.name.clone();
            match awaited.readiness(self.record_for(&name), crate::process::is_alive) {
                Readiness::Ready => {
                    let label = self.label_of(&name);
                    self.set_success_behind_error(match awaited.url {
                        Some(url) => format!("{label} is ready — {url}"),
                        None => format!("{label} is ready"),
                    });
                }
                // Kept, with what this read has shown.
                Readiness::Waiting => self.awaiting_ready.push(awaited),
                Readiness::Failed => {
                    if died.iter().any(|(n, ..)| *n == name) {
                        continue;
                    }
                    let label = self.label_of(&name);
                    self.set_error_about(&name, format!("{label} failed — l shows the log"));
                }
                Readiness::Gone => {}
            }
        }
    }

    /// Waits for a start of `awaited.name` to be ready, in place of any
    /// earlier start of the same worktree still being waited for: that one
    /// was replaced, and the others are waited for alongside it.
    pub(super) fn await_ready(&mut self, awaited: AwaitingReady) {
        self.awaiting_ready.retain(|a| a.name != awaited.name);
        self.awaiting_ready.push(awaited);
    }

    fn expire_status(&mut self) -> bool {
        let expired = self.status.as_ref().is_some_and(|s| !s.fresh());
        if expired {
            self.status = None;
        }
        expired
    }

    #[cfg(test)]
    pub fn new_for_test(paths: PandoPaths, config: Config, worktrees: Vec<Worktree>) -> Self {
        let (event_tx, event_rx) = mpsc::channel();
        let theme_settings = config.ui.theme_settings();
        let sort = ListSort::from_config(&config);
        let themes_dir = paths.themes_dir();
        let mut app = Self {
            paths,
            config,
            namespace_plan: std::cell::OnceCell::new(),
            app_manifests: Default::default(),
            main: None,
            default_base: Some("main".into()),
            worktrees,
            created_by_pando: BTreeMap::new(),
            state: State::new(),
            service_health: ServiceHealth::default(),
            question_checked: Vec::new(),
            refreshing: false,
            discovering: false,
            discover_again: false,
            resolve_base: false,
            log_tails: LogTails::default(),
            tail_index: 0,
            tail_scroll: 0,
            tail_rows: 0,
            view: View::List,
            inspect: None,
            viewer_height: 0,
            state_warning: None,
            prs: HashMap::new(),
            pr_list: Vec::new(),
            pr_fetching: false,
            pr_fetch_again: false,
            focus_refreshed: None,
            git_again: None,
            app_opening: false,
            pr_error: None,
            gh_account: None,
            list_state: ListState::default(),
            sort,
            filter: String::new(),
            filtered_indices: Vec::new(),
            marks: BTreeSet::new(),
            mode: Mode::Normal,
            modal: None,
            armed: None,
            leader: false,
            help_scroll: 0,
            help_scroll_max: usize::MAX,
            status: None,
            messages: VecDeque::new(),
            select_on_arrival: None,
            queued_questions: VecDeque::new(),
            pending: Vec::new(),
            removals: BTreeMap::new(),
            enriching: 0,
            git_refreshing: false,
            commit_seen: HashMap::new(),
            nothing_to_run: false,
            awaiting_ready: Vec::new(),
            deaths_told: HashMap::new(),
            silence_told: std::collections::HashSet::new(),
            should_quit: false,
            quitting: false,
            tick: 0,
            hits: Hits::default(),
            last_click: None,
            drag_told: false,
            event_tx,
            event_rx: Some(event_rx),
            launch_env: LaunchEnv::default(),
            launch: None,
            theme: ThemeState::new(theme_settings, &themes_dir),
            setup_screen: None,
            setup_row: SetupRow::default(),
            setup_watch: SetupWatch::default(),
            checks_started: 0,
            clipboard: None,
            opened: None,
            opened_apps: Vec::new(),
        };
        app.refilter();
        app
    }
}

#[cfg(test)]
pub mod tests;
