//! Overlays: create, pull requests, remove, share, unshare, switching
//! mode, questions, help, messages. Pure
//! painting, like `render`.
//!
//! Every popup is sized to what it holds, with a margin inside its border,
//! and clamped to the screen: a two-line confirmation is a small box, and a
//! question with long options grows to fit them before it ever cuts one.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};

use super::app::{
    App, BY_HAND_KEY, BranchLoadState, CreateRow, GitStage, INSPECT_LEGEND, KeyHelp, LIST_KEYS,
    LIST_LEGEND, LOG_KEYS, Modal, RemoveBlocker, SETUP_KEYS, SETUP_LEGEND, SPINNER_FRAMES,
    StatusKind, compact_age, create_rows, pr_rows,
};
use super::render::{centered_box, chunk_cells, text_width, truncate, truncate_middle, wrap_text};
use crate::actions;
use crate::actions::git::{GitAction, GitRead, Ran};
use crate::state::ServiceMode;
use crate::theme::{
    AppearanceOrigin, blue, border, cyan, green, highlight_bg, magenta, namespaced, orange, red,
    surface, text, text_dim, text_muted, yellow,
};
use crate::worktree::{BranchSource, PrState};

/// Popups never shrink below this; a narrow tmux split gets a readable box
/// rather than a sliver.
const MIN_POPUP_WIDTH: u16 = 34;

/// The footer of help and messages. `j` and `k` scroll rather than close,
/// so "any key closes" was not true; these are the keys that do.
pub(super) const CLOSE_HINT: &str = "esc/q/? close";

pub(super) const SCROLL_CLOSE_HINT: &str = "j/k g/G scroll · esc/q/? close";

/// Branch rows the create picker shows at once. Fixed, so the box does not
/// jump about as typing narrows the list.
const CREATE_LIST_ROWS: usize = 8;

/// The widest a popup's content may be on this screen: nine tenths of it,
/// less the border and the margin.
fn max_content_width(area: Rect) -> usize {
    ((area.width as usize * 9) / 10).saturating_sub(6).max(20)
}

/// Paints `modal`. Returns, for help and messages, the furthest they can
/// scroll on this screen, which the scroll keys clamp to.
pub fn render_modal(f: &mut Frame, area: Rect, modal: &Modal, app: &App) -> Option<usize> {
    match modal {
        Modal::Create {
            input,
            branches,
            selected,
            base,
        } => render_create(f, area, input, branches, *selected, base.as_deref(), app),
        Modal::PullRequests { input, selected } => {
            render_pull_requests(f, area, input, *selected, app)
        }
        Modal::Remove { name, .. } => {
            render_remove(f, area, &app.label_of(name), &app.remove_blockers(name))
        }
        Modal::Unshare { name, url } => render_unshare(f, area, &app.label_of(name), url),
        Modal::StopAll { names } => render_stop_all(f, area, names, app),
        Modal::Share { name } => {
            render_share(f, area, &app.label_of(name), app.url_of(name).as_deref())
        }
        Modal::Mode { name, selected } => render_mode_chooser(f, area, app, name, *selected),
        Modal::SwitchMode { name, to } => {
            let processes: Vec<String> =
                app.processes_of(name).into_iter().map(|(p, _)| p).collect();
            let from = app.record_for(name).map(|r| r.mode()).unwrap_or_default();
            render_switch_mode(f, area, &app.label_of(name), &processes, from, *to)
        }
        Modal::Question {
            question,
            selected,
            custom,
            ..
        } => render_question(
            f,
            area,
            question,
            *selected,
            custom.as_deref(),
            &app.question_checked,
        ),
        Modal::Help => {
            let (keys, legend_title, legend): (&[KeyHelp], &str, &[(&str, &str)]) =
                if app.setup_screen.is_some() {
                    (SETUP_KEYS, "on the setup screen", SETUP_LEGEND)
                } else if app.log_view().is_some() {
                    (LOG_KEYS, "in the inspect overlay", INSPECT_LEGEND)
                } else {
                    (LIST_KEYS, "in the list", LIST_LEGEND)
                };
            return Some(render_help(
                f,
                area,
                app.help_scroll,
                keys,
                legend_title,
                legend,
            ));
        }
        Modal::Messages => return Some(render_messages(f, area, app)),
        Modal::Git { name, stage } => render_git(f, area, app, name, stage),
        Modal::Theme { selected, .. } => render_theme_picker(f, area, app, *selected),
    }
    None
}

/// Draws a popup whose content is `width` × `height`, with a margin inside
/// the border when the screen can afford one. Returns the content area.
fn popup(
    f: &mut Frame,
    area: Rect,
    title: &str,
    footer: Option<&str>,
    width: usize,
    height: usize,
) -> Option<Rect> {
    let (width, height) = (width as u16, height as u16);
    // Two cells either side and a row above and below, then one and none,
    // then nothing: a margin is the first thing a small screen gives up.
    let (px, py) = if area.width >= width + 6 + 2 && area.height >= height + 4 + 2 {
        (2, 1)
    } else if area.width >= width + 4 {
        (1, 0)
    } else {
        (0, 0)
    };
    let rect = centered_box(
        (width + 2 + 2 * px).max(MIN_POPUP_WIDTH).min(area.width),
        height + 2 + 2 * py,
        area,
    );
    if rect.width < 3 || rect.height < 3 {
        return None;
    }
    let mut block = Block::bordered()
        .title(Span::styled(
            truncate(&format!(" {title} "), rect.width.saturating_sub(2) as usize),
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ))
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border()))
        .style(Style::new().bg(surface()))
        .padding(Padding::new(px, px, py, py));
    if let Some(footer) = footer {
        block = block.title_bottom(Span::styled(
            truncate(
                &format!(" {footer} "),
                rect.width.saturating_sub(2) as usize,
            ),
            Style::new().fg(text_muted()),
        ));
    }
    let inner = block.inner(rect);
    f.render_widget(Clear, rect);
    f.render_widget(block, rect);
    (inner.width > 0 && inner.height > 0).then_some(inner)
}

/// The width of the widest line.
fn widest(lines: &[Line]) -> usize {
    lines
        .iter()
        .map(|line| line.spans.iter().map(|s| text_width(&s.content)).sum())
        .max()
        .unwrap_or(0)
}

fn key_span(key: &str) -> Span<'static> {
    Span::styled(
        key.to_string(),
        Style::new().fg(orange()).add_modifier(Modifier::BOLD),
    )
}

fn hint_span(label: &str) -> Span<'static> {
    Span::styled(label.to_string(), Style::new().fg(text_muted()))
}

/// A login as it is typed, with the password after its first `:` as dots:
/// the user is worth seeing, the password is nobody's to read off a
/// screen.
fn masked(typed: &str) -> String {
    match typed.split_once(':') {
        Some((user, password)) => format!("{user}:{}", "•".repeat(password.chars().count())),
        None => typed.to_string(),
    }
}

/// A question, its options with the signal that found each one, and a line
/// for a command typed by hand. Every slot accepts one, so there is never a
/// dead end.
///
/// Nothing in it is cut: the prompt and the report wrap, and each option's
/// value is shown whole — it is the command being chosen — with the reason
/// for it on the row below, dimmed. A question too tall for the screen
/// scrolls to keep the selected option in view.
fn render_question(
    f: &mut Frame,
    area: Rect,
    question: &crate::actions::Question,
    selected: usize,
    custom: Option<&str>,
    checked: &[usize],
) {
    // "type the variable names" for ports, "a shell line" for the prelude:
    // what a typed answer is, as the CLI's prompt says it.
    let type_it = format!("type the {}", question.slot.custom_noun());
    // A question only a person may answer is one whose answer destroys
    // data: the slot to free. Its action says so, in the destructive
    // colour, and there is nothing to type in place of a choice.
    let destroys = question.slot.takes_a_person();
    let footer: Vec<(&str, &str)> = match custom {
        Some(_) => vec![("⏎", "accept"), ("esc", "back")],
        None if destroys => vec![
            ("⏎", "empty its slot and free it"),
            ("n", "free none"),
            ("esc", "cancel"),
        ],
        None if question.multi => vec![
            ("space", "toggle"),
            ("⏎", "accept"),
            ("n", "none"),
            ("esc", "cancel"),
        ],
        None if question.allow_none => vec![
            ("⏎", "choose"),
            ("c", type_it.as_str()),
            ("n", "none"),
            ("esc", "cancel"),
        ],
        None => vec![("⏎", "choose"), ("c", type_it.as_str()), ("esc", "cancel")],
    };
    let footer_line = Line::from(
        footer
            .iter()
            .enumerate()
            .flat_map(|(i, (key, label))| {
                let gap = if i == 0 { "" } else { "   " };
                let label = match destroys && i == 0 && custom.is_none() {
                    true => Span::styled(format!(" {label}"), Style::new().fg(red())),
                    false => hint_span(&format!(" {label}")),
                };
                [hint_span(gap), key_span(key), label]
            })
            .collect::<Vec<_>>(),
    );

    // The natural width: whatever the longest unbroken thing needs, capped
    // at what the screen has.
    let cap = max_content_width(area);
    let natural = std::iter::once(text_width(&question.prompt))
        .chain(question.details.iter().map(|d| text_width(d)))
        .chain(
            question
                .options
                .iter()
                .flat_map(|(value, why)| [text_width(value) + 6, text_width(why) + 6]),
        )
        .chain(std::iter::once(widest(std::slice::from_ref(&footer_line))))
        .max()
        .unwrap_or(0);
    let width = natural.clamp(40, cap.max(40)).min(cap);

    let mut body: Vec<Line> = Vec::new();
    for row in wrap_text(&question.prompt, width) {
        body.push(Line::styled(
            row,
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ));
    }
    for detail in &question.details {
        for row in wrap_text(detail, width) {
            body.push(Line::styled(row, Style::new().fg(text_muted())));
        }
    }
    body.push(Line::raw(""));

    // Where each option's rows start and end, to scroll the selected one
    // into view.
    let mut spans_of: Vec<(usize, usize)> = Vec::new();
    for (i, (value, why)) in question.options.iter().enumerate() {
        let cursor = i == selected && custom.is_none();
        let marker = match (question.multi, cursor) {
            // A set question shows what it would take as well as where the
            // cursor is; one marker cannot say both.
            (true, cursor) => {
                let box_ = if checked.contains(&i) { "[x]" } else { "[ ]" };
                format!("{} {box_} ", if cursor { "▸" } else { " " })
            }
            (false, true) => "▸ ".to_string(),
            (false, false) => "  ".to_string(),
        };
        let indent = text_width(&marker);
        let start = body.len();
        let value_style = if cursor {
            Style::new()
                .fg(text())
                .bg(highlight_bg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(text())
        };
        for (row_at, row) in wrap_text(value, width.saturating_sub(indent))
            .into_iter()
            .enumerate()
        {
            let lead = if row_at == 0 {
                marker.clone()
            } else {
                " ".repeat(indent)
            };
            body.push(Line::from(vec![
                Span::styled(lead, Style::new().fg(orange())),
                Span::styled(row, value_style),
            ]));
        }
        if !why.is_empty() {
            for row in wrap_text(why, width.saturating_sub(indent + 2)) {
                body.push(Line::from(vec![
                    Span::raw(" ".repeat(indent + 2)),
                    Span::styled(row, Style::new().fg(text_muted())),
                ]));
            }
        }
        spans_of.push((start, body.len()));
    }
    if question.options.is_empty() {
        body.push(Line::styled(
            "pando found nothing to suggest here",
            Style::new().fg(text_muted()),
        ));
    }
    if let Some(typed) = custom {
        body.push(Line::raw(""));
        let shown = match question.slot.is_secret() {
            true => masked(typed),
            false => typed.to_string(),
        };
        for (row_at, row) in wrap_text(&format!("{shown}▏"), width.saturating_sub(2))
            .into_iter()
            .enumerate()
        {
            let lead = if row_at == 0 { "> " } else { "  " };
            body.push(Line::from(vec![
                Span::styled(lead, Style::new().fg(orange())),
                Span::styled(row, Style::new().fg(text())),
            ]));
        }
        spans_of.clear();
        spans_of.push((body.len().saturating_sub(1), body.len()));
    }

    // The body scrolls; the key line under it does not.
    let height = body.len() + 2;
    let title = match destroys {
        true => "free a slot",
        false => "pando needs an answer",
    };
    let Some(inner) = popup(f, area, title, None, width, height) else {
        return;
    };
    let [body_area, footer_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(2)]).areas(inner);
    let visible = body_area.height as usize;
    let target = if custom.is_some() {
        spans_of.first().copied()
    } else {
        spans_of.get(selected).copied()
    };
    let offset = match target {
        Some((_, end)) if end > visible => end - visible,
        _ => 0,
    };
    let offset = match target {
        Some((start, _)) if start < offset => start,
        _ => offset,
    };
    let more = body.len().saturating_sub(offset + visible);
    f.render_widget(
        Paragraph::new(body.clone()).scroll((offset as u16, 0)),
        body_area,
    );
    let mut footer_lines = vec![Line::raw("")];
    let mut footer_line = footer_line;
    if more > 0 || offset > 0 {
        footer_line.spans.push(hint_span("   ↑↓ more"));
    }
    footer_lines.push(footer_line);
    f.render_widget(Paragraph::new(footer_lines), footer_area);
}

fn render_create(
    f: &mut Frame,
    area: Rect,
    input: &str,
    branches: &BranchLoadState,
    selected: usize,
    base: Option<&str>,
    app: &App,
) {
    let width = 60.min(max_content_width(area));
    // The prompt, the base, a gap, the list, a gap, the keys.
    let height = 3 + CREATE_LIST_ROWS + 2;
    let Some(inner) = popup(f, area, "new worktree", None, width, height) else {
        return;
    };
    let [prompt, hint, _, list_area, _, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let width = inner.width as usize;

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("branch ", Style::new().fg(text_muted())),
            Span::styled(
                truncate_middle(input, width.saturating_sub(8)),
                Style::new().fg(text()).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▏", Style::new().fg(orange())),
        ])),
        prompt,
    );

    // The base is shown, and tab walks it: the one the typed name forks
    // from untouched — the config's, or the repository's default — then
    // every branch the picker read.
    let chosen = base.is_some();
    let implied = app.implied_base(input);
    let base = base.or(implied.as_deref()).unwrap_or("the default base");
    let base_line = Line::from(vec![
        Span::styled("new branches fork from ", Style::new().fg(text_muted())),
        Span::styled(
            base.to_string(),
            Style::new()
                .fg(if chosen { orange() } else { text_dim() })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  tab changes it", Style::new().fg(text_muted())),
    ]);
    f.render_widget(
        Paragraph::new(super::render::truncate_line(base_line, width)),
        hint,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            key_span("⏎"),
            hint_span(" create   "),
            key_span("↑↓"),
            hint_span(" choose   "),
            key_span("tab"),
            hint_span(" base   "),
            key_span("esc"),
            hint_span(" cancel"),
        ])),
        keys,
    );

    if branches.is_loading() && input.trim().is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                truncate("reading branches…", width),
                Style::new().fg(text_muted()),
            )),
            list_area,
        );
        return;
    }
    let rows = create_rows(input, branches.as_slice());
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::styled(
                truncate("type a name for the new branch", width),
                Style::new().fg(text_muted()),
            )),
            list_area,
        );
        return;
    }

    let visible = list_area.height as usize;
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, row)| {
            let marker = if i == selected { "▸ " } else { "  " };
            let label = match row {
                CreateRow::NewBranch(name) => name.clone(),
                CreateRow::Existing(entry) => entry.name.clone(),
            };
            // A branch that already has a worktree says so before enter
            // is pressed, and enter goes to it rather than failing.
            let (tag, color) = if app.is_main_branch(&label) {
                ("checked out in the main checkout", text_muted())
            } else if app.worktree_for_branch(&label).is_some() {
                ("has a worktree · ⏎ selects it", orange())
            } else {
                match row {
                    CreateRow::NewBranch(_) => ("new branch", green()),
                    CreateRow::Existing(entry) => match entry.source {
                        BranchSource::Local => ("local", text_dim()),
                        BranchSource::Remote => ("remote", text_dim()),
                    },
                }
            };
            let tag_width = text_width(tag) + 2;
            let label_width = width.saturating_sub(text_width(marker) + tag_width);
            let shown = truncate_middle(&label, label_width);
            let gap = width
                .saturating_sub(text_width(marker) + text_width(&shown) + text_width(tag))
                .max(1);
            // Not a choice: git keeps one checkout per branch.
            let label_style = if app.is_main_branch(&label) {
                Style::new().fg(text_muted())
            } else {
                Style::new().fg(text())
            };
            let mut line = vec![
                Span::styled(marker, Style::new().fg(orange())),
                Span::styled(shown, label_style),
                Span::raw(" ".repeat(gap)),
                Span::styled(tag.to_string(), Style::new().fg(color)),
            ];
            if i == selected {
                line = line
                    .into_iter()
                    .map(|s| Span::styled(s.content, s.style.bg(highlight_bg())))
                    .collect();
            }
            Line::from(line)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list_area);
}

/// The open pull requests, one a row: number, title, and what enter does
/// with it. The selected one's branch sits under the list, since that is
/// what the worktree will check out.
fn render_pull_requests(f: &mut Frame, area: Rect, input: &str, selected: usize, app: &App) {
    let width = 76.min(max_content_width(area));
    // The filter, a gap, the list, a gap, the branch, the keys.
    let height = 2 + CREATE_LIST_ROWS + 3;
    let Some(inner) = popup(f, area, "open pull requests", None, width, height) else {
        return;
    };
    let [prompt, _, list_area, _, branch_row, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let width = inner.width as usize;

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("filter ", Style::new().fg(text_muted())),
            Span::styled(
                truncate_middle(input, width.saturating_sub(8)),
                Style::new().fg(text()).add_modifier(Modifier::BOLD),
            ),
            Span::styled("▏", Style::new().fg(orange())),
        ])),
        prompt,
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            key_span("⏎"),
            hint_span(" worktree   "),
            key_span("↑↓"),
            hint_span(" choose   "),
            key_span("esc"),
            hint_span(" cancel"),
        ])),
        keys,
    );

    let rows = pr_rows(&app.pr_list, input);
    if rows.is_empty() {
        let (message, color) = if app.pr_fetching {
            ("asking GitHub…".to_string(), text_muted())
        } else if let Some(error) = app.pr_error.as_deref() {
            (error.to_string(), red())
        } else if app.pr_list.iter().any(|pr| pr.state == PrState::Open) {
            ("no open pull request matches".to_string(), text_muted())
        } else {
            ("no open pull requests".to_string(), text_muted())
        };
        let lines: Vec<Line> = wrap_text(&message, width)
            .into_iter()
            .take(list_area.height as usize)
            .map(|line| Line::styled(line, Style::new().fg(color)))
            .collect();
        f.render_widget(Paragraph::new(lines), list_area);
        return;
    }

    let selected = selected.min(rows.len() - 1);
    let visible = list_area.height as usize;
    let start = selected.saturating_sub(visible.saturating_sub(1));
    let number_width = rows
        .iter()
        .map(|pr| text_width(&format!("#{}", pr.number)))
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = rows
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, pr)| {
            let marker = if i == selected { "▸ " } else { "  " };
            let number = format!("{:<number_width$} ", format!("#{}", pr.number));
            let branch = pr.local_branch();
            let (tag, color) = if app.is_main_branch(&branch) {
                ("main checkout".to_string(), text_muted())
            } else if app.worktree_for_branch(&branch).is_some() {
                ("has a worktree".to_string(), orange())
            } else if pr.draft {
                (format!("draft · @{}", pr.author), text_dim())
            } else {
                (format!("@{}", pr.author), text_dim())
            };
            let fixed = text_width(marker) + text_width(&number) + text_width(&tag) + 2;
            let title = truncate(&pr.title, width.saturating_sub(fixed));
            let gap = width
                .saturating_sub(
                    text_width(marker)
                        + text_width(&number)
                        + text_width(&title)
                        + text_width(&tag),
                )
                .max(1);
            let mut line = vec![
                Span::styled(marker, Style::new().fg(orange())),
                Span::styled(number, Style::new().fg(green())),
                Span::styled(title, Style::new().fg(text())),
                Span::raw(" ".repeat(gap)),
                Span::styled(tag, Style::new().fg(color)),
            ];
            if i == selected {
                line = line
                    .into_iter()
                    .map(|s| Span::styled(s.content, s.style.bg(highlight_bg())))
                    .collect();
            }
            super::render::truncate_line(Line::from(line), width)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list_area);

    // What enter will do with the selected one, said before it is pressed.
    let pr = rows[selected];
    let branch = pr.local_branch();
    let what = if app.is_main_branch(&branch) {
        "checked out in the main checkout"
    } else if app.worktree_for_branch(&branch).is_some() {
        "⏎ selects its worktree"
    } else if pr.cross_repository {
        "from a fork · ⏎ fetches it into a worktree"
    } else {
        "⏎ makes a worktree for it"
    };
    let branch_line = Line::from(vec![
        Span::styled(
            truncate_middle(&branch, width / 2),
            Style::new().fg(cyan()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {what}"), Style::new().fg(text_muted())),
    ]);
    f.render_widget(
        Paragraph::new(super::render::truncate_line(branch_line, width)),
        branch_row,
    );
}

/// Confirming that a public URL goes away.
///
/// The URL is shown in full, wrapped if it has to be, because the thing
/// being taken away is exactly the thing somebody may have open in another
/// window.
fn render_unshare(f: &mut Frame, area: Rect, label: &str, url: &str) {
    let width = (text_width(url).max(40)).min(max_content_width(area));
    let mut lines = vec![Line::styled(
        truncate(&format!("stop sharing {label}?"), width),
        Style::new().fg(text()).add_modifier(Modifier::BOLD),
    )];
    for chunk in chunk_cells(url, width) {
        lines.push(Line::styled(chunk, Style::new().fg(cyan())));
    }
    lines.push(Line::styled(
        truncate("anyone with that link loses it at once", width),
        Style::new().fg(yellow()),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        key_span("y"),
        hint_span(" stop sharing   "),
        key_span("esc"),
        hint_span(" keep it"),
    ]));
    let width = widest(&lines).min(width);
    let Some(inner) = popup(f, area, "stop sharing", None, width, lines.len()) else {
        return;
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// Confirming `t`: the local address that is about to be reachable from
/// anywhere, before it is.
fn render_share(f: &mut Frame, area: Rect, label: &str, url: Option<&str>) {
    let width = (url.map(text_width).unwrap_or(0).max(40)).min(max_content_width(area));
    let mut lines = vec![Line::styled(
        truncate(&format!("share {label} publicly?"), width),
        Style::new().fg(text()).add_modifier(Modifier::BOLD),
    )];
    if let Some(url) = url {
        for chunk in chunk_cells(url, width) {
            lines.push(Line::styled(chunk, Style::new().fg(cyan())));
        }
    }
    lines.push(Line::styled(
        truncate("anyone with the public link reaches it — t stops it", width),
        Style::new().fg(yellow()),
    ));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        key_span("y"),
        hint_span(" share   "),
        key_span("esc"),
        hint_span(" cancel"),
    ]));
    let width = widest(&lines).min(width);
    let Some(inner) = popup(f, area, "share", None, width, lines.len()) else {
        return;
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// Confirming a mode key on a worktree running in another mode: which
/// services it leaves, and what restarts onto the others.
fn render_switch_mode(
    f: &mut Frame,
    area: Rect,
    label: &str,
    processes: &[String],
    from: ServiceMode,
    to: ServiceMode,
) {
    let cap = max_content_width(area);
    let how = to.word();
    let now = match from {
        ServiceMode::Shared => "it runs on the project's shared services now",
        ServiceMode::Namespaced => "it runs on namespaces of its own in the project's services now",
        ServiceMode::Isolated => "it runs private copies of the services now",
    };
    let then = match (from, to) {
        (ServiceMode::Isolated, ServiceMode::Shared) => {
            "they stop, and it moves to the project's own"
        }
        (_, ServiceMode::Shared) => {
            "it moves to the main checkout's data; its namespaces are kept until rm"
        }
        (ServiceMode::Isolated, ServiceMode::Namespaced) => {
            "they stop, and it moves to its own namespaces in the project's services"
        }
        (_, ServiceMode::Namespaced) => "it moves to namespaces of its own, kept until rm",
        (_, ServiceMode::Isolated) => "private copies of the services start for it",
    };
    let restarts = match processes.len() {
        0 => "its processes restart".to_string(),
        1 => format!("{} restarts", processes[0]),
        _ => format!("{} restart", processes.join(", ")),
    };
    let lines = vec![
        Line::styled(
            truncate(&format!("restart {label} {how}?"), cap),
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ),
        Line::styled(truncate(now, cap), Style::new().fg(text_dim())),
        Line::styled(truncate(then, cap), Style::new().fg(text_dim())),
        Line::styled(truncate(&restarts, cap), Style::new().fg(yellow())),
        Line::raw(""),
        Line::from(vec![
            key_span("y"),
            hint_span(&format!(" restart {how}   ")),
            key_span("esc"),
            hint_span(" keep it as it is"),
        ]),
    ];
    let width = widest(&lines).min(cap);
    let title = format!("restart {how}");
    let Some(inner) = popup(f, area, &title, None, width, lines.len()) else {
        return;
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// ⏎: shared, namespaced, isolated — one row each, in the colour each
/// mode is painted in wherever it is shown, with what it means and, beside
/// the mode it runs in or last ran in, `running` or `last used`.
fn render_mode_chooser(f: &mut Frame, area: Rect, app: &App, name: &str, selected: usize) {
    let cap = max_content_width(area);
    let record = app.record_for(name);
    let running = app.is_up(name);
    // A running worktree always runs in one, shared when its record never
    // said — a 0.3.0 start — as `choose_mode` reads it; a stopped one has
    // a last-used mode only once one was written down.
    let current = if running {
        record.map(|r| r.mode())
    } else {
        record.and_then(|r| r.mode)
    };
    let rows: Vec<(ServiceMode, &str, &str)> = ServiceMode::ALL
        .iter()
        .map(|mode| match mode {
            ServiceMode::Shared => (*mode, "shared", "the main checkout's servers and its data"),
            ServiceMode::Namespaced => (
                *mode,
                "namespaced (experimental)",
                "its own database and slot in the main checkout's servers",
            ),
            ServiceMode::Isolated => (*mode, "isolated", "servers of its own, on ports of its own"),
        })
        .collect();
    let word_width = rows
        .iter()
        .map(|(_, word, _)| text_width(word))
        .max()
        .unwrap_or(0);
    let mut lines: Vec<Line> = vec![
        Line::styled(
            truncate_middle(
                &format!(
                    "{} {} on which services?",
                    if running { "run" } else { "start" },
                    app.label_of(name)
                ),
                cap,
            ),
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ),
        Line::raw(""),
    ];
    for (i, (mode, word, what)) in rows.iter().enumerate() {
        let here = i == selected;
        let color = match mode {
            ServiceMode::Shared => text(),
            ServiceMode::Namespaced => namespaced(),
            ServiceMode::Isolated => magenta(),
        };
        let mut word_style = Style::new().fg(color);
        if here {
            word_style = word_style.add_modifier(Modifier::BOLD);
        }
        let (label, label_color) = match (current == Some(*mode), running) {
            (true, true) => ("  running", green()),
            (true, false) => ("  last used", text_dim()),
            (false, _) => ("", text_dim()),
        };
        let mut spans = vec![
            Span::styled(if here { "▸ " } else { "  " }, Style::new().fg(blue())),
            Span::styled(format!("{word:<word_width$}"), word_style),
            Span::styled(label, Style::new().fg(label_color)),
        ];
        let used: usize = spans.iter().map(|s| text_width(&s.content)).sum();
        spans.push(Span::styled(
            format!("  {}", truncate(what, cap.saturating_sub(used + 2))),
            Style::new().fg(text_muted()),
        ));
        lines.push(Line::from(spans));
    }
    lines.push(Line::raw(""));
    // On a running worktree the one it runs in changes nothing, and every
    // other one restarts every process: said on the key line, before it.
    let chosen = ServiceMode::ALL.get(selected).copied();
    let action = match (running, chosen == current) {
        (true, true) => " keeps it as it is   ",
        (true, false) => " switches it — every process restarts   ",
        (false, _) => " starts it   ",
    };
    lines.push(Line::from(vec![
        key_span("↑↓"),
        hint_span(" choose   "),
        key_span("⏎"),
        hint_span(action),
        key_span("esc"),
        hint_span(" cancel"),
    ]));
    let width = widest(&lines).min(cap);
    let Some(inner) = popup(f, area, "mode", None, width, lines.len()) else {
        return;
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// `T`: every theme with a swatch of its accents in their own colours,
/// so they can be compared without trying each, and the screen behind in
/// the one under the cursor.
fn render_theme_picker(f: &mut Frame, area: Rect, app: &App, selected: usize) {
    let themes = &app.theme.themes;
    let appearance = app.theme.appearance;
    let cap = max_content_width(area);
    let name_width = themes
        .iter()
        .map(|t| text_width(&t.name))
        .max()
        .unwrap_or(0);
    // The popup keeps the rest of the screen in view: that is where the
    // preview is.
    let rows = themes
        .len()
        .min((area.height as usize).saturating_sub(8).max(3));
    let first = selected.saturating_sub(rows.saturating_sub(1));
    let mut lines: Vec<Line> = themes
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, theme)| {
            let here = i == selected;
            let palette = theme.palette(appearance);
            let mut spans = vec![
                Span::styled(if here { "▸ " } else { "  " }, Style::new().fg(blue())),
                Span::styled(
                    format!("{:<name_width$}  ", theme.name),
                    if here {
                        Style::new().fg(text()).add_modifier(Modifier::BOLD)
                    } else {
                        Style::new().fg(text_dim())
                    },
                ),
            ];
            for color in [
                palette.red,
                palette.orange,
                palette.yellow,
                palette.green,
                palette.cyan,
                palette.blue,
                palette.magenta,
            ] {
                spans.push(Span::styled("●", Style::new().fg(color)));
            }
            let mut note = String::new();
            if theme.name == app.theme.name {
                note.push_str("  in use");
            }
            if matches!(theme.source, crate::theme::Source::File(_)) {
                note.push_str("  yours");
            }
            spans.push(Span::styled(note, Style::new().fg(green())));
            let used: usize = spans.iter().map(|s| text_width(&s.content)).sum();
            spans.push(Span::styled(
                format!(
                    "  {}",
                    truncate(&theme.description, cap.saturating_sub(used + 2))
                ),
                Style::new().fg(text_muted()),
            ));
            Line::from(spans)
        })
        .collect();
    lines.push(Line::raw(""));
    // Why this half, and the knob that would change it: only a system
    // choice points at the key that pins one.
    let half = appearance.word();
    let why = match app.theme.appearance_origin {
        AppearanceOrigin::System => {
            format!("{half} half, as the system is — [ui] appearance pins one")
        }
        AppearanceOrigin::Config => format!("{half} half, as [ui] appearance pins it"),
        AppearanceOrigin::Env => {
            format!("{half} half, as {} pins it", crate::theme::APPEARANCE_ENV)
        }
    };
    lines.push(Line::styled(
        truncate(&why, cap),
        Style::new().fg(text_muted()),
    ));
    lines.push(Line::from(vec![
        key_span("↑↓"),
        hint_span(" try it   "),
        key_span("⏎"),
        hint_span(" keep it   "),
        key_span("esc"),
        hint_span(" put back"),
    ]));
    let width = widest(&lines).min(cap);
    let Some(inner) = popup(f, area, "theme", None, width, lines.len()) else {
        return;
    };
    f.render_widget(Paragraph::new(lines), inner);
}

/// A dialog's keys, each with what it does, as many to a row as `width`
/// takes: one line cut at the border lost the key that cancels.
fn key_rows(keys: Vec<(Span<'static>, Span<'static>)>, width: usize) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<Span<'static>>> = Vec::new();
    let mut used = 0;
    for (key, what) in keys {
        let needs = text_width(&key.content) + text_width(&what.content);
        match rows.last_mut() {
            Some(row) if used + 3 + needs <= width => {
                row.extend([hint_span("   "), key, what]);
                used += 3 + needs;
            }
            _ => {
                rows.push(vec![key, what]);
                used = needs;
            }
        }
    }
    rows.into_iter()
        .map(|row| super::render::truncate_line(Line::from(row), width))
        .collect()
}

/// Paints `body` over `keys`, the keys on the box's last rows, with a
/// blank row between them when there is room for one. A body taller
/// than the box loses its last rows, and a `…` says so; the keys that
/// answer it are never the ones to go.
fn paint_over_keys(
    f: &mut Frame,
    inner: Rect,
    mut body: Vec<Line<'static>>,
    keys: Vec<Line<'static>>,
) {
    let keys_height = (keys.len() as u16).min(inner.height);
    let room = inner.height - keys_height;
    if body.len() < room as usize {
        body.push(Line::raw(""));
    } else if body.len() > room as usize && room > 0 {
        body.truncate(room as usize - 1);
        body.push(hint_span("…").into());
    }
    let body_area = Rect {
        height: room,
        ..inner
    };
    let keys_area = Rect {
        y: inner.y + room,
        height: keys_height,
        ..inner
    };
    f.render_widget(Paragraph::new(body), body_area);
    f.render_widget(Paragraph::new(keys), keys_area);
}

/// Confirming `X`: what is up, one row apiece, before any of it goes down.
fn render_stop_all(f: &mut Frame, area: Rect, names: &[String], app: &App) {
    let cap = max_content_width(area);
    // Past this many names the rest are counted, and sooner in a short
    // tmux split: the names get only the rows the rest leave them.
    const MAX_ROWS: usize = 12;
    // A running check is not one of the developer's worktrees: it is
    // counted apart, and named by `label_of` below.
    let check = names.iter().any(|name| crate::worktree::is_check(name));
    let worktrees = names.len() - usize::from(check);
    let asked = match (worktrees, check) {
        (0, true) => "stop the running pando check?".to_string(),
        (1, false) => "stop the one worktree that is up?".to_string(),
        (1, true) => "stop the one worktree that is up, and the pando check?".to_string(),
        (n, false) => format!("stop all {n} worktrees that are up?"),
        (n, true) => format!("stop all {n} worktrees that are up, and the pando check?"),
    };
    let title = Line::styled(
        truncate(&asked, cap),
        Style::new().fg(text()).add_modifier(Modifier::BOLD),
    );
    let warning: Vec<Line> = wrap_text("their services and public URLs go down too", cap)
        .into_iter()
        .map(|row| Line::styled(row, Style::new().fg(yellow())))
        .collect();
    let keys = key_rows(
        vec![
            (key_span("y"), hint_span(" stop all")),
            (key_span("esc"), hint_span(" cancel")),
        ],
        cap,
    );
    // The border, the title, the warning, the blank row and the keys.
    let room = (area.height as usize).saturating_sub(2 + 1 + warning.len() + 1 + keys.len());
    let shown = match names.len() <= room.min(MAX_ROWS) {
        true => names.len(),
        false => room.saturating_sub(1).min(MAX_ROWS),
    };
    let mut lines = vec![title];
    for name in names.iter().take(shown) {
        let (glyph, color) = super::render::run_marker(app.phase_of(name).as_ref());
        lines.push(Line::from(vec![
            Span::styled(glyph, Style::new().fg(color)),
            Span::styled(
                truncate_middle(&app.label_of(name), cap.saturating_sub(2)),
                Style::new().fg(text_dim()),
            ),
        ]));
    }
    if names.len() > shown {
        lines.push(hint_span(&format!("… and {} more", names.len() - shown)).into());
    }
    lines.extend(warning);
    let width = widest(&lines).max(widest(&keys)).min(cap);
    let height = lines.len() + 1 + keys.len();
    let Some(inner) = popup(f, area, "stop everything", None, width, height) else {
        return;
    };
    paint_over_keys(f, inner, lines, keys);
}

/// What removing takes with it and what would stop it, all of it before
/// the key is pressed: running, uncommitted changes, not pando's, locked.
/// A pane too short for all of it keeps what refuses the removal, what it
/// destroys and the keys.
fn render_remove(f: &mut Frame, area: Rect, label: &str, blockers: &[RemoveBlocker]) {
    let cap = max_content_width(area);
    // Each block with how soon it goes in a pane too short for all of
    // them, the highest first: the caption, then whether it runs, then
    // whether pando made it. The title never goes.
    let mut blocks: Vec<(u8, Vec<Line<'static>>)> = vec![
        (
            0,
            vec![Line::styled(
                truncate_middle(&format!("remove {label}?"), cap),
                Style::new().fg(text()).add_modifier(Modifier::BOLD),
            )],
        ),
        (
            5,
            wrap_text("the branch is kept; its logs and data are deleted", cap)
                .into_iter()
                .map(|row| Line::styled(row, Style::new().fg(text_muted())))
                .collect(),
        ),
    ];
    // Said here rather than found out from the progress line afterwards,
    // or from git refusing after the dialog has gone.
    for blocker in blockers {
        let (mark, color, sheds) = match blocker {
            RemoveBlocker::Locked(_) => ("⚠", yellow(), 1),
            RemoveBlocker::Dirty => ("*", yellow(), 2),
            RemoveBlocker::DirtyUnknown => ("?", text_muted(), 2),
            RemoveBlocker::Running => ("●", yellow(), 4),
            RemoveBlocker::NotOurs => ("⚠", yellow(), 3),
            // Data that goes for good: the destructive colour.
            RemoveBlocker::Drops(_) => ("✕", red(), 1),
        };
        let rows = wrap_text(&blocker.line(), cap.saturating_sub(2))
            .into_iter()
            .enumerate()
            .map(|(i, row)| {
                let lead = if i == 0 { mark } else { " " };
                Line::styled(format!("{lead} {row}"), Style::new().fg(color))
            })
            .collect();
        blocks.push((sheds, rows));
    }
    let dirty = blockers.contains(&RemoveBlocker::Dirty);
    let keys = if blockers.iter().any(RemoveBlocker::is_fatal) {
        vec![(
            key_span("esc"),
            Span::styled(
                " close — this one cannot be removed",
                Style::new().fg(red()),
            ),
        )]
    } else if dirty {
        // `y` would only be refused by git, so the key offered is the one
        // that works — and what it costs is on the line above.
        vec![
            (key_span("F"), hint_span(" remove, discarding changes")),
            (key_span("esc"), hint_span(" cancel")),
        ]
    } else {
        vec![
            (key_span("y"), hint_span(" remove")),
            (key_span("F"), hint_span(" force")),
            (key_span("esc"), hint_span(" cancel")),
        ]
    };
    let keys = key_rows(keys, cap);
    // The rows inside the border that the keys leave.
    let room = (area.height as usize).saturating_sub(2 + keys.len());
    let rows = |blocks: &[(u8, Vec<Line>)]| blocks.iter().map(|(_, b)| b.len()).sum::<usize>();
    let shed = rows(&blocks) > room;
    // Down to what fits with a `…` under it, which says something went.
    while shed && rows(&blocks) + 1 > room {
        let Some(at) = (0..blocks.len())
            .filter(|&i| blocks[i].0 > 0)
            .max_by_key(|&i| (blocks[i].0, i))
        else {
            break;
        };
        blocks.remove(at);
    }
    let mut lines: Vec<Line<'static>> = blocks.into_iter().flat_map(|(_, b)| b).collect();
    if shed {
        lines.push(hint_span("…").into());
    }
    let width = widest(&lines).max(widest(&keys)).min(cap);
    let height = lines.len() + 1 + keys.len();
    let Some(inner) = popup(f, area, "remove worktree", None, width, height) else {
        return;
    };
    paint_over_keys(f, inner, lines, keys);
}

/// Every key of the view it was opened from, then what the marks mean.
/// Scrollable, because a tmux split is often shorter than the keymap.
fn render_help(
    f: &mut Frame,
    area: Rect,
    scroll: usize,
    keys: &[KeyHelp],
    legend_title: &str,
    legend: &[(&str, &str)],
) -> usize {
    // The key column is as wide as its widest entry, so `PgUp PgDn` never
    // runs into what it does.
    let key_width = keys
        .iter()
        .map(|k| text_width(k.keys))
        .chain(legend.iter().map(|(mark, _)| text_width(mark)))
        .max()
        .unwrap_or(0)
        + 2;
    let row = |key: &str, what: &str, key_style: Style| {
        Line::from(vec![
            Span::styled(format!("{key:<key_width$}"), key_style),
            Span::styled(what.to_string(), Style::new().fg(text_dim())),
        ])
    };
    let key_style = Style::new().fg(orange()).add_modifier(Modifier::BOLD);
    let mut lines: Vec<Line> = keys
        .iter()
        .map(|k| row(k.keys, k.action, key_style))
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        legend_title.to_string(),
        Style::new().fg(text_muted()).add_modifier(Modifier::BOLD),
    ));
    for (mark, what) in legend {
        lines.push(row(mark, what, Style::new().fg(blue())));
    }

    let width = widest(&lines).min(max_content_width(area));
    let lines: Vec<Line> = lines
        .into_iter()
        .map(|line| super::render::truncate_line(line, width))
        .collect();
    let fits = lines.len() as u16 + 2 <= area.height;
    let footer = if fits { CLOSE_HINT } else { SCROLL_CLOSE_HINT };
    let Some(inner) = popup(f, area, "keys", Some(footer), width, lines.len()) else {
        return 0;
    };
    let visible = inner.height as usize;
    let bottom = lines.len().saturating_sub(visible);
    let start = scroll.min(bottom);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>()),
        inner,
    );
    bottom
}

/// Everything the header has said this session, newest first and in full.
fn render_messages(f: &mut Frame, area: Rect, app: &App) -> usize {
    let width = 72.min(max_content_width(area));
    let mut lines: Vec<Line> = Vec::new();
    for status in app.messages.iter().rev() {
        let (mark, color) = match status.kind {
            StatusKind::Success => ("✓ ", green()),
            StatusKind::Error => ("✗ ", red()),
            StatusKind::Info | StatusKind::Progress => ("› ", blue()),
        };
        let age = ago(status.at.elapsed());
        let style = if status.is_error() {
            Style::new().fg(red())
        } else {
            Style::new().fg(text())
        };
        for (i, row) in wrap_text(&status.message, width.saturating_sub(2))
            .into_iter()
            .enumerate()
        {
            let lead = if i == 0 { mark } else { "  " };
            lines.push(Line::from(vec![
                Span::styled(lead, Style::new().fg(color).add_modifier(Modifier::BOLD)),
                Span::styled(row, style),
            ]));
        }
        lines.push(Line::styled(
            format!("  {age}"),
            Style::new().fg(text_muted()),
        ));
    }
    if lines.is_empty() {
        lines.push(Line::styled(
            "nothing yet — what pando says about your actions collects here",
            Style::new().fg(text_muted()),
        ));
    }
    let width = widest(&lines).min(width);
    let fits = lines.len() as u16 + 2 <= area.height;
    let footer = if fits { CLOSE_HINT } else { SCROLL_CLOSE_HINT };
    let Some(inner) = popup(f, area, "messages", Some(footer), width, lines.len()) else {
        return 0;
    };
    let visible = inner.height as usize;
    let bottom = lines.len().saturating_sub(visible);
    let start = app.help_scroll.min(bottom);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(start).collect::<Vec<_>>()),
        inner,
    );
    bottom
}

fn ago(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..5 => "just now".to_string(),
        5..60 => format!("{secs}s ago"),
        60..3600 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

/// The git menu's first lines: where the checkout stands against its base
/// and its upstream, and whether anything in it would stop a move.
fn git_header(read: &GitRead) -> Vec<Line<'static>> {
    let dim = Style::new().fg(text_dim());
    let drift = |counts: Option<(u32, u32)>| -> Vec<Span<'static>> {
        match counts {
            Some((ahead, behind)) => {
                let mut spans = vec![Span::styled(
                    format!("↓{behind}"),
                    Style::new().fg(if behind > 0 { yellow() } else { text_dim() }),
                )];
                if ahead > 0 {
                    spans.push(Span::styled(format!(" ↑{ahead}"), Style::new().fg(green())));
                }
                spans.push(Span::raw(" "));
                spans
            }
            None => Vec::new(),
        }
    };
    let fetched = match read.fetched.and_then(|at| at.elapsed().ok()) {
        Some(ago) => format!(" · fetched {} ago", compact_age(ago.as_secs() as i64)),
        None => " · never fetched".to_string(),
    };
    let mut base = vec![Span::styled("base     ", dim)];
    match &read.base {
        Some(name) => {
            base.extend(drift(read.base_drift));
            base.push(Span::styled(name.clone(), dim));
        }
        None => base.push(Span::styled("none found", dim)),
    }
    base.push(Span::styled(fetched, Style::new().fg(text_muted())));
    let mut upstream = vec![Span::styled("upstream ", dim)];
    match &read.upstream {
        Some(name) => {
            upstream.extend(drift(read.upstream_drift));
            upstream.push(Span::styled(name.clone(), dim));
        }
        None => upstream.push(Span::styled("none", dim)),
    }
    let tree = match (read.in_progress, read.dirty) {
        (Some(op), _) => Span::styled(
            format!("a {} is in progress", op.noun()),
            Style::new().fg(orange()),
        ),
        (None, None) => Span::styled(
            "unknown — git status did not answer",
            Style::new().fg(yellow()),
        ),
        (None, Some(0)) => Span::styled("clean", Style::new().fg(green())),
        (None, Some(1)) => Span::styled("✎ 1 uncommitted file", Style::new().fg(yellow())),
        (None, Some(n)) => Span::styled(
            format!("✎ {n} uncommitted files"),
            Style::new().fg(yellow()),
        ),
    };
    vec![
        Line::from(base),
        Line::from(upstream),
        Line::from(vec![Span::styled("tree     ", dim), tree]),
    ]
}

/// `space g`: the git menu, at whichever stage it is — the same box throughout,
/// its title saying what it is about.
fn render_git(f: &mut Frame, area: Rect, app: &App, name: &str, stage: &GitStage) {
    let cap = max_content_width(area);
    let label = app.label_of(name);
    let dim = Style::new().fg(text_dim());
    let muted = Style::new().fg(text_muted());
    let mut lines: Vec<Line<'static>> = Vec::new();
    let menu_title = format!("git · {label}");
    let keys = |pairs: &[(&str, &str)]| {
        let mut spans = Vec::new();
        for (i, (key, what)) in pairs.iter().enumerate() {
            if i > 0 {
                spans.push(hint_span("   "));
            }
            spans.push(key_span(key));
            spans.push(hint_span(&format!(" {what}")));
        }
        Line::from(spans)
    };
    let title = match stage {
        GitStage::Reading => {
            lines.push(Line::styled("reading git…", muted));
            lines.push(Line::raw(""));
            lines.push(keys(&[("esc", "close")]));
            menu_title
        }
        GitStage::Menu { read, selected } => {
            lines.extend(git_header(read));
            lines.push(Line::raw(""));
            let offers = actions::git::offers(read);
            let word_width = offers
                .iter()
                .map(|o| text_width(o.action.word()))
                .chain([text_width("by hand")])
                .max()
                .unwrap_or(0);
            let rows = offers
                .iter()
                .map(|o| {
                    let (what, refused) = match &o.refused {
                        Some(why) => (why.clone(), true),
                        None => (o.what.clone(), false),
                    };
                    (o.action.key(), o.action.word(), what, refused)
                })
                .chain([(
                    BY_HAND_KEY,
                    "by hand",
                    "a shell in it, to do it yourself".to_string(),
                    false,
                )]);
            for (i, (key, word, what, refused)) in rows.enumerate() {
                let here = i == *selected;
                let cursor = Span::styled(if here { "▸ " } else { "  " }, Style::new().fg(blue()));
                let head = format!("{key}  {word:<word_width$}  ");
                let room = cap.saturating_sub(2 + text_width(&head));
                let line = if refused {
                    Line::from(vec![
                        cursor,
                        Span::styled(format!("{head}{}", truncate(&what, room)), muted),
                    ])
                } else {
                    let mut word_style = Style::new().fg(text());
                    if here {
                        word_style = word_style.add_modifier(Modifier::BOLD);
                    }
                    Line::from(vec![
                        cursor,
                        key_span(&key.to_string()),
                        Span::raw("  "),
                        Span::styled(format!("{word:<word_width$}"), word_style),
                        Span::raw("  "),
                        Span::styled(truncate(&what, room), dim),
                    ])
                };
                lines.push(line);
            }
            lines.push(Line::raw(""));
            lines.push(keys(&[
                ("↑↓", "choose"),
                ("⏎", "or its letter: preview"),
                ("esc", "close"),
            ]));
            menu_title
        }
        GitStage::Preview { read, action } => {
            let plan = actions::git::plan(read, *action);
            for (i, command) in plan.commands.iter().enumerate() {
                lines.push(Line::from(vec![
                    Span::styled(if i == 0 { "runs   " } else { "       " }, dim),
                    Span::styled(command.clone(), Style::new().fg(cyan())),
                ]));
            }
            lines.push(Line::raw(""));
            if let Some(moves) = &plan.moves {
                lines.push(Line::styled(moves.clone(), Style::new().fg(text())));
            }
            for note in &plan.notes {
                lines.push(Line::styled(note.clone(), dim));
            }
            for warning in &plan.warnings {
                for (i, row) in wrap_text(warning, cap.saturating_sub(2))
                    .into_iter()
                    .enumerate()
                {
                    let mark = if i == 0 { "! " } else { "  " };
                    lines.push(Line::styled(
                        format!("{mark}{row}"),
                        Style::new().fg(yellow()),
                    ));
                }
            }
            if app.is_up(name) && *action != GitAction::Fetch {
                lines.push(Line::styled("it runs: r restarts it after", dim));
            }
            lines.push(Line::raw(""));
            lines.push(keys(&[("⏎", action.word()), ("esc", "back")]));
            plan.title
        }
        GitStage::Running { read, action } => {
            let pending = app.pending_on(name);
            let frame = pending.map_or(0, |p| p.spinner_frame as usize);
            let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()];
            let doing = pending
                .and_then(|p| p.stage.clone())
                .unwrap_or_else(|| format!("{}…", action.verb()));
            lines.push(Line::from(vec![
                Span::styled(format!("{glyph} "), Style::new().fg(blue())),
                Span::styled(doing, Style::new().fg(text())),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "it cannot be stopped halfway: it finishes, or aborts what it started",
                dim,
            ));
            actions::git::plan(read, *action).title
        }
        GitStage::Result {
            read,
            action,
            ran,
            restart,
        } => {
            let ok = Style::new().fg(green());
            let width = cap.saturating_sub(2);
            let mut said = |mark: &str, style: Style, text: &str| {
                for (i, row) in wrap_text(text, width).into_iter().enumerate() {
                    let lead = if i == 0 { mark } else { "  " };
                    lines.push(Line::styled(format!("{lead}{row}"), style));
                }
            };
            match ran {
                Ok(ran @ (Ran::Moved(_) | Ran::Unchanged(_))) => {
                    said("✓ ", ok, &ran.summary());
                }
                Ok(ran @ Ran::Diverged { .. }) => {
                    said("○ ", Style::new().fg(yellow()), &ran.summary());
                }
                Ok(Ran::Conflict { op, files, at }) => {
                    said(
                        "✗ ",
                        Style::new().fg(red()),
                        &format!("conflict in {}", actions::git::file_list(files)),
                    );
                    if let Some(at) = at {
                        said("  ", dim, &format!("at commit {at}"));
                    }
                    said(
                        "  ",
                        ok,
                        &format!("git {} --abort: {label} is exactly as it was", op.noun()),
                    );
                }
                Err(e) => said("✗ ", Style::new().fg(red()), e),
            }
            lines.push(Line::raw(""));
            let stuck = !matches!(ran, Ok(Ran::Moved(_) | Ran::Unchanged(_)));
            if *restart {
                lines.push(Line::styled(
                    "it runs on the old files until it restarts",
                    dim,
                ));
                lines.push(keys(&[("r", "restart now"), ("esc", "later")]));
            } else if stuck {
                let by_hand = format!("{} by hand in a shell", action.word());
                lines.push(keys(&[
                    (&BY_HAND_KEY.to_string(), &by_hand),
                    ("esc", "close"),
                ]));
            } else {
                lines.push(keys(&[("esc", "close")]));
            }
            actions::git::plan(read, *action).title
        }
    };
    let width = widest(&lines).min(cap);
    let Some(inner) = popup(f, area, &title, None, width, lines.len()) else {
        return;
    };
    f.render_widget(Paragraph::new(lines), inner);
}
