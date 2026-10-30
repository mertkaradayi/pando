//! Marks: rows picked with `+`, by worktree name, so that a key acts on
//! all of them at once where several at once is natural. Every other key
//! still acts on the row under the cursor.
//!
//! A mark is a name, not a row: it survives a refresh, a re-sort and a
//! filter that hides its row, and goes when its worktree does. The main
//! checkout may be marked — it starts, stops and restarts like any row —
//! and a removal of the marks skips it, saying so.

use super::App;

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
}
