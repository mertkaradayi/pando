//! The log viewer's keys, and opening, switching and polling its source.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::Line;

use crate::actions;
use crate::log_tail::{LogLevel, LogTail, colorize_json};

use super::dialogs::Modal;
use super::log_view::{
    LineInspect, LogView, SearchMode, SearchState, block_bounds, error_ranks, filtered_rank,
    first_url, inspect_content, joined_block, recompute_matches, step_match,
};
use super::merged::{ALL_SOURCE, MergedTail, ViewTail};
use super::{App, View};

impl App {
    // ---- the log viewer --------------------------------------------------

    /// Every log source a worktree has, in tab order: its processes first,
    /// then its hooks, then whatever else is in the directory.
    ///
    /// The set comes from the files that are there, not from an enum —
    /// pando cannot know in advance what a project calls its processes. The
    /// order comes from config, so the tabs read the way `pando status`
    /// lists them rather than the way the filesystem happens to.
    pub fn log_sources(&self, name: &str) -> Vec<String> {
        let mut present: Vec<String> = match std::fs::read_dir(self.paths.logs_dir(name)) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| {
                    let file = entry.file_name().to_string_lossy().into_owned();
                    file.strip_suffix(".log").map(str::to_string)
                })
                .filter(|source| !source.is_empty() && !source.starts_with('.'))
                .collect(),
            Err(_) => Vec::new(),
        };
        present.sort();
        present.dedup();
        let mut ordered = Vec::new();
        for known in self.known_sources() {
            if let Some(at) = present.iter().position(|p| *p == known) {
                ordered.push(present.remove(at));
            }
        }
        // Anything else the directory holds — `tunnel`, `proxy`, a process
        // that has since been renamed — alphabetically, after the rest.
        ordered.extend(present);
        ordered
    }

    /// The viewer's tabs: [`App::log_sources`], led by `all` when more
    /// than one process has a log to merge.
    pub fn viewer_sources(&self, name: &str) -> Vec<String> {
        let mut sources = self.log_sources(name);
        if self.process_sources(name, &sources).len() > 1
            && !sources.iter().any(|s| s == ALL_SOURCE)
        {
            sources.insert(0, ALL_SOURCE.to_string());
        }
        sources
    }

    /// Which of `sources` are processes — configured, or running under a
    /// record — rather than hooks or a tunnel: what `all` merges.
    fn process_sources(&self, name: &str, sources: &[String]) -> Vec<String> {
        let running = self.record_for(name).map(|r| &r.processes);
        sources
            .iter()
            .filter(|s| s.as_str() != ALL_SOURCE)
            .filter(|s| {
                self.config.processes.contains_key(s.as_str())
                    || running.is_some_and(|p| p.contains_key(s.as_str()))
            })
            .cloned()
            .collect()
    }

    /// The sources config accounts for, in the order they belong in:
    /// processes, then hooks (pando's own install hook first).
    fn known_sources(&self) -> Vec<String> {
        let mut known: Vec<String> = self.config.processes.keys().cloned().collect();
        known.push(actions::INSTALL_HOOK.to_string());
        known.extend(self.config.hooks.iter().map(|hook| hook.name.clone()));
        // Last, and in this order: a share is the newest thing here, and
        // when it is misbehaving the tunnel's own log is the first place to
        // look. Without naming them they would sort in among the rest.
        known.push(crate::tunnel::TUNNEL_LOG.to_string());
        known.push(crate::share_proxy::PROXY_LOG.to_string());
        known
    }

    pub fn log_view(&self) -> Option<&LogView> {
        match &self.view {
            View::Log(view) => Some(view),
            View::List => None,
        }
    }

    pub fn log_view_mut(&mut self) -> Option<&mut LogView> {
        match &mut self.view {
            View::Log(view) => Some(view),
            View::List => None,
        }
    }

    /// Opens the viewer on the selected worktree. It starts on the source
    /// the detail pane's tail was already showing, when that one has a log
    /// — pressing `l` while reading the api's tail should not land on the
    /// web server's.
    pub fn open_log_viewer(&mut self) {
        let Some(name) = self.selected_name() else {
            return;
        };
        let available = self.viewer_sources(&name);
        let tailed = self.tail_target().map(|(_, process, _)| process);
        let source = tailed
            .clone()
            .filter(|process| available.contains(process))
            .or_else(|| {
                // Not `all` by default: it is the first tab, one key away,
                // and a single process's log is what a reader coming from
                // one row expects.
                available.iter().find(|s| s.as_str() != ALL_SOURCE).cloned()
            })
            .or(tailed)
            .unwrap_or_else(|| crate::detect::DEV.to_string());
        self.open_log_source(name, source, available);
    }

    fn open_log_source(&mut self, name: String, source: String, available: Vec<String>) {
        let tail = if source == ALL_SOURCE {
            let merged = self
                .process_sources(&name, &available)
                .into_iter()
                .map(|process| {
                    let path = self.paths.log_file(&name, &process);
                    (process, path)
                })
                .collect();
            ViewTail::All(MergedTail::new(merged, super::LOG_VIEWER_CAPACITY))
        } else {
            let path = self.paths.log_file(&name, &source);
            ViewTail::One(LogTail::new(path, super::LOG_VIEWER_CAPACITY))
        };
        self.inspect = None;
        self.view = View::Log(Box::new(LogView::new(name, source, available, tail)));
    }

    pub fn close_log_viewer(&mut self) {
        self.inspect = None;
        self.view = View::List;
    }

    /// Next (or previous) tab. The list is the one the last paint read off
    /// disk, so no key handler goes to the filesystem.
    /// The tab a digit names, counted from one as the tabs are labelled.
    /// A digit past the last tab does nothing. A click on a tab names it
    /// the same way, past nine too.
    pub(super) fn jump_to_log_source(&mut self, number: usize) {
        let Some(view) = self.log_view() else { return };
        let Some(source) = number
            .checked_sub(1)
            .and_then(|at| view.available.get(at))
            .cloned()
        else {
            return;
        };
        if source == view.source {
            return;
        }
        let name = view.name.clone();
        let available = view.available.clone();
        self.open_log_source(name, source, available);
    }

    fn switch_log_source(&mut self, delta: isize) {
        let Some(view) = self.log_view() else { return };
        if view.available.len() < 2 {
            return;
        }
        let at = view
            .available
            .iter()
            .position(|source| *source == view.source)
            .unwrap_or(0);
        let next = (at as isize + delta).rem_euclid(view.available.len() as isize) as usize;
        let source = view.available[next].clone();
        let name = view.name.clone();
        let available = view.available.clone();
        self.open_log_source(name, source, available);
    }

    /// Every key the viewer answers to. Pure state: nothing here forks, and
    /// nothing here reads the filesystem — the tab list was read by the
    /// last paint and the tail is polled on the tick.
    pub(super) fn handle_log_key(&mut self, key: KeyEvent) {
        let page = self.viewer_height.max(1) / 2;
        let viewer_height = self.viewer_height;

        // The overlay sits on top of the viewer, so it answers first.
        if self.inspect.is_some() {
            self.handle_inspect_key(key, page);
            return;
        }

        // While a query is being typed every key belongs to it, esc
        // included — so this comes before anything else.
        if self
            .log_view()
            .is_some_and(|view| view.search_mode == SearchMode::Typing)
        {
            self.handle_search_typing_key(key, viewer_height);
            return;
        }

        // A digit is a tab: the tabs are numbered, and `2` is what a
        // reader presses to see the second one.
        if let KeyCode::Char(c @ '1'..='9') = key.code {
            self.jump_to_log_source(c as usize - '0' as usize);
            return;
        }

        // Keys that act on the app rather than on the viewport.
        match key.code {
            KeyCode::Char('?') => {
                self.help_scroll = 0;
                self.modal = Some(Modal::Help);
                return;
            }
            // Esc unwinds one layer at a time: a live search first — which
            // is what the search bar's own hint promises — and only then
            // the viewer. `q` always leaves.
            KeyCode::Esc
                if self
                    .log_view()
                    .is_some_and(|view| view.search_mode == SearchMode::Active) =>
            {
                if let Some(view) = self.log_view_mut() {
                    view.search_mode = SearchMode::Inactive;
                    view.search = SearchState::default();
                    view.filter_to_matches = false;
                }
                return;
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.close_log_viewer();
                return;
            }
            KeyCode::Tab => {
                self.switch_log_source(1);
                return;
            }
            KeyCode::BackTab => {
                self.switch_log_source(-1);
                return;
            }
            KeyCode::Enter | KeyCode::Char('J') => {
                self.open_inspect();
                return;
            }
            KeyCode::Char('y') => {
                if let Some(line) = self.current_log_line() {
                    self.copy_to_clipboard(&line);
                    // The viewer paints full screen, so the header that
                    // normally carries a confirmation is not there: the
                    // footer says it instead, and expires like any status.
                    self.set_success("copied line");
                }
                return;
            }
            KeyCode::Char('Y') => {
                match self.current_log_line().as_deref().and_then(first_url) {
                    Some(url) => {
                        self.copy_to_clipboard(&url);
                        self.set_success(format!("copied {url}"));
                    }
                    None => self.set_status("no URL on this line"),
                }
                return;
            }
            _ => {}
        }

        let Some(view) = self.log_view_mut() else {
            return;
        };
        // The visible list may have shrunk under the cursor since the last
        // paint — a filter change, a truncation, an eviction — so clamp
        // before moving rather than after.
        let visible_len = view.visible_len();
        let last_rank = visible_len.saturating_sub(1);
        let max_scroll = visible_len.saturating_sub(viewer_height.max(1));
        view.scroll = view.scroll.min(max_scroll);
        if !view.follow {
            view.cursor = view.cursor.min(last_rank);
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if !view.follow {
                    view.cursor = view.cursor.saturating_add(1).min(last_rank);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if view.follow {
                    // Breaking follow puts the cursor where it visibly was:
                    // on the last line, moving up from there.
                    view.follow = false;
                    view.cursor = last_rank.saturating_sub(1);
                } else {
                    view.cursor = view.cursor.saturating_sub(1);
                }
            }
            KeyCode::Char('d') if ctrl => {
                if !view.follow {
                    view.cursor = view.cursor.saturating_add(page).min(last_rank);
                }
            }
            KeyCode::Char('u') if ctrl => {
                if view.follow {
                    view.follow = false;
                    view.cursor = last_rank.saturating_sub(page);
                } else {
                    view.cursor = view.cursor.saturating_sub(page);
                }
            }
            KeyCode::Char('g') | KeyCode::Home => {
                view.cursor = 0;
                view.scroll = 0;
                view.follow = false;
            }
            // `End` too, because the list binds it to "last".
            KeyCode::Char('G') | KeyCode::End => {
                view.follow = true;
                view.new_below = 0;
            }
            // Jump to the next (e) or previous (E) error — lowercase
            // forward, as `n`/`N` are — relative to the cursor, so
            // repeated presses walk the list rather than re-finding the
            // same line.
            KeyCode::Char('e') | KeyCode::Char('E') => {
                let forward = matches!(key.code, KeyCode::Char('e'));
                let targets = error_ranks(view);
                if !targets.is_empty() {
                    let from = if view.follow { last_rank } else { view.cursor };
                    let target = if forward {
                        targets
                            .iter()
                            .copied()
                            .find(|rank| *rank > from)
                            .or_else(|| targets.first().copied())
                    } else {
                        targets
                            .iter()
                            .copied()
                            .rev()
                            .find(|rank| *rank < from)
                            .or_else(|| targets.last().copied())
                    };
                    if let Some(rank) = target {
                        view.cursor = rank;
                        view.scroll = rank.saturating_sub(viewer_height / 2).min(max_scroll);
                        view.follow = false;
                    }
                }
            }
            KeyCode::Char('w') => view.wrap = !view.wrap,
            KeyCode::Char('/') => {
                view.search_mode = SearchMode::Typing;
                view.search = SearchState::default();
                view.filter_to_matches = false;
            }
            // Any modifier: ctrl-n is the same "next match" as n, which is
            // what a reader's fingers reach for either way.
            KeyCode::Char('n') if view.search_mode == SearchMode::Active => {
                step_match(view, true, viewer_height);
            }
            KeyCode::Char('N') if view.search_mode == SearchMode::Active => {
                step_match(view, false, viewer_height);
            }
            KeyCode::Char('p') if ctrl && view.search_mode == SearchMode::Active => {
                step_match(view, false, viewer_height);
            }
            KeyCode::Char('f') => {
                view.log_filter = view.log_filter.cycle();
                view.scroll = 0;
                view.follow = true;
                view.new_below = 0;
                // The new filter may hide lines that matched under the old
                // one; drop them, or the search cursor lands on a row that
                // is no longer painted.
                if view.search_mode == SearchMode::Active {
                    recompute_matches(view);
                }
            }
            // Collapse to the matching lines, grep-style, or expand back.
            // Either way, land at the top.
            KeyCode::Char('&')
                if view.search_mode == SearchMode::Active && !view.search.query.is_empty() =>
            {
                view.filter_to_matches = !view.filter_to_matches;
                view.scroll = 0;
                view.cursor = 0;
                view.follow = false;
                view.search.cursor = 0;
            }
            _ => {}
        }
    }

    fn handle_inspect_key(&mut self, key: KeyEvent, page: usize) {
        let Some(inspect) = &mut self.inspect else {
            return;
        };
        let mut close = false;
        let mut yank = None;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('J') => close = true,
            KeyCode::Char('j') | KeyCode::Down => {
                inspect.scroll = inspect.scroll.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                inspect.scroll = inspect.scroll.saturating_sub(1);
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                inspect.scroll = inspect.scroll.saturating_add(page);
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                inspect.scroll = inspect.scroll.saturating_sub(page);
            }
            KeyCode::Char('g') => inspect.scroll = 0,
            // The paint clamps it, so this lands on the real last page.
            KeyCode::Char('G') => inspect.scroll = usize::MAX,
            // The whole block, pretty-printed — which is the form worth
            // pasting into an issue.
            KeyCode::Char('y') => yank = Some(inspect.text.clone()),
            _ => {}
        }
        if close {
            self.inspect = None;
        }
        if let Some(text) = yank {
            let count = text.lines().count();
            self.copy_to_clipboard(&text);
            let unit = if count == 1 { "line" } else { "lines" };
            self.set_success(format!("copied {count} {unit}"));
        }
    }

    /// Opens the pretty-print overlay on the line the cursor is on. A line
    /// that belongs to a multi-line JSON block expands to the whole block,
    /// rejoined and re-prettified: one log entry is one thing to read.
    fn open_inspect(&mut self) {
        let Some(at) = self.current_log_index() else {
            return;
        };
        let Some(view) = self.log_view() else { return };
        let buffer = view.tail.lines();
        let Some(parsed) = buffer.get(at) else {
            return;
        };
        let (text, lines) = match parsed.block_id {
            Some(id) => {
                let (start, end) = block_bounds(buffer, at, id);
                let joined = joined_block(buffer, start, end, |plain| view.tail.unprefixed(plain));
                match serde_json::from_str::<serde_json::Value>(&joined)
                    .ok()
                    .and_then(|value| serde_json::to_string_pretty(&value).ok())
                {
                    Some(pretty) => {
                        let styled = pretty
                            .lines()
                            .map(|line| Line::from(colorize_json(line)))
                            .collect();
                        (pretty, styled)
                    }
                    // A partial block — its head evicted, or raw output
                    // interleaved into it — is shown verbatim rather than
                    // not at all.
                    None => (
                        joined,
                        buffer
                            .range(start..=end)
                            .map(|p| p.styled.clone())
                            .collect(),
                    ),
                }
            }
            None => inspect_content(parsed),
        };
        self.inspect = Some(LineInspect {
            lines,
            text,
            scroll: 0,
        });
    }

    /// The plain text of the line the viewer calls "current": the one under
    /// the cursor, or the last visible line while following. Exactly one
    /// line, even when it is part of a block — `J` is for the block.
    fn current_log_line(&self) -> Option<String> {
        let at = self.current_log_index()?;
        let view = self.log_view()?;
        // Without the `all` tab's `api │ `: what is copied is the line.
        view.tail
            .lines()
            .get(at)
            .map(|parsed| view.tail.unprefixed(&parsed.plain).to_string())
    }

    /// The buffer index of the line the viewer calls "current": the one
    /// under the cursor, or the last visible line while following.
    fn current_log_index(&self) -> Option<usize> {
        let view = self.log_view()?;
        let visible = view.visible();
        if visible.is_empty() {
            return None;
        }
        let at = if view.follow {
            visible.len() - 1
        } else {
            view.cursor.min(visible.len() - 1)
        };
        Some(visible[at])
    }

    fn handle_search_typing_key(&mut self, key: KeyEvent, viewer_height: usize) {
        let Some(view) = self.log_view_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                view.search_mode = SearchMode::Inactive;
                view.search = SearchState::default();
                view.filter_to_matches = false;
            }
            KeyCode::Enter => {
                view.search_mode = SearchMode::Active;
                if let Some(&first) = view.search.matches.first() {
                    let rank = filtered_rank(view, first);
                    view.cursor = rank;
                    view.scroll = rank.saturating_sub(viewer_height / 2);
                    view.follow = false;
                    view.search.cursor = 0;
                }
            }
            KeyCode::Backspace => {
                view.search.query.pop();
                recompute_matches(view);
            }
            KeyCode::Char(c) => {
                view.search.query.push(c);
                recompute_matches(view);
            }
            _ => {}
        }
    }

    /// Polls the open viewer's tail and keeps the viewport honest about
    /// what the ring buffer did: eviction shifts every absolute index down,
    /// and lines arriving below a scrolled-back viewport are what the
    /// `↓ N new` badge counts.
    pub(super) fn poll_viewer(&mut self) -> bool {
        // The `all` tab merges every process the worktree has *now*: one
        // started since the tab opened would otherwise get a tab of its
        // own and never appear in the merge. The tab list is the one the
        // last paint read, so this touches no directory.
        let joining: Vec<(String, std::path::PathBuf)> = match self.log_view() {
            Some(view) if view.source == ALL_SOURCE => self
                .process_sources(&view.name, &view.available)
                .into_iter()
                .map(|process| {
                    let path = self.paths.log_file(&view.name, &process);
                    (process, path)
                })
                .collect(),
            _ => Vec::new(),
        };
        let Some(view) = self.log_view_mut() else {
            return false;
        };
        if let Some(merged) = view.tail.merged_mut() {
            for (process, path) in joining {
                merged.add_source(process, path);
            }
        }
        // One `exists` per tick, in both directions. The viewer is most
        // often opened on a source whose file does not exist yet — before
        // `start`, or while it is still installing — and this is what
        // turns that into a live tail the moment the process writes its
        // first line, instead of a pane that says `no log file for this
        // source yet` until it is reopened.
        let exists = view.tail.exists();
        let mut changed = false;
        if view.missing {
            if !exists {
                return false;
            }
            view.missing = false;
            view.follow = true;
            changed = true;
        }
        // The other direction: the paint decides `gone` from the log
        // directory, and when a plain stat disagrees the frame on screen
        // is out of date. Nothing else would repaint it — a tail whose
        // file has been deleted never grows again.
        if view.gone == exists {
            changed = true;
        }
        let before = view.tail.lines().len();
        let (grew, restarted) = view.tail.poll_for_restart();
        if restarted {
            // A new run: whatever the viewport, the cursor and the search
            // pointed at belonged to the old one. Back to the live tail.
            view.follow = true;
            view.scroll = usize::MAX;
            view.cursor = usize::MAX;
            view.new_below = 0;
            if view.search_mode == SearchMode::Active {
                recompute_matches(view);
            }
            self.set_status("the log started over — this is the new run");
            return true;
        }
        let evicted: Vec<LogLevel> = view.tail.evicted_levels().to_vec();
        if !evicted.is_empty() {
            let count = evicted.len();
            // Collapsed to the matches, the visible list *is* the match
            // list, so what it lost is the matches that were evicted — not
            // every evicted line the level filter passes.
            let matches_evicted = view.search.matches.iter().filter(|&&at| at < count).count();
            // Absolute buffer indices all shift down by `count`.
            view.search.matches.retain_mut(|at| {
                if *at < count {
                    false
                } else {
                    *at -= count;
                    true
                }
            });
            if view.search.cursor >= view.search.matches.len() {
                view.search.cursor = view.search.matches.len().saturating_sub(1);
            }
            // `scroll` and `cursor` are positions in the *filtered* list,
            // so only the evicted lines the filter was showing move them.
            // While following, the paint re-pins them anyway.
            if !view.follow {
                let visible_evicted = if view.collapsed() {
                    matches_evicted
                } else {
                    evicted
                        .iter()
                        .filter(|level| view.log_filter.passes(**level))
                        .count()
                };
                view.scroll = view.scroll.saturating_sub(visible_evicted);
                view.cursor = view.cursor.saturating_sub(visible_evicted);
            }
        }
        // A line that just arrived may match, so the live count has to stay
        // honest — but recomputing resets the ordinal to the first match,
        // which would yank the reader back to match 1 on every tick. Put
        // the cursor back on the *line* it was on.
        if grew && view.search_mode == SearchMode::Active && !view.search.query.is_empty() {
            let was = view.search.matches.get(view.search.cursor).copied();
            recompute_matches(view);
            if let Some(line) = was {
                view.search.cursor = view
                    .search
                    .matches
                    .iter()
                    .position(|&at| at == line)
                    .unwrap_or_else(|| {
                        view.search
                            .cursor
                            .min(view.search.matches.len().saturating_sub(1))
                    });
            }
        }
        if view.follow {
            view.scroll = usize::MAX;
        } else if grew {
            let added = view.tail.lines().len() + evicted.len() - before;
            // What the viewer will show of them: collapsed, only the ones
            // that match.
            let query = view.search.query.to_lowercase();
            let collapsed = view.collapsed();
            view.new_below += view
                .tail
                .lines()
                .iter()
                .rev()
                .take(added)
                .filter(|parsed| {
                    view.log_filter.passes(parsed.level)
                        && (!collapsed || parsed.plain_lower.contains(&query))
                })
                .count();
        }
        grew || changed
    }
}
