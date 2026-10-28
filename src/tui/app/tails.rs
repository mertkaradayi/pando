//! The detail pane's log tails.

use std::collections::HashMap;

use crate::log_tail::LogTail;

use super::App;

/// How many worktrees' log tails are kept open at once. Each holds a file
/// handle and a ring buffer; the selected row is always one of them.
pub(super) const MAX_LOG_TAILS: usize = 8;

/// Lines of the dev log the detail pane keeps.
const TAIL_CAPACITY: usize = 256;

/// A bounded set of open log tails, evicted by least-recent use.
///
/// A tail holds a file handle and a ring buffer, and a project with fifty
/// worktrees would otherwise keep fifty of each. The selected row is touched
/// on every poll, so it never evicts itself.
#[derive(Default)]
pub struct LogTails {
    open: HashMap<String, (LogTail, u64)>,
    clock: u64,
}

impl LogTails {
    pub fn get(&self, name: &str) -> Option<&LogTail> {
        self.open.get(name).map(|(tail, _)| tail)
    }

    pub fn len(&self) -> usize {
        self.open.len()
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    pub fn forget(&mut self, name: &str) {
        self.open.remove(name);
    }

    /// Forgets every tail of a worktree, whatever process it belonged to.
    /// A start replaces the log files, so what was read of them is gone.
    pub fn forget_worktree(&mut self, name: &str) {
        let prefix = format!("{name}/");
        self.open.retain(|key, _| !key.starts_with(&prefix));
    }

    /// The tail for `name`, opening it if it is not already, and marking it
    /// as the most recently used.
    pub fn touch(&mut self, name: &str, path: std::path::PathBuf) -> &mut LogTail {
        self.clock += 1;
        if !self.open.contains_key(name)
            && self.open.len() >= MAX_LOG_TAILS
            && let Some(coldest) = self
                .open
                .iter()
                .min_by_key(|(_, (_, seen))| *seen)
                .map(|(name, _)| name.clone())
        {
            self.open.remove(&coldest);
        }
        let clock = self.clock;
        let entry = self
            .open
            .entry(name.to_string())
            .or_insert_with(|| (LogTail::new(path, TAIL_CAPACITY), clock));
        entry.1 = clock;
        &mut entry.0
    }
}

impl App {
    /// The process whose log the tail is showing, and the key its tail is
    /// kept under. `None` when the selected worktree is running nothing.
    pub fn tail_target(&self) -> Option<(String, String, std::path::PathBuf)> {
        let name = self.selected_worktree().map(|w| w.name.clone())?;
        let processes = self.processes_of(&name);
        if processes.is_empty() {
            return None;
        }
        let (process, record) = &processes[self.tail_index.min(processes.len() - 1)];
        Some((
            format!("{name}/{process}"),
            process.clone(),
            record.log_path.clone(),
        ))
    }

    /// Moves the tail to the next process of the selected worktree.
    pub(super) fn cycle_tail(&mut self) {
        let Some(name) = self.selected_worktree().map(|w| w.name.clone()) else {
            self.set_error("nothing selected");
            return;
        };
        let processes = self.processes_of(&name);
        if processes.len() < 2 {
            self.set_error(match processes.len() {
                0 => format!("{name} is running nothing to tail"),
                _ => format!("{name} runs one process: {}", processes[0].0),
            });
            return;
        }
        self.tail_index = (self.tail_index + 1) % processes.len();
        self.tail_scroll = 0;
        self.set_status(format!("log: {}", processes[self.tail_index].0));
    }

    /// Moves the tail to `process` of the selected worktree, as `tab` would
    /// get to it: a click on its name in the preview's header.
    pub(super) fn show_tail_of(&mut self, process: &str) {
        let Some(name) = self.selected_worktree().map(|w| w.name.clone()) else {
            return;
        };
        let Some(index) = self
            .processes_of(&name)
            .iter()
            .position(|(p, _)| p == process)
        else {
            return;
        };
        let shown = self.tail_target().map(|(_, shown, _)| shown);
        if shown.as_deref() != Some(process) {
            self.tail_index = index;
            self.tail_scroll = 0;
        }
        self.set_status(format!("log: {process}"));
    }

    pub(super) fn scroll_tail(&mut self, delta: isize) {
        let lines = self
            .tail_target()
            .and_then(|(key, _, _)| self.log_tails.get(&key).map(|t| t.lines().len()))
            .unwrap_or(0);
        let max = self.max_tail_scroll(lines);
        // Negative is "up", which in a tail means further back.
        let next = self.tail_scroll as isize - delta;
        self.tail_scroll = next.clamp(0, max as isize) as usize;
    }

    /// How far back a tail of `lines` lines scrolls: to its oldest page,
    /// which still fills the tail's rows.
    fn max_tail_scroll(&self, lines: usize) -> usize {
        lines.saturating_sub(self.tail_rows.max(1))
    }

    /// Reads whatever the log on screen has grown by — the viewer's when it
    /// is open, the detail pane's otherwise. Cheap: an offset-based read of
    /// one file, nothing forked.
    pub(super) fn poll_logs(&mut self) -> bool {
        if self.log_view().is_some() {
            return self.poll_viewer();
        }
        let Some((key, _, path)) = self.tail_target() else {
            return false;
        };
        let known = self.log_tails.get(&key).is_some();
        let tail = self.log_tails.touch(&key, path);
        let seen = tail.lines_seen();
        let grew = tail.poll().unwrap_or(false);
        let lines = tail.lines().len();
        // The scroll counts back from the newest line, so a tail scrolled
        // back moves by what arrived and stays on the lines being read. A
        // tail just opened — a theme change reads every log again — read
        // what was already there, and none of that arrived.
        let arrived = if known {
            (tail.lines_seen() - seen) as usize
        } else {
            0
        };
        if self.tail_scroll > 0 {
            let max = self.max_tail_scroll(lines);
            self.tail_scroll = self.tail_scroll.saturating_add(arrived).min(max);
        }
        grew
    }
}
