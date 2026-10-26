//! The full-screen log viewer: tabs, gutters, wrapping, search highlight,
//! the footer, and the inspect overlay.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::log_tail::{LogLevel, ParsedLine};
use crate::theme::{
    blue, border, green, highlight_bg, orange, red, search_cursor_bg, search_match_bg, surface,
    text, text_dim, text_muted, yellow,
};
use crate::tui::app::{App, LogFilter, LogView, SearchMode, Status};

use super::chrome::{hint_line, status_mark};
use super::{centered_rect, text_width, truncate, truncate_line};

/// Key hints for the viewer's footer. The counters and the position badge
/// claim their width first; these collapse into whatever is left.
pub(super) const LOG_HINTS: [(&str, &str, bool); 8] = [
    ("j/k", "move", true),
    ("g/G", "top/live", false),
    ("/", "search", true),
    ("f", "filter", false),
    ("w", "wrap", false),
    ("1-9", "source", false),
    ("?", "help", false),
    ("q", "back", true),
];

/// What the title says after `branch · source`: the state of the *source*
/// on screen — a process's own phase, not the worktree's — and, when the
/// worktree has failed because of a different process, which one.
pub(super) fn viewer_title_suffix(
    app: &App,
    name: &str,
    source: &str,
    missing: bool,
    gone: bool,
) -> String {
    if missing {
        return " (no log yet)".to_string();
    }
    if gone {
        return " (log deleted)".to_string();
    }
    let aggregate = app.phase_of(name);
    let failed_elsewhere = match &aggregate {
        Some(crate::state::Aggregate::Failed { process, .. }) if process != source => {
            format!(" · {process} failed")
        }
        _ => String::new(),
    };
    let own = app
        .record_for(name)
        .and_then(|record| record.processes.get(source));
    match own {
        Some(process) => {
            let word = match process.phase {
                crate::state::Phase::Running { .. } => "running",
                crate::state::Phase::Starting { .. } => "starting",
                crate::state::Phase::Failed { .. } => "failed",
            };
            format!(" ({word}){failed_elsewhere}")
        }
        // Not a process: the merged tab, a hook, the tunnel. Its state is
        // the worktree's.
        None => match aggregate {
            None => " (not running)".to_string(),
            Some(crate::state::Aggregate::Starting { .. }) => " (starting)".to_string(),
            Some(crate::state::Aggregate::Running { .. }) => String::new(),
            Some(crate::state::Aggregate::Failed { .. }) => failed_elsewhere,
        },
    }
}

/// One worktree's log, full screen: a tab per source, the lines the filter
/// shows, and a footer that says where the cursor is.
pub(super) fn render_log_viewer(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(view) = app.log_view() else { return };
    // Only what is about this worktree, or about none: an error another
    // worktree's start raised does not follow the reader in here.
    let status = app.flash_for(&view.name).cloned();

    // The tab list is read here, every frame, so a hook that has just run
    // gets a tab and one that never ran never does. It is a `read_dir` of
    // pando's own log directory: no git, no network, nothing forked.
    let mut sources = app.viewer_sources(&view.name);
    // The file behind the current tab can go away while the viewer is on
    // it — a `rm`, a `start` that has not written yet, a stray `rm -rf`.
    // The source stays in the list either way, because that list is what
    // `tab` walks: drop it and, with one source left, `tab` is a no-op and
    // nothing can leave the dead tab but closing the viewer.
    let gone = !view.missing && !sources.contains(&view.source);
    if !sources.contains(&view.source) {
        sources.push(view.source.clone());
    }

    let suffix = viewer_title_suffix(app, &view.name, &view.source, view.missing, gone);
    let title = format!(" {} · {}{suffix} ", app.label_of(&view.name), view.source);
    let block = Block::bordered()
        .title(Span::styled(
            truncate(&title, area.width.saturating_sub(2) as usize),
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ))
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        app.viewer_height = 0;
        return;
    }

    let has_tabs = sources.len() > 1;
    let (tab_area, content) = if has_tabs {
        let [tabs, rest] =
            Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(inner);
        (Some(tabs), rest)
    } else {
        (None, inner)
    };
    if let Some(tabs) = tab_area {
        // In a multi-repo project, a process's tab names the repository
        // it runs in, while the strip has room for it.
        let repos: Vec<Option<&str>> = sources.iter().map(|s| app.process_repo(s)).collect();
        let strip = source_tabs(&sources, &repos, &view.source, tabs.width as usize);
        f.render_widget(Paragraph::new(strip), tabs);
    }

    let [body_area, footer_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(content);
    let body_height = body_area.height as usize;
    let width = inner.width as usize;

    let view = app.log_view().expect("still the viewer");
    let visible = view.visible();
    let visible_count = visible.len();
    let (errors, warnings) = view
        .tail
        .lines()
        .iter()
        .fold((0usize, 0usize), |(e, w), parsed| match parsed.level {
            LogLevel::Error => (e + 1, w),
            LogLevel::Warn => (e, w + 1),
            _ => (e, w),
        });
    let last_rank = visible_count.saturating_sub(1);
    let cursor = if view.follow {
        last_rank
    } else {
        view.cursor.min(last_rank)
    };
    let scroll = view
        .scroll
        .min(visible_count.saturating_sub(body_height.max(1)));

    let (rendered, scroll_out) = viewer_rows(view, &visible, cursor, scroll, body_height, width);
    f.render_widget(Paragraph::new(rendered), body_area);

    if !view.missing && visible_count > body_height && body_height > 0 {
        let mut state = ScrollbarState::new(visible_count)
            .viewport_content_length(body_height)
            .position(scroll_out);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_style(Style::new().fg(border()).add_modifier(Modifier::BOLD)),
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }

    let footer = viewer_footer(
        view,
        ViewerCounts {
            visible: visible_count,
            total: view.tail.lines().len(),
            errors,
            warnings,
        },
        cursor,
        status,
        has_tabs,
        footer_area.width as usize,
    );
    f.render_widget(Paragraph::new(footer), footer_area);

    // What the paint decided, so the next key press moves by what is on
    // screen and `G`'s usize::MAX lands on a real line.
    app.viewer_height = body_height;
    if let Some(view) = app.log_view_mut() {
        view.available = sources;
        // The tick compares a stat against this to know whether the frame
        // still tells the truth about the file.
        view.gone = gone;
        view.scroll = scroll_out;
        view.cursor = cursor;
        if !view.follow && visible_count > 0 && cursor == last_rank {
            view.new_below = 0;
        }
    }
}

/// Counters the footer shows about the whole buffer, not the viewport.
struct ViewerCounts {
    visible: usize,
    total: usize,
    errors: usize,
    warnings: usize,
}

/// The rows to paint and the viewport top they imply.
///
/// Two passes, because a wrapped line is several rows: walk forward from
/// the scroll position while the cursor still fits, and fall back to
/// anchoring the bottom of the window on the cursor (or on the tail, while
/// following) when it does not.
fn viewer_rows(
    view: &LogView,
    visible: &[usize],
    cursor: usize,
    scroll: usize,
    body_height: usize,
    width: usize,
) -> (Vec<Line<'static>>, usize) {
    if view.missing {
        return (
            vec![Line::styled(
                truncate("  no log file for this source yet", width),
                Style::new().fg(text_muted()),
            )],
            scroll,
        );
    }
    let lines = view.tail.lines();
    let body_height = body_height.max(1);
    let last_rank = visible.len().saturating_sub(1);
    let query = view.search.query.clone();
    let matched: std::collections::HashSet<usize> = view.search.matches.iter().copied().collect();
    let search_cursor = view.search.matches.get(view.search.cursor).copied();
    let build = |rank: usize, at: usize| -> Vec<Line<'static>> {
        let Some(parsed) = lines.get(at) else {
            return Vec::new();
        };
        let styled = if !query.is_empty() && matched.contains(&at) {
            highlight_search_in_line(
                parsed.styled.clone(),
                &parsed.plain,
                &query,
                Some(at) == search_cursor,
            )
        } else {
            parsed.styled.clone()
        };
        let at_cursor = rank == cursor;
        // The cursor line always renders in full: with wrap off it is the
        // one line that expands, so the line being read stays readable.
        let rows = gutter_rows(
            styled,
            parsed.level,
            width,
            view.wrap || at_cursor,
            block_glyph(lines, at),
        );
        if at_cursor {
            cursor_highlight_rows(rows, width)
        } else {
            rows
        }
    };

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut scroll_out = scroll;
    let mut cursor_fits = false;
    if !view.follow {
        let start = scroll.min(cursor);
        scroll_out = start;
        for (rank, at) in visible.iter().copied().enumerate().skip(start) {
            let built = build(rank, at);
            if rank == cursor && rows.len() + built.len() <= body_height {
                cursor_fits = true;
            }
            rows.extend(built);
            if rows.len() >= body_height {
                break;
            }
        }
    }

    // A cursor line taller than the body shows from its start, where the
    // stamp and the level are: j/k move by whole lines, so a head cut off
    // could never be scrolled into view.
    let tall_cursor = (!view.follow && !cursor_fits)
        .then(|| visible.get(cursor).map(|&at| build(cursor, at)))
        .flatten()
        .filter(|own| own.len() > body_height);
    if let Some(own) = tall_cursor {
        rows = own;
        rows.truncate(body_height);
        scroll_out = cursor;
    } else if view.follow || !cursor_fits || rows.len() < body_height {
        // Anchor the bottom of the window: on the tail while following, on
        // the cursor when it fell below the forward window.
        let anchor = if !view.follow && !cursor_fits {
            cursor
        } else {
            last_rank
        };
        rows.clear();
        let mut top = anchor;
        for (rank, at) in visible.iter().copied().enumerate().take(anchor + 1).rev() {
            let mut built = build(rank, at);
            built.append(&mut rows);
            rows = built;
            top = rank;
            if rows.len() >= body_height {
                break;
            }
        }
        scroll_out = top;
        let start = rows.len().saturating_sub(body_height);
        rows = rows.split_off(start);
    } else {
        rows.truncate(body_height);
    }

    // Nothing but blank lines is no output, and a lone gutter mark on an
    // empty row reads as a broken paint.
    if !view.collapsed() && lines.iter().all(|line| line.plain.trim().is_empty()) {
        rows.clear();
    }
    // An empty body looks the same whether there is no output, the filter
    // hid everything, or the paint is broken. Say which.
    if rows.is_empty() {
        // The collapse is checked first: with a query that matches
        // nothing, the level filter is not what emptied the body and `f`
        // cannot bring anything back.
        let placeholder = if view.collapsed() {
            format!(
                "  no line matches `{}` — & expands, esc clears",
                view.search.query
            )
        } else if view
            .tail
            .lines()
            .iter()
            .all(|line| line.plain.trim().is_empty())
        {
            // A source that has written only blank lines — an install
            // with nothing to say — reads the same as one that has
            // written nothing.
            "  (no output yet)".to_string()
        } else {
            format!(
                "  nothing at level {} — press f to change the filter",
                view.log_filter.label()
            )
        };
        rows.push(Line::styled(
            truncate(&placeholder, width),
            Style::new().fg(text_muted()),
        ));
    }
    (rows, scroll_out)
}

/// Pretty-printed JSON needs room to nest before wrapping stops helping.
const INSPECT_MIN_WIDTH: u16 = 48;

/// The overlay: one log line (or one JSON block) pretty-printed, wrapped to
/// the popup width, scrollable.
pub(super) fn render_inspect(f: &mut Frame, area: Rect, app: &mut App) {
    let status = app
        .log_view()
        .and_then(|view| app.flash_for(&view.name))
        .cloned();
    let Some(inspect) = &app.inspect else { return };
    let height = area
        .height
        .saturating_sub(4)
        .clamp(8.min(area.height), area.height);
    let popup = centered_rect(80, INSPECT_MIN_WIDTH, height, area);
    if popup.width < 3 || popup.height < 3 {
        return;
    }
    let block = Block::bordered()
        .title(Span::styled(
            " inspect ",
            Style::new().fg(blue()).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::from(match &status {
            Some(status) => Span::styled(
                truncate(
                    &format!(" {}{} ", status_mark(status).0, status.message),
                    popup.width.saturating_sub(2) as usize,
                ),
                Style::new()
                    .fg(status_mark(status).1)
                    .add_modifier(Modifier::BOLD),
            ),
            None => Span::styled(
                truncate(
                    " j/k scroll · y copy · q close ",
                    popup.width.saturating_sub(2) as usize,
                ),
                Style::new().fg(text_muted()),
            ),
        }))
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border()))
        .style(Style::new().bg(surface()));
    let inner = block.inner(popup);
    f.render_widget(Clear, popup);
    f.render_widget(block, popup);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let width = (inner.width as usize).max(1);
    let rows: Vec<Line<'static>> = inspect
        .lines
        .iter()
        .cloned()
        .flat_map(|line| wrap_line_to_rows(line, width))
        .collect();
    let body_height = (inner.height as usize).max(1);
    let scroll = inspect.scroll.min(rows.len().saturating_sub(body_height));
    let visible: Vec<Line<'static>> = rows
        .iter()
        .skip(scroll)
        .take(body_height)
        .cloned()
        .collect();
    f.render_widget(Paragraph::new(visible), inner);

    if rows.len() > body_height {
        let mut state = ScrollbarState::new(rows.len())
            .viewport_content_length(body_height)
            .position(scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_style(Style::new().fg(border()).add_modifier(Modifier::BOLD)),
            popup.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut state,
        );
    }

    // Persist the clamp, so a `G` (which asks for usize::MAX) lands on the
    // real last page rather than off the end.
    if let Some(inspect) = &mut app.inspect {
        inspect.scroll = scroll;
    }
}

/// One log line as visual rows: wrapped (or truncated, when wrap is off) to
/// the width minus the one-column gutter, then every row prefixed with the
/// severity bar — so a tall wrapped error reads as one continuous red edge.
fn gutter_rows(
    line: Line<'static>,
    level: LogLevel,
    width: usize,
    wrap: bool,
    glyph: &'static str,
) -> Vec<Line<'static>> {
    let content_width = width.saturating_sub(1).max(1);
    let mut rows = if wrap {
        wrap_line_to_rows(line, content_width)
    } else {
        vec![truncate_line(line, content_width)]
    };
    for row in &mut rows {
        row.spans.insert(0, level_gutter(level, glyph));
    }
    rows
}

/// The coloured bar at a line's left edge: red for an error, yellow for a
/// warning, muted otherwise — so problems are visible while scanning,
/// without reading a word.
fn level_gutter(level: LogLevel, glyph: &'static str) -> Span<'static> {
    let colour = match level {
        LogLevel::Error => red(),
        LogLevel::Warn => yellow(),
        _ => text_muted(),
    };
    Span::styled(glyph, Style::new().fg(colour))
}

/// The gutter glyph for one buffer line: a multi-line JSON block draws as a
/// bracket (╭ │ ╰) so the whole entry reads as one unit; a standalone line
/// keeps the plain bar.
pub(super) fn block_glyph(
    buffer: &std::collections::VecDeque<ParsedLine>,
    at: usize,
) -> &'static str {
    let Some(id) = buffer.get(at).and_then(|parsed| parsed.block_id) else {
        return "▎";
    };
    let starts = at == 0 || buffer[at - 1].block_id != Some(id);
    let ends = at + 1 >= buffer.len() || buffer[at + 1].block_id != Some(id);
    match (starts, ends) {
        (true, true) => "▎",
        (true, false) => "╭",
        (false, true) => "╰",
        (false, false) => "│",
    }
}

/// Paints the search query wherever it occurs in a line — every hit in the
/// match background, the one the search cursor is on in the cursor
/// background.
///
/// The offsets are computed in a lowercased copy and mapped back, because
/// `to_lowercase` is not length-preserving (`İ` is two bytes and lowercases
/// to three), and slicing the original at lowercased offsets would cut a
/// character in half and panic.
pub(super) fn highlight_search_in_line(
    line: Line<'static>,
    plain: &str,
    query: &str,
    is_cursor: bool,
) -> Line<'static> {
    let bg = if is_cursor {
        search_cursor_bg()
    } else {
        search_match_bg()
    };

    let mut lowered = String::with_capacity(plain.len());
    let mut back: Vec<usize> = Vec::with_capacity(plain.len() + 1);
    for (at, ch) in plain.char_indices() {
        for lower in ch.to_lowercase() {
            for _ in 0..lower.len_utf8() {
                back.push(at);
            }
            lowered.push(lower);
        }
    }
    back.push(plain.len()); // the end maps to the end

    let needle = query.to_lowercase();
    if needle.is_empty() {
        return line;
    }
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut from = 0usize;
    while let Some(at) = lowered[from..].find(&needle) {
        let start = from + at;
        let end = start + needle.len();
        ranges.push((back[start], back[end]));
        from = end;
    }
    if ranges.is_empty() {
        return line;
    }

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut span_start = 0usize;
    for span in line.spans {
        let text: &str = span.content.as_ref();
        let len = text.len();
        let span_end = span_start + len;
        let mut offset = 0usize;
        for &(start, end) in &ranges {
            if end <= span_start || start >= span_end {
                continue;
            }
            let local_start = start.saturating_sub(span_start).max(offset);
            let local_end = (end - span_start).min(len);
            if local_start > offset {
                spans.push(Span::styled(
                    text[offset..local_start].to_string(),
                    span.style,
                ));
            }
            spans.push(Span::styled(
                text[local_start..local_end].to_string(),
                span.style.bg(bg),
            ));
            offset = local_end;
        }
        if offset < len {
            spans.push(Span::styled(text[offset..].to_string(), span.style));
        }
        span_start = span_end;
    }
    Line::from(spans)
}

/// Char-wrap a styled line into rows of at most `width` chars, keeping each
/// span's style across the split. Always at least one row, so a blank line
/// still occupies one.
///
/// A break that would fall inside a URL's `scheme://host` is moved to just
/// before the URL instead, so `http://` never ends up on one row with the
/// host on the next — a URL cut in half is one nobody can click, copy, or
/// read aloud. The path is fair game; only the part that has to stay
/// together is protected, and a host wider than the whole viewport is cut
/// like anything else.
pub(super) fn wrap_line_to_rows(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let cells: Vec<(char, Style)> = line
        .spans
        .iter()
        .flat_map(|span| {
            let style = span.style;
            span.content
                .chars()
                .map(move |c| (c, style))
                .collect::<Vec<_>>()
        })
        .collect();
    if cells.is_empty() {
        return vec![Line::from(Vec::<Span<'static>>::new())];
    }
    let text: String = cells.iter().map(|(c, _)| *c).collect();
    let protected = protected_ranges(&text);

    // Rows are measured in terminal cells, not characters: a line of CJK
    // cut every `width` characters is twice as wide as the viewport, and
    // the half past the edge is never painted.
    let span_width = |from: usize, to: usize| -> usize {
        cells[from..to]
            .iter()
            .map(|(c, _)| super::char_width(*c))
            .sum()
    };
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut start = 0usize;
    while start < cells.len() {
        let mut end = start;
        let mut used = 0usize;
        while end < cells.len() {
            let w = super::char_width(cells[end].0);
            // At least one character a row, or a glyph wider than the
            // whole viewport would never leave.
            if used + w > width && end > start {
                break;
            }
            used += w;
            end += 1;
        }
        if end < cells.len()
            && let Some(&(from, to)) = protected.iter().find(|&&(from, to)| from < end && end < to)
            && from > start
            && span_width(from, to) <= width
        {
            end = from;
        }
        rows.push(row_of(&cells[start..end]));
        start = end;
    }
    rows
}

/// Regroups consecutive cells that share a style back into spans.
fn row_of(cells: &[(char, Style)]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let mut current: Option<Style> = None;
    for (c, style) in cells {
        if current != Some(*style) {
            if let Some(style) = current {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
            }
            current = Some(*style);
        }
        buf.push(*c);
    }
    if let Some(style) = current {
        spans.push(Span::styled(buf, style));
    }
    Line::from(spans)
}

/// Char ranges a wrap should not split: each URL's `scheme://authority`,
/// ending at the first `/`, `?`, `#`, closing delimiter, or space.
pub(super) fn protected_ranges(text: &str) -> Vec<(usize, usize)> {
    const SCHEMES: [&str; 2] = ["https://", "http://"];
    let chars: Vec<char> = text.chars().collect();
    let mut ranges = Vec::new();
    let mut at = 0usize;
    while at < chars.len() {
        let ahead: String = chars[at..].iter().take(8).collect();
        let Some(scheme) = SCHEMES.iter().find(|s| ahead.starts_with(**s)) else {
            at += 1;
            continue;
        };
        let mut end = at + scheme.chars().count();
        while end < chars.len() {
            let c = chars[end];
            if c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\'' | ')' | '>' | ',') {
                break;
            }
            end += 1;
        }
        ranges.push((at, end));
        at = end.max(at + 1);
    }
    ranges
}

/// Paints the cursor line: every span without a background of its own gets
/// the highlight, and each row is padded to the full width so the bar spans
/// the viewport the way the list's highlight does.
fn cursor_highlight_rows(rows: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    rows.into_iter()
        .map(|row| {
            let mut used = 0usize;
            let mut spans: Vec<Span<'static>> = row
                .spans
                .into_iter()
                .map(|span| {
                    used += text_width(&span.content);
                    if span.style.bg.is_none() {
                        let style = span.style.bg(highlight_bg());
                        Span::styled(span.content, style)
                    } else {
                        span
                    }
                })
                .collect();
            if used < width {
                spans.push(Span::styled(
                    " ".repeat(width - used),
                    Style::new().bg(highlight_bg()),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

/// The tab strip. `repos` names the repository each source runs in, where
/// it is a process of a multi-repo project's: `2:api (backend)`. When that
/// does not fit the repositories go first; when the labelled tabs still do
/// not fit, only the active one keeps its label so the rest do not slide
/// off the edge.
pub(super) fn source_tabs(
    sources: &[String],
    repos: &[Option<&str>],
    active: &str,
    width: usize,
) -> Line<'static> {
    let tabs = |labelled: bool, with_repo: bool| -> Vec<Span<'static>> {
        sources
            .iter()
            .enumerate()
            .flat_map(|(i, source)| {
                let repo = match repos.get(i).copied().flatten() {
                    Some(repo) if with_repo => format!(" ({repo})"),
                    _ => String::new(),
                };
                let label = if labelled || source == active {
                    format!(" {}:{source}{repo} ", i + 1)
                } else {
                    format!(" {} ", i + 1)
                };
                let style = if source == active {
                    Style::new()
                        .fg(blue())
                        .add_modifier(Modifier::BOLD)
                        .bg(highlight_bg())
                } else {
                    Style::new().fg(text_dim())
                };
                [Span::styled(label, style), Span::raw(" ")]
            })
            .collect()
    };
    let fits = |spans: &[Span]| {
        spans
            .iter()
            .map(|span| text_width(&span.content))
            .sum::<usize>()
            <= width
    };
    for with_repo in [true, false] {
        let labelled = tabs(true, with_repo);
        if fits(&labelled) {
            return Line::from(labelled);
        }
    }
    truncate_line(Line::from(tabs(false, false)), width)
}

/// The viewer's one-row footer: hints or a confirmation on the left, the
/// counters and the position badge pinned right.
fn viewer_footer(
    view: &LogView,
    counts: ViewerCounts,
    cursor: usize,
    status: Option<Status>,
    has_tabs: bool,
    width: usize,
) -> Line<'static> {
    // While a query is being typed, or once it is live, the footer is the
    // search bar: it is the only row the viewer can spare.
    match view.search_mode {
        SearchMode::Typing => {
            return truncate_line(
                Line::from(vec![
                    Span::styled("/", Style::new().fg(orange()).add_modifier(Modifier::BOLD)),
                    Span::styled(view.search.query.clone(), Style::new().fg(text())),
                    Span::styled("▏", Style::new().fg(orange())),
                ]),
                width,
            );
        }
        SearchMode::Active => {
            let total = view.search.matches.len();
            let at = if total == 0 {
                0
            } else {
                view.search.cursor + 1
            };
            let hint = if view.filter_to_matches {
                "  [grep] n/N next/prev · & expand · esc clear"
            } else {
                "  n/N next/prev · & grep · esc clear"
            };
            return truncate_line(
                Line::from(vec![
                    Span::styled("/", Style::new().fg(orange())),
                    Span::styled(view.search.query.clone(), Style::new().fg(text())),
                    Span::styled(
                        format!("  [{at}/{total}]"),
                        Style::new().fg(yellow()).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(hint.to_string(), Style::new().fg(text_muted())),
                ]),
                width,
            );
        }
        SearchMode::Inactive => {}
    }

    // `new_below` counts the lines that arrived below a scrolled-back
    // viewport, and nothing reduces it when the ring buffer evicts those
    // very lines — so the badge could offer a jump to more lines than the
    // buffer holds. What it may claim is what is under the cursor now.
    let new_below = view
        .new_below
        .min(counts.visible.saturating_sub(cursor + 1));
    let (badge, badge_style) = if view.follow {
        (
            "FOLLOW ●".to_string(),
            Style::new().fg(green()).add_modifier(Modifier::BOLD),
        )
    } else if new_below > 0 {
        (
            format!("↓ {new_below} new (G)"),
            Style::new().fg(orange()).add_modifier(Modifier::BOLD),
        )
    } else if counts.visible == 0 {
        ("0/0".to_string(), Style::new().fg(text_dim()))
    } else {
        (
            format!("{}/{}", cursor + 1, counts.visible),
            Style::new().fg(text_dim()),
        )
    };

    // Counters are state, not reminders: they claim their width before the
    // hints do.
    let mut tail: Vec<Span<'static>> = Vec::new();
    if view.log_filter != LogFilter::All {
        tail.push(Span::styled(
            format!(
                "  [{} · {}/{}]",
                view.log_filter.label(),
                counts.visible,
                counts.total
            ),
            Style::new().fg(yellow()),
        ));
    }
    if counts.errors > 0 {
        tail.push(Span::styled(
            format!("  {}E", counts.errors),
            Style::new().fg(red()).add_modifier(Modifier::BOLD),
        ));
    }
    if counts.warnings > 0 {
        // Two cells from the hints, as the errors are, and one from the
        // errors' count when it follows that.
        let gap = if counts.errors > 0 { " " } else { "  " };
        tail.push(Span::styled(
            format!("{gap}{}W", counts.warnings),
            Style::new().fg(yellow()),
        ));
    }

    let badge_width = text_width(&badge);
    let tail_width: usize = tail
        .iter()
        .map(|span| text_width(&span.content))
        .sum::<usize>();
    let hint_budget = width.saturating_sub(tail_width + badge_width + 1);
    let mut spans = match &status {
        Some(status) => {
            let (mark, color) = status_mark(status);
            vec![Span::styled(
                format!(
                    " {mark}{}",
                    truncate(
                        &status.message,
                        hint_budget.saturating_sub(1 + text_width(mark))
                    )
                ),
                Style::new().fg(color).add_modifier(Modifier::BOLD),
            )]
        }
        None => log_hint_spans(has_tabs, hint_budget),
    };
    spans.extend(tail);

    let used: usize = spans
        .iter()
        .map(|span| text_width(&span.content))
        .sum::<usize>();
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(used + badge_width)),
    ));
    spans.push(Span::styled(badge, badge_style));
    truncate_line(Line::from(spans), width)
}

fn log_hint_spans(has_tabs: bool, width: usize) -> Vec<Span<'static>> {
    let shown: Vec<(&str, &str, bool)> = LOG_HINTS
        .iter()
        .filter(|(key, _, _)| has_tabs || *key != "1-9")
        .copied()
        .collect();
    truncate_line(hint_line(&shown, width), width).spans
}
