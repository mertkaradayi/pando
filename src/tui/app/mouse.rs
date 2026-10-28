//! The mouse: a click on a key hint presses its key, a click on a
//! worktree, a picker's option or a line of the log viewer moves the
//! cursor there and a second one is ⏎ on it, a click on a process the log
//! preview names or on a viewer's tab shows that log, and the wheel
//! scrolls the preview under the pointer or moves the list.
//!
//! The paint records where those are, in [`Hits`]. Where the wheel has
//! nothing of its own — a modal, the log viewer — a step of it is the
//! arrow key a terminal sends for it when nobody has the mouse, sent only
//! where an arrow moves something: an arrow closes the remove dialog.

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

/// How soon a second press on the same thing has to follow the first to
/// be a double click: what desktops take when nobody has set it.
pub(super) const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// What a press landed on, so a second press can tell whether it is on
/// the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clicked {
    /// A worktree's row in the list.
    Row(String),
    /// An option of the open picker, by the index its highlight takes.
    Choice(usize),
    /// A line of the log viewer, by its rank among the lines it shows.
    Line(usize),
}

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
    /// Each key hint on top — an open modal's, or the footer's when none
    /// is — with its key as the hint writes it.
    pub keys: Vec<(Rect, String)>,
    /// The open picker's options on screen, each with the index its
    /// highlight takes there.
    pub options: Vec<(Rect, usize)>,
    /// The log viewer's body, and the rank of the line each of its rows
    /// shows, top to bottom: a wrapped line is several rows.
    pub viewer_lines: Option<(Rect, Vec<usize>)>,
    /// The log viewer's tabs, each with the number it is labelled with.
    pub viewer_tabs: Vec<(Rect, usize)>,
}

/// The keys a hint's key stands for, pressed in turn: `esc`, `⏎`, `tab`,
/// one character, or the leader and the key after it (`space g`). `None`
/// for a hint that names a choice of keys rather than one, such as `↑↓`
/// or `j/k`.
pub(super) fn keys_of(hint: &str) -> Option<Vec<KeyCode>> {
    let one = |key: &str| match key {
        "esc" => Some(KeyCode::Esc),
        "⏎" => Some(KeyCode::Enter),
        "tab" => Some(KeyCode::Tab),
        "space" => Some(KeyCode::Char(' ')),
        _ => {
            let mut chars = key.chars();
            let c = chars.next()?;
            chars.next().is_none().then_some(KeyCode::Char(c))
        }
    };
    match hint.split_once(' ') {
        Some(("space", then)) => Some(vec![KeyCode::Char(' '), one(then)?]),
        Some(_) => None,
        None => Some(vec![one(hint)?]),
    }
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

    /// A left press. A press on anything but what the last one landed on
    /// ends a double click begun there.
    fn click(&mut self, at: Position) -> bool {
        let last = self.last_click.take();
        // A hint says what its key does where it is drawn, so a click on
        // it is that key, pressed: `esc cancel` cancels. Nothing is taken
        // back first — `x` twice and `space g` are a key's own business.
        let keys = self
            .hits
            .keys
            .iter()
            .find(|(cells, _)| cells.contains(at))
            .and_then(|(_, key)| keys_of(key));
        if let Some(keys) = keys {
            for code in keys {
                self.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
            }
            return true;
        }
        let choice = self
            .hits
            .options
            .iter()
            .find(|(rows, _)| rows.contains(at))
            .map(|(_, index)| *index);
        if let Some(index) = choice {
            self.choose(index, last);
            return true;
        }
        if self.viewer_has_the_mouse() {
            return self.click_in_viewer(at, last);
        }
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
        // While the filter is being typed, ⏎ keeps the filter rather than
        // acting on a worktree, so a second click there is only a click.
        if self.second_press(last, Clicked::Row(name)) && !self.typing_filter() {
            self.enter_selected();
        }
        true
    }

    /// Whether the log viewer's lines and tabs take a click: nothing over
    /// them, and no query being typed, which has the keyboard.
    fn viewer_has_the_mouse(&self) -> bool {
        self.modal.is_none()
            && self.inspect.is_none()
            && self
                .log_view()
                .is_some_and(|view| view.search_mode != SearchMode::Typing)
    }

    /// A press in the log viewer: a tab shows its source, as its digit
    /// does, and a line takes the cursor, leaving the live tail as `k`
    /// does. A second press on the line is ⏎, which inspects it.
    fn click_in_viewer(&mut self, at: Position, last: Option<(Instant, Clicked)>) -> bool {
        let tab = self
            .hits
            .viewer_tabs
            .iter()
            .find(|(cells, _)| cells.contains(at))
            .map(|(_, number)| *number);
        if let Some(number) = tab {
            self.jump_to_log_source(number);
            return true;
        }
        let Some(rank) = self.hits.viewer_lines.as_ref().and_then(|(body, ranks)| {
            body.contains(at)
                .then(|| ranks.get(usize::from(at.y - body.y)))
                .flatten()
                .copied()
        }) else {
            return false;
        };
        if let Some(view) = self.log_view_mut() {
            view.follow = false;
            view.cursor = rank;
        }
        if self.second_press(last, Clicked::Line(rank)) {
            self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
        true
    }

    /// A press on a picker's option. The highlight goes there by the
    /// picker's own arrows, so what moving does — the theme picker trying
    /// each theme on — happens as it does from the keyboard. A second
    /// press is ⏎; in a set question a press ticks the option instead, as
    /// space does, and ⏎ is its hint's.
    fn choose(&mut self, index: usize, last: Option<(Instant, Clicked)>) {
        let Some((from, set)) = self.highlight() else {
            return;
        };
        let code = if index < from {
            KeyCode::Up
        } else {
            KeyCode::Down
        };
        for _ in 0..index.abs_diff(from) {
            self.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        // Not there after all — the picker changed under the frame — so
        // nothing is chosen.
        if self.highlight().map(|(at, _)| at) != Some(index) {
            return;
        }
        if set {
            self.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        } else if self.second_press(last, Clicked::Choice(index)) {
            self.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
    }

    /// Where the open picker's highlight is, and whether it is a set
    /// question's, where a press ticks rather than chooses. `None` with no
    /// picker open, or while a question's answer is typed.
    fn highlight(&self) -> Option<(usize, bool)> {
        match self.modal.as_ref()? {
            Modal::Create { selected, .. }
            | Modal::PullRequests { selected, .. }
            | Modal::Theme { selected, .. }
            | Modal::Mode { selected, .. }
            | Modal::Git {
                stage: GitStage::Menu { selected, .. },
                ..
            } => Some((*selected, false)),
            Modal::Question {
                question,
                selected,
                custom: None,
                ..
            } => Some((*selected, question.multi)),
            _ => None,
        }
    }

    /// Whether a press on `target` now is the second of a double click
    /// after `last`. One that is not is remembered as a first.
    fn second_press(&mut self, last: Option<(Instant, Clicked)>, target: Clicked) -> bool {
        let now = Instant::now();
        let double = last.is_some_and(|(when, before)| {
            before == target && now.saturating_duration_since(when) <= DOUBLE_CLICK
        });
        if !double {
            self.last_click = Some((now, target));
        }
        double
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
