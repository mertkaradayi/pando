//! Marks: rows picked with `+`, by worktree name, so that a key acts on
//! all of them at once where several at once is natural. Every other key
//! still acts on the row under the cursor.
//!
//! A mark is a name, not a row: it survives a refresh, a re-sort and a
//! filter that hides its row, and goes when its worktree does. The main
//! checkout may be marked — it starts, stops and restarts like any row —
//! and a removal of the marks skips it, saying so.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::App;
use super::dialogs::{Modal, RemoveBlocker};
use super::pending::PendingKind;
use super::removing::Asked;
use crate::actions;

/// What `x` and `r` arm on with rows marked. No worktree is called this,
/// so the second press counts only on the same set: a mark added or taken
/// away between the two presses asks again.
fn armed_on(names: &[String]) -> String {
    format!("\u{0}marks\u{0}{}", names.join("\u{0}"))
}

/// What removing the marks does with one of them, read again at every
/// paint and every key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkedRemoval {
    /// `y` removes it, with what it takes or stops on the way.
    Goes(Vec<RemoveBlocker>),
    /// Only `F` removes it: git would refuse its changes, or what sits
    /// beside a set's checkouts.
    NeedsForce(Vec<RemoveBlocker>),
    /// Never removed, and why: locked, holding a checkout that is not the
    /// set's, the main checkout, or gone already.
    Skipped(String),
}

impl MarkedRemoval {
    /// Whether a confirm with `force` or without removes it.
    pub fn removed(&self, force: bool) -> bool {
        match self {
            MarkedRemoval::Goes(_) => true,
            MarkedRemoval::NeedsForce(_) => force,
            MarkedRemoval::Skipped(_) => false,
        }
    }
}

impl App {
    /// `+`: marks the row under the cursor, or unmarks it, and moves down,
    /// as a file manager's selection key does, so a run of rows is marked
    /// by holding it.
    pub(super) fn toggle_mark(&mut self) {
        let Some(name) = self.selected_worktree().map(|w| w.name.clone()) else {
            self.set_error("nothing selected");
            return;
        };
        if !self.marks.remove(&name) {
            self.marks.insert(name);
        }
        self.move_cursor(1);
    }

    /// Whether `name` is marked.
    pub fn is_marked(&self, name: &str) -> bool {
        self.marks.contains(name)
    }

    /// The marked worktrees in the order the list shows them, then any a
    /// filter hides, in discovery's order.
    pub fn marked(&self) -> Vec<String> {
        let shown = self
            .filtered_indices
            .iter()
            .map(|&i| &self.worktrees[i].name);
        let hidden = self.worktrees.iter().map(|w| &w.name).filter(|name| {
            !self
                .filtered_indices
                .iter()
                .any(|&i| self.worktrees[i].name == **name)
        });
        shown
            .chain(hidden)
            .filter(|name| self.marks.contains(*name))
            .cloned()
            .collect()
    }

    /// How many of the marks a filter hides.
    pub fn marks_hidden(&self) -> usize {
        self.marks
            .iter()
            .filter(|name| {
                !self
                    .filtered_indices
                    .iter()
                    .any(|&i| self.worktrees[i].name == **name)
            })
            .count()
    }

    /// Drops the mark of every worktree the list no longer has: removed
    /// here, or by `pando rm` in another pane.
    pub(super) fn forget_gone_marks(&mut self) {
        let worktrees = &self.worktrees;
        self.marks
            .retain(|name| worktrees.iter().any(|w| w.name == *name));
    }

    /// esc with marks: unmarks every row.
    pub(super) fn clear_marks(&mut self) {
        let count = std::mem::take(&mut self.marks).len();
        self.set_status(match count {
            1 => "unmarked 1 worktree".to_string(),
            n => format!("unmarked {n} worktrees"),
        });
    }

    /// `d` with rows marked. One marked row gets the dialog one row
    /// always gets; several get one dialog for them all.
    pub(super) fn open_remove_marked(&mut self) {
        let names = self.marked();
        if let [name] = names.as_slice() {
            return self.open_remove_named(name.clone());
        }
        // What git said may be minutes old; the dialog is where it
        // matters, and each answer repaints it as it lands.
        self.refresh_git(Some(names.clone()));
        self.modal = Some(Modal::RemoveMarked { names });
    }

    /// What removing the marks does with `name`.
    pub fn marked_removal(&self, name: &str) -> MarkedRemoval {
        if !self.worktrees.iter().any(|w| w.name == name) {
            return MarkedRemoval::Skipped("gone already".to_string());
        }
        if let Some(actions::RemovalState::UnderWay(mark)) = self.removal_elsewhere(name) {
            return MarkedRemoval::Skipped(format!(
                "already being removed (pid {})",
                mark.owner_pid
            ));
        }
        if self.is_main(name) {
            return MarkedRemoval::Skipped("the main checkout, never removed".to_string());
        }
        let blockers = self.remove_blockers(name);
        if let Some(fatal) = blockers.iter().find(|b| b.is_fatal()) {
            return MarkedRemoval::Skipped(fatal.short());
        }
        match blockers.iter().any(RemoveBlocker::needs_force) {
            true => MarkedRemoval::NeedsForce(blockers),
            false => MarkedRemoval::Goes(blockers),
        }
    }

    /// The dialog's `y` removes every marked worktree git would let go
    /// of, and `F` every one it can, changes and all: each its own
    /// removal, all of them started at once. Anything else closes it.
    pub(super) fn handle_remove_marked_key(&mut self, key: KeyEvent, names: Vec<String>) {
        let force = match key.code {
            KeyCode::Char('y') | KeyCode::Enter => false,
            KeyCode::Char('F') => true,
            _ => return,
        };
        let verdicts: Vec<(String, MarkedRemoval)> = names
            .iter()
            .map(|name| (name.clone(), self.marked_removal(name)))
            .collect();
        let going: Vec<&String> = verdicts
            .iter()
            .filter(|(_, verdict)| verdict.removed(force))
            .map(|(name, _)| name)
            .collect();
        let forced: Vec<String> = verdicts
            .iter()
            .filter(|(_, verdict)| matches!(verdict, MarkedRemoval::NeedsForce(_)))
            .map(|(name, _)| self.label_of(name))
            .collect();
        if going.is_empty() {
            // Only F would remove any, as the single dialog says of one
            // with changes: it stays, and says which key does it.
            if !forced.is_empty() {
                self.set_error(format!(
                    "{} {} uncommitted changes — F removes them anyway, esc keeps them",
                    crate::detect::listed(&forced),
                    crate::detect::has_or_have(forced.len())
                ));
                self.modal = Some(Modal::RemoveMarked { names });
            } else {
                let why: Vec<String> = verdicts
                    .iter()
                    .filter_map(|(name, verdict)| match verdict {
                        MarkedRemoval::Skipped(why) => {
                            Some(format!("{}: {why}", self.label_of(name)))
                        }
                        _ => None,
                    })
                    .collect();
                self.set_error(format!(
                    "none of the marked worktrees can be removed — {}",
                    why.join(" · ")
                ));
            }
            return;
        }
        let mut waiting = Vec::new();
        for name in going {
            // Said once for the set below, rather than an error apiece.
            if let Some(busy) = self.busy_with(PendingKind::Remove, name) {
                waiting.push(format!("{} waits for {busy}", self.label_of(name)));
                continue;
            }
            let created_by_pando = self.created_by_pando.get(name).copied().unwrap_or(false);
            if self.spawn_remove(name.clone(), !created_by_pando, force) {
                // On its way out: the mark has nothing left to do, and a
                // key pressed meanwhile is not for it.
                self.marks.remove(name);
            }
        }
        // What stays, and why, after the spinner's first line: the rows
        // still marked say which.
        let mut stays: Vec<String> = waiting;
        if !force && !forced.is_empty() {
            let (its, it) = match forced.len() {
                1 => ("its", "it"),
                _ => ("their", "them"),
            };
            stays.push(format!(
                "{} kept for {its} changes — still marked, d then F removes {it}",
                crate::detect::listed(&forced),
            ));
        }
        for (name, verdict) in &verdicts {
            if let MarkedRemoval::Skipped(why) = verdict {
                stays.push(format!("{} skipped: {why}", self.label_of(name)));
            }
        }
        if !stays.is_empty() {
            self.set_error(stays.join(" · "));
        }
    }

    /// Marked worktrees as a sentence names them: by branch while that
    /// reads, counted after three.
    fn named(&self, names: &[String]) -> String {
        match names.len() {
            0..=3 => {
                let labels: Vec<String> = names.iter().map(|n| self.label_of(n)).collect();
                crate::detect::listed(&labels)
            }
            n => format!("{n} marked worktrees"),
        }
    }

    /// What an action of `kind` on `name` would wait for, as the spinner
    /// says it: `stopping feat/x`. Asked first, so a set says it once for
    /// all its rows rather than an error apiece.
    fn busy_with(&self, kind: PendingKind, name: &str) -> Option<String> {
        self.pending
            .iter()
            .find(|p| p.conflicts(kind, &[name.to_string()]))
            .map(|p| format!("{} {}", p.doing(), p.label))
    }

    /// What a key on the marks left alone, and why, after what it started.
    fn say_what_was_left(&mut self, started: usize, left: Vec<String>) {
        if left.is_empty() {
            return;
        }
        let left = left.join(" · ");
        match started {
            0 => self.set_status(left),
            _ => self.set_status(format!("left alone: {left}")),
        }
    }

    /// `s` with rows marked: every marked worktree that is not up starts,
    /// in the mode it last ran in, each on its own worker and all at once.
    pub(super) fn start_marked(&mut self) {
        if self.refuses_nothing_to_run() {
            return;
        }
        let mut started = 0;
        let mut left = Vec::new();
        for name in self.marked() {
            let label = self.label_of(&name);
            if let Some(why) = self.removal_refusal(&name, Asked::Start) {
                left.push(why);
            } else if self.is_up(&name) {
                left.push(format!("{label} is up already"));
            } else if let Some(busy) = self.busy_with(PendingKind::Start, &name) {
                left.push(format!("{label} waits for {busy}"));
            } else {
                self.start_named(name, actions::Mode::Remembered);
                started += 1;
            }
        }
        self.say_what_was_left(started, left);
    }

    /// `x` with rows marked: every marked worktree with something up
    /// stops, at once, and a start or a restart in flight on one is
    /// cancelled, as `x` on its row does. While any of them runs, `x` asks
    /// for a second press first, once for the whole set, as it does for
    /// one.
    pub(super) fn stop_marked(&mut self) {
        let marked = self.marked();
        let starting: Vec<String> = marked
            .iter()
            .filter(|name| self.start_to_cancel(name))
            .cloned()
            .collect();
        let removed: Vec<String> = marked
            .iter()
            .filter_map(|name| self.removal_refusal(name, Asked::Stop))
            .collect();
        let targets: Vec<String> = marked
            .iter()
            .filter(|name| !starting.contains(name))
            .filter(|name| self.removal_refusal(name, Asked::Stop).is_none())
            .filter(|name| self.phase_of(name).is_some() || self.is_live(name))
            .cloned()
            .collect();
        if targets.is_empty() && starting.is_empty() && !removed.is_empty() {
            return self.set_error(removed.join(" · "));
        }
        if targets.is_empty() && starting.is_empty() {
            let message = match marked.as_slice() {
                [one] => format!("{} is not running", self.label_of(one)),
                _ => format!("none of the {} marked worktrees is running", marked.len()),
            };
            return self.set_status(message);
        }
        let acting: Vec<String> = starting.iter().chain(&targets).cloned().collect();
        let interrupts = targets.iter().any(|name| self.is_up(name));
        let what = format!("stop {}", self.named(&acting));
        if interrupts && !self.pressed_again('x', &armed_on(&acting), &what) {
            return;
        }
        let cancelled: Vec<String> = starting
            .iter()
            .filter_map(|name| self.cancel_start(name))
            .collect();
        if !cancelled.is_empty() {
            self.set_status(format!("cancelling {}…", cancelled.join(", ")));
        }
        let mut started = 0;
        let mut left = removed;
        for name in targets {
            match self.busy_with(PendingKind::Stop, &name) {
                Some(busy) => left.push(format!("{} waits for {busy}", self.label_of(&name))),
                None => {
                    self.stop_named(name);
                    started += 1;
                }
            }
        }
        self.say_what_was_left(started, left);
    }

    /// `r` with rows marked: every marked worktree restarts in the mode it
    /// last ran in, and one that is stopped starts, at once. While any of
    /// them runs, `r` asks for a second press first, once for the set.
    pub(super) fn restart_marked(&mut self) {
        let marked = self.marked();
        let interrupts = marked.iter().any(|name| self.is_up(name));
        let what = format!("restart {}", self.named(&marked));
        if interrupts && !self.pressed_again('r', &armed_on(&marked), &what) {
            return;
        }
        let mut started = 0;
        let mut left = Vec::new();
        for name in marked {
            if let Some(why) = self.removal_refusal(&name, Asked::Start) {
                left.push(why);
                continue;
            }
            if let Some(busy) = self.busy_with(PendingKind::Restart, &name) {
                left.push(format!("{} waits for {busy}", self.label_of(&name)));
                continue;
            }
            match self.phase_of(&name) {
                None => self.start_named(name, actions::Mode::Remembered),
                Some(_) => self.restart_named(name, actions::Mode::Remembered, None),
            }
            started += 1;
        }
        self.say_what_was_left(started, left);
    }
}
