//! The header and the footer: counts, status, and key hints that shed
//! themselves to fit.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::state::Aggregate;
use crate::theme::{blue, green, orange, red, surface, text, text_dim, text_muted, yellow};
use crate::tui::app::{App, LEADER_KEYS, Mode, SetupHint, Status, StatusKind};
use crate::worktree::GhAccount;

use super::{text_width, truncate, truncate_line, wrap_text};

/// The most rows an error may take from the header before it is cut, with
/// `m` offered for the rest.
const MAX_ERROR_ROWS: usize = 3;

/// On a screen shorter than this an error gets two rows, not three: at
/// 80×24 a third row is a row of the list.
const SHORT_SCREEN: u16 = 30;

const SHORT_SCREEN_ERROR_ROWS: usize = 2;

/// How many rows an error may wrap to on a screen this tall.
pub(super) fn max_error_rows(height: u16) -> usize {
    if height < SHORT_SCREEN {
        SHORT_SCREEN_ERROR_ROWS
    } else {
        MAX_ERROR_ROWS
    }
}

/// The mark a status message carries, and its colour: an error must never
/// read like a success at a glance.
pub(super) fn status_mark(status: &Status) -> (&'static str, ratatui::style::Color) {
    match status.kind {
        StatusKind::Success => ("✓ ", green()),
        StatusKind::Error => ("✗ ", red()),
        StatusKind::Info => ("› ", blue()),
        // The spinner is already the message's first character.
        StatusKind::Progress => ("", yellow()),
    }
}

/// The header's rows as the flash wraps them: one for anything but an
/// error that does not fit, up to `max_rows` for that one.
pub(super) fn flash_rows(status: &Status, width: usize, max_rows: usize) -> Vec<String> {
    let budget = width.saturating_sub(4).max(1);
    if !status.is_error() || text_width(&status.message) <= budget {
        return vec![truncate(&status.message, budget)];
    }
    let max_rows = max_rows.max(1);
    let mut rows = wrap_text(&status.message, budget);
    if rows.len() > max_rows {
        rows.truncate(max_rows);
        // The pointer to `m` is what survives: the row gives way to it,
        // not the other way round.
        const MORE: &str = " … m shows it all";
        let last = rows.pop().unwrap_or_default();
        let room = budget.saturating_sub(text_width(MORE));
        let kept = &last[..super::fitting_prefix(&last, room)];
        rows.push(truncate(
            &format!("{} {}", kept.trim_end(), MORE.trim_start()),
            budget,
        ));
    }
    rows
}

/// Below this a screen gives no row to the setup line: the list needs
/// what there is.
const SETUP_LINE_MIN_HEIGHT: u16 = 6;

/// How many rows the header needs: one, unless an error has to wrap, and
/// one more for the setup line when there is one to say. Never more than
/// a third of the screen, save the setup line on a screen of three rows.
pub(super) fn header_height(app: &App, width: u16, height: u16) -> u16 {
    let setup = u16::from(app.setup_row.hint().is_some() && height >= SETUP_LINE_MIN_HEIGHT);
    let max_rows = max_error_rows(height).min((height / 3).max(1) as usize);
    let rows = app
        .flash()
        .map(|status| flash_rows(status, width as usize, max_rows).len())
        .unwrap_or(1) as u16;
    rows.max(1) + setup
}

/// The header's last row: where the project's setup stands and the key
/// that moves it on, until a test has passed.
pub(super) fn setup_hint_line(app: &App, width: usize) -> Option<Line<'static>> {
    let hint = app.setup_row.hint()?;
    let mut spans = vec![Span::styled(" setup: ", Style::new().fg(text_muted()))];
    let (lead, lead_color, said) = match hint {
        SetupHint::Testing(Some(line)) => (
            app.spinner(),
            yellow(),
            format!("testing the settings · {line}"),
        ),
        SetupHint::Testing(None) => (app.spinner(), yellow(), "testing the settings…".into()),
        SetupHint::Starting => (app.spinner(), yellow(), "starting the test…".into()),
        SetupHint::Note { text, failed: true } => ("✗", red(), text),
        SetupHint::Note { text, .. } => ("", yellow(), text),
    };
    if !lead.is_empty() {
        spans.push(Span::styled(
            format!("{lead} "),
            Style::new().fg(lead_color),
        ));
    }
    // What stands in the first part, the key that acts on it after the
    // last ` · `, its letter as every key is shown.
    let (what, key_part) = match said.rsplit_once(" · ") {
        Some((what, key_part)) if key_part.len() > 2 && key_part.as_bytes()[1] == b' ' => {
            (what.to_string(), Some(key_part.to_string()))
        }
        _ => (said, None),
    };
    spans.push(Span::styled(what, Style::new().fg(lead_color)));
    if let Some(key_part) = key_part {
        let (key, rest) = key_part.split_at(1);
        spans.push(Span::styled(" · ", Style::new().fg(text_muted())));
        spans.push(Span::styled(
            key.to_string(),
            Style::new().fg(orange()).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(rest.to_string(), Style::new().fg(text_dim())));
    }
    Some(truncate_line(Line::from(spans), width))
}

pub(super) fn render_header(f: &mut Frame, area: Rect, app: &App) {
    // The setup line keeps the header's last row whatever else it says.
    let area = match setup_hint_line(app, area.width as usize) {
        // As `header_height` counted it.
        Some(line) if f.area().height >= SETUP_LINE_MIN_HEIGHT && area.height >= 2 => {
            let [top, bottom] =
                Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
            f.render_widget(
                Paragraph::new(line).style(Style::new().bg(surface())),
                bottom,
            );
            top
        }
        _ => area,
    };
    // A message takes over the whole bar for its few seconds; the footer
    // keeps its key hints throughout.
    if let Some(status) = app.flash() {
        let (mark, color) = status_mark(status);
        let message_style = if status.is_error() {
            Style::new().fg(red())
        } else {
            Style::new().fg(text())
        };
        // The area is already as tall as `header_height` allowed.
        let lines: Vec<Line> = flash_rows(status, area.width as usize, area.height as usize)
            .into_iter()
            .enumerate()
            .map(|(i, row)| {
                let lead = if i == 0 { mark } else { "  " };
                Line::from(vec![
                    Span::raw(" "),
                    Span::styled(lead, Style::new().fg(color).add_modifier(Modifier::BOLD)),
                    Span::styled(row, message_style),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).style(Style::new().bg(surface())),
            area,
        );
        return;
    }

    let project = &app.paths.project.display_name;
    let branch = app
        .main
        .as_ref()
        .and_then(|m| m.branch.clone())
        .unwrap_or_else(|| "(detached)".to_string());
    // Worktrees, which the main checkout is not; what runs counts it.
    let count = app.linked_count();
    let (mut running, mut failed) = (0, 0);
    for wt in &app.worktrees {
        match app.phase_of(&wt.name) {
            Some(Aggregate::Failed { .. }) => failed += 1,
            Some(_) => running += 1,
            None => {}
        }
    }
    let mut spans = vec![Span::styled(
        " pando ",
        Style::new().fg(blue()).add_modifier(Modifier::BOLD),
    )];
    spans.extend(build_label_spans(crate::version::label()));
    spans.extend([
        Span::styled(project.clone(), Style::new().fg(text())),
        Span::styled(" · ", Style::new().fg(text_muted())),
        Span::styled(branch, Style::new().fg(blue())),
    ]);
    // Right after the branch, so a narrow header sheds it last: which
    // GitHub account this project's pushes and pull requests go out as.
    spans.extend(gh_account_spans(app.gh_account.as_ref()));
    spans.extend([
        Span::styled(" · ", Style::new().fg(text_muted())),
        Span::styled(
            format!("{count} worktree{}", if count == 1 { "" } else { "s" }),
            Style::new().fg(text_dim()),
        ),
    ]);
    // The list's own glyphs, so the header and the rows speak one
    // language: the glyph for the colour, the word for whoever cannot tell
    // the colours apart.
    if running > 0 {
        spans.push(Span::styled(
            format!("  ● {running} running"),
            Style::new().fg(green()),
        ));
    }
    if failed > 0 {
        spans.push(Span::styled(
            format!("  ✗ {failed} failed"),
            Style::new().fg(red()).add_modifier(Modifier::BOLD),
        ));
    }
    // One chip per shared service, so a dev server that will not connect
    // says whose fault it is before the developer reads a stack trace —
    // under a label that says what the dots are: the project's own
    // services, probed at their default ports. Shed first: on a narrow
    // pane the project and the branch matter more.
    if !app.service_health.shared.is_empty() {
        spans.push(Span::styled(" · shared:", Style::new().fg(text_muted())));
        for service in &app.service_health.shared {
            spans.push(Span::styled(
                format!(" {} ", service.name),
                Style::new().fg(text_dim()),
            ));
            // Said in a word as well: a dot that differs only in colour
            // says nothing to somebody who cannot tell the two apart.
            let (word, color) = if service.up {
                ("● up", green())
            } else {
                ("● down", red())
            };
            spans.push(Span::styled(word, Style::new().fg(color)));
        }
    }
    if app.enriching > 0 {
        spans.push(Span::styled(
            " · reading git",
            Style::new().fg(text_muted()),
        ));
    }
    f.render_widget(
        Paragraph::new(truncate_line(Line::from(spans), area.width as usize))
            .style(Style::new().bg(surface())),
        area,
    );
}

/// The header's account chip: ` · gh @login`, or why there is none.
///
/// Asked of `gh` from the project's directory, so a `gh` that picks its
/// account per directory shows the one this project gets, which is the one
/// worth checking before a push goes out as somebody else.
/// A development build's branch, right after the name, so a pando tried
/// on a real project never passes for the released one. Nothing for a
/// release.
pub(super) fn build_label_spans(label: Option<&str>) -> Vec<Span<'static>> {
    match label {
        Some(label) => vec![
            Span::styled(
                format!("⎇ {label}"),
                Style::new().fg(yellow()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::new().fg(text_muted())),
        ],
        None => Vec::new(),
    }
}

pub(super) fn gh_account_spans(account: Option<&GhAccount>) -> Vec<Span<'static>> {
    let dot = Span::styled(" · ", Style::new().fg(text_muted()));
    let label = Span::styled("gh ", Style::new().fg(text_muted()));
    let (text_of, style) = match account {
        None => ("…".to_string(), Style::new().fg(text_muted())),
        Some(GhAccount::Login(login)) => (
            format!("@{login}"),
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ),
        Some(GhAccount::SignedOut) => ("not signed in".to_string(), Style::new().fg(orange())),
        Some(GhAccount::Missing) => ("not installed".to_string(), Style::new().fg(text_dim())),
        // The reason is a stderr line and can be long; the chip says only
        // that `gh` could not tell, and `R` asks again.
        Some(GhAccount::Unknown(_)) => ("unknown".to_string(), Style::new().fg(text_dim())),
    };
    vec![dot, label, Span::styled(text_of, style)]
}

/// Key hints for a selected worktree that runs, most valuable first. The
/// essential ones are never dropped.
pub(super) const RUNNING_HINTS: [(&str, &str, bool); 16] = [
    ("j/k", "move", true),
    ("l", "logs", true),
    ("x", "stop", true),
    ("⏎", "mode", false),
    ("r", "restart", false),
    ("space g", "git", false),
    ("o", "open", false),
    ("c", "copy url", false),
    ("t", "share", false),
    ("tab", "process", false),
    ("!", "shell", false),
    ("e", "edit", false),
    ("n", "new", false),
    ("/", "filter", false),
    ("?", "help", true),
    ("q", "quit", true),
];

/// And for one that is stopped: what starts it, in each mode.
pub(super) const STOPPED_HINTS: [(&str, &str, bool); 12] = [
    ("j/k", "move", true),
    ("⏎", "start", true),
    ("space g", "git", false),
    ("i", "isolated", false),
    ("l", "logs", false),
    ("!", "shell", false),
    ("e", "edit", false),
    ("n", "new", false),
    ("d", "remove", false),
    ("/", "filter", false),
    ("?", "help", true),
    ("q", "quit", true),
];

/// For a stopped worktree of a project that has nothing to run: no start
/// key is offered, because none would start anything.
pub(super) const NOTHING_TO_RUN_HINTS: [(&str, &str, bool); 10] = [
    ("j/k", "move", true),
    ("space g", "git", false),
    ("l", "logs", false),
    ("!", "shell", false),
    ("e", "edit", false),
    ("n", "new", false),
    ("d", "remove", false),
    ("/", "filter", false),
    ("?", "help", true),
    ("q", "quit", true),
];

/// With nothing to select, only what gets something onto the list.
pub(super) const EMPTY_HINTS: [(&str, &str, bool); 3] = [
    ("n", "new worktree", true),
    ("?", "help", true),
    ("q", "quit", true),
];

pub(super) fn render_footer(f: &mut Frame, area: Rect, app: &App) {
    let width = area.width as usize;
    let selected = app.selected_worktree().map(|w| w.name.clone());
    // A worker blocked on the question dialog owns the keyboard: the
    // selected row's keys would only be answered once it closes, so the
    // footer says what is being waited for instead of offering them.
    if app.awaiting_answer() {
        let label = app
            .pending
            .as_ref()
            .map(|p| p.label.clone())
            .unwrap_or_default();
        let line = Line::from(vec![
            Span::styled(
                format!(" ? {label} is waiting for your answer"),
                Style::new().fg(yellow()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · answer in the dialog", Style::new().fg(text_muted())),
        ]);
        f.render_widget(
            Paragraph::new(truncate_line(line, width)).style(Style::new().bg(surface())),
            area,
        );
        return;
    }
    // After the leader, what may follow it, as neovim's which-key would.
    if app.leader {
        let mut hints: Vec<(&str, &str, bool)> = LEADER_KEYS
            .iter()
            .map(|k| (k.keys, k.action, true))
            .collect();
        hints.push(("esc", "cancel", true));
        let mut line = hint_line(&hints, width.saturating_sub(8));
        line.spans
            .insert(0, Span::styled(" space ›", Style::new().fg(blue())));
        f.render_widget(
            Paragraph::new(truncate_line(line, width)).style(Style::new().bg(surface())),
            area,
        );
        return;
    }
    let line = match app.mode {
        Mode::Filter => hint_line(
            &[
                ("↑↓", "move", true),
                ("⏎", "keep", true),
                ("esc", "clear", true),
            ],
            width,
        ),
        Mode::Normal => match selected {
            None => hint_line(&EMPTY_HINTS, width),
            // A shared worktree's public URL is what gets handed out, so
            // its keys come right after the one that shared it.
            Some(name) if app.public_url_of(&name).is_some() => {
                let mut hints = RUNNING_HINTS.to_vec();
                insert_after(
                    &mut hints,
                    "t",
                    &[("O", "public", false), ("C", "copy public", false)],
                );
                hint_line(&with_pr_hint(app, hints), width)
            }
            Some(name) if app.phase_of(&name).is_some() => {
                hint_line(&with_pr_hint(app, RUNNING_HINTS.to_vec()), width)
            }
            // Said first, in the footer's own row: what `⏎` would have
            // done, and the pane beside it says where to fix it.
            Some(_) if app.nothing_to_run => {
                let mut line = hint_line(
                    &with_pr_hint(app, NOTHING_TO_RUN_HINTS.to_vec()),
                    width.saturating_sub(17),
                );
                line.spans.insert(
                    0,
                    Span::styled(" nothing to run ·", Style::new().fg(yellow())),
                );
                truncate_line(line, width)
            }
            Some(_) => hint_line(&with_pr_hint(app, STOPPED_HINTS.to_vec()), width),
        },
    };
    f.render_widget(Paragraph::new(line).style(Style::new().bg(surface())), area);
}

/// The key that opens the selected row's pull request, beside git's, on a
/// row that has one: on any other `space p` only says there is none.
pub(super) const PR_HINT: (&str, &str, bool) = ("space p", "open PR", false);

fn with_pr_hint<'a>(
    app: &App,
    mut hints: Vec<(&'a str, &'a str, bool)>,
) -> Vec<(&'a str, &'a str, bool)> {
    let openable = app
        .selected_worktree()
        .and_then(|wt| app.pr_for(wt))
        .is_some_and(|pr| !pr.url.is_empty());
    if openable {
        insert_after(&mut hints, "space g", &[PR_HINT]);
    }
    hints
}

/// Puts `extra` right after the hint for `key`, or first without one.
fn insert_after<'a>(
    hints: &mut Vec<(&'a str, &'a str, bool)>,
    key: &str,
    extra: &[(&'a str, &'a str, bool)],
) {
    let at = hints
        .iter()
        .position(|(k, ..)| *k == key)
        .map_or(0, |i| i + 1);
    hints.splice(at..at, extra.iter().copied());
}

pub(super) fn hint_line(hints: &[(&str, &str, bool)], width: usize) -> Line<'static> {
    const SEPARATOR: &str = " · ";
    let items: Vec<(usize, bool)> = hints
        .iter()
        .map(|(key, label, essential)| (text_width(key) + 1 + text_width(label), *essential))
        .collect();
    let keep = keep_hints(&items, text_width(SEPARATOR), width.saturating_sub(1));

    let mut spans = vec![Span::raw(" ")];
    let mut first = true;
    for ((key, label, _), keep) in hints.iter().zip(&keep) {
        if !keep {
            continue;
        }
        if !first {
            spans.push(Span::styled(SEPARATOR, Style::new().fg(text_muted())));
        }
        first = false;
        spans.push(Span::styled(
            (*key).to_string(),
            Style::new().fg(orange()).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::new().fg(text_muted()),
        ));
    }
    Line::from(spans)
}

/// Width-adaptive hint selection: keep the essential hints, drop
/// lower-priority ones from the tail until the line fits. Without this the
/// rightmost hints fall off the edge with no ellipsis.
pub fn keep_hints(items: &[(usize, bool)], separator: usize, width: usize) -> Vec<bool> {
    let mut keep = vec![true; items.len()];
    while kept_width(items, separator, &keep) > width {
        match (0..items.len()).rev().find(|&i| keep[i] && !items[i].1) {
            Some(i) => keep[i] = false,
            // Only essentials left: let it clip rather than vanish.
            None => break,
        }
    }
    keep
}

fn kept_width(items: &[(usize, bool)], separator: usize, keep: &[bool]) -> usize {
    let kept = keep.iter().filter(|&&k| k).count();
    let widths: usize = items
        .iter()
        .zip(keep)
        .filter(|&(_, &k)| k)
        .map(|(&(w, _), _)| w)
        .sum();
    widths + separator * kept.saturating_sub(1)
}
