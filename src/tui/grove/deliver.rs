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
            GroveEvent::NotSaved(why) | GroveEvent::Unread(why) => {
                self.say(why, true);
                true
            }
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
                // Its own undo has run, or it had none to run: what is
                // left to ask is the grove's, and only a key answers it.
                // Left set, every tick would ask to quit again and answer
                // the grove's question for the developer.
                opened.app.quitting = false;
            }
            self.request_quit(Some(&id));
        }
        // The confirming `q` cancelled the starts in flight; pando goes
        // once every one has run its undo.
        if self.quitting && !self.cancelling() {
            self.should_quit = true;
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
        self.forget_opening_note(&id);
        // A project opened only to stop what `X` confirmed stays behind:
        // the developer asked for a stop, not to leave where they are.
        let for_stop = self.stop_after_open.contains_key(&id);
        let wanted = self.front.as_deref() == Some(id.as_str())
            || (!for_stop && self.selected().is_some_and(|s| s.id == id));
        // Never from under a dialog, a filter or the log viewer opened in
        // the meantime: what is typed there would land on the list of the
        // project that just opened, as its keys.
        let free = self.front_app().is_none_or(|app| {
            app.idle_on_list() || (app.setup_screen.is_some() && app.modal.is_none())
        });
        match result {
            Ok(app) => {
                if wanted && !free {
                    let name = app.paths.project.display_name.clone();
                    self.adopt(app, false);
                    self.say(
                        format!("{name} is open — ] or ⏎ in the column brings it"),
                        false,
                    );
                } else {
                    self.adopt(app, wanted);
                }
                // Its own stop only: another project's waits for its own.
                if let Some(names) = self.stop_after_open.remove(&id) {
                    self.stop_in(&id, names);
                }
            }
            Err(e) => {
                self.stop_after_open.remove(&id);
                self.say(e, true);
            }
        }
        true
    }

    /// "opening <name>…" was said on whichever app was in front when the
    /// open began; once the open has landed it is news about nothing, and
    /// left there it would greet the developer on coming back to that app.
    fn forget_opening_note(&mut self, id: &str) {
        let Some(name) = self
            .summaries
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.name.clone())
        else {
            return;
        };
        let said = opening_note(&name);
        for opened in self.open.values_mut() {
            if opened
                .app
                .status
                .as_ref()
                .is_some_and(|s| s.message == said)
            {
                opened.app.status = None;
            }
        }
        if self.note.as_ref().is_some_and(|(n, _)| *n == said) {
            self.note = None;
        }
    }

    /// A press of `q` that would end the TUI, from the app of `from` or
    /// from the column. Work in flight in another project is named first,
    /// and only a second `q` pressed while that is fresh goes on
    /// ([`confirm_quit`](Self::confirm_quit)): its own app asked about its
    /// own already. Never confirms by itself, so an app asking again is
    /// never the developer's answer.
    pub(super) fn request_quit(&mut self, from: Option<&str>) {
        let busy: Vec<String> = self
            .open
            .iter()
            .filter(|(id, o)| Some(id.as_str()) != from && !o.app.pending.is_empty())
            .map(|(_, o)| o.app.paths.project.display_name.clone())
            .collect();
        if busy.is_empty() {
            self.should_quit = true;
            return;
        }
        self.quit_armed = Some(Instant::now());
        let message = format!(
            "{} still in flight in {} — q again cancels any start, abandons the rest and quits",
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

    /// Whether a first `q` is waiting for its second.
    pub(super) fn quit_is_armed(&self) -> bool {
        self.quit_armed.is_some_and(|at| at.elapsed() < ARM_TTL)
    }

    /// The second `q`: what a project's own second `q` does, in every
    /// open project. A start or a restart in flight is cancelled rather
    /// than abandoned, and pando quits once each has run its undo —
    /// abandoned, a switch to isolated mode leaves containers up that its
    /// record does not say are isolated. Anything else is abandoned. A `q`
    /// meanwhile quits at once, and so does ctrl-c.
    pub(super) fn confirm_quit(&mut self) {
        self.quit_armed = None;
        let mut cancelled = Vec::new();
        for opened in self.open.values() {
            for pending in &opened.app.pending {
                if let Some(cancel) = &pending.cancel {
                    cancel.cancel();
                    cancelled.push(format!(
                        "{} in {}",
                        pending.label, opened.app.paths.project.display_name
                    ));
                }
            }
        }
        if cancelled.is_empty() {
            self.should_quit = true;
            return;
        }
        self.quitting = true;
        self.say(
            format!(
                "cancelling {} — pando quits once it is undone · q quits now",
                cancelled.join(", ")
            ),
            false,
        );
    }

    /// Whether a start or a restart some open app runs is still in flight
    /// with a cancel, which a quit waits for.
    fn cancelling(&self) -> bool {
        self.open
            .values()
            .any(|o| o.app.pending.iter().any(|p| p.cancel.is_some()))
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
        // The grove's own dialog or help has the screen, as it has the
        // keys: nothing behind it takes a click, which on the front app's
        // footer would press a key of its list.
        if self.confirm.is_some() || self.help.is_some() {
            return false;
        }
        let at = Position::new(mouse.column, mouse.row);
        if !self.hits.column.contains(at) {
            // A press on the project in front gives it the keys, as `tab`
            // does: the row clicked is the one `s` then starts.
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) && self.front.is_some()
            {
                self.focus = Focus::List;
            }
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

/// What the grove says while a project opens, on the app in front.
pub(super) fn opening_note(name: &str) -> String {
    format!("opening {name}…")
}
