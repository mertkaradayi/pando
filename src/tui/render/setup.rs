//! The setup screen, and the ready view it turns into: the whole frame,
//! with a header and a footer of its own.
//!
//! What a short or narrow terminal cannot hold is dropped by priority,
//! blank rows first and the prompt and the live line last, so a tmux
//! split still shows what to copy and where the setup stands.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::art::WordmarkSize;
use crate::setup::{FailureKind, SETUP_PROMPT};
use crate::theme::{green, orange, red, surface, text, text_dim, text_muted, yellow};
use crate::tui::app::{App, SetupLine, SetupScreen, Trying};

use super::chrome::{hint_line, status_mark};
use super::welcome::{ago, ready_view};
use super::{home_relative, text_width, truncate, truncate_line, wrap_text};

/// A block of rows and how much it matters: the least is dropped first
/// when the screen is too short for all of them.
struct Block {
    priority: u8,
    lines: Vec<Line<'static>>,
}

const BLANK: u8 = 0;
/// The wordmark, then the grove: the first things a short screen gives
/// up, after blank rows. They are sized to the rows there are, so a
/// screen only drops them when even their smallest does not fit.
const WORDMARK: u8 = 1;
const GROVE: u8 = 2;
const EXPLANATION: u8 = 3;
/// The prompt card's top and bottom edges.
const CARD_EDGE: u8 = 4;
const STEPS: u8 = 5;
const HEADING: u8 = 6;
const STEP_ONE: u8 = 7;
const LIVE: u8 = 8;
const PROMPT: u8 = 9;

/// The tallest and widest the grove is drawn: past these it is a forest
/// that crowds out what the screen is for.
const GROVE_ROWS: usize = 11;
const GROVE_WIDTH: usize = 100;

/// Rows the grove's block has besides the picture: the names under the
/// stems, the caption, and a blank row under them.
const GROVE_EXTRA_ROWS: usize = 3;

/// The smallest grove worth the big wordmark over it.
const GROVE_ROWS_UNDER_BIG_MARK: usize = 8;

/// What `a` says once it has copied: the flash the copy leaves, which the
/// key cap reads to turn into "✓ copied" while it shows.
const COPIED: &str = "copied the setup prompt";

pub(super) fn render_setup(f: &mut Frame, area: Rect, app: &App) {
    let Some(screen) = app.setup_screen.as_ref() else {
        return;
    };
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(area);
    render_setup_header(f, header, app, screen);
    let width = body.width as usize;
    let blocks = if screen.is_ready() {
        ready_blocks(screen, app, width)
    } else {
        setup_blocks(app, screen, width)
    };
    let blocks = with_art(blocks, app, width, body.height as usize, screen.is_ready());
    let mut lines = fit(blocks, body.height as usize);
    // A little lower than the top on a tall screen, so it does not sit in
    // a corner over an empty half.
    let spare = (body.height as usize).saturating_sub(lines.len());
    lines.splice(0..0, std::iter::repeat_n(Line::raw(""), spare / 3));
    f.render_widget(Paragraph::new(lines), body);
    render_setup_footer(f, footer, screen);
}

/// `pando — <project>`, and on the right what the screen is — or the
/// flash, which has nowhere else to go here.
fn render_setup_header(f: &mut Frame, area: Rect, app: &App, screen: &SetupScreen) {
    let width = area.width as usize;
    let title = format!(" pando — {}", app.paths.project.display_name);
    let title_width = text_width(&title);
    let room = width.saturating_sub(title_width + 3);
    let right: Vec<Span<'static>> = match app.flash() {
        Some(status) if room > 4 => {
            let (mark, color) = status_mark(status);
            let message = truncate(&status.message, room.saturating_sub(text_width(mark)));
            vec![
                Span::styled(mark, Style::new().fg(color)),
                Span::styled(message, Style::new().fg(text_dim())),
            ]
        }
        None if !screen.is_ready() && room >= "first time here".len() => {
            vec![Span::styled(
                "first time here",
                Style::new().fg(text_muted()),
            )]
        }
        _ => Vec::new(),
    };
    let right_width: usize = right.iter().map(|s| text_width(&s.content)).sum();
    let gap = width.saturating_sub(title_width + right_width + 1);
    let mut spans = vec![Span::styled(
        title,
        Style::new().fg(text()).add_modifier(Modifier::BOLD),
    )];
    if !right.is_empty() {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.extend(right);
    }
    f.render_widget(
        Paragraph::new(truncate_line(Line::from(spans), width)),
        area,
    );
}

fn render_setup_footer(f: &mut Frame, area: Rect, screen: &SetupScreen) {
    let hints = setup_hints(screen);
    f.render_widget(
        Paragraph::new(hint_line(&hints, area.width as usize)).style(Style::new().bg(surface())),
        area,
    );
}

/// The footer's hints, each a key of `SETUP_KEYS`.
pub(super) fn setup_hints(screen: &SetupScreen) -> Vec<(&'static str, &'static str, bool)> {
    let mut hints = Vec::new();
    if screen.is_ready() {
        hints.push(("⏎", "open pando", true));
        if screen.may_test() {
            hints.push(("v", "test again", false));
        }
    } else {
        hints.push(("a", "copy the prompt", true));
        if screen.may_test() {
            hints.push(("v", "test it", true));
        }
        if screen.may_try() {
            hints.push(("⏎", "let pando try on its own", true));
        }
        hints.push(("esc", "just manage worktrees", true));
    }
    hints.push(("?", "help", false));
    hints.push(("q", "quit", true));
    hints
}

/// The left margin: two cells where there is room for them.
fn indent(width: usize) -> usize {
    if width >= 30 { 2 } else { 0 }
}

fn setup_blocks(app: &App, screen: &SetupScreen, width: usize) -> Vec<Block> {
    let project = &app.paths.project.display_name;
    let pad = indent(width);
    let room = width.saturating_sub(pad * 2).max(1);
    let mut blocks = vec![blank()];

    blocks.push(Block {
        priority: HEADING,
        lines: wrapped(
            &format!("Let's set pando up for {project}."),
            pad,
            room,
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ),
    });
    blocks.push(blank());
    blocks.push(Block {
        priority: EXPLANATION,
        lines: wrapped(
            "Every project is a little different, so the surest start is to let your coding \
             agent look at it. It sets pando up, tests it, and tells you when you're ready. It \
             never changes a file in your project.",
            pad,
            room,
            Style::new().fg(text_dim()),
        ),
    });
    blocks.push(blank());
    blocks.push(Block {
        priority: STEP_ONE,
        lines: vec![step_one(app, pad, room)],
    });
    blocks.extend(prompt_card(pad, room));
    blocks.push(blank());
    let root = home_relative(app.paths.root());
    blocks.push(Block {
        priority: STEPS,
        lines: step(
            "2",
            &format!("Paste it into Claude Code or Codex, opened in {root}"),
            pad,
            room,
        ),
    });
    blocks.push(Block {
        priority: STEPS,
        lines: step(
            "3",
            "Come back here: this screen turns green by itself when it's done",
            pad,
            room,
        ),
    });
    blocks.push(blank());
    blocks.push(Block {
        priority: LIVE,
        lines: live_lines(app, screen, pad, room),
    });
    blocks
}

/// The ready view: the welcome's renderer with the test row, fed from the
/// screen's own config and record, and the key onward.
fn ready_blocks(screen: &SetupScreen, app: &App, width: usize) -> Vec<Block> {
    let pad = indent(width);
    let room = width.saturating_sub(pad * 2).max(1);
    let view = ready_view(app, &screen.config, &screen.setup, room);
    let margin = |lines: Vec<Line<'static>>| -> Vec<Line<'static>> {
        lines
            .into_iter()
            .map(|line| {
                let mut spans = vec![Span::raw(" ".repeat(pad))];
                spans.extend(line.spans);
                truncate_line(Line::from(spans), width)
            })
            .collect()
    };
    let mut blocks = vec![
        blank(),
        Block {
            priority: PROMPT,
            lines: margin(view.headline),
        },
        Block {
            priority: LIVE,
            lines: margin(view.when),
        },
        blank(),
    ];
    // The test row matters more than the facts it tested.
    let test_at = view.facts.len().saturating_sub(1);
    for (i, fact) in view.facts.into_iter().enumerate() {
        blocks.push(Block {
            priority: if i == test_at { HEADING } else { STEPS },
            lines: margin(vec![fact]),
        });
    }
    blocks.push(blank());
    blocks.push(Block {
        priority: EXPLANATION,
        lines: margin(view.settings),
    });
    blocks.push(blank());
    blocks.push(Block {
        priority: STEP_ONE,
        lines: margin(vec![Line::from(vec![
            key_cap("⏎"),
            Span::styled(
                " open pando",
                Style::new().fg(text()).add_modifier(Modifier::BOLD),
            ),
        ])]),
    });
    blocks
}

/// The live line, from what the files say.
fn live_lines(app: &App, screen: &SetupScreen, pad: usize, room: usize) -> Vec<Line<'static>> {
    let project = &app.paths.project.display_name;
    let spinner = app.spinner();
    let dim = Style::new().fg(text_dim());
    match screen.line() {
        SetupLine::Reading => marked(
            spinner,
            yellow(),
            &format!("reading {project}…"),
            dim,
            pad,
            room,
        ),
        SetupLine::Waiting => marked(spinner, yellow(), "waiting for the setup…", dim, pad, room),
        SetupLine::Starting => marked(spinner, yellow(), "starting the test…", dim, pad, room),
        SetupLine::SettingsSaved => {
            let when = screen
                .settings_seen_at
                .map(|at| format!(" {}", ago(at.elapsed().as_secs() as i64)))
                .unwrap_or_default();
            marked(
                "✓",
                green(),
                &format!("settings saved{when} · v tests them"),
                dim,
                pad,
                room,
            )
        }
        SetupLine::Testing(progress) => {
            let said = if progress.is_empty() {
                "testing the setup…".to_string()
            } else {
                format!("testing: {}…", progress.join(" · "))
            };
            marked(spinner, yellow(), &said, dim, pad, room)
        }
        SetupLine::Passed => marked("✓", green(), "the test passed", dim, pad, room),
        // Whose it is decides the rest. A settings failure goes back to
        // the agent with the prompt; a machine's, or a base's, no setting
        // fixes, so the agent has stopped and the developer is who acts,
        // on the command the reason carries.
        SetupLine::Failed {
            reason,
            kind: FailureKind::Settings,
            by_program,
        } => {
            let mut lines = marked(
                "✗",
                red(),
                &format!(
                    "the test failed: {reason} · a copies the prompt, which now includes this"
                ),
                dim,
                pad,
                room,
            );
            if by_program {
                lines.extend(wrapped(
                    "Your agent is probably on it.",
                    pad + 2,
                    room.saturating_sub(2).max(1),
                    Style::new().fg(text_muted()),
                ));
            }
            lines
        }
        SetupLine::Failed { reason, kind, .. } => {
            let mut lines = marked(
                "✗",
                red(),
                &format!("the test failed: {reason}"),
                dim,
                pad,
                room,
            );
            let yours = match kind {
                FailureKind::Base => {
                    "This one is yours, not your agent's: which branch work starts from is \
                     your call. Run the command above, or tell your agent the branch · v tests \
                     again"
                }
                _ => {
                    "This one is yours, not your agent's: no setting fixes it. Run the command \
                     above · v tests again"
                }
            };
            lines.extend(wrapped(
                yours,
                pad + 2,
                room.saturating_sub(2).max(1),
                Style::new().fg(text_muted()),
            ));
            lines
        }
        SetupLine::Interrupted => marked(
            "○",
            yellow(),
            "the last test was interrupted · v tests again",
            dim,
            pad,
            room,
        ),
        SetupLine::NotSetUp { slot } => marked(
            "○",
            yellow(),
            &format!("not set up: {slot} is open · a copies the prompt"),
            dim,
            pad,
            room,
        ),
        SetupLine::OwnGuess(trying) => guess_lines(app, &trying, pad, room),
    }
}

/// pando's own guess: working it out, or why it stopped with nothing
/// written.
fn guess_lines(app: &App, trying: &Trying, pad: usize, room: usize) -> Vec<Line<'static>> {
    let project = &app.paths.project.display_name;
    let dim = Style::new().fg(text_dim());
    match trying {
        Trying::Resolving => marked(
            app.spinner(),
            yellow(),
            &format!("pando is trying its own guess for {project}…"),
            dim,
            pad,
            room,
        ),
        Trying::CannotTell => marked(
            "✗",
            red(),
            &format!(
                "pando can't tell how {project} starts; this one needs your agent · a copies \
                 the prompt"
            ),
            dim,
            pad,
            room,
        ),
        // Doctor's line, and its fix a line at a time: each is one a
        // developer may copy.
        Trying::NeedsPrelude { line, fix } => {
            let mut lines = marked("!", yellow(), line, dim, pad, room);
            let muted = Style::new().fg(text_muted());
            for row in fix.iter().flat_map(|fix| fix.lines()) {
                lines.extend(wrapped(row, pad + 2, room.saturating_sub(2).max(1), muted));
            }
            lines.extend(wrapped(
                "pando never sets a runtime prelude on its own: it is for every project on this \
                 machine · a copies the prompt",
                pad + 2,
                room.saturating_sub(2).max(1),
                dim,
            ));
            lines
        }
    }
}

fn blank() -> Block {
    Block {
        priority: BLANK,
        lines: vec![Line::raw("")],
    }
}

/// `1  Copy this prompt:` and, right after it, the key that does it as a
/// key cap — or, just after it was pressed, "✓ copied" in its place.
fn step_one(app: &App, pad: usize, room: usize) -> Line<'static> {
    let copied = app
        .flash()
        .is_some_and(|status| status.message.starts_with(COPIED));
    let mut spans = vec![
        Span::raw(" ".repeat(pad)),
        Span::styled(
            "1  ",
            Style::new().fg(orange()).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "Copy this prompt:",
            Style::new().fg(text()).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    match copied {
        true => spans.push(Span::styled(
            "✓ copied",
            Style::new().fg(green()).add_modifier(Modifier::BOLD),
        )),
        false => {
            spans.push(key_cap("a"));
            spans.push(Span::styled(" copy", Style::new().fg(text_dim())));
        }
    }
    truncate_line(Line::from(spans), pad + room)
}

/// A key, drawn as a key cap: the one thing on the screen to press.
fn key_cap(key: &str) -> Span<'static> {
    Span::styled(
        format!(" {key} "),
        Style::new()
            .fg(surface())
            .bg(orange())
            .add_modifier(Modifier::BOLD),
    )
}

/// The prompt, on a panel of its own under step one: a top and a bottom
/// edge in half blocks, and the prompt's rows on the panel's colour
/// between them.
///
/// No character either side of the prompt: selected with a mouse over
/// SSH, the row brings the prompt and spaces, nothing else. The edges
/// are rows of their own, and a short screen gives them up before the
/// prompt.
fn prompt_card(pad: usize, room: usize) -> Vec<Block> {
    let inner = text_width(SETUP_PROMPT);
    let card = room.min(inner + 4);
    let rows = wrap_text(SETUP_PROMPT, card.saturating_sub(4).max(1));
    let edge = |ch: &str| Block {
        priority: CARD_EDGE,
        lines: vec![Line::from(vec![
            Span::raw(" ".repeat(pad)),
            Span::styled(ch.repeat(card), Style::new().fg(surface())),
        ])],
    };
    let panel = Style::new().bg(surface());
    let body = rows
        .into_iter()
        .map(|row| {
            let fill = card.saturating_sub(text_width(&row) + 2);
            Line::from(vec![
                Span::raw(" ".repeat(pad)),
                Span::styled("  ", panel),
                Span::styled(row, panel.fg(text()).add_modifier(Modifier::BOLD)),
                Span::styled(" ".repeat(fill), panel),
            ])
        })
        .collect();
    vec![
        edge("▄"),
        Block {
            priority: PROMPT,
            lines: body,
        },
        edge("▀"),
    ]
}

/// A numbered step, its text wrapped under itself.
fn step(number: &str, what: &str, pad: usize, room: usize) -> Vec<Line<'static>> {
    let rows = wrap_text(what, room.saturating_sub(3).max(1));
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| {
            let lead = if i == 0 {
                Span::styled(
                    format!("{number}  "),
                    Style::new().fg(orange()).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("   ")
            };
            Line::from(vec![
                Span::raw(" ".repeat(pad)),
                lead,
                Span::styled(row, Style::new().fg(text())),
            ])
        })
        .collect()
}

/// `text` wrapped to `room` cells, each row indented by `pad`.
fn wrapped(text: &str, pad: usize, room: usize, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, room)
        .into_iter()
        .map(|row| Line::from(vec![Span::raw(" ".repeat(pad)), Span::styled(row, style)]))
        .collect()
}

/// A mark and its text, the text wrapped under itself.
fn marked(
    mark: &str,
    color: Color,
    text: &str,
    style: Style,
    pad: usize,
    room: usize,
) -> Vec<Line<'static>> {
    let lead = text_width(mark) + 1;
    wrap_text(text, room.saturating_sub(lead).max(1))
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            let head = if i == 0 {
                Span::styled(format!("{mark} "), Style::new().fg(color))
            } else {
                Span::raw(" ".repeat(lead))
            };
            Line::from(vec![
                Span::raw(" ".repeat(pad)),
                head,
                Span::styled(row, style),
            ])
        })
        .collect()
}

/// `blocks` with the first run's pictures above them, in the rows they
/// leave over: the PANDO wordmark, big where there is room for it and a
/// grove under it, compact where there is less, and the grove, as tall
/// as [`GROVE_ROWS`] where there is room and never shorter than a grove
/// reads. On a classic 80×24 a blank row or two between the steps is a
/// fair price for the smallest grove, and `fit` pays blank rows first.
///
/// The grove is pando's namesake — one aspen, thousands of stems, one
/// root system — seeded by the project, so each project has its own, and
/// its stems carry branch names, `main` first: each stem a branch in a
/// worktree of its own. The leaves are gold; the roots are dark while
/// pando is being set up, and light up once the check passes.
fn with_art(
    mut blocks: Vec<Block>,
    app: &App,
    width: usize,
    height: usize,
    alive: bool,
) -> Vec<Block> {
    use crate::art::{COMPACT_WIDTH, GROVE_MIN_HEIGHT, WORDMARK_HEIGHT, WORDMARK_WIDTH};
    let used: usize = blocks.iter().map(|b| b.lines.len()).sum();
    let spare = height.saturating_sub(used);
    let pad = indent(width);
    let room = width.saturating_sub(pad * 2);
    let art_width = room.min(GROVE_WIDTH);
    let left = pad + (room - art_width) / 2;
    let grove_rows = |rows: usize| rows.saturating_sub(GROVE_EXTRA_ROWS).min(GROVE_ROWS);
    // The wordmark, the credit under it, and a blank row.
    let big = WORDMARK_HEIGHT + 2;
    let compact = 2 + 2;
    let (mark, rows) =
        if room >= WORDMARK_WIDTH && spare >= big + GROVE_ROWS_UNDER_BIG_MARK + GROVE_EXTRA_ROWS {
            (Some(WordmarkSize::Big), grove_rows(spare - big))
        } else if room >= COMPACT_WIDTH && spare >= compact + GROVE_MIN_HEIGHT + GROVE_EXTRA_ROWS {
            (Some(WordmarkSize::Compact), grove_rows(spare - compact))
        } else {
            // The smallest grove does not fit as the screen stands: the
            // explanation gives way to it first, then blank rows, so the
            // steps still read as steps.
            if spare < GROVE_MIN_HEIGHT + GROVE_EXTRA_ROWS
                && let Some(i) = blocks.iter().position(|b| b.priority == EXPLANATION)
            {
                let after = usize::from(blocks.get(i + 1).is_some_and(|b| b.priority == BLANK));
                blocks.drain(i..=i + after);
            }
            let used: usize = blocks.iter().map(|b| b.lines.len()).sum();
            (
                None,
                grove_rows(height.saturating_sub(used)).max(GROVE_MIN_HEIGHT),
            )
        };
    let seed = crate::art::seed_of(&app.paths.project.id);
    let frame = u64::from(app.tick);
    let grove = crate::art::grove(art_width, rows, frame, seed);
    let mut art = Vec::new();
    if let Some(size) = mark {
        // Centred on the screen, with the credit centred under it.
        let mark_width = match size {
            WordmarkSize::Big => WORDMARK_WIDTH,
            WordmarkSize::Compact => COMPACT_WIDTH,
        };
        let centre = |w: usize| width.saturating_sub(w) / 2;
        let mut lines: Vec<Line<'static>> = crate::art::wordmark(size, frame)
            .iter()
            .map(|row| art_line(row, centre(mark_width), alive))
            .collect();
        let credit = truncate(&crate::art::credit(), width);
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(centre(text_width(&credit)))),
            Span::styled(credit, Style::new().fg(text_muted())),
        ]));
        art.push(Block {
            priority: WORDMARK,
            lines,
        });
        art.push(blank());
    }
    if !grove.is_empty() {
        let mut lines: Vec<Line<'static>> = grove
            .cells
            .iter()
            .map(|row| art_line(row, left, alive))
            .collect();
        lines.push(stem_names(&grove.stems, left, art_width));
        let caption = match (alive, app.paths.project.is_multi_repo()) {
            (true, _) => "one root, every branch alive",
            (false, false) => {
                "Pando: one aspen, 47,000 stems, one root. Your repo, every branch alive."
            }
            (false, true) => {
                "Pando: one aspen, 47,000 stems, one root. Your repos, every branch alive."
            }
        };
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(left)),
            Span::styled(
                truncate(caption, art_width),
                Style::new()
                    .fg(if alive { green() } else { text_muted() })
                    .add_modifier(Modifier::ITALIC),
            ),
        ]));
        art.push(Block {
            priority: GROVE,
            lines,
        });
        art.push(blank());
    }
    // Under the blank the screen opens with.
    let at = usize::from(blocks.first().is_some_and(|b| b.priority == BLANK));
    blocks.splice(at..at, art);
    blocks
}

/// Branch names for the stems: the picture's, not the project's. Each
/// stem is a branch living in a worktree of its own, and a name under it
/// says so; a real branch there would put a project's work in a picture
/// meant for any project.
const STEM_NAMES: [&str; 7] = [
    "main",
    "feat/checkout",
    "fix/login-loop",
    "feat/search",
    "chore/deps",
    "feat/dark-mode",
    "fix/typo",
];

/// The row under the root line: a branch name under each stem there is
/// room for, left to right, `main` first.
fn stem_names(stems: &[usize], left: usize, width: usize) -> Line<'static> {
    let labels: Vec<String> = STEM_NAMES.iter().map(|n| n.to_string()).collect();
    let mut spans = vec![Span::raw(" ".repeat(left))];
    let mut at = 0;
    for (x, label) in crate::art::label_stems(stems, &labels, width) {
        spans.push(Span::raw(" ".repeat(x - at)));
        let style = match label == STEM_NAMES[0] {
            true => Style::new().fg(text()).add_modifier(Modifier::BOLD),
            false => Style::new().fg(text_dim()),
        };
        at = x + text_width(&label);
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

/// One row of a picture as spans, a run of one colour at a time.
fn art_line(row: &[crate::art::Cell], left: usize, alive: bool) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(left))];
    let mut run = String::new();
    let mut style = Style::new();
    for cell in row {
        let next = art_style(cell, alive);
        if next != style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        style = next;
        run.push(cell.ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, style));
    }
    Line::from(spans)
}

/// A picture cell's colour: gold letters and leaves, white stems, and
/// roots dark while pando is being set up and lit once it is ready.
fn art_style(cell: &crate::art::Cell, alive: bool) -> Style {
    use crate::art::Material;
    let fg = match (cell.material, alive) {
        (Material::Sky, _) => return Style::new(),
        (Material::Letter, _) => {
            return Style::new().fg(yellow()).add_modifier(Modifier::BOLD);
        }
        (Material::Canopy, _) => yellow(),
        (Material::Trunk, _) => text(),
        (Material::Eye | Material::Ground | Material::Shadow, _) => text_muted(),
        (Material::Root, false) => text_dim(),
        (Material::Root, true) => green(),
    };
    Style::new().fg(fg)
}

/// The rows that fit in `height`, dropping the least important blocks
/// first — the last of equals first, so the top of the screen holds.
fn fit(mut blocks: Vec<Block>, height: usize) -> Vec<Line<'static>> {
    let rows = |blocks: &[Block]| blocks.iter().map(|b| b.lines.len()).sum::<usize>();
    while rows(&blocks) > height {
        let Some(least) = blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| b.priority < PROMPT)
            .min_by_key(|(i, b)| (b.priority, std::cmp::Reverse(*i)))
            .map(|(i, _)| i)
        else {
            break;
        };
        blocks.remove(least);
    }
    blocks.into_iter().flat_map(|b| b.lines).collect()
}
