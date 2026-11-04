//! `ls --all`: every project pando knows on this machine, grouped by
//! project with the live ones first, and its JSON.
//!
//! Read from the home alone, from any directory: no repository is looked
//! for, no config loaded, and nothing written but the registry of a
//! project that had none.

use super::JSON_VERSION;
use super::ls::{LsView, ProjectOut, terminal_width};
use super::status::{phase_since, phase_word};
use crate::actions::{self, LiveWorktree, ProjectSummary};
use crate::state::Aggregate;
use crate::term::{Paint, Style, ellipsize_end, ellipsize_middle, text_width};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

/// `pando ls --all`, as text or as JSON, for the home given.
pub fn ls_machine<W: Write>(home: &Path, json: bool, long: bool, out: &mut W) -> Result<()> {
    if json {
        return ls_machine_json(home, out);
    }
    let view = LsView {
        width: terminal_width(),
        long,
        style: Style::for_stdout(),
    };
    ls_machine_text_with(home, out, &view)
}

/// Every port two projects claim, by project id and worktree name.
fn clashing(summaries: &[ProjectSummary]) -> BTreeMap<(String, String), BTreeSet<u16>> {
    let mut out: BTreeMap<(String, String), BTreeSet<u16>> = BTreeMap::new();
    for (port, claims) in actions::port_clashes(summaries) {
        for claim in claims {
            out.entry(claim).or_default().insert(port);
        }
    }
    out
}

/// The text listing: a line per project, its live worktrees under it.
pub fn ls_machine_text_with<W: Write>(home: &Path, out: &mut W, view: &LsView) -> Result<()> {
    let summaries = actions::summaries(home)?;
    if summaries.is_empty() {
        // The home named: a mistyped `PANDO_HOME` shows here.
        writeln!(
            out,
            "pando knows no project yet in {} — `pando new <branch>` in a repository makes its \
             first",
            view.style.tilde(&home.display().to_string())
        )?;
        return Ok(());
    }
    let clashes = clashing(&summaries);
    let style = &view.style;
    let name_width = summaries
        .iter()
        .map(|s| text_width(&s.name))
        .max()
        .unwrap_or(0);
    let counts: Vec<String> = summaries.iter().map(counts_word).collect();
    let counts_width = counts.iter().map(|c| text_width(c)).max().unwrap_or(0);
    let worktree_width = summaries
        .iter()
        .flat_map(|s| s.worktrees.iter())
        .map(|w| text_width(&w.name))
        .max()
        .unwrap_or(0);
    for (summary, counts) in summaries.iter().zip(&counts) {
        let (mark, paint) = project_mark(summary);
        let name = pad(&summary.name, name_width);
        let counts = pad(counts, counts_width);
        let lead = format!("{mark} {name}  {counts}");
        // No record says where it is: the place of a path is how to
        // bring it back.
        let root = match summary.located() {
            false => summary.not_located().to_string(),
            true => style.tilde(&summary.root.display().to_string()),
        };
        let room = view.width.saturating_sub(text_width(&lead) + 2);
        let mut line = format!(
            "{} {}  {}",
            style.paint(mark, paint),
            style.paint(
                &name,
                if summary.moved {
                    Paint::Faint
                } else {
                    Paint::Heading
                }
            ),
            style.paint(&counts, paint),
        );
        if room >= 8 {
            line.push_str("  ");
            line.push_str(&style.paint(&ellipsize_middle(&root, room), Paint::Faint));
        }
        writeln!(out, "{}", line.trim_end())?;
        // How to bring it back, whole on a line of its own: its end is
        // the command.
        if !summary.located() {
            writeln!(out, "    {}", summary.find_again())?;
        }
        if let Some(warning) = &summary.warning {
            writeln!(
                out,
                "    {}",
                style.paint(
                    &ellipsize_end(warning, view.width.saturating_sub(4)),
                    Paint::Bad
                )
            )?;
        }
        for worktree in &summary.worktrees {
            let clashing = clashes
                .get(&(summary.id.clone(), worktree.name.clone()))
                .cloned()
                .unwrap_or_default();
            write_worktree(out, view, worktree, worktree_width, &clashing)?;
        }
    }
    let lines = clash_lines(&summaries, style);
    if !lines.is_empty() {
        writeln!(out)?;
        for line in lines {
            writeln!(out, "{line}")?;
        }
    }
    Ok(())
}

/// The trailer's line for each port two projects claim. A project is
/// named by its name, or by where it is when another listed project
/// shares that name — two clones of one repository — and a claim by its
/// main checkout says so rather than repeating the name.
pub(super) fn clash_lines(summaries: &[ProjectSummary], style: &Style) -> Vec<String> {
    let label = |id: &str| -> String {
        let Some(summary) = summaries.iter().find(|s| s.id == id) else {
            return id.to_string();
        };
        let shared = summaries.iter().filter(|o| o.name == summary.name).count() > 1;
        match (shared, summary.located()) {
            (false, _) => summary.name.clone(),
            (true, true) => style.tilde(&summary.root.display().to_string()),
            (true, false) => summary.id.clone(),
        }
    };
    actions::port_clashes(summaries)
        .into_iter()
        .map(|(port, claims)| {
            let by: Vec<String> = claims
                .iter()
                .map(|(id, worktree)| {
                    let main = summaries
                        .iter()
                        .find(|s| &s.id == id)
                        .is_some_and(|s| &s.name == worktree);
                    match main {
                        true => format!("{} (main checkout)", label(id)),
                        false => format!("{} {worktree}", label(id)),
                    }
                })
                .collect();
            format!(
                "{} port {port} is claimed by {}",
                style.paint("!", Paint::Bad),
                by.join(" and ")
            )
        })
        .collect()
}

/// One live worktree's line under its project: its name, its phase, its
/// ports with the ones another project claims marked, and with `-l` its
/// path.
fn write_worktree<W: Write>(
    out: &mut W,
    view: &LsView,
    worktree: &LiveWorktree,
    name_width: usize,
    clashing: &BTreeSet<u16>,
) -> Result<()> {
    let style = &view.style;
    let (word, paint) = match &worktree.phase {
        Aggregate::Running { .. } => ("running", Paint::Good),
        Aggregate::Starting { .. } => ("starting", Paint::Warn),
        Aggregate::Failed { .. } => ("failed", Paint::Bad),
    };
    let mut line = format!(
        "    {}  {}",
        pad(&worktree.name, name_width),
        style.paint(&pad(word, 8), paint)
    );
    let mut ports: Vec<String> = Vec::new();
    for port in &worktree.claimed {
        match clashing.contains(port) {
            true => ports.push(style.paint(&format!("{port}!"), Paint::Bad)),
            false => ports.push(style.paint(&port.to_string(), Paint::Link)),
        }
    }
    if !ports.is_empty() {
        line.push_str("  ");
        line.push_str(&ports.join(" "));
    }
    // With `-l` the path comes first, fitted as a project's root is; the
    // reason gets what is left, and neither runs past the width.
    let path = match view.long {
        true => {
            let room = view
                .width
                .saturating_sub(crate::term::visible_width(&line) + 2);
            let path = style.tilde(&worktree.path.display().to_string());
            (room >= 8).then(|| ellipsize_middle(&path, room))
        }
        false => None,
    };
    let path_takes = path.as_ref().map_or(0, |p| text_width(p) + 2);
    if let Some(reason) = worktree.phase.reason() {
        let used = crate::term::visible_width(&line) + 2 + path_takes;
        let room = view.width.saturating_sub(used);
        if room >= 8 {
            line.push_str("  ");
            line.push_str(&style.paint(&ellipsize_end(&reason, room), Paint::Faint));
        }
    }
    if let Some(path) = path {
        line.push_str("  ");
        line.push_str(&style.paint(&path, Paint::Faint));
    }
    writeln!(out, "{line}")?;
    Ok(())
}

/// The mark a project's line opens with, in the list's glyphs.
fn project_mark(summary: &ProjectSummary) -> (&'static str, Paint) {
    match () {
        _ if summary.moved && summary.up + summary.starting + summary.failed == 0 => {
            (" ", Paint::Faint)
        }
        _ if summary.up > 0 && summary.failed == 0 && summary.starting == 0 => ("●", Paint::Good),
        _ if summary.failed > 0 => ("✗", Paint::Bad),
        _ if summary.starting > 0 => ("◌", Paint::Warn),
        _ => ("○", Paint::Faint),
    }
}

/// What a project has, in words: `2 up · 1 starting · 1 failed · 3
/// stopped`, with `· moved` after it for a moved project, `moved` alone,
/// or `nothing recorded`.
fn counts_word(summary: &ProjectSummary) -> String {
    let mut parts: Vec<String> = [
        (summary.up, "up"),
        (summary.starting, "starting"),
        (summary.failed, "failed"),
        (summary.stopped, "stopped"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, word)| format!("{n} {word}"))
    .collect();
    if summary.moved {
        parts.push(summary.hands_off_word().to_string());
    }
    match parts.is_empty() {
        true => "nothing recorded".to_string(),
        false => parts.join(" · "),
    }
}

fn pad(text: &str, width: usize) -> String {
    let fill = width.saturating_sub(text_width(text));
    format!("{text}{}", " ".repeat(fill))
}

// ---- JSON -----------------------------------------------------------------

#[derive(Serialize)]
struct MachineOutput {
    version: u32,
    projects: Vec<MachineProjectOut>,
}

#[derive(Serialize)]
struct MachineProjectOut {
    #[serde(flatten)]
    project: ProjectOut,
    moved: bool,
    unreadable: bool,
    last_used: Option<DateTime<Utc>>,
    counts: CountsOut,
    /// Why its state could not be read; `null` when it could.
    warning: Option<String>,
    worktrees: Vec<MachineWorktreeOut>,
}

#[derive(Serialize)]
struct CountsOut {
    up: usize,
    starting: usize,
    failed: usize,
    stopped: usize,
}

#[derive(Serialize)]
struct MachineWorktreeOut {
    name: String,
    path: String,
    status: &'static str,
    reason: Option<String>,
    processes: BTreeMap<String, MachineProcessOut>,
    ports: BTreeMap<String, u16>,
    claimed: Vec<u16>,
    clashes: Vec<u16>,
}

#[derive(Serialize)]
struct MachineProcessOut {
    pid: u32,
    phase: &'static str,
    since: DateTime<Utc>,
    reason: Option<String>,
}

/// The documented shape of `ls --all --json`.
pub fn ls_machine_json<W: Write>(home: &Path, out: &mut W) -> Result<()> {
    let summaries = actions::summaries(home)?;
    let clashes = clashing(&summaries);
    let projects = summaries
        .iter()
        .map(|summary| MachineProjectOut {
            project: ProjectOut {
                id: summary.id.clone(),
                root: summary.root.display().to_string(),
                name: summary.name.clone(),
                repos: summary.repos.clone(),
            },
            moved: summary.moved,
            unreadable: summary.unreadable,
            last_used: summary.last_used.map(DateTime::<Utc>::from),
            counts: CountsOut {
                up: summary.up,
                starting: summary.starting,
                failed: summary.failed,
                stopped: summary.stopped,
            },
            warning: summary.warning.clone(),
            worktrees: summary
                .worktrees
                .iter()
                .map(|worktree| MachineWorktreeOut {
                    name: worktree.name.clone(),
                    path: worktree.path.display().to_string(),
                    status: worktree.phase.word(),
                    reason: worktree.phase.reason(),
                    processes: worktree
                        .processes
                        .iter()
                        .map(|(name, process)| {
                            let reason = match &process.phase {
                                crate::state::Phase::Failed { reason, .. } => Some(reason.clone()),
                                _ => None,
                            };
                            (
                                name.clone(),
                                MachineProcessOut {
                                    pid: process.pid,
                                    phase: phase_word(&process.phase),
                                    since: phase_since(&process.phase),
                                    reason,
                                },
                            )
                        })
                        .collect(),
                    ports: worktree.ports.clone(),
                    claimed: worktree.claimed.iter().copied().collect(),
                    clashes: clashes
                        .get(&(summary.id.clone(), worktree.name.clone()))
                        .map(|ports| ports.iter().copied().collect())
                        .unwrap_or_default(),
                })
                .collect(),
        })
        .collect();
    let output = MachineOutput {
        version: JSON_VERSION,
        projects,
    };
    writeln!(out, "{}", serde_json::to_string_pretty(&output)?)?;
    Ok(())
}
