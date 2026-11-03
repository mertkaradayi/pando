//! The grove: every project pando knows, in one TUI.
//!
//! A column on the left lists the projects on the machine, the ones with
//! something up first, read from the home alone (`actions::summaries`) on
//! a worker every few seconds and at once after an action. Right of it is
//! the TUI of the project in front, as it is when pando runs in one
//! project: an [`App`] per project opened, made the first time it is
//! brought to the front and kept while the TUI runs, so what is in flight
//! in one finishes while another is in front.
//!
//! One channel feeds the loop: the terminal's input and the ticks, and
//! every app's own events, forwarded under its project's id and handed to
//! that app whether or not it is in front (`deliver.rs`). Only the front
//! app is drawn, into the area right of the column, so where it records
//! its clickable regions is where it was drawn. The grove takes its own
//! keys only from the column, or from a list with nothing else open on it
//! (`keys.rs`); everything else is the front app's. Opening a project
//! does what `main` does before the TUI starts, on a worker (`open.rs`).
//!
//! Outside any repository there is no project in front: the right side is
//! the home pane, the brand's grove with a stem for each project.

mod deliver;
mod keys;
mod open;
#[cfg(test)]
mod tests;

pub use keys::COLUMN_HINTS;
pub use open::spawn_summaries;

use ratatui::crossterm::event::Event;
use ratatui::layout::Rect;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::actions::ProjectSummary;
use crate::config::UiSection;
use crate::theme::{Resolved, Settings};

use super::app::{App, AppEvent};

/// The column's width when nothing has changed it, and the narrowest and
/// widest `{` and `}` take it: a name of sixteen cells beside its marks,
/// and never so wide the project's own list is squeezed.
pub const COLUMN_WIDTH: u16 = 26;
pub const COLUMN_MIN_WIDTH: u16 = 18;
pub const COLUMN_MAX_WIDTH: u16 = 40;

/// How far one `{` or `}` moves the column's edge.
pub const COLUMN_STEP: u16 = 2;

/// What the grove's loop takes: the terminal, time, and every open app's
/// own events under its project's id.
pub enum GroveEvent {
    Input(Event),
    Tick,
    Theme(Box<Resolved>),
    /// Every project, read again by the summaries worker.
    Summaries(Vec<ProjectSummary>),
    /// A project opened on a worker, or why it could not be.
    Opened(String, Box<Result<App, String>>),
    /// One of an open app's own events, for that app.
    Project(String, AppEvent),
}

/// Where the keys go: the column, or the list of the project in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Column,
    List,
}

/// The column shown or hidden by `w`, or as the grove sees fit: shown
/// when there are two projects or more, or none in front.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    Auto,
    On,
    Off,
}

/// `X` in the column asked to stop everything up in one project, and
/// waits for its `y`: the project, and each worktree it would stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopConfirm {
    pub id: String,
    pub name: String,
    pub worktrees: Vec<String>,
}

/// Where the last frame put what the mouse acts on in the grove's own
/// part of the screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GroveHits {
    /// The whole column, or the strip.
    pub column: Rect,
    /// Each project row of the column, by its index in the summaries.
    pub rows: Vec<(Rect, usize)>,
    /// The strip's `[` and `]`.
    pub prev: Rect,
    pub next: Rect,
}

/// A project held open: its app, and the watcher on its worktrees
/// directory, which runs as long as it is held here.
pub struct Opened {
    pub app: App,
    pub _watcher: Option<super::FsWatcher>,
}

pub struct Grove {
    /// The pando home every project is read from.
    pub home: PathBuf,
    /// The user layer's `[ui]`: the mouse, when no project is in front.
    pub ui: UiSection,
    /// Every project, in the column's order.
    pub summaries: Vec<ProjectSummary>,
    /// The column's cursor, an index into `summaries`.
    pub cursor: usize,
    /// The project in front, by id; none outside a repository until one
    /// is opened.
    pub front: Option<String>,
    /// Every project opened, by id.
    pub open: BTreeMap<String, Opened>,
    /// Projects being opened on a worker.
    pub opening: Vec<String>,
    /// A stop `X` confirmed for a project still being opened, carried
    /// out once it is.
    pub stop_after_open: Option<(String, Vec<String>)>,
    pub focus: Focus,
    pub shown: Shown,
    pub width: u16,
    pub confirm: Option<StopConfirm>,
    /// What the grove itself has to say: a project that could not be
    /// opened, a moved one. Shown on the home pane or the front app's
    /// header.
    pub note: Option<(String, Instant)>,
    /// The first `q` while other projects have work in flight.
    pub quit_armed: Option<Instant>,
    pub should_quit: bool,
    pub tick: u32,
    pub hits: GroveHits,
    /// The theme settings every open app shares with the watcher that
    /// follows them.
    pub theme_settings: Arc<Mutex<Settings>>,
    /// The theme last resolved, which a project opened later takes.
    pub theme: Option<Resolved>,
    /// Asks the summaries worker to read again now.
    pub poke: Option<Sender<()>>,
    /// The actions in flight across every open app at the last look, so
    /// one starting or ending reads the summaries again.
    pub in_flight: usize,
    pub tx: Sender<GroveEvent>,
    pub rx: Option<Receiver<GroveEvent>>,
    /// The projects a test asked to open, instead of opening them.
    #[cfg(test)]
    pub open_requests: Vec<String>,
}

impl Grove {
    /// A grove over `home`, with the projects there are and none open.
    pub fn new(home: PathBuf, ui: UiSection, summaries: Vec<ProjectSummary>) -> Self {
        let (tx, rx) = mpsc::channel();
        let theme_settings = Arc::new(Mutex::new(ui.theme_settings()));
        Grove {
            home,
            ui,
            summaries,
            cursor: 0,
            front: None,
            open: BTreeMap::new(),
            opening: Vec::new(),
            stop_after_open: None,
            focus: Focus::Column,
            shown: Shown::Auto,
            width: COLUMN_WIDTH,
            confirm: None,
            note: None,
            quit_armed: None,
            should_quit: false,
            tick: 0,
            hits: GroveHits::default(),
            theme_settings,
            theme: None,
            poke: None,
            in_flight: 0,
            tx,
            rx: Some(rx),
            #[cfg(test)]
            open_requests: Vec::new(),
        }
    }

    /// Takes `app` in: held open under its project's id, its events
    /// forwarded into the grove's channel, and in front when `front`.
    pub fn adopt(&mut self, mut app: App, front: bool) {
        let id = app.paths.project.id.clone();
        app.in_grove = true;
        app.theme.settings = Arc::clone(&self.theme_settings);
        if let Some(resolved) = &mut self.theme {
            app.adopt_theme(resolved.clone());
            // Said once, by the first project to take it.
            resolved.warnings.clear();
        }
        if let Some(rx) = app.event_rx.take() {
            let tx = self.tx.clone();
            let forwarded = id.clone();
            std::thread::spawn(move || {
                for ev in rx {
                    if tx.send(GroveEvent::Project(forwarded.clone(), ev)).is_err() {
                        return;
                    }
                }
            });
        }
        let watcher = match cfg!(test) {
            true => None,
            false => {
                super::spawn_fs_watcher(&app.config.worktrees_dir(&app.paths), app.event_tx.clone())
            }
        };
        self.opening.retain(|o| o != &id);
        self.open.insert(
            id.clone(),
            Opened {
                app,
                _watcher: watcher,
            },
        );
        if front {
            self.bring_send_front(&id);
        }
    }

    /// Makes `id` the project in front, its list with the keys.
    fn bring_send_front(&mut self, id: &str) {
        if let Some(i) = self.summaries.iter().position(|s| s.id == id) {
            self.cursor = i;
        }
        let changed = self.front.as_deref() != Some(id);
        self.front = Some(id.to_string());
        self.focus = Focus::List;
        if changed && let Some(app) = self.front_app_mut() {
            app.brought_to_front();
        }
    }

    pub fn front_app(&self) -> Option<&App> {
        self.open.get(self.front.as_deref()?).map(|o| &o.app)
    }

    pub fn front_app_mut(&mut self) -> Option<&mut App> {
        let id = self.front.clone()?;
        self.open.get_mut(&id).map(|o| &mut o.app)
    }

    /// Whether the column is drawn: as `w` left it, or, by default, when
    /// there are two projects or more, or no project in front to show
    /// instead.
    pub fn column_shown(&self) -> bool {
        match self.shown {
            Shown::On => true,
            Shown::Off => false,
            Shown::Auto => self.summaries.len() >= 2 || self.front.is_none(),
        }
    }

    /// The theme resolved before the first frame: on screen, and taken by
    /// every project opened. Returns what the theme watcher compares
    /// against.
    pub fn take_theme(&mut self, resolved: Resolved) -> (String, crate::theme::Appearance) {
        crate::theme::set_palette(resolved.palette);
        let watched = (resolved.name.clone(), resolved.appearance);
        self.theme = Some(resolved);
        watched
    }

    /// The project under the column's cursor.
    pub fn selected(&self) -> Option<&ProjectSummary> {
        self.summaries.get(self.cursor)
    }

    /// Whether the terminal should report the mouse: as the project in
    /// front wants, or as the user layer says when there is none.
    pub fn wants_mouse(&self) -> bool {
        match self.front_app() {
            Some(app) => app.wants_mouse(),
            None => self.ui.mouse(),
        }
    }

    /// Something for the grove to say, for a few seconds: on the front
    /// app's header when there is one, on the home pane otherwise.
    pub fn say(&mut self, message: impl Into<String>, error: bool) {
        let message = message.into();
        match self.front_app_mut() {
            Some(app) if error => app.set_error(message),
            Some(app) => app.set_status(message),
            None => self.note = Some((message, Instant::now())),
        }
    }

    /// The note the home pane shows, while it is fresh.
    pub fn fresh_note(&self) -> Option<&str> {
        self.note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTE_TTL)
            .map(|(note, _)| note.as_str())
    }

    #[cfg(test)]
    pub fn new_for_test(summaries: Vec<ProjectSummary>) -> Self {
        Grove::new(
            PathBuf::from("/pando-test-does-not-exist/home"),
            UiSection::default(),
            summaries,
        )
    }
}

/// How long the home pane shows a note.
const NOTE_TTL: std::time::Duration = std::time::Duration::from_secs(10);
