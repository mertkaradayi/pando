//! The first run: what pando is, what it already knows about the project,
//! and the one key that gets a worktree onto the list.
//!
//! Everything shown is already in memory — the config and the paths the
//! app was started with. Detection is not run for it: a first frame that
//! waits on a scan of the project is a first frame that is late.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding, Paragraph};

use crate::config::{Config, ServiceConfig};
use crate::setup::{CheckRecord, Setup, SetupState};
use crate::theme::{blue, border, green, orange, text, text_dim, text_muted};
use crate::tui::app::{App, compact_age};

use super::{home_relative, truncate, truncate_middle, wrap_text};

/// `"  worktrees  "` — the widest fact label and its gap.
const FACT_LABEL: usize = 11;

pub(super) fn render_welcome(f: &mut Frame, area: Rect, app: &App) {
    let block = Block::bordered()
        .title(Span::styled(" welcome ", Style::new().fg(text_dim())))
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border()))
        .padding(if area.width > 40 && area.height > 12 {
            Padding::new(2, 2, 1, 0)
        } else {
            Padding::ZERO
        });
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    // A project whose test passed says what it proved, never "not settled
    // yet": the ready view's facts, the same renderer the setup screen
    // turns green with.
    let ready = app
        .setup_row
        .setup
        .as_ref()
        .filter(|setup| setup.state == SetupState::Ready);
    if let Some(setup) = ready {
        let view = ready_view(app, &app.config, setup, width);
        lines.extend(view.headline);
        lines.extend(view.when);
        lines.push(Line::raw(""));
        lines.extend(view.facts);
        lines.push(Line::raw(""));
        lines.extend(view.settings);
        lines.push(Line::raw(""));
    } else {
        welcome_facts(app, &mut lines, width);
    }
    let worktrees_dir = app.config.worktrees_dir(&app.paths);

    for (key, what) in [
        (
            "n",
            "create a worktree: type a branch name, new or existing",
        ),
        ("?", "every key, and what the marks in the list mean"),
        ("q", "quit"),
    ] {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{key:<4}"),
                Style::new().fg(orange()).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate(what, width.saturating_sub(4)),
                Style::new().fg(text_dim()),
            ),
        ]));
    }
    lines.push(Line::raw(""));
    // The real home, not `~/.pando`: `PANDO_HOME` moves it, and a
    // welcome that names the wrong directory sends somebody looking there.
    // So does `[project] worktrees_dir`, for the worktrees alone.
    let home = home_relative(&app.paths.home);
    let where_they_live = if worktrees_dir.starts_with(&app.paths.home) {
        format!("worktrees, logs and config live under {home}")
    } else {
        format!(
            "worktrees live in {}, logs and config under {home}",
            home_relative(&worktrees_dir)
        )
    };
    let yours = match app.paths.project.is_multi_repo() {
        true => "Your checkouts are",
        false => "Your checkout is",
    };
    for row in wrap_text(&format!("{yours} never touched: {where_they_live}."), width) {
        lines.push(Line::styled(row, Style::new().fg(text_muted())));
    }

    // Top-aligned in a short pane, a little lower in a tall one, so it
    // does not sit in a corner of an empty screen.
    let content = lines.len() as u16;
    let [_, body, _] = Layout::vertical([
        Constraint::Length(inner.height.saturating_sub(content) / 4),
        Constraint::Length(content.min(inner.height)),
        Constraint::Fill(1),
    ])
    .areas(inner);
    f.render_widget(Paragraph::new(lines), body);
}

/// What pando is, and what it already knows about the project: the
/// welcome before a test has passed.
fn welcome_facts(app: &App, lines: &mut Vec<Line<'static>>, width: usize) {
    let project = &app.paths.project.display_name;
    let repos = &app.paths.project.repos;
    // A multi-repo project is said to be what it is: a folder of
    // repositories, each branch of it a set of worktrees.
    let intro = match app.paths.project.is_multi_repo() {
        false => format!(
            "pando runs one dev environment per branch of {project}: a git worktree, \
             its dev server on its own port, and its logs."
        ),
        true => format!(
            "{project} is a folder of {} repositories: {}. pando runs one dev environment \
             per branch of it: a set of git worktrees, one of each repository, its dev \
             servers on their own ports, and its logs.",
            repos.len(),
            repos.join(", ")
        ),
    };
    for row in wrap_text(&intro, width) {
        lines.push(Line::styled(row, Style::new().fg(text())));
    }
    lines.push(Line::raw(""));
    let root = app
        .main
        .as_ref()
        .map(|m| m.path.clone())
        .unwrap_or_else(|| app.paths.root().to_path_buf());
    // A folder whose repositories are on different branches says each.
    let branch = app
        .main
        .as_ref()
        .filter(|m| m.branch.is_some() || m.is_mixed())
        .map(|m| format!("  on {}", m.branch_shown()))
        .unwrap_or_default();
    lines.push(fact(
        "project",
        format!("{}{branch}", home_relative(&root)),
        width,
    ));
    lines.push(fact("dev", dev_summary(app), width));
    if let Some(services) = services_summary(app) {
        lines.push(fact("services", services, width));
    }
    let worktrees_dir = app.config.worktrees_dir(&app.paths);
    lines.push(fact("worktrees", home_relative(&worktrees_dir), width));
    lines.push(Line::raw(""));
}

fn fact<'a>(label: &str, value: String, width: usize) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{label:<FACT_LABEL$}"),
            Style::new().fg(text_muted()),
        ),
        Span::styled(
            truncate_middle(&value, width.saturating_sub(FACT_LABEL)),
            Style::new().fg(blue()),
        ),
    ])
}

/// What `start` will run, as far as config already says.
fn dev_summary(app: &App) -> String {
    let processes: Vec<String> = app
        .config
        .runnable_processes()
        .map(|(name, process)| format!("{name}: {}", process.cmd))
        .collect();
    if processes.is_empty() {
        "not settled yet: the first start detects it, and asks if it has to".to_string()
    } else {
        processes.join(" · ")
    }
}

fn services_summary(app: &App) -> Option<String> {
    let names: Vec<String> = app
        .config
        .services
        .iter()
        .map(|service| match service {
            ServiceConfig::Compose { file, include, .. } if include.is_empty() => {
                format!("compose ({file})")
            }
            ServiceConfig::Compose { file, include, .. } => {
                format!("{} ({file})", include.join(", "))
            }
            ServiceConfig::Native { name, .. } => name.clone(),
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    Some(format!(
        "{} — shared by default, i isolates one worktree",
        names.join(" · ")
    ))
}

/// The ready view's parts: one renderer, fed from a config and the setup
/// read against it, for the setup screen to lay out by priority and the
/// dashboard's welcome to show whole. Rows carry no left margin.
pub(super) struct ReadyView {
    /// `✓ You're ready to use pando in <project>`.
    pub headline: Vec<Line<'static>>,
    /// Who set it up, when it was tested, and with which pando.
    pub when: Vec<Line<'static>>,
    /// apps, install, services, and the test row.
    pub facts: Vec<Line<'static>>,
    /// Where the settings are, and whose.
    pub settings: Vec<Line<'static>>,
}

pub(super) fn ready_view(app: &App, config: &Config, setup: &Setup, width: usize) -> ReadyView {
    let project = &app.paths.project.display_name;
    let width = width.max(1);
    let record = setup.last_check.as_ref();

    let headline = wrap_text(
        &format!("You're ready to use pando in {project}"),
        width.saturating_sub(2).max(1),
    )
    .into_iter()
    .enumerate()
    .map(|(i, row)| {
        let lead = if i == 0 {
            Span::styled("✓ ", Style::new().fg(green()).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("  ")
        };
        Line::from(vec![
            lead,
            Span::styled(row, Style::new().fg(text()).add_modifier(Modifier::BOLD)),
        ])
    })
    .collect();

    // "pando's own guess" only when the passing test came after pando
    // tried on its own: a rule-decided answer reads the same whoever
    // wrote it, so nothing else can say so honestly.
    let by_pando = match (setup.memory.tried_by_pando_at, record) {
        (Some(tried), Some(record)) => record.started_at >= tried,
        _ => false,
    };
    let who = if by_pando {
        "set up by pando's own guess and tested"
    } else {
        "set up and tested"
    };
    let when = record
        .and_then(|r| r.finished_at)
        .map(|at| ago((chrono::Utc::now() - at).num_seconds()))
        .unwrap_or_else(|| "just now".to_string());
    let mut said = format!("{who} {when}");
    if let Some(record) = record {
        said.push_str(&format!(" · tested with pando {}", record.pando_version));
    }
    let when = wrap_text(&said, width.saturating_sub(2).max(1))
        .into_iter()
        .map(|row| Line::styled(format!("  {row}"), Style::new().fg(text_dim())))
        .collect();

    let room = width.saturating_sub(2);
    let mut facts = Vec::new();
    if let Some(apps) = apps_summary(config) {
        facts.push(indented(fact("apps", apps, room)));
    }
    if let Some(install) = config
        .project
        .install
        .as_deref()
        .filter(|cmd| !cmd.trim().is_empty())
    {
        facts.push(indented(fact("install", install.to_string(), room)));
    }
    if let Some(names) = service_names(config) {
        // The check tests the shared mode: the developer's own services,
        // the ones the main checkout talks to.
        facts.push(indented(fact(
            "services",
            format!("{names} (yours, used as main uses them)"),
            room,
        )));
    }
    if let Some(record) = record {
        facts.push(indented(test_row(record, room)));
    }

    let settings = wrap_text(
        &format!(
            "settings: {}, yours to edit",
            home_relative(&app.paths.config_file())
        ),
        width.saturating_sub(2).max(1),
    )
    .into_iter()
    .map(|row| Line::styled(format!("  {row}"), Style::new().fg(text_muted())))
    .collect();

    ReadyView {
        headline,
        when,
        facts,
        settings,
    }
}

/// "just now" under a minute, a compact age after.
pub(super) fn ago(secs: i64) -> String {
    if secs < 60 {
        "just now".to_string()
    } else {
        format!("{} ago", compact_age(secs))
    }
}

fn indented(line: Line<'static>) -> Line<'static> {
    let mut spans = vec![Span::raw("  ")];
    spans.extend(line.spans);
    Line::from(spans)
}

/// `web + api   pnpm dev` when the apps share one command, each app with
/// its own otherwise.
fn apps_summary(config: &Config) -> Option<String> {
    let processes: Vec<(&String, &crate::config::ProcessConfig)> =
        config.runnable_processes().collect();
    let (_, first) = processes.first()?;
    if processes.iter().all(|(_, p)| p.cmd == first.cmd) {
        let names: Vec<&str> = processes.iter().map(|(n, _)| n.as_str()).collect();
        return Some(format!("{}   {}", names.join(" + "), first.cmd));
    }
    Some(
        processes
            .iter()
            .map(|(name, p)| format!("{name}: {}", p.cmd))
            .collect::<Vec<_>>()
            .join(" · "),
    )
}

/// `✓ passed   web answered (HTTP 200) · commit a1b2c3d of main`: what
/// the check proved, and on which commit.
fn test_row(record: &CheckRecord, width: usize) -> Line<'static> {
    let mut said = Vec::new();
    match record.processes.iter().find(|p| p.http_status.is_some()) {
        Some(probed) => said.push(format!(
            "{} answered (HTTP {})",
            probed.name,
            probed.http_status.unwrap_or_default()
        )),
        None if !record.processes.is_empty() => {
            let names: Vec<&str> = record.processes.iter().map(|p| p.name.as_str()).collect();
            said.push(format!("{} ready", names.join(", ")));
        }
        None => {}
    }
    if let Some(commit) = &record.commit {
        let short: String = commit.chars().take(7).collect();
        said.push(match &record.base_ref {
            Some(base) => format!("commit {short} of {base}"),
            None => format!("commit {short}"),
        });
    }
    let passed = "✓ passed   ";
    let room = width.saturating_sub(FACT_LABEL + passed.chars().count());
    Line::from(vec![
        Span::styled(
            format!("{:<FACT_LABEL$}", "test"),
            Style::new().fg(text_muted()),
        ),
        Span::styled(passed, Style::new().fg(green())),
        Span::styled(truncate(&said.join(" · "), room), Style::new().fg(blue())),
    ])
}

/// The services' names, one list, as the welcome names them.
fn service_names(config: &Config) -> Option<String> {
    let names: Vec<String> = config
        .services
        .iter()
        .map(|service| match service {
            ServiceConfig::Compose { file, include, .. } if include.is_empty() => {
                format!("compose ({file})")
            }
            ServiceConfig::Compose { include, .. } => include.join(", "),
            ServiceConfig::Native { name, .. } => name.clone(),
        })
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}
