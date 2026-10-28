//! Painting. Nothing here talks to git, the filesystem, or a subprocess.
//!
//! The TUI is used in tmux splits, so nothing may assume a wide terminal:
//! list rows shed columns, the footer sheds hints, popups keep a minimum
//! width, and text truncates rather than clipping at a pane edge.
//!
//! The frame layout and the text helpers every part shares live here; the
//! list, the detail pane, the log viewer, the setup screen and the header
//! and footer each have their own file.

mod chrome;
mod detail;
mod hints;
mod list;
mod log_viewer;
mod setup;
mod welcome;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};

use super::app::{App, Hits};

use chrome::{header_height, render_footer, render_header};
use detail::render_detail;
use list::render_list;
pub(super) use list::run_marker;
use log_viewer::{render_inspect, render_log_viewer};
use setup::render_setup;
use welcome::render_welcome;

/// Below this the list and the detail pane stop both being readable side by
/// side — the list can no longer hold a name and the detail pane can no
/// longer hold `http://localhost:17000` — so they stack instead.
pub const SIDE_BY_SIDE_MIN_WIDTH: u16 = 72;

/// Stacking costs two extra border rows. Under this the panes would be a
/// couple of rows each, and a cramped side-by-side is the lesser evil.
const STACK_MIN_LIST_HEIGHT: u16 = 5;

const STACK_MIN_DETAIL_HEIGHT: u16 = 6;

/// List and detail rects for the body: side by side when there is width for
/// both, stacked when there is not. pando is used in a tmux split, so the
/// narrow case is the normal one.
pub fn body_layout(body: Rect) -> [Rect; 2] {
    let stack = body.width < SIDE_BY_SIDE_MIN_WIDTH
        && body.height >= STACK_MIN_LIST_HEIGHT + STACK_MIN_DETAIL_HEIGHT;
    // The list is a table and the thing the screen is for; the detail
    // pane is read a row at a time, and the full log has its own screen.
    if !stack {
        return Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .areas(body);
    }
    let list_height = (body.height * 2 / 5)
        .max(STACK_MIN_LIST_HEIGHT)
        .min(body.height.saturating_sub(STACK_MIN_DETAIL_HEIGHT));
    Layout::vertical([Constraint::Length(list_height), Constraint::Fill(1)]).areas(body)
}

pub fn render(f: &mut Frame, app: &mut App) {
    // Each part records where it put what the mouse acts on, so nothing
    // this frame does not draw can be clicked.
    app.hits = Hits::default();
    // The viewer takes the whole screen: the header and footer it would
    // share with the list are the two rows a long stack trace needs most.
    if app.log_view().is_some() {
        render_log_viewer(f, f.area(), app);
        if app.inspect.is_some() {
            render_inspect(f, f.area(), app);
        }
        paint_modal(f, app);
        return;
    }
    // So does the setup screen, which has a header and a footer of its
    // own, and is up whatever git lists.
    if app.setup_screen.is_some() {
        render_setup(f, f.area(), app);
        paint_modal(f, app);
        return;
    }

    // The header grows for an error that does not fit on one row: the end
    // of an error is usually the part that says what to do.
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(header_height(app, f.area().width, f.area().height)),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(f.area());

    render_header(f, header, app);
    // No worktrees at all is the first run, and a welcome says more than
    // an empty box beside a pane about nothing. A main checkout pando has
    // never run is no row of its own yet: see `App::main_row_shown`.
    if !app.main_row_shown() {
        render_welcome(f, body, app);
    } else {
        let [list_area, detail_area] = body_layout(body);
        render_list(f, list_area, app);
        render_detail(f, detail_area, app);
    }
    render_footer(f, footer, app);
    app.hits.keys = hints::key_hints(f.buffer_mut(), footer);

    paint_modal(f, app);
}

/// The open modal, on top of whatever is under it, keeping how far help
/// or messages can scroll for the keys that scroll them. Only its own
/// hints take a click: what is under it is not what the keys answer.
fn paint_modal(f: &mut Frame, app: &mut App) {
    let Some(modal) = &app.modal else {
        return;
    };
    let painted = super::modal::render_modal(f, f.area(), modal, app);
    if let Some(bottom) = painted.scroll_bottom {
        app.help_scroll_max = bottom;
    }
    app.hits.options = painted.options;
    app.hits.keys = painted
        .content
        .map(|content| hints::key_hints(f.buffer_mut(), content))
        .unwrap_or_default();
}

/// A centred popup of exactly `width` × `height`, clamped to the area.
pub fn centered_box(width: u16, height: u16, area: Rect) -> Rect {
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, popup, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width.min(area.width)),
        Constraint::Fill(1),
    ])
    .areas(middle);
    popup
}

/// A centred popup that keeps a minimum width, so a narrow split gets a
/// readable box rather than a sliver.
pub fn centered_rect(percent_x: u16, min_width: u16, height: u16, area: Rect) -> Rect {
    // In u32: a percentage of a width past 1092 overflows a u16 before the
    // division brings it back — a panic in debug, and silently the wrong
    // popup width in release.
    let target = (area.width as u32 * percent_x as u32 / 100) as u16;
    let width = target.max(min_width).min(area.width);
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, popup, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width),
        Constraint::Fill(1),
    ])
    .areas(middle);
    popup
}

/// How many terminal cells `c` takes: two for a CJK ideograph or an
/// emoji, none for a combining mark, and none for a control character —
/// the paint drops those rather than drawing them.
pub fn char_width(c: char) -> usize {
    if c.is_control() {
        return 0;
    }
    let mut buf = [0u8; 4];
    Span::raw(&*c.encode_utf8(&mut buf)).width()
}

/// How many terminal cells `s` takes. Every budget in the TUI is in cells:
/// counting characters lets a CJK branch name run twice as far as the
/// room it was given, past the column beside it or off the pane.
pub fn text_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// The longest prefix of `s` that fits in `cells`, as a byte index.
fn fitting_prefix(s: &str, cells: usize) -> usize {
    let mut used = 0usize;
    for (at, c) in s.char_indices() {
        let w = char_width(c);
        if used + w > cells {
            return at;
        }
        used += w;
    }
    s.len()
}

pub fn truncate(s: &str, max: usize) -> String {
    if text_width(s) <= max {
        s.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let head = &s[..fitting_prefix(s, max - 1)];
        format!("{head}…")
    }
}

/// Cuts the middle out of a string that does not fit, keeping both ends:
/// the start of a path says where it is, the end says what it is.
pub fn truncate_middle(s: &str, max: usize) -> String {
    if text_width(s) <= max {
        return s.to_string();
    }
    if max < 5 {
        return truncate(s, max);
    }
    let tail_room = (max - 1) / 2;
    let head_room = max - 1 - tail_room;
    let head = &s[..fitting_prefix(s, head_room)];
    // The tail, walked from the end for as many cells as it may have.
    let mut used = 0usize;
    let mut tail_at = s.len();
    for (at, c) in s.char_indices().rev() {
        let w = char_width(c);
        if used + w > tail_room {
            break;
        }
        used += w;
        tail_at = at;
    }
    format!("{head}…{}", &s[tail_at..])
}

/// A path as a person writes it: `~` for the home directory.
pub fn home_relative(path: &std::path::Path) -> String {
    let shown = path.display().to_string();
    let Some(home) = crate::platform::dirs::home() else {
        return shown;
    };
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => shown,
    }
}

/// Cuts a label so that what tells it apart from its neighbours survives.
///
/// `distinct_from` is where the label first differs from the most similar
/// other label in the list. When that is past what a plain cut would keep —
/// thirty branches called `feature/very-long-branch-name-number-N` — the
/// cut is made at the front instead, starting shortly before the part
/// that differs.
pub fn truncate_distinct(s: &str, max: usize, distinct_from: usize) -> String {
    /// Characters kept before the point of difference, so `number-12`
    /// reads as a number and not as a stray `12`.
    const CONTEXT: usize = 8;
    let len = s.chars().count();
    // The offsets are in characters, which is a cell apiece only when
    // nothing in the label is wide: a label that is not is cut plainly.
    if text_width(s) <= max || max < 4 || distinct_from + 1 < max || text_width(s) != len {
        return truncate(s, max);
    }
    // A short leading segment — `feature/` — is kept: it is the kind of
    // branch, and cheap.
    let head: String = match s.find('/') {
        Some(slash) if s[..=slash].chars().count() <= max / 3 => s[..=slash].to_string(),
        _ => String::new(),
    };
    let head_len = head.chars().count();
    let room = max - head_len - 1;
    let start = distinct_from.saturating_sub(CONTEXT).max(head_len).min(len);
    if len - start <= room {
        // The rest fits after the ellipsis: keep as much of the end as the
        // width allows.
        let tail: String = s.chars().skip(len - room).collect();
        return format!("{head}…{tail}");
    }
    let middle: String = s.chars().skip(start).take(room - 1).collect();
    format!("{head}…{middle}…")
}

/// Where each label first differs from the most similar of the others: the
/// length of its longest common prefix with any of them. Sorted, the most
/// similar label is always a neighbour, so this is one sort and one pass.
pub fn distinct_offsets(labels: &[String]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..labels.len()).collect();
    order.sort_by(|&a, &b| labels[a].cmp(&labels[b]));
    let common = |a: &str, b: &str| a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let mut offsets = vec![0; labels.len()];
    for (at, &i) in order.iter().enumerate() {
        let before = at
            .checked_sub(1)
            .map(|p| common(&labels[i], &labels[order[p]]))
            .unwrap_or(0);
        let after = order
            .get(at + 1)
            .map(|&n| common(&labels[i], &labels[n]))
            .unwrap_or(0);
        offsets[i] = before.max(after);
    }
    offsets
}

/// Word-wraps plain text to `width` cells, breaking a word only when it is
/// wider than a whole row. Always at least one row.
pub fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for paragraph in text.split('\n') {
        let mut row = String::new();
        let mut row_len = 0usize;
        for word in paragraph.split(' ') {
            let mut word: Vec<char> = word.chars().collect();
            loop {
                let word_len = cells(&word);
                let needed = if row_len == 0 {
                    word_len
                } else {
                    row_len + 1 + word_len
                };
                if needed <= width {
                    if row_len > 0 {
                        row.push(' ');
                    }
                    row.extend(word.iter());
                    row_len = needed;
                    break;
                }
                // A word that fits a row of its own goes to the next one
                // whole. One wider than any row — a path, usually — breaks
                // after a `/` where it can, filling what is left of this
                // row first, and is only cut mid-name when it has none.
                let fits_alone = word_len <= width;
                let room = if row_len > 0 {
                    width.saturating_sub(row_len + 1)
                } else {
                    width
                };
                let slash = if fits_alone {
                    None
                } else {
                    slash_break(&word, room)
                };
                if let Some(at) = slash {
                    let rest = word.split_off(at);
                    if row_len > 0 {
                        row.push(' ');
                    }
                    row.extend(word.iter());
                    rows.push(std::mem::take(&mut row));
                    row_len = 0;
                    word = rest;
                    continue;
                }
                if row_len > 0 {
                    rows.push(std::mem::take(&mut row));
                    row_len = 0;
                    continue;
                }
                // Wider than the row, and nowhere to break it: cut it,
                // carry the rest. At least one character goes, or a glyph
                // wider than the whole row would never leave.
                let rest = word.split_off(fitting_chars(&word, width).max(1));
                if rest.is_empty() {
                    // The last piece is the row the next word joins.
                    row.extend(word.iter());
                    row_len = cells(&word);
                    break;
                }
                rows.push(word.iter().collect());
                word = rest;
            }
        }
        rows.push(row);
    }
    rows
}

/// `s` cut into rows of at most `width` cells, anywhere — for a value
/// that is never truncated, a URL or a branch, where a word wrap has no
/// words to break at. Always at least one row.
pub fn chunk_cells(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = vec![String::new()];
    let mut used = 0usize;
    for c in s.chars() {
        let w = char_width(c);
        if used + w > width && used > 0 {
            rows.push(String::new());
            used = 0;
        }
        used += w;
        rows.last_mut().expect("never empty").push(c);
    }
    rows
}

fn cells(chars: &[char]) -> usize {
    chars.iter().copied().map(char_width).sum()
}

/// How many of `chars` fit in `room` cells.
fn fitting_chars(chars: &[char], room: usize) -> usize {
    let mut used = 0usize;
    for (at, &c) in chars.iter().enumerate() {
        used += char_width(c);
        if used > room {
            return at;
        }
    }
    chars.len()
}

/// Where to break `word` so its first part fits in `room` cells: just
/// after the last `/` that leaves a part that fits. `None` when there is
/// no such `/`, or it would leave nothing on the row.
fn slash_break(word: &[char], room: usize) -> Option<usize> {
    let limit = fitting_chars(word, room);
    (1..=limit).rev().find(|&at| word[at - 1] == '/')
}

fn pad(s: &str, width: usize) -> String {
    let len = text_width(s);
    if len >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - len))
    }
}

/// Cuts a styled line to `width` cells, keeping the styling of the spans
/// that survive.
pub fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    let total: usize = line.spans.iter().map(|s| text_width(&s.content)).sum();
    if total <= width {
        return line;
    }
    let mut remaining = width;
    let mut spans = Vec::new();
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let len = text_width(&span.content);
        if len <= remaining {
            remaining -= len;
            spans.push(span);
        } else {
            let cut = truncate(&span.content, remaining);
            remaining = 0;
            spans.push(Span::styled(cut, span.style));
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests;
