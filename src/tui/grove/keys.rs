//! The grove's keys: the column's own while it has the focus, and from
//! the list only `keymap::GROVE_KEYS`, and only while nothing else is
//! open on it. Everything else is the front app's, so a key a modal, the
//! log viewer or a filter answers still reaches it.

use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use crate::tui::app::{AppEvent, DETAIL_STEP, Modal};

use super::{COLUMN_STEP, Focus, Grove, Shown, StopConfirm};

/// The column's hints while it has the focus, most useful first: each a
/// key `keymap::COLUMN_KEYS` documents, which a test holds it to.
pub const COLUMN_HINTS: [(&str, &str); 4] = [
    ("⏎", "open"),
    ("X", "stop all"),
    ("tab", "list"),
    ("w", "hide"),
];

impl Grove {
    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if let Some(confirm) = self.confirm.take() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.stop_project(confirm),
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => {}
                _ => self.confirm = Some(confirm),
            }
            return;
        }
        // The second `q` the grove asked for is the grove's, wherever the
        // focus is; given to the front app instead, it would ask its own
        // question again. A dialog on the front app keeps its `q`.
        if key.code == KeyCode::Char('q')
            && (self.quitting || self.quit_is_armed())
            && self.front_app().is_none_or(|app| app.idle_on_list())
        {
            match self.quitting {
                true => self.should_quit = true,
                false => self.confirm_quit(),
            }
            return;
        }
        // A question, a dialog, the log viewer or a filter on the front
        // app has the keyboard, wherever the focus was.
        if self.front_app().is_some_and(|app| !app.idle_on_list()) {
            self.send_front(AppEvent::Input(Event::Key(key)));
            return;
        }
        if self.focus == Focus::Column || self.front.is_none() {
            self.column_key(key);
            return;
        }
        if self.grove_key(key) {
            return;
        }
        self.send_front(AppEvent::Input(Event::Key(key)));
    }

    /// One of `keymap::GROVE_KEYS`, from the list or the column. Returns
    /// whether it was one.
    pub(super) fn grove_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('w') => self.toggle_column(),
            KeyCode::Char('[') => self.step_project(-1),
            KeyCode::Char(']') => self.step_project(1),
            KeyCode::Char('{') => self.resize_column(-(COLUMN_STEP as i32)),
            KeyCode::Char('}') => self.resize_column(COLUMN_STEP as i32),
            KeyCode::Char('|') => self.cycle_detail(),
            KeyCode::Char('<') => self.resize_detail(-(DETAIL_STEP as i32)),
            KeyCode::Char('>') => self.resize_detail(DETAIL_STEP as i32),
            KeyCode::Left => {
                if !self.column_shown() {
                    self.shown = Shown::On;
                }
                self.focus = Focus::Column;
            }
            _ => return false,
        }
        true
    }

    /// Every key of `keymap::COLUMN_KEYS`, while the column has the focus
    /// or there is no project in front to have it.
    fn column_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('g') | KeyCode::Home => self.cursor = 0,
            KeyCode::Char('G') | KeyCode::End => {
                self.cursor = self.summaries.len().saturating_sub(1)
            }
            KeyCode::Enter | KeyCode::Right => self.open_selected(),
            KeyCode::Tab if self.front.is_some() => self.focus = Focus::List,
            KeyCode::Esc if self.front.is_some() => self.focus = Focus::List,
            KeyCode::Esc => self.request_quit(None),
            KeyCode::Char('X') => self.confirm_stop(),
            KeyCode::Char('?') => {
                if let Some(app) = self.front_app_mut() {
                    app.help_scroll = 0;
                    app.modal = Some(Modal::Help);
                }
            }
            // The front app's own `q`, which asks first about its own
            // work in flight; with none in front, the grove's.
            KeyCode::Char('q') if self.front.is_some() => {
                self.send_front(AppEvent::Input(Event::Key(key)));
            }
            KeyCode::Char('q') => self.request_quit(None),
            _ => {
                self.grove_key(key);
            }
        }
    }

    pub(super) fn move_cursor(&mut self, delta: isize) {
        let last = self.summaries.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// `[` and `]`: the previous or the next project that can be opened,
    /// round the end of the list, brought to the front.
    pub(super) fn step_project(&mut self, delta: isize) {
        let count = self.summaries.len();
        if count == 0 {
            return;
        }
        let from = self
            .front
            .as_deref()
            .and_then(|id| self.summaries.iter().position(|s| s.id == id))
            .unwrap_or(self.cursor);
        let mut at = from;
        for _ in 0..count {
            at = (at as isize + delta).rem_euclid(count as isize) as usize;
            if !self.summaries[at].moved {
                break;
            }
        }
        if self.summaries[at].moved || (at == from && self.front.is_some()) {
            return;
        }
        let focus = self.focus;
        self.cursor = at;
        self.open_selected();
        // From the list, the keys stay on a list; from the column, on it.
        self.focus = match self.front.is_some() {
            true => focus,
            false => Focus::Column,
        };
    }

    /// `⏎` on a project: brought to the front if it is open, opened on a
    /// worker otherwise, and its list given the keys. A moved project is
    /// never opened: its repository is not where pando knew it.
    pub(super) fn open_selected(&mut self) {
        let Some(summary) = self.selected().cloned() else {
            return;
        };
        if summary.moved {
            self.say(
                format!(
                    "{} is not at {} any more — run pando in it where it is now",
                    summary.name,
                    summary.root.display()
                ),
                true,
            );
            return;
        }
        if self.open.contains_key(&summary.id) {
            self.bring_send_front(&summary.id);
            return;
        }
        self.request_open(&summary);
    }

    /// `X`: asks first, naming every worktree of the project it would
    /// stop.
    fn confirm_stop(&mut self) {
        let Some(summary) = self.selected() else {
            return;
        };
        if summary.moved {
            let name = summary.name.clone();
            self.say(
                format!("{name} has moved — nothing of it is acted on"),
                true,
            );
            return;
        }
        let worktrees: Vec<String> = summary.worktrees.iter().map(|w| w.name.clone()).collect();
        if worktrees.is_empty() {
            let name = summary.name.clone();
            self.say(format!("nothing is up in {name}"), false);
            return;
        }
        self.confirm = Some(StopConfirm {
            id: summary.id.clone(),
            name: summary.name.clone(),
            worktrees,
        });
    }

    /// The confirmation's `y`: the project's own app stops what it
    /// showed, opened for it first when it is not.
    fn stop_project(&mut self, confirm: StopConfirm) {
        if let Some(opened) = self.open.get_mut(&confirm.id) {
            opened.app.stop_everything(confirm.worktrees);
            return;
        }
        let Some(summary) = self.summaries.iter().find(|s| s.id == confirm.id).cloned() else {
            return;
        };
        self.stop_after_open = Some((confirm.id, confirm.worktrees));
        self.request_open(&summary);
    }
}
