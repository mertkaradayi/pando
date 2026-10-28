//! The mouse: a click on a worktree selects it and a second one acts on
//! it, a click on a process the log preview names shows its log, and the
//! wheel scrolls the preview under the pointer or moves the list.
//!
//! The paint records where those are, in [`Hits`]. Where the mouse has
//! nothing of its own yet — a modal, the log viewer — a step of the wheel
//! is the arrow key a terminal sends for it when nobody has the mouse,
//! sent only where an arrow moves something: an arrow closes the remove
//! dialog.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};

use super::{App, GitStage, Modal, SearchMode, View};

/// Lines a step of the wheel moves what is read by scrolling: help, the
/// messages, the log viewer, the log preview. Three is what a terminal
/// sends for a step when it turns the wheel into arrow keys. A cursor on
/// a list moves one row a step, as an arrow moves it.
const WHEEL_LINES: usize = 3;

/// How soon a second press on the same worktree has to follow the first
/// to be a double click: what desktops take when nobody has set it.
pub(super) const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// Where the last frame put what the mouse acts on. The paint fills it,
/// from empty every frame, so nothing that frame did not draw can be
/// clicked.
#[derive(Debug, Default)]
pub struct Hits {
    /// The list's rows, below its header, and the worktree each one shows,
    /// top to bottom.
    pub list: Option<(Rect, Vec<String>)>,
    /// The log preview in the detail pane: its header and its lines.
    pub preview: Option<Rect>,
    /// Each process the preview's header names, and where.
    pub preview_processes: Vec<(Rect, String)>,
}

impl App {
    /// Whether the terminal should report the mouse to pando now: unless
    /// `[ui] mouse = false` says not, and never on the setup screen, whose
    /// prompt is laid out to be selected with the mouse and pasted.
    pub fn wants_mouse(&self) -> bool {
        self.config.ui.mouse() && self.setup_screen.is_none()
    }

    /// Returns whether the frame needs repainting.
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        let at = Position::new(mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::ScrollUp => self.wheel(at, -1),
            MouseEventKind::ScrollDown => self.wheel(at, 1),
            MouseEventKind::Down(MouseButton::Left) => self.click(at),
            _ => false,
        }
    }

    /// Whether the list and the detail pane have the mouse: no modal over
    /// them, and no other screen in their place.
    fn list_has_the_mouse(&self) -> bool {
        self.modal.is_none() && self.setup_screen.is_none() && matches!(self.view, View::List)
    }

    /// A step of the wheel: up is `-1`.
    fn wheel(&mut self, at: Position, step: isize) -> bool {
        if self.list_has_the_mouse() {
            self.take_back_keys();
            if self
                .hits
                .preview
                .is_some_and(|preview| preview.contains(at))
            {
                // Down is newer, as PgDn is.
                self.scroll_tail(step * WHEEL_LINES as isize);
            } else {
                self.move_cursor(step);
            }
            return true;
        }
        let Some(steps) = self.wheel_steps() else {
            return false;
        };
        let code = if step < 0 { KeyCode::Up } else { KeyCode::Down };
        for _ in 0..steps {
            self.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        true
    }

    /// A left press. Anything but a worktree's row ends a double click
    /// begun on one.
    fn click(&mut self, at: Position) -> bool {
        let last = self.last_click.take();
        if !self.list_has_the_mouse() {
            return false;
        }
        let process = self
            .hits
            .preview_processes
            .iter()
            .find(|(cells, _)| cells.contains(at))
            .map(|(_, process)| process.clone());
        if let Some(process) = process {
            self.take_back_keys();
            self.show_tail_of(&process);
            return true;
        }
        let Some(name) = self.hits.list.as_ref().and_then(|(rows, names)| {
            rows.contains(at)
                .then(|| names.get(usize::from(at.y - rows.y)))
                .flatten()
                .cloned()
        }) else {
            return false;
        };
        let Some(row) = self
            .filtered_indices
            .iter()
            .position(|&i| self.worktrees[i].name == name)
        else {
            return false;
        };
        self.take_back_keys();
        // A row of its own starts at the end of its own log, from its
        // first process, as `j` and `k` move to one. The row already
        // selected keeps its preview where it was: this may be the first
        // half of a double click on it.
        if self.list_state.selected() != Some(row) {
            self.tail_scroll = 0;
            self.tail_index = 0;
            self.select_index(row);
        }
        let now = Instant::now();
        let double = last.is_some_and(|(when, before)| {
            before == name && now.saturating_duration_since(when) <= DOUBLE_CLICK
        });
        // While the filter is being typed, ⏎ keeps the filter rather than
        // acting on a worktree, so a second click there is only a click.
        if double && !self.typing_filter() {
            self.enter_selected();
        } else {
            self.last_click = Some((now, name));
        }
        true
    }

    /// What any key but its own takes back: the leader, and a first press
    /// waiting for its second. A click is never the second press.
    fn take_back_keys(&mut self) {
        self.leader = false;
        self.armed = None;
    }

    /// How many arrow presses a step of the wheel is in a modal or the log
    /// viewer, or `None` where an arrow moves nothing.
    fn wheel_steps(&self) -> Option<usize> {
        if let Some(modal) = &self.modal {
            // Every modal by name, so a new one decides.
            return match modal {
                Modal::Help | Modal::Messages => Some(WHEEL_LINES),
                Modal::Create { .. }
                | Modal::PullRequests { .. }
                | Modal::Theme { .. }
                | Modal::Mode { .. }
                | Modal::Question { .. }
                | Modal::Git {
                    stage: GitStage::Menu { .. },
                    ..
                } => Some(1),
                Modal::Git { .. }
                | Modal::Remove { .. }
                | Modal::StopAll { .. }
                | Modal::Unshare { .. }
                | Modal::Share { .. }
                | Modal::SwitchMode { .. } => None,
            };
        }
        if self.setup_screen.is_some() {
            return None;
        }
        if let View::Log(view) = &self.view {
            let typing = view.search_mode == SearchMode::Typing;
            return (self.inspect.is_some() || !typing).then_some(WHEEL_LINES);
        }
        None
    }
}
