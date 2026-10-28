//! The worktree list: a table, which columns survive the width, and each
//! row.
//!
//! A row reads left to right as the question it answers: is it up, which
//! branch, where, and anything unusual about it. A header row names every
//! column, so a cell never has to be decoded from help:
//!
//! ```text
//!      branch                  │ PR     │ port   │ git
//! ─────────────────────────────┼────────┼────────┼──────────
//!  ▸ ● feat/checkout ▣         │ ◍ #482 │ :17342 │ ↓1  ↑2 ✎
//!    ✗ fix/crash        failed │ ✓ #463 │        │ ↓14
//! ```
//!
//! Faint lines divide the columns and rule the header off; the rows sit
//! one under the other, and the highlight is what marks the cursor's. The
//! columns after the label are as wide as their widest cells and sit
//! against the right edge; the label takes whatever room is left.
//!
//! A column is only there when some row has something in it, and its
//! title goes with it.
//!
//! The glyph says whether it runs, so the steady states have no word: a
//! word is kept for what needs reading — a failure, or an action in
//! flight — and goes at the right end of the label's cell, which is as
//! wide with it as without, so it moves nothing as it comes and goes. The full URL is the detail pane's; the row has its port. Being
//! adopted is not on the row: most worktrees are, and it changes nothing
//! about what a key does — the detail pane and the remove dialog say it
//! where it matters.
//!
//! The pull request sits right after the label — its number, and a mark
//! for whether it is open, a draft, merged or closed — because whether a
//! branch has landed is what somebody choosing among thirty worktrees asks
//! first. A mode other than the default is a mark after the branch, in the
//! mode's colour, so it costs a column nobody else needs.
//!
//! The git cell is three slots, each as wide as its widest in the list, so
//! every arrow sits under the one above it: behind in yellow, ahead in
//! green, then `✎` for uncommitted work. That column is the last to be
//! shed, because the `✎` is what stops a removal.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::actions;
use crate::state::{Aggregate, ServiceMode};
use crate::theme::{
    blue, border, cyan, green, highlight_bg, magenta, namespaced, orange, red, surface, text,
    text_dim, text_muted, yellow,
};
use crate::tui::app::{App, Mode};
use crate::worktree::{PrInfo, PrState, Worktree};

use super::{distinct_offsets, pad, text_width, truncate, truncate_distinct, truncate_line};

/// `"● "` — what the worktree is doing. Never shed: it is the question
/// this list exists to answer, and the one column that still answers it
/// when the words have gone.
const ROW_RUN_WIDTH: usize = 2;

/// `" ▸ "` — the list's own highlight column.
const ROW_CHROME_WIDTH: usize = 3;

/// Between two columns: a faint rule with a space either side, so each
/// value reads as under its title.
const COL_GAP: usize = 3;
const COL_RULE: &str = " │ ";

/// Below this a label stops being an identifier, so a column is dropped to
/// buy it back.
const ROW_NAME_MIN: usize = 12;

/// The optional columns of a row, in the order they are painted after the
/// label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Col {
    /// Its pull request: `◍ #482`, `✓ #463`.
    Pr,
    /// The directory name, when it is not just the branch with its
    /// slashes encoded — an adopted worktree somebody named themselves.
    Aside,
    /// The port of its URL, `:17342`, while something runs.
    Port,
    /// The other ports it was given, `api:17343`, for a worktree that
    /// runs more than one process. What a wide screen has room for, and
    /// the first thing a narrow one gives up.
    Ports,
    /// `◈` for a worktree with a public URL.
    Share,
    /// Behind, ahead and `✎` for uncommitted changes, each in a slot of
    /// its own; or prunable or locked. The last column shed.
    Signals,
}

impl Col {
    /// What the header row calls it.
    pub fn title(self) -> &'static str {
        match self {
            Col::Pr => "PR",
            Col::Aside => "dir",
            Col::Port => "port",
            Col::Ports => "other ports",
            Col::Share => "public",
            Col::Signals => "git",
        }
    }
}

/// The title over the glyph and the label.
const LABEL_TITLE: &str = "branch";

/// Left to right.
const LAYOUT: [Col; 6] = [
    Col::Pr,
    Col::Aside,
    Col::Port,
    Col::Ports,
    Col::Share,
    Col::Signals,
];

/// The order columns give way in when the label is too narrow to read:
/// the least useful first. The git column goes last, because it carries
/// the uncommitted mark; only a sliver of a pane, where the label alone
/// fits, loses it.
const SHED: [Col; 6] = [
    Col::Ports,
    Col::Aside,
    Col::Port,
    Col::Share,
    Col::Pr,
    Col::Signals,
];

/// Which optional columns survive at this pane width, given each one's
/// widest cell (`0` for a column no row has anything in). Decided once for
/// the whole list, so every row sheds the same column and each column
/// keeps one straight edge.
pub fn list_columns(list_width: usize, widths: &[(Col, usize)]) -> Vec<Col> {
    let width_of = |col: Col| {
        widths
            .iter()
            .find(|(c, _)| *c == col)
            .map(|(_, w)| *w)
            .unwrap_or(0)
    };
    let used = |shown: &[Col]| {
        ROW_CHROME_WIDTH
            + ROW_RUN_WIDTH
            + shown.iter().map(|&c| width_of(c) + COL_GAP).sum::<usize>()
    };
    let mut shown: Vec<Col> = LAYOUT.into_iter().filter(|&c| width_of(c) > 0).collect();
    for col in SHED {
        if list_width.saturating_sub(used(&shown)) >= ROW_NAME_MIN {
            break;
        }
        shown.retain(|&c| c != col);
    }
    shown
}

/// What one cell says: a run of text in one colour, or several.
type Cell = Vec<Span<'static>>;

fn cell_width(cell: &Cell) -> usize {
    cell.iter().map(|span| text_width(&span.content)).sum()
}

/// What git says about one row, before the list sizes the slots.
enum GitParts {
    /// Prunable or locked: a word, which says more than any drift.
    Entry(String, Color),
    Drift {
        behind: Option<String>,
        ahead: Option<String>,
        dirty: bool,
        /// `1 missing`: how many of a set's repositories it has no
        /// checkout of, or one whose directory is gone — a set whose row
        /// would otherwise read as whole.
        missing: Option<String>,
    },
}

/// One row's cells, before the list decides which columns it can afford.
struct RowCells {
    glyph: (&'static str, Color),
    label: String,
    /// The mark of a mode other than the default, after the label.
    mode: Option<(&'static str, Color)>,
    /// `failed`, or what is being done to it: `starting`, `stopping`…
    /// At the right end of the label's cell. None for a worktree that
    /// simply runs or is stopped.
    status: Option<(&'static str, Style)>,
    git: GitParts,
    /// Whether anything of it is up, or being done to it: its label is
    /// full brightness, and a stopped one's recedes, so the eye goes to
    /// the few rows that run in a list of thirty.
    live: bool,
    cells: Vec<(Col, Cell)>,
}

impl RowCells {
    fn cell(&self, col: Col) -> Option<&Cell> {
        self.cells
            .iter()
            .find(|(c, _)| *c == col)
            .map(|(_, cell)| cell)
    }
}

fn one(text: impl Into<String>, style: Style) -> Cell {
    vec![Span::styled(text.into(), style)]
}

fn row_cells(app: &App, wt: &Worktree) -> RowCells {
    let mut cells = Vec::new();
    let label = wt.branch.clone().unwrap_or_else(|| wt.name.clone());
    let main = app.is_main(&wt.name);
    let aside = match &wt.branch {
        // Said where a renamed worktree says its directory: it is what
        // this row is, and why `d` refuses it.
        _ if main => Some("main checkout".to_string()),
        Some(branch) if actions::sanitize_branch_to_dir(branch) != wt.name => Some(wt.name.clone()),
        Some(_) => None,
        None if wt.is_mixed() => Some("mixed branches".to_string()),
        None => Some("detached".to_string()),
    };
    if let Some(aside) = aside {
        cells.push((Col::Aside, one(aside, Style::new().fg(text_muted()))));
    }

    let phase = app.phase_of(&wt.name);
    let pending = app.pending_on(&wt.name);
    let (glyph, color) = row_marker(app, &wt.name);
    let word = match pending {
        Some(_) if app.awaiting_answer() => Some("waiting"),
        Some(pending) => Some(pending.kind.verb()),
        // Running and stopped are the glyph's to say; the rest have
        // something worth reading.
        None => match &phase {
            Some(Aggregate::Starting { .. }) => Some("starting"),
            Some(Aggregate::Failed { .. }) => Some("failed"),
            // A rebase or a merge stopped halfway in a shell: whatever
            // runs there runs on its conflict markers.
            _ => wt.in_progress.map(|op| op.word()),
        },
    };
    let halfway = pending.is_none()
        && !matches!(
            phase,
            Some(Aggregate::Starting { .. } | Aggregate::Failed { .. })
        )
        && wt.in_progress.is_some();
    let status = word.map(|word| {
        let color = if halfway { orange() } else { color };
        let mut style = Style::new().fg(color);
        if matches!(phase, Some(Aggregate::Failed { .. })) && pending.is_none() {
            style = style.add_modifier(Modifier::BOLD);
        }
        (word, style)
    });

    // Only while something runs: a stopped or failed worktree keeps its
    // ports, but one on its row would promise a page that is not there.
    let up = matches!(
        phase,
        Some(Aggregate::Running { .. } | Aggregate::Starting { .. })
    );
    let url_port = app.url_of(&wt.name).as_deref().and_then(port_of);
    // Nor while the process the URL points at is stopped and a sibling runs.
    let served = app.url_owner_not_running(&wt.name).is_none();
    if up
        && served
        && let Some(port) = &url_port
    {
        cells.push((Col::Port, one(format!(":{port}"), Style::new().fg(cyan()))));
    }
    if up
        && let Some(record) = app.record_for(&wt.name)
        && record.ports.len() > 1
    {
        let others = record
            .ports
            .iter()
            .filter(|(_, port)| url_port.as_deref() != Some(port.to_string().as_str()))
            .map(|(role, port)| format!("{role}:{port}"))
            .collect::<Vec<_>>()
            .join(" ");
        if !others.is_empty() {
            cells.push((Col::Ports, one(others, Style::new().fg(text_dim()))));
        }
    }
    if app.public_url_of(&wt.name).is_some() {
        cells.push((Col::Share, one("◈", Style::new().fg(green()))));
    }
    if let Some(pr) = app.pr_for(wt) {
        cells.push((Col::Pr, one(pr_chip(pr), Style::new().fg(pr_color(pr)))));
    }
    RowCells {
        glyph: (glyph, color),
        label,
        // The main checkout only ever runs shared, so its mark is where
        // a mode's would be: `⌂`, kept when the column that names it goes.
        mode: match main {
            true => Some((MAIN_MARK, text_muted())),
            false => app.record_for(&wt.name).and_then(|r| mode_mark(r.mode())),
        },
        status,
        git: git_parts(app, wt),
        live: phase.is_some() || pending.is_some(),
        cells,
    }
}

pub(super) fn render_list(f: &mut Frame, area: Rect, app: &mut App) {
    // Worktrees, counted without the main checkout's row: it is not one.
    // Then their order, which `,` changes.
    let count = if app.filter.is_empty() {
        app.linked_count().to_string()
    } else {
        let matched = app
            .filtered_indices
            .iter()
            .filter(|&&idx| !app.is_main(&app.worktrees[idx].name))
            .count();
        format!("{matched}/{}", app.linked_count())
    };
    let title = format!(" worktrees ({count}) · {} ", app.sort.title());
    let block = Block::bordered()
        .title(Span::styled(title, Style::new().fg(text_dim())))
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let (filter_area, list_area) = if app.mode == Mode::Filter || !app.filter.is_empty() {
        let [fa, la] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        (Some(fa), la)
    } else {
        (None, inner)
    };

    if let Some(fa) = filter_area {
        let cursor = if app.mode == Mode::Filter { "▏" } else { "" };
        let hint = if app.mode == Mode::Filter {
            "  ⏎ keep · esc clear"
        } else {
            "  / edits · esc clears"
        };
        f.render_widget(
            Paragraph::new(truncate_line(
                Line::from(vec![
                    Span::styled(
                        " / ",
                        Style::new().fg(orange()).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(app.filter.clone(), Style::new().fg(text())),
                    Span::styled(cursor, Style::new().fg(orange())),
                    Span::styled(hint, Style::new().fg(text_muted())),
                ]),
                fa.width as usize,
            )),
            fa,
        );
    }

    if app.filtered_indices.is_empty() {
        let message = if app.filter.is_empty() {
            "no worktrees — press n to create one".to_string()
        } else {
            format!("no matches for \"{}\" — esc to clear", app.filter)
        };
        let [_, middle, _] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(list_area);
        f.render_widget(
            Paragraph::new(Line::styled(
                truncate(&message, list_area.width as usize),
                Style::new().fg(text_muted()),
            ))
            .alignment(Alignment::Center),
            middle,
        );
        return;
    }

    let mut rows: Vec<RowCells> = app
        .filtered_indices
        .iter()
        .map(|&idx| row_cells(app, &app.worktrees[idx]))
        .collect();
    add_git_cells(&mut rows);
    // Column widths are a list-wide decision, so every row sacrifices the
    // same column and each column keeps one straight edge.
    let widths: Vec<(Col, usize)> = LAYOUT
        .into_iter()
        .map(|col| {
            let widest = rows
                .iter()
                .filter_map(|row| row.cell(col))
                .map(cell_width)
                .max()
                .unwrap_or(0);
            // A column with something in it is at least as wide as its
            // title; one with nothing is not there, title and all.
            let widest = if widest > 0 {
                widest.max(text_width(col.title()))
            } else {
                0
            };
            (col, widest)
        })
        .collect();
    // Text stops one cell short of the border, so the last column never
    // runs into it; rules and highlight bands still reach it.
    let full = list_area.width as usize;
    let width = full.saturating_sub(1);
    let mut shown = list_columns(width, &widths);
    // The columns are as wide as their widest cells and no wider, and sit
    // against the right edge: the room left over goes to the label, so a
    // wide screen has no stretch of nothing after the last column.
    let widest_label = rows
        .iter()
        .map(|row| text_width(&row.label) + mode_width(row) + status_width(row))
        .max()
        .unwrap_or(0);
    let label_room = |shown: &[Col]| {
        let columns_width: usize = shown
            .iter()
            .map(|col| widths.iter().find(|(c, _)| c == col).map_or(0, |(_, w)| *w) + COL_GAP)
            .sum();
        width.saturating_sub(ROW_CHROME_WIDTH + ROW_RUN_WIDTH + columns_width)
    };
    // A branch cut short is the one thing the list exists to show. The
    // columns the detail pane repeats in full give way, least useful
    // first, before any label is cut.
    for col in [Col::Ports, Col::Aside] {
        if label_room(&shown) >= widest_label {
            break;
        }
        shown.retain(|&c| c != col);
    }
    let label_width = label_room(&shown);
    let col_width = |col: &Col| widths.iter().find(|(c, _)| c == col).map_or(0, |(_, w)| *w);
    let grid = Style::new().fg(border());
    let lead = ROW_CHROME_WIDTH + ROW_RUN_WIDTH;
    // A rule across the whole pane, crossing each column line with `┼`.
    let rule = || {
        let mut out = "─".repeat(lead + label_width + 1);
        for col in &shown {
            out.push('┼');
            out.push_str(&"─".repeat(col_width(col) + 2));
        }
        let out = truncate(&out, full).trim_end_matches('…').to_string();
        let fill = full.saturating_sub(text_width(&out));
        Line::styled(format!("{out}{}", "─".repeat(fill)), grid)
    };
    // Spaces to the pane's edge, so a highlight or a header band is one
    // unbroken bar.
    let fill_to = |mut spans: Vec<Span<'static>>| {
        let line = truncate_line(Line::from(std::mem::take(&mut spans)), width);
        let used: usize = line.spans.iter().map(|s| text_width(&s.content)).sum();
        let mut spans = line.spans;
        spans.push(Span::raw(" ".repeat(full.saturating_sub(used))));
        Line::from(spans)
    };

    let height = list_area.height as usize;
    let mut lines: Vec<Line> = Vec::new();
    // The header when there is room for rows under it; the rule under
    // the header only when it would not push a row off a short pane.
    if height >= 3 {
        let title_style = Style::new().fg(text_muted()).add_modifier(Modifier::BOLD);
        let mut spans = vec![Span::styled(
            format!(
                "{}{}",
                " ".repeat(lead),
                pad(&truncate(LABEL_TITLE, label_width), label_width)
            ),
            title_style,
        )];
        for col in &shown {
            let w = col_width(col);
            spans.push(Span::styled(COL_RULE, grid));
            spans.push(Span::styled(pad(&truncate(col.title(), w), w), title_style));
        }
        lines.push(fill_to(spans).style(Style::new().bg(surface())));
        if height >= 3 + rows.len().min(3) {
            lines.push(rule());
        }
    }
    let visible = height.saturating_sub(lines.len()).max(1);
    let selected = app.list_state.selected();
    let mut offset = app.list_state.offset();
    if let Some(sel) = selected {
        if sel < offset {
            offset = sel;
        } else if sel >= offset + visible {
            offset = sel + 1 - visible;
        }
    }
    offset = offset.min(rows.len().saturating_sub(visible));
    *app.list_state.offset_mut() = offset;

    // Where each label differs from its nearest neighbour, so thirty
    // branches that share a long prefix keep the part that tells them
    // apart.
    let labels: Vec<String> = rows.iter().map(|row| row.label.clone()).collect();
    let distinct = distinct_offsets(&labels);

    // The header and its rule sit above the rows when the pane is tall
    // enough for them, so the rows start wherever they ended.
    let first_row = lines.len();
    for (i, (row, &differs_at)) in rows.iter().zip(&distinct).enumerate().skip(offset) {
        if lines.len() + 1 > height {
            break;
        }
        let is_selected = selected == Some(i);
        let (glyph, color) = row.glyph;
        let mut spans = vec![
            // The cursor in the accent, not only the band behind it: a
            // highlight alone is easy to lose on a low-contrast screen.
            Span::styled(
                if is_selected { " ▸ " } else { "   " },
                Style::new().fg(blue()),
            ),
            Span::styled(glyph, Style::new().fg(color)),
        ];
        // The mode's mark is kept whole, and so is the status word while
        // the label keeps enough to be told apart; the label gives way.
        let mark = mode_width(row).min(label_width);
        let status = status_width(row);
        let status = if label_width >= mark + status + ROW_NAME_MIN {
            status
        } else {
            0
        };
        let label = truncate_distinct(&row.label, label_width - mark - status, differs_at);
        let used = text_width(&label) + mark + status;
        spans.push(Span::styled(
            label,
            Style::new().fg(if row.live { text() } else { text_dim() }),
        ));
        if let Some((glyph, color)) = row.mode
            && mark > 0
        {
            spans.push(Span::styled(format!(" {glyph}"), Style::new().fg(color)));
        }
        spans.push(Span::raw(" ".repeat(label_width.saturating_sub(used))));
        if let Some((word, style)) = row.status
            && status > 0
        {
            spans.push(Span::styled(format!(" {word}"), style));
        }
        for col in &shown {
            let w = col_width(col);
            let cell = row.cell(*col).cloned().unwrap_or_default();
            let cell = truncate_line(Line::from(cell), w).spans;
            let used = cell_width(&cell);
            spans.push(Span::styled(COL_RULE, grid));
            spans.extend(cell);
            spans.push(Span::raw(" ".repeat(w.saturating_sub(used))));
        }
        let mut line = fill_to(spans);
        if is_selected {
            line = line.style(Style::new().bg(highlight_bg()).add_modifier(Modifier::BOLD));
        }
        lines.push(line);
    }
    // Each row by the worktree it shows, so a click lands on that one
    // even if the list has changed since this frame.
    let shown = lines.len() - first_row;
    let names = app.filtered_indices[offset..offset + shown]
        .iter()
        .map(|&idx| app.worktrees[idx].name.clone())
        .collect();
    let rows_area = Rect::new(
        list_area.x,
        list_area.y + first_row as u16,
        list_area.width,
        shown as u16,
    );
    app.hits.list = Some((rows_area, names));
    f.render_widget(Paragraph::new(lines), list_area);
}

/// The port of a URL, `17342` of `http://localhost:17342/app`, when it
/// names one.
pub(super) fn port_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let (_, port) = host.rsplit_once(':')?;
    (!port.is_empty() && port.chars().all(|c| c.is_ascii_digit())).then(|| port.to_string())
}

/// A worktree's row glyph, in its colour. What is being done to it
/// outranks what it was doing: a stop in flight is not `running`, and a
/// start waiting on a question is not `stopped`.
pub(super) fn row_marker(app: &App, name: &str) -> (&'static str, Color) {
    match app.pending_on(name) {
        Some(_) if app.awaiting_answer() => ("? ", orange()),
        Some(_) => ("◌ ", yellow()),
        None => run_marker(app.phase_of(name).as_ref()),
    }
}

/// What a worktree is doing, in one cell.
pub(in crate::tui) fn run_marker(phase: Option<&Aggregate>) -> (&'static str, Color) {
    match phase {
        Some(Aggregate::Running { .. }) => ("● ", green()),
        Some(Aggregate::Starting { .. }) => ("◌ ", yellow()),
        Some(Aggregate::Failed { .. }) => ("✗ ", red()),
        None => ("○ ", text_muted()),
    }
}

/// The main checkout's mark after its branch: home, the checkout every
/// worktree came from.
pub(super) const MAIN_MARK: &str = "⌂";

/// The mark for a mode that is not the default one, in its colour: `▣`,
/// a box of its own, for private copies of the services; `◧`, part of a
/// box, for a namespace in the project's own servers.
pub(super) fn mode_mark(mode: ServiceMode) -> Option<(&'static str, Color)> {
    match mode {
        ServiceMode::Isolated => Some(("▣", magenta())),
        ServiceMode::Namespaced => Some(("◧", namespaced())),
        ServiceMode::Shared => None,
    }
}

/// The cells a mode's mark takes after the label: a space and the mark.
fn mode_width(row: &RowCells) -> usize {
    row.mode.map_or(0, |(glyph, _)| 1 + text_width(glyph))
}

/// The cells the status word takes at the end of the label's: a space
/// and the word.
fn status_width(row: &RowCells) -> usize {
    row.status.map_or(0, |(word, _)| 1 + text_width(word))
}

/// What goes in a row's git slots: what is wrong with its entry, or `↓n`
/// behind — something to rebase onto — `↑n` ahead — work of its own, ready
/// to push — and whether it has uncommitted changes.
fn git_parts(app: &App, wt: &Worktree) -> GitParts {
    if wt.prunable || wt.locked {
        return GitParts::Entry(signal_text(wt), signal_color(wt));
    }
    let (ahead, behind) = wt.ahead_behind.unwrap_or((0, 0));
    let missing = app.missing_checkouts(wt).len() + wt.repos.iter().filter(|r| r.prunable).count();
    GitParts::Drift {
        behind: (behind > 0).then(|| format!("↓{}", cap(behind))),
        ahead: (ahead > 0).then(|| format!("↑{}", cap(ahead))),
        dirty: wt.dirty == Some(true),
        missing: (missing > 0).then(|| format!("{missing} missing")),
    }
}

/// Gives every row its git cell, with each slot as wide as the widest in
/// the list — behind, ahead, uncommitted — so the arrows and the `✎` line
/// up down the column. A slot no row fills takes no room.
fn add_git_cells(rows: &mut [RowCells]) {
    let widest = |slot: fn(&GitParts) -> Option<&str>| {
        rows.iter()
            .filter_map(|row| slot(&row.git))
            .map(text_width)
            .max()
            .unwrap_or(0)
    };
    let widths = [
        widest(|git| match git {
            GitParts::Drift { behind, .. } => behind.as_deref(),
            GitParts::Entry(..) => None,
        }),
        widest(|git| match git {
            GitParts::Drift { ahead, .. } => ahead.as_deref(),
            GitParts::Entry(..) => None,
        }),
        widest(|git| match git {
            GitParts::Drift { dirty: true, .. } => Some("✎"),
            _ => None,
        }),
        widest(|git| match git {
            GitParts::Drift { missing, .. } => missing.as_deref(),
            GitParts::Entry(..) => None,
        }),
    ];
    for row in rows.iter_mut() {
        let cell = match &row.git {
            GitParts::Entry(word, color) => one(word.clone(), Style::new().fg(*color)),
            GitParts::Drift {
                behind,
                ahead,
                dirty,
                missing,
            } => {
                if behind.is_none() && ahead.is_none() && !dirty && missing.is_none() {
                    continue;
                }
                let slots = [
                    (behind.as_deref(), yellow()),
                    (ahead.as_deref(), green()),
                    (dirty.then_some("✎"), yellow()),
                    (missing.as_deref(), red()),
                ];
                let mut cell = Vec::new();
                for ((text, color), width) in slots.into_iter().zip(widths) {
                    if width == 0 {
                        continue;
                    }
                    if !cell.is_empty() {
                        cell.push(Span::raw(" "));
                    }
                    let text = text.unwrap_or("");
                    cell.push(Span::styled(text.to_string(), Style::new().fg(color)));
                    cell.push(Span::raw(" ".repeat(width - text_width(text))));
                }
                cell
            }
        };
        row.cells.push((Col::Signals, cell));
    }
}

/// The git state of a row, in one short cell: what is wrong first, then
/// how far the branch has drifted.
pub(super) fn signal_text(wt: &Worktree) -> String {
    if wt.prunable {
        return "prunable".to_string();
    }
    if wt.locked {
        return "locked".to_string();
    }
    let mut out = String::new();
    if wt.dirty == Some(true) {
        out.push('*');
    }
    if let Some((ahead, behind)) = wt.ahead_behind {
        if ahead > 0 {
            out.push_str(&format!("↑{}", cap(ahead)));
        }
        if behind > 0 {
            out.push_str(&format!("↓{}", cap(behind)));
        }
    }
    out
}

/// Gone is red; locked and uncommitted both need a look, which is yellow.
pub(super) fn signal_color(wt: &Worktree) -> Color {
    if wt.prunable {
        red()
    } else if wt.locked || wt.dirty == Some(true) {
        yellow()
    } else {
        text_dim()
    }
}

fn cap(n: u32) -> String {
    if n > 99 {
        "99+".to_string()
    } else {
        n.to_string()
    }
}

/// `◍ #482`: the state as a mark — `◍` open, `◌` draft, `✓` merged, `✗`
/// closed — in its colour, and the number.
pub(super) fn pr_chip(pr: &PrInfo) -> String {
    let mark = match pr.state {
        PrState::Open if pr.draft => "◌",
        PrState::Open => "◍",
        PrState::Merged => "✓",
        PrState::Closed => "✗",
    };
    format!("{mark} #{}", pr.number)
}

/// GitHub's own colours for a pull request's state, so the chip reads the
/// way the pull request page does.
pub(super) fn pr_color(pr: &PrInfo) -> Color {
    match pr.state {
        PrState::Open if pr.draft => text_muted(),
        PrState::Open => green(),
        PrState::Merged => magenta(),
        PrState::Closed => red(),
    }
}
