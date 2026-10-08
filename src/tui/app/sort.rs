//! The order of the worktree list: the one `,` cycles, the list's title
//! names, and `[ui] sort` saves.
//!
//! The main checkout is the first row in every order, and the worktrees
//! with something up come next, so what runs is never scrolled out of
//! sight. Within each, the order chosen; whatever it cannot tell apart
//! keeps discovery's order, newest worktree first, so a row never moves
//! for a reason the title or its glyph does not give.

use std::cmp::Reverse;

use super::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListSort {
    /// Worktrees with a pull request first, the highest number first.
    #[default]
    Pr,
    /// The worktree made last first: discovery's own order.
    Newest,
    /// The worktree started last first; one never started goes after.
    Run,
    /// By branch, alphabetically.
    Name,
}

impl ListSort {
    /// Every order, in the order `,` cycles them.
    pub const ALL: [ListSort; 4] = [
        ListSort::Pr,
        ListSort::Newest,
        ListSort::Run,
        ListSort::Name,
    ];

    /// Its spelling in `[ui] sort`, one of
    /// [`crate::config::LIST_SORTS`].
    pub fn word(self) -> &'static str {
        match self {
            ListSort::Pr => "pr",
            ListSort::Newest => "newest",
            ListSort::Run => "run",
            ListSort::Name => "name",
        }
    }

    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sort| sort.word() == word)
    }

    /// What `[ui] sort` says, or the default. Validation already refused
    /// a word it does not know.
    pub fn from_config(config: &crate::config::Config) -> Self {
        config
            .ui
            .sort
            .as_deref()
            .and_then(Self::from_word)
            .unwrap_or_default()
    }

    /// What the list's title says about it.
    pub fn title(self) -> &'static str {
        match self {
            ListSort::Pr => "by PR",
            ListSort::Newest => "newest first",
            ListSort::Run => "last run first",
            ListSort::Name => "by name",
        }
    }

    pub fn next(self) -> Self {
        let at = Self::ALL.iter().position(|&s| s == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }
}

impl App {
    /// Puts `rows`, indices into `worktrees`, in the order chosen: the
    /// main checkout, then what is up, then the rest. Stable, so ties keep
    /// the order they came in.
    pub(super) fn sort_rows(&self, rows: &mut [usize]) {
        let group = |i: usize| {
            let name = &self.worktrees[i].name;
            (!self.is_main(name), !self.is_live(name))
        };
        match self.sort {
            ListSort::Pr => rows.sort_by_key(|&i| {
                let pr = self.pr_for(&self.worktrees[i]).map(|pr| pr.number);
                (group(i), Reverse(pr))
            }),
            ListSort::Newest => rows.sort_by_key(|&i| group(i)),
            ListSort::Run => rows.sort_by_key(|&i| {
                let ran = self
                    .record_for(&self.worktrees[i].name)
                    .and_then(|record| record.last_run());
                (group(i), Reverse(ran))
            }),
            ListSort::Name => rows.sort_by_cached_key(|&i| {
                let wt = &self.worktrees[i];
                let label = wt.branch.as_deref().unwrap_or(&wt.name).to_lowercase();
                (group(i), label)
            }),
        }
    }

    /// `,`: the next order, the cursor kept on its worktree, and the
    /// choice saved for the next session.
    pub(super) fn cycle_sort(&mut self) {
        self.sort = self.sort.next();
        self.refilter();
        self.save_sort(self.sort);
    }

    /// Written to the user layer on a worker, like a theme: how somebody
    /// likes their list is theirs, not the project's.
    #[cfg(not(test))]
    fn save_sort(&mut self, sort: ListSort) {
        use super::background::AppEvent;
        use crate::config::{self, Layer};
        self.set_status(format!("sorted {}", sort.title()));
        let paths = self.paths.clone();
        let tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let saved = config::patch(&paths, Layer::User, |doc| {
                set_sort(doc, sort);
                Ok(())
            });
            if let Err(e) = saved {
                let _ = tx.send(AppEvent::LaunchFailed(format!(
                    "could not save the list's order: {e:#}"
                )));
            }
        });
    }

    #[cfg(test)]
    fn save_sort(&mut self, sort: ListSort) {
        self.set_status(format!("sorted {}", sort.title()));
    }
}

/// `[ui] sort = "<word>"`, leaving the rest of the file as it was.
pub(super) fn set_sort(doc: &mut toml_edit::DocumentMut, sort: ListSort) {
    let ui = doc
        .entry("ui")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
    // `ui = { ... }` and `ui.sort = ...` are the same table to a reader,
    // and a save that only found `[ui]` reported success and wrote nothing.
    if let Some(table) = ui.as_table_like_mut() {
        table.insert("sort", toml_edit::value(sort.word()));
    }
}
