//! The mouse: for now the wheel, which goes where the keyboard goes.
//!
//! A terminal that nobody has asked for the mouse turns the wheel into
//! arrow keys on the alternate screen, so the wheel moved the list, the
//! viewer and a picker before pando took the mouse. A step of it is
//! still an arrow key, sent where an arrow means moving through
//! something, and nowhere else: an arrow closes the remove dialog.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use super::{App, GitStage, Modal, SearchMode, View};

/// Lines a step of the wheel moves what is read by scrolling: help, the
/// messages, the log viewer. Three is what a terminal sends for a step
/// when it turns the wheel into arrow keys. A cursor on a list moves one
/// row a step, as an arrow moves it.
const WHEEL_LINES: usize = 3;

impl App {
    /// Whether the terminal should report the mouse to pando now: unless
    /// `[ui] mouse = false` says not, and never on the setup screen, whose
    /// prompt is laid out to be selected with the mouse and pasted.
    pub fn wants_mouse(&self) -> bool {
        self.config.ui.mouse() && self.setup_screen.is_none()
    }

    /// Returns whether the frame needs repainting.
    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        let code = match mouse.kind {
            MouseEventKind::ScrollUp => KeyCode::Up,
            MouseEventKind::ScrollDown => KeyCode::Down,
            _ => return false,
        };
        let Some(steps) = self.wheel_steps() else {
            return false;
        };
        for _ in 0..steps {
            self.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
        true
    }

    /// How many arrow presses a step of the wheel is where the keyboard is
    /// now, or `None` where an arrow moves nothing.
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
        Some(1)
    }
}
