//! Worktrees a removal has begun on that this TUI is not running: `pando
//! rm` in another pane, an agent's, or one a quit interrupted. Their rows
//! say so, and the keys that would start, stop or remove them again are
//! refused before any worker is sent, in the words `rm`'s own refusals
//! use.

use std::collections::BTreeMap;

use crate::actions::{self, BEING_REMOVED, RemovalState};
use crate::paths::PandoPaths;
use crate::state::State;

use super::App;

/// Where each removal the state just read marks stands, asked only when it
/// marks one: [`actions::removals`] takes the state lock for a moment, and
/// a project with no removal under way should not pay for that once a
/// second. Never called with the lock held, and never waits for it.
pub(super) fn removals_in(paths: &PandoPaths, state: &State) -> BTreeMap<String, RemovalState> {
    if !state.worktrees.values().any(|r| r.removing.is_some()) {
        return BTreeMap::new();
    }
    actions::removals(paths).unwrap_or_default()
}

/// What a refused key would have done, for the words after the dash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Asked {
    Start,
    Stop,
    Remove,
    Share,
}

impl Asked {
    fn nothing(self) -> &'static str {
        match self {
            Asked::Start => "nothing was started",
            Asked::Stop => "it stops what runs itself",
            Asked::Remove => "it is going already",
            Asked::Share => "nothing was shared",
        }
    }
}

impl App {
    /// Where a removal of `name` stands, when one has begun that this TUI
    /// is not running: its own removal is an action in flight, and the
    /// row and the keys already say that one.
    pub fn removal_elsewhere(&self, name: &str) -> Option<&RemovalState> {
        if self.pending.iter().any(|p| p.name == name) {
            return None;
        }
        self.removals.get(name)
    }

    /// Why `asked` is refused on `name`, if it is: everything while a
    /// removal is under way, and a start or a share once one was
    /// interrupted, which `d` finishes.
    pub(super) fn removal_refusal(&self, name: &str, asked: Asked) -> Option<String> {
        let label = self.label_of(name);
        match self.removal_elsewhere(name)? {
            RemovalState::UnderWay(mark) => Some(format!(
                "{label} {BEING_REMOVED} (pid {}) — {}",
                mark.owner_pid,
                asked.nothing()
            )),
            RemovalState::Interrupted(_) if matches!(asked, Asked::Start | Asked::Share) => {
                Some(format!(
                    "a removal of {label} was interrupted — {}; d finishes it",
                    asked.nothing()
                ))
            }
            RemovalState::Interrupted(_) => None,
        }
    }

    /// Refuses `asked` on `name` where a removal has begun, saying why.
    /// Returns whether it did.
    pub(super) fn refuse_removed(&mut self, name: &str, asked: Asked) -> bool {
        match self.removal_refusal(name, asked) {
            Some(why) => {
                self.set_error_about(name, why);
                true
            }
            None => false,
        }
    }
}
