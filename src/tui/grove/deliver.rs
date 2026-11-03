//! Every event the grove's loop takes, handed to whom it is for: the
//! terminal's input to the column or the app in front, time to every open
//! app, an app's own events to that app wherever it is.

use ratatui::crossterm::event::{Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;
use std::time::Instant;

use crate::actions::ProjectSummary;
use crate::tui::app::{ARM_TTL, AppEvent, LaunchRequest};

use super::{Focus, Grove, GroveEvent};

impl Grove {
    /// Returns whether the frame needs repainting.
    pub fn handle_event(&mut self, ev: GroveEvent) -> bool {
        let repaint = match ev {
            GroveEvent::Input(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                self.handle_key(key);
                true
            }
            GroveEvent::Input(Event::Key(_)) => false,
            GroveEvent::Input(Event::Resize(..)) => true,
            GroveEvent::Input(Event::Mouse(mouse)) => self.handle_mouse(mouse),
            GroveEvent::Input(other) => self.send_front(AppEvent::Input(other)),
            GroveEvent::Tick => self.tick_all(),
            GroveEvent::Theme(resolved) => {
                crate::theme::set_palette(resolved.palette);
                for opened in self.open.values_mut() {
                    opened.app.adopt_theme((*resolved).clone());
                }
                self.theme = Some(*resolved);
                true
            }
            GroveEvent::Summaries(summaries) => {
                self.take_summaries(summaries);
                true
            }
            GroveEvent::Opened(id, result) => self.opened(id, *result),
            GroveEvent::Project(id, ev) => {
                let front = self.front.as_deref() == Some(id.as_str());
                match self.open.get_mut(&id) {
                    Some(opened) => opened.app.handle_event(ev) && front,
                    None => false,
                }
            }
        };
        self.after_event();
        repaint
    }

    /// After every event: an app that asked to quit, and the summaries
    /// read again when an action began or ended anywhere.
    fn after_event(&mut self) {
        let quitting: Vec<String> = self
            .open
            .iter()
            .filter(|(_, o)| o.app.should_quit)
            .map(|(id, _)| id.clone())
            .collect();
        for id in quitting {
            if let Some(opened) = self.open.get_mut(&id) {
                opened.app.should_quit = false;
            }
            self.request_quit(Some(&id));
        }
        let in_flight: usize = self.open.values().map(|o| o.app.pending.len()).sum();
        if in_flight != self.in_flight {
            self.in_flight = in_flight;
            self.read_summaries_now();
        }
    }

    /// Hands an event to the app in front, if there is one.
    pub(super) fn send_front(&mut self, ev: AppEvent) -> bool {
        match self.front_app_mut() {
            Some(app) => app.handle_event(ev),
            None => false,
        }
    }

    /// Time for every open app: the one in front all of it, the rest what
    /// keeps their work moving.
    fn tick_all(&mut self) -> bool {
        self.tick = self.tick.wrapping_add(1);
        let front = self.front.clone();
        let mut repaint = false;
        for (id, opened) in self.open.iter_mut() {
            if Some(id) == front.as_ref() {
                repaint |= opened.app.handle_event(AppEvent::Tick);
            } else {
                // The column's mark of a project with work in flight.
                repaint |= opened.app.background_tick();
            }
        }
        // The home pane's leaves quake.
        repaint || (front.is_none() && self.tick.is_multiple_of(2))
    }

    /// Every project read again: the cursor stays on the project it was
    /// on, wherever it is now listed.
    pub fn take_summaries(&mut self, summaries: Vec<ProjectSummary>) {
        let on = self.selected().map(|s| s.id.clone());
        self.summaries = summaries;
        self.cursor = on
            .and_then(|id| self.summaries.iter().position(|s| s.id == id))
            .unwrap_or(self.cursor)
            .min(self.summaries.len().saturating_sub(1));
    }

    /// Asks the summaries worker to read every project again now.
    pub(super) fn read_summaries_now(&self) {
        if let Some(poke) = &self.poke {
            let _ = poke.send(());
        }
    }

    fn opened(&mut self, id: String, result: Result<crate::tui::app::App, String>) -> bool {
        self.opening.retain(|o| o != &id);
        let wanted = self.front.as_deref() == Some(id.as_str())
            || self.selected().is_some_and(|s| s.id == id);
        match result {
            Ok(app) => {
                self.adopt(app, wanted);
                if let Some((stop, names)) = self.stop_after_open.take() {
                    match self.open.get_mut(&stop) {
                        Some(opened) => opened.app.stop_everything(names),
                        None => self.stop_after_open = Some((stop, names)),
                    }
                }
            }
            Err(e) => {
                if self.stop_after_open.as_ref().is_some_and(|(s, _)| s == &id) {
                    self.stop_after_open = None;
                }
                self.say(e, true);
            }
        }
        true
    }

    /// A press of `q` or ctrl-c that would end the TUI, from the app of
    /// `from` or from the column. Work in flight in another project is
    /// named first and abandoned only on a second `q`: its own app asked
    /// about its own already.
    pub(super) fn request_quit(&mut self, from: Option<&str>) {
        let busy: Vec<String> = self
            .open
            .iter()
            .filter(|(id, o)| Some(id.as_str()) != from && !o.app.pending.is_empty())
            .map(|(_, o)| o.app.paths.project.display_name.clone())
            .collect();
        let armed = self.quit_armed.is_some_and(|at| at.elapsed() < ARM_TTL);
        if busy.is_empty() || armed {
            self.should_quit = true;
            return;
        }
        self.quit_armed = Some(Instant::now());
        let message = format!(
            "{} still in flight in {} — q again abandons it and quits",
            match busy.len() {
                1 => "an action is",
                _ => "actions are",
            },
            busy.join(", ")
        );
        match self.front_app_mut() {
            Some(app) => app.set_prompt(message),
            None => self.say(message, false),
        }
    }

    /// A shell or an editor some open app asked for, with the app's id.
    pub fn take_launch(&mut self) -> Option<(String, LaunchRequest)> {
        self.open
            .iter_mut()
            .find_map(|(id, o)| o.app.launch.take().map(|r| (id.clone(), r)))
    }

    /// The mouse: the column's or the strip's own presses, and everything
    /// else the front app's, whose regions are where it was drawn.
    fn handle_mouse(&mut self, mouse: MouseEvent) -> bool {
        let at = Position::new(mouse.column, mouse.row);
        if !self.hits.column.contains(at) {
            return self.send_front(AppEvent::Input(Event::Mouse(mouse)));
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.hits.prev.contains(at) {
                    self.step_project(-1);
                } else if self.hits.next.contains(at) {
                    self.step_project(1);
                } else if let Some(&(_, i)) = self.hits.rows.iter().find(|(r, _)| r.contains(at)) {
                    self.cursor = i;
                    self.open_selected();
                    self.focus = Focus::Column;
                }
                true
            }
            MouseEventKind::ScrollUp => {
                self.move_cursor(-1);
                true
            }
            MouseEventKind::ScrollDown => {
                self.move_cursor(1);
                true
            }
            _ => false,
        }
    }
}
