//! The grove's own part of the screen: the column of every project, the
//! one-line strip it folds to on a narrow screen, and the home pane
//! outside any repository, the brand's grove with a stem per project.
//!
//! Each project row is a stem in the list's glyph language: `┃` with `●`
//! up, `◌` coming up, `✗` failed, `│` dim while idle, `╎` for one that
//! moved, all standing on one root line that is lit while anything on
//! the machine is up.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::actions::ProjectSummary;
use crate::state::Aggregate;
use crate::theme::{
    blue, border, green, highlight_bg, hint_key, orange, red, surface, text, text_dim, text_muted,
    yellow,
};
use crate::tui::grove::{COLUMN_HINTS, COLUMN_MIN_WIDTH, Focus, Grove, GroveHits};

use super::chrome::build_label_spans;
use super::setup::{art_line_with, art_style};
use super::{SIDE_BY_SIDE_MIN_WIDTH, render_in, text_width, truncate};

/// The cells a row gives its marks before the name: the cursor, the
/// stem, the glyph.
const ROW_LEAD: usize = 5;

/// The column of the stem in a row, where the root line's `┻` goes.
const STEM_AT: u16 = 2;

/// The tallest and widest the home pane's grove is drawn.
const HOME_GROVE_ROWS: usize = 11;
const HOME_GROVE_WIDTH: usize = 100;

/// The whole screen as the grove draws it: the column or the strip, then
/// the project in front, or the home pane, in what is left, and the
/// grove's own confirmation over all of it.
pub fn render_grove(f: &mut Frame, grove: &mut Grove) {
    let area = f.area();
    grove.hits = GroveHits::default();
    let shown = grove.column_shown();
    let wide = area.width >= SIDE_BY_SIDE_MIN_WIDTH;
    let rest = match (shown, wide) {
        (false, _) => area,
        (true, true) => {
            let width = grove
                .width
                .min(area.width / 2)
                .max(COLUMN_MIN_WIDTH.min(area.width));
            let [column, rest] =
                Layout::horizontal([Constraint::Length(width), Constraint::Fill(1)]).areas(area);
            render_column(f, column, grove);
            rest
        }
        (true, false) => {
            let [strip, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
            render_strip(f, strip, grove);
            rest
        }
    };
    let front = grove.front.clone();
    match front.and_then(|id| grove.open.get_mut(&id)) {
        Some(opened) => {
            opened.app.grove_chrome = shown && wide;
            opened.app.detail = grove.detail;
            render_in(f, rest, &mut opened.app);
        }
        None => render_home(f, rest, grove),
    }
    if let Some(confirm) = &grove.confirm {
        let phases: Vec<(String, Option<Aggregate>)> = grove
            .summaries
            .iter()
            .find(|s| s.id == confirm.id)
            .map(|s| {
                confirm
                    .worktrees
                    .iter()
                    .map(|name| {
                        let phase = s
                            .worktrees
                            .iter()
                            .find(|w| &w.name == name)
                            .map(|w| w.phase.clone());
                        (name.clone(), phase)
                    })
                    .collect()
            })
            .unwrap_or_default();
        crate::tui::modal::render_stop_project(f, area, &confirm.name, &phases);
    }
}

/// How a project's row reads: its stem, its glyph, the colour of its
/// name, and what is counted at its end.
struct RowLook {
    stem: (char, Color),
    glyph: (&'static str, Color),
    name: Color,
    /// A project with something up or failed is named in bold: a theme
    /// whose dim text is its text still tells it from an idle one.
    bold: bool,
    count: Vec<Span<'static>>,
}

fn row_look(grove: &Grove, summary: &ProjectSummary) -> RowLook {
    let live = summary.is_live();
    let open = grove.open.get(&summary.id).map(|o| &o.app);
    let glyph = match () {
        _ if summary.moved => ("  ", text_muted()),
        _ if open.is_some_and(|app| app.awaiting_answer()) => ("? ", orange()),
        _ if open.is_some_and(|app| !app.pending.is_empty()) => ("◌ ", yellow()),
        _ if grove.opening.contains(&summary.id) => ("◌ ", yellow()),
        _ if summary.up > 0 => ("● ", green()),
        _ if summary.starting > 0 => ("◌ ", yellow()),
        _ if summary.failed > 0 => ("✗ ", red()),
        _ => ("  ", text_muted()),
    };
    let stem = match () {
        _ if summary.moved => ('╎', text_muted()),
        _ if live => ('┃', text()),
        // As the home pane draws a failed project's trunk.
        _ if summary.failed > 0 => ('┃', red()),
        _ => ('│', text_dim()),
    };
    let name = match () {
        _ if grove.front.as_deref() == Some(summary.id.as_str()) => blue(),
        _ if summary.moved => text_muted(),
        _ if live || summary.failed > 0 => text(),
        _ => text_dim(),
    };
    let mut count = Vec::new();
    if summary.moved {
        count.push(Span::styled("moved", Style::new().fg(text_muted())));
    } else {
        let up = summary.up + summary.starting;
        if up > 0 {
            count.push(Span::styled(up.to_string(), Style::new().fg(green())));
        }
        if summary.failed > 0 {
            if !count.is_empty() {
                count.push(Span::raw(" "));
            }
            count.push(Span::styled(
                format!("✗{}", summary.failed),
                Style::new().fg(red()),
            ));
        }
    }
    RowLook {
        stem,
        glyph,
        name,
        bold: !summary.moved && (live || summary.failed > 0),
        count,
    }
}

/// The column: pando's name and what is up on the whole machine in its
/// border, a row per project, the keys while it has the focus, and the
/// root line every stem stands on.
fn render_column(f: &mut Frame, area: Rect, grove: &mut Grove) {
    grove.hits.column = area;
    let focused = grove.focus == Focus::Column || grove.front.is_none();
    let (up, failed) = grove.summaries.iter().fold((0, 0), |(up, failed), s| {
        (up + s.up + s.starting, failed + s.failed)
    });
    let (title, tally) = column_title(area.width, crate::version::label(), up, failed);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { blue() } else { border() }))
        .title_top(title)
        .title_top(tally);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let hints = match focused && inner.height >= 6 {
        true => column_hint_lines(width),
        false => Vec::new(),
    };
    let footer = 1 + hints.len() as u16;
    let rows_height = inner.height.saturating_sub(footer);
    let [rows_area, hints_area, root_area] = Layout::vertical([
        Constraint::Length(rows_height),
        Constraint::Length(hints.len() as u16),
        Constraint::Length(1),
    ])
    .areas(inner);
    if grove.summaries.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                truncate(" no project yet", width),
                Style::new().fg(text_muted()),
            )),
            rows_area,
        );
    }
    let visible = rows_area.height as usize;
    let offset = grove
        .cursor
        .saturating_sub(visible.saturating_sub(1))
        .min(grove.summaries.len().saturating_sub(visible));
    let mut rows = Vec::new();
    for (row, (i, summary)) in grove
        .summaries
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .enumerate()
    {
        let rect = Rect::new(rows_area.x, rows_area.y + row as u16, rows_area.width, 1);
        let selected = i == grove.cursor;
        f.render_widget(column_row(grove, summary, selected, focused, width), rect);
        rows.push((rect, i));
    }
    grove.hits.rows = rows;
    f.render_widget(Paragraph::new(hints), hints_area);
    let alive = grove.summaries.iter().any(|s| s.is_live());
    f.render_widget(Paragraph::new(root_line(width, alive)), root_area);
}

/// The column's top border: pando's name, the build's branch when this
/// is not a release, and what is up and failed on the whole machine at
/// its right. The branch gives way to the tally, shortened with `…`, and
/// goes when not even its first letter fits: the tally is the news.
pub(crate) fn column_title(
    width: u16,
    label: Option<&str>,
    up: usize,
    failed: usize,
) -> (Line<'static>, Line<'static>) {
    let tally_of = |gap: &str| {
        let mut tally = Vec::new();
        if up > 0 {
            tally.push(Span::styled(
                format!(" ●{gap}{up}"),
                Style::new().fg(green()),
            ));
        }
        if failed > 0 {
            tally.push(Span::styled(
                format!(" ✗{gap}{failed}"),
                Style::new().fg(red()).add_modifier(Modifier::BOLD),
            ));
        }
        if !tally.is_empty() {
            tally.push(Span::raw(" "));
        }
        let width: usize = tally.iter().map(|s| text_width(&s.content)).sum();
        (tally, width)
    };
    let name = " pando ";
    // Two corners, the name, a cell of border before the tally: spaced
    // out where that fits, tight where it does not, and pando's name
    // given up only when not even that does.
    let fits = |tally: usize| text_width(name) + tally + 3 <= usize::from(width);
    let (mut tally, mut tally_width) = tally_of(" ");
    if !fits(tally_width) {
        (tally, tally_width) = tally_of("");
    }
    if !fits(tally_width) {
        return (Line::default(), Line::from(tally).right_aligned());
    }
    let mut title = vec![Span::styled(
        name,
        Style::new().fg(blue()).add_modifier(Modifier::BOLD),
    )];
    // Two corners, the name, a cell of border before the tally and
    // after the branch.
    let room = usize::from(width).saturating_sub(2 + text_width(name) + tally_width + 2);
    // The build's branch, without the separator the header puts after it.
    if let Some(label) = build_label_spans(label).into_iter().next()
        && room >= 4
    {
        let short = truncate(&label.content, room - 1);
        title.push(Span::styled(short, label.style));
        title.push(Span::raw(" "));
    }
    (Line::from(title), Line::from(tally).right_aligned())
}

/// One project's row: ` ▸┃● name      2`, the cursor's on a band.
fn column_row(
    grove: &Grove,
    summary: &ProjectSummary,
    selected: bool,
    focused: bool,
    width: usize,
) -> Paragraph<'static> {
    let look = row_look(grove, summary);
    let cursor = match (selected, focused) {
        (true, true) => Span::styled("▸ ", Style::new().fg(blue())),
        (true, false) => Span::styled("▸ ", Style::new().fg(text_muted())),
        (false, _) => Span::raw("  "),
    };
    let count_width: usize = look.count.iter().map(|s| text_width(&s.content)).sum();
    let room = width.saturating_sub(ROW_LEAD + count_width + usize::from(count_width > 0));
    let name = truncate(&summary.name, room);
    let gap = width.saturating_sub(ROW_LEAD + text_width(&name) + count_width);
    let mut spans = vec![
        cursor,
        Span::styled(look.stem.0.to_string(), Style::new().fg(look.stem.1)),
        Span::styled(look.glyph.0, Style::new().fg(look.glyph.1)),
        Span::styled(name, name_style(&look)),
        Span::raw(" ".repeat(gap)),
    ];
    spans.extend(look.count);
    let line = Line::from(spans);
    // The band says where the cursor is, focused or not; the cursor's
    // colour says whether the column has the keys. Bold stays the name's,
    // so a selected idle project still reads as idle.
    let band = match selected {
        true => Style::new().bg(highlight_bg()),
        false => Style::new(),
    };
    Paragraph::new(line).style(band)
}

fn name_style(look: &RowLook) -> Style {
    let style = Style::new().fg(look.name);
    match look.bold {
        true => style.add_modifier(Modifier::BOLD),
        false => style,
    }
}

/// The column's keys while it has them, as many rows as they take.
fn column_hint_lines(width: usize) -> Vec<Line<'static>> {
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0;
    for (key, label) in COLUMN_HINTS {
        let needs = 1 + text_width(key) + 1 + text_width(label);
        if used + needs > width && used > 0 {
            lines.push(Vec::new());
            used = 0;
        }
        let line = lines.last_mut().expect("one line at least");
        line.push(Span::raw(" "));
        line.push(Span::styled(
            key,
            Style::new().fg(hint_key()).add_modifier(Modifier::BOLD),
        ));
        line.push(Span::styled(
            format!(" {label}"),
            Style::new().fg(text_muted()),
        ));
        used += needs;
    }
    lines.into_iter().map(Line::from).collect()
}

/// The root line under the stems: `╺━┻━━…━╸`, lit while anything is up,
/// as the setup screen's roots are once the project runs.
fn root_line(width: usize, alive: bool) -> Line<'static> {
    let line: String = (0..width)
        .map(|x| match x {
            _ if x == STEM_AT as usize => '┻',
            0 => '╺',
            _ if x + 1 == width => '╸',
            _ => '━',
        })
        .collect();
    Line::styled(
        line,
        Style::new().fg(if alive { green() } else { text_dim() }),
    )
}

/// The column folded to one row on a narrow screen: the project in
/// front, or the cursor's while the strip has the keys, with `[` and `]`
/// either side to step through them.
fn render_strip(f: &mut Frame, area: Rect, grove: &mut Grove) {
    grove.hits.column = area;
    let focused = grove.focus == Focus::Column || grove.front.is_none();
    let index = match focused {
        true => Some(grove.cursor),
        false => grove
            .front
            .as_deref()
            .and_then(|id| grove.summaries.iter().position(|s| s.id == id)),
    }
    .filter(|&i| i < grove.summaries.len());
    let key = Style::new().fg(hint_key()).add_modifier(Modifier::BOLD);
    let muted = Style::new().fg(text_muted());
    let lead = vec![
        Span::raw(" "),
        Span::styled("[", key),
        Span::styled(" ◀ ", muted),
    ];
    let tail = vec![Span::styled(" ▶ ", muted), Span::styled("]", key)];
    let mut middle = Vec::new();
    match index.and_then(|i| Some((i, grove.summaries.get(i)?))) {
        Some((i, summary)) => {
            let look = row_look(grove, summary);
            middle.push(Span::styled(look.glyph.0, Style::new().fg(look.glyph.1)));
            let state = match () {
                _ if summary.moved => " · moved".to_string(),
                _ if summary.is_live() => format!(" · {} up", summary.up + summary.starting),
                _ if summary.failed > 0 => format!(" · ✗{} failed", summary.failed),
                _ => " · idle".to_string(),
            };
            let place = format!(" · {}/{}", i + 1, grove.summaries.len());
            let fixed: usize = lead
                .iter()
                .chain(&tail)
                .map(|s| text_width(&s.content))
                .sum::<usize>()
                + 2
                + text_width(&state)
                + text_width(&place);
            let name = truncate(&summary.name, (area.width as usize).saturating_sub(fixed));
            middle.push(Span::styled(
                name,
                Style::new().fg(look.name).add_modifier(Modifier::BOLD),
            ));
            let state_colour = match () {
                _ if summary.is_live() => green(),
                _ if summary.failed > 0 && !summary.moved => red(),
                _ => text_dim(),
            };
            middle.push(Span::styled(state, Style::new().fg(state_colour)));
            middle.push(Span::styled(place, muted));
        }
        None => middle.push(Span::styled("no project yet", muted)),
    }
    let lead_width: u16 = lead.iter().map(|s| text_width(&s.content) as u16).sum();
    let middle_width: u16 = middle.iter().map(|s| text_width(&s.content) as u16).sum();
    grove.hits.prev = Rect::new(area.x, area.y, lead_width.min(area.width), 1);
    let next_x = area.x + lead_width + middle_width;
    if next_x < area.right() {
        grove.hits.next = Rect::new(next_x, area.y, 4.min(area.right() - next_x), 1);
    }
    let mut spans: Vec<Span> = lead.into_iter().chain(middle).chain(tail).collect();
    // With the keys taken from the list in front, the strip says what they
    // do now and how to give them back, at its right end: the footer below
    // it is the list's, and its keys are not the strip's.
    if grove.focus == Focus::Column && grove.front.is_some() {
        let used: usize = spans.iter().map(|s| text_width(&s.content)).sum();
        let hints = strip_hints(area.width as usize - used.min(area.width as usize));
        let hints_width: usize = hints.iter().map(|s| text_width(&s.content)).sum();
        if hints_width > 0 {
            let gap = area.width as usize - used - hints_width;
            spans.push(Span::raw(" ".repeat(gap)));
            spans.extend(hints);
        }
    }
    let background = match focused {
        true => highlight_bg(),
        false => surface(),
    };
    f.render_widget(
        Paragraph::new(super::truncate_line(Line::from(spans), area.width as usize))
            .style(Style::new().bg(background)),
        area,
    );
}

/// The column's hints that fit in `room` cells, the way back to the list
/// first, each a key of `COLUMN_HINTS`, and a cell to spare at the end.
fn strip_hints(room: usize) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut used = 1;
    for want in ["⏎", "tab", "X", "w"] {
        let Some(&(key, label)) = COLUMN_HINTS.iter().find(|(k, _)| *k == want) else {
            continue;
        };
        let needs = 2 + text_width(key) + 1 + text_width(label);
        if used + needs > room {
            break;
        }
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            key,
            Style::new().fg(hint_key()).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::new().fg(text_muted()),
        ));
        used += needs;
    }
    if !spans.is_empty() {
        spans.push(Span::raw(" "));
    }
    spans
}

/// The home pane, with no project in front: the wordmark, then the grove
/// with a stem for each project, its name under it and the ones with
/// something up in leaf, and the keys that open one.
fn render_home(f: &mut Frame, area: Rect, grove: &Grove) {
    use crate::art::{
        COMPACT_WIDTH, GROVE_MIN_HEIGHT, WORDMARK_HEIGHT, WORDMARK_WIDTH, WordmarkSize,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border()))
        .title(Span::styled(" grove ", Style::new().fg(text_dim())));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let height = inner.height as usize;
    let art_width = width.saturating_sub(4).min(HOME_GROVE_WIDTH);
    let left = (width - art_width) / 2;
    let projects: Vec<&ProjectSummary> = grove.summaries.iter().filter(|s| !s.moved).collect();
    let stems = projects.len().min(crate::art::stems_that_fit(art_width));
    let more = projects.len() - stems;
    // Under the picture: the names, a `+n more` when some have no stem,
    // the caption, a blank row, the keys, and a note when there is one.
    let under = 1 + usize::from(more > 0) + 1 + 1 + 1 + usize::from(grove.fresh_note().is_some());
    let big = WORDMARK_HEIGHT + 2;
    let compact = 2 + 2;
    let mark = match () {
        _ if width >= WORDMARK_WIDTH && height >= big + 8 + under => Some(WordmarkSize::Big),
        _ if width >= COMPACT_WIDTH && height >= compact + GROVE_MIN_HEIGHT + under => {
            Some(WordmarkSize::Compact)
        }
        _ => None,
    };
    let mark_rows = match mark {
        Some(WordmarkSize::Big) => big,
        Some(WordmarkSize::Compact) => compact,
        None => 0,
    };
    let rows = height
        .saturating_sub(mark_rows + under)
        .min(HOME_GROVE_ROWS);
    let frame = u64::from(grove.tick);
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(size) = mark {
        let mark_width = match size {
            WordmarkSize::Big => WORDMARK_WIDTH,
            WordmarkSize::Compact => COMPACT_WIDTH,
        };
        let centre = |w: usize| width.saturating_sub(w) / 2;
        let alive = projects.iter().any(|s| s.is_live());
        lines.extend(
            crate::art::wordmark(size, frame)
                .iter()
                .map(|row| art_line_with(row, centre(mark_width), |_, c| art_style(c, alive))),
        );
        let credit = truncate(&crate::art::credit(), width);
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(centre(text_width(&credit)))),
            Span::styled(credit, Style::new().fg(text_muted())),
        ]));
        lines.push(Line::raw(""));
    }
    let shown: Vec<&ProjectSummary> = projects.iter().take(stems).copied().collect();
    let leafed: Vec<bool> = shown.iter().map(|s| s.is_live()).collect();
    let picture = crate::art::grove_of(
        art_width,
        rows,
        frame,
        crate::art::seed_of("pando"),
        &leafed,
    );
    let alive = leafed.iter().any(|&l| l);
    let selected = grove.selected().map(|s| s.id.as_str());
    if !picture.is_empty() {
        // A failed project's stem in red: the leaves say what is up, the
        // trunk what went wrong.
        let failed: Vec<usize> = picture
            .stems
            .iter()
            .zip(&shown)
            .filter(|(_, s)| s.failed > 0 && !s.is_live())
            .map(|(&x, _)| x)
            .collect();
        lines.extend(picture.cells.iter().map(|row| {
            art_line_with(row, left, |x, cell| match cell.material {
                crate::art::Material::Trunk | crate::art::Material::Eye if failed.contains(&x) => {
                    Style::new().fg(red())
                }
                _ => art_style(cell, alive),
            })
        }));
        let labels: Vec<String> = shown
            .iter()
            .map(|s| match s.failed > 0 && !s.is_live() {
                true => format!("✗{}", s.name),
                false => s.name.clone(),
            })
            .collect();
        let mut spans = vec![Span::raw(" ".repeat(left))];
        let mut at = 0;
        for (x, label) in crate::art::label_each_stem(&picture.stems, &labels, art_width) {
            let Some(i) = picture.stems.iter().position(|&s| s == x) else {
                continue;
            };
            let summary = shown[i];
            let style = match () {
                _ if Some(summary.id.as_str()) == selected => {
                    Style::new().fg(blue()).add_modifier(Modifier::BOLD)
                }
                _ if summary.is_live() => Style::new().fg(text()).add_modifier(Modifier::BOLD),
                _ if summary.failed > 0 => Style::new().fg(red()),
                _ => Style::new().fg(text_dim()),
            };
            spans.push(Span::raw(" ".repeat(x.saturating_sub(at))));
            at = x + text_width(&label);
            spans.push(Span::styled(label, style));
        }
        lines.push(Line::from(spans));
    }
    if more > 0 {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(left)),
            Span::styled(
                format!("+{more} more in the column"),
                Style::new().fg(text_muted()),
            ),
        ]));
    }
    let caption = match () {
        _ if projects.is_empty() => "no project yet — run pando in a repository and it grows here",
        _ if alive => "one root, every project alive",
        _ => "Pando: one aspen, 47,000 stems, one root. Every project, every branch alive.",
    };
    let caption = truncate(caption, art_width.max(1));
    lines.push(Line::from(vec![
        Span::raw(" ".repeat(width.saturating_sub(text_width(&caption)) / 2)),
        Span::styled(
            caption,
            Style::new()
                .fg(if alive { green() } else { text_muted() })
                .add_modifier(Modifier::ITALIC),
        ),
    ]));
    lines.push(Line::raw(""));
    let key =
        |k: &'static str| Span::styled(k, Style::new().fg(orange()).add_modifier(Modifier::BOLD));
    let said = |s: &'static str| Span::styled(s, Style::new().fg(text_dim()));
    let keys = vec![
        key("⏎"),
        said(" open  "),
        key("j k"),
        said(" project  "),
        key("w"),
        said(" column  "),
        key("q"),
        said(" quit"),
    ];
    let keys_width: usize = keys.iter().map(|s| text_width(&s.content)).sum();
    let mut centred = vec![Span::raw(" ".repeat(width.saturating_sub(keys_width) / 2))];
    centred.extend(keys);
    lines.push(super::truncate_line(Line::from(centred), width));
    if let Some(note) = grove.fresh_note() {
        let note = truncate(note, art_width.max(1));
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(width.saturating_sub(text_width(&note)) / 2)),
            Span::styled(note, Style::new().fg(yellow())),
        ]));
    }
    // Centred top to bottom in what is left.
    let top = height.saturating_sub(lines.len()) / 2;
    let body = Rect {
        y: inner.y + top as u16,
        height: inner.height - top as u16,
        ..inner
    };
    f.render_widget(Paragraph::new(lines), body);
}
