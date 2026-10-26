//! `ls`: the worktree table, fitted to the terminal, and its JSON.

use super::JSON_VERSION;
use super::report_refresh;
use super::short_head;
use crate::actions;
use crate::actions::{url_owner_not_running, worktree_url};
use crate::cache;
use crate::paths::PandoPaths;
use crate::state::{Aggregate, ServiceMode, WorktreeRecord};
use crate::term::{Paint, Style, ellipsize_distinct};
use crate::worktree::{PrState, Worktree};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;

/// How the text listing is drawn.
#[derive(Debug, Clone)]
pub struct LsView {
    /// Columns available; `usize::MAX` when stdout is not a terminal.
    pub width: usize,
    /// `-l`: add the sha and the full path.
    pub long: bool,
    pub style: Style,
}

impl LsView {
    /// No colour, no `~`, the default columns: what the tests read.
    pub fn plain(width: usize) -> Self {
        Self {
            width,
            long: false,
            style: Style::plain(),
        }
    }
}

/// The listing for a person at a terminal: its width, its colour, `~`.
pub fn ls_text<W: Write>(paths: &PandoPaths, out: &mut W, long: bool) -> Result<()> {
    let view = LsView {
        width: terminal_width(),
        long,
        style: Style::for_stdout(),
    };
    ls_text_with(paths, out, &view)
}

/// Columns of the text listing, in the order they are printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Col {
    Name,
    Status,
    Url,
    Ports,
    Mode,
    Public,
    Git,
    Branch,
    Head,
    Path,
}

impl Col {
    fn header(self) -> &'static str {
        match self {
            Col::Name => "NAME",
            Col::Status => "STATUS",
            Col::Url => "URL",
            Col::Ports => "PORTS",
            Col::Mode => "MODE",
            Col::Public => "PUBLIC",
            Col::Git => "GIT",
            Col::Branch => "BRANCH",
            Col::Head => "HEAD",
            Col::Path => "PATH",
        }
    }
}

pub(super) const ORDER: [Col; 10] = [
    Col::Name,
    Col::Status,
    Col::Url,
    Col::Ports,
    Col::Mode,
    Col::Public,
    Col::Git,
    Col::Branch,
    Col::Head,
    Col::Path,
];

/// What a narrowing terminal gives up, in order. The name is the
/// identifier and the status is the question the listing exists to
/// answer, so neither ever goes; the URL is what somebody came to copy, so
/// it outlives everything but them. A public URL is first shortened to
/// `yes` rather than dropped: that it is public is the fact that matters,
/// and `pando status` has the address. A long name is cut to a readable
/// width before the ports and the URL go, and only cut further after.
const SHED_ORDER: [Shed; 12] = [
    Shed::Drop(Col::Path),
    Shed::Drop(Col::Head),
    Shed::Compact(Col::Public),
    Shed::Drop(Col::Branch),
    Shed::Drop(Col::Mode),
    Shed::Drop(Col::Git),
    Shed::NameTo(NAME_READABLE),
    Shed::Drop(Col::Ports),
    Shed::Drop(Col::Public),
    Shed::NameTo(NAME_SHORT),
    Shed::Drop(Col::Url),
    Shed::NameTo(NAME_FLOOR),
];

/// One step of fitting the listing to a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shed {
    Drop(Col),
    /// Swap the column for its short form.
    Compact(Col),
    /// Cut the name column by as much as the row is over, to no less than
    /// this.
    NameTo(usize),
}

/// Two spaces between columns, so a value with a space in it still reads as
/// one cell.
pub(super) const COL_GAP: usize = 2;

/// How far a name is cut before anything that matters goes: enough of a
/// branch name to tell it from its neighbours.
const NAME_READABLE: usize = 24;

/// How far a name is cut to keep the URL.
const NAME_SHORT: usize = 16;

/// The narrowest a name is ever cut to, here and in `status`.
pub(super) const NAME_FLOOR: usize = 12;

/// Which columns fit in `width`, given each column's width. Dropping is
/// all-or-nothing per column, so every column keeps one straight edge.
pub fn keep_columns(width: usize, widths: &BTreeMap<Col, usize>) -> Vec<Col> {
    fit(width, widths, &BTreeMap::new()).kept
}

/// What fitting decided.
#[derive(Debug)]
struct Fit {
    kept: Vec<Col>,
    /// Columns swapped for their short form.
    compacted: Vec<Col>,
    /// Every column's width, with the short forms and the name's cut in.
    widths: BTreeMap<Col, usize>,
}

/// [`keep_columns`], with the short forms some columns have, and the name
/// allowed to be cut.
fn fit(width: usize, widths: &BTreeMap<Col, usize>, compact: &BTreeMap<Col, usize>) -> Fit {
    let mut fit = Fit {
        kept: ORDER
            .iter()
            .copied()
            .filter(|c| widths.contains_key(c))
            .collect(),
        compacted: Vec::new(),
        widths: widths.clone(),
    };
    for step in SHED_ORDER {
        let over = row_width(&fit.kept, &fit.widths).saturating_sub(width);
        if over == 0 {
            break;
        }
        match step {
            Shed::Drop(col) => fit.kept.retain(|c| *c != col),
            Shed::Compact(col) => {
                if let Some(short) = compact.get(&col)
                    && fit.kept.contains(&col)
                {
                    fit.compacted.push(col);
                    fit.widths.insert(col, *short);
                }
            }
            Shed::NameTo(floor) => {
                if let Some(name) = fit.widths.get_mut(&Col::Name) {
                    *name = (*name).min(name.saturating_sub(over).max(floor));
                }
            }
        }
    }
    fit
}

fn row_width(kept: &[Col], widths: &BTreeMap<Col, usize>) -> usize {
    let sum: usize = kept
        .iter()
        .map(|c| widths.get(c).copied().unwrap_or(0))
        .sum();
    sum + COL_GAP * kept.len().saturating_sub(1)
}

/// The terminal's width, or "as wide as you like" when the output is not a
/// terminal — a listing being piped into a file should not lose columns
/// because the window happened to be narrow.
pub(super) fn terminal_width() -> usize {
    use std::io::IsTerminal;
    width_from(
        std::io::stdout().is_terminal(),
        std::env::var("COLUMNS").ok().as_deref(),
        crossterm::terminal::size().ok().map(|(cols, _)| cols),
    )
}

/// The width decision, with what it reads passed in. `COLUMNS`, when it
/// is a number, is the width a person asked for, as it is for `ls`. A
/// terminal that reports zero columns — a pty nobody sized, which some
/// CI runners and `script` hand out — is not a zero-wide screen: fitting
/// to it shed every column but the name and status and cut the name to
/// its floor.
pub(super) fn width_from(terminal: bool, columns: Option<&str>, reported: Option<u16>) -> usize {
    if !terminal {
        return usize::MAX;
    }
    let asked = columns
        .and_then(|c| c.trim().parse::<usize>().ok())
        .filter(|c| *c > 0);
    asked
        .or(reported.filter(|c| *c > 0).map(usize::from))
        .unwrap_or(80)
}

/// One cell: its text, and how it is painted on a terminal.
#[derive(Debug, Clone)]
struct Cell {
    text: String,
    paint: Option<Paint>,
}

impl Cell {
    fn new(text: impl Into<String>, paint: Option<Paint>) -> Self {
        Self {
            text: text.into(),
            paint,
        }
    }

    fn dash() -> Self {
        Self::new("-", Some(Paint::Faint))
    }

    fn width(&self) -> usize {
        crate::term::text_width(&self.text)
    }
}

/// [`ls_text_with`] in the plain view at `width`.
pub fn ls_text_at<W: Write>(paths: &PandoPaths, out: &mut W, width: usize) -> Result<()> {
    ls_text_with(paths, out, &LsView::plain(width))
}

pub fn ls_text_with<W: Write>(paths: &PandoPaths, out: &mut W, view: &LsView) -> Result<()> {
    // The listing forks git and the refresh scans sockets; neither needs
    // the other, so they run side by side.
    let (listing, refreshed) = std::thread::scope(|scope| {
        let refreshing = scope.spawn(|| actions::refresh(paths));
        let listing = actions::ls_all(paths);
        (listing, refreshing.join().unwrap_or_default())
    });
    let listing = listing?;
    report_refresh(&refreshed);
    let owned = actions::ownership(&refreshed.state, &listing.worktrees);
    // A project with no worktree whose main checkout pando never ran has
    // nothing to list but the way to start: the listing a first run gets.
    if listing.worktrees.is_empty() && !refreshed.state.worktrees.contains_key(&listing.main.name) {
        writeln!(out, "no worktrees — `pando new <branch>` creates one")?;
        return Ok(());
    }
    // The main checkout first: pando runs it too, and it is the one place
    // every worktree came from.
    let main_name = listing.main.name.clone();
    let worktrees: Vec<Worktree> = std::iter::once(listing.main)
        .chain(listing.worktrees)
        .collect();

    let records: Vec<Option<&WorktreeRecord>> = worktrees
        .iter()
        .map(|w| refreshed.state.worktrees.get(&w.name))
        .collect();
    // A column earns its place when some row has something to say in it:
    // a project that never leaves the shared services has no MODE, one
    // with nothing shared has no PUBLIC, and one whose every worktree is
    // named for its branch has no BRANCH — the name already said it.
    let any_own_data = records
        .iter()
        .flatten()
        .any(|r| r.mode() != ServiceMode::Shared);
    let any_shared = records.iter().flatten().any(|r| r.share.is_some());
    // The main checkout is named by its branch here, as the TUI names
    // it, so its directory's name never brings the column on its own.
    let any_renamed = worktrees
        .iter()
        .any(|w| w.name != main_name && !named_for_branch(w));

    let mut rows: Vec<BTreeMap<Col, Cell>> = Vec::new();
    let mut compact_rows: Vec<BTreeMap<Col, Cell>> = Vec::new();
    for (w, record) in worktrees.iter().zip(records.iter().copied()) {
        let created = owned.get(&w.name).copied().unwrap_or(false);
        let aggregate = record.and_then(crate::state::aggregate_phase);
        let is_main = w.name == main_name;
        let name = match (&w.branch, is_main) {
            (Some(branch), true) => branch.clone(),
            _ => display_name(w),
        };
        let mut row = BTreeMap::from([
            (Col::Name, Cell::new(name, None)),
            (Col::Status, status_cell(record, aggregate.as_ref())),
            (Col::Url, url_cell(record, aggregate.as_ref())),
            (Col::Ports, ports_cell(record, aggregate.as_ref())),
            (Col::Git, git_cell(w, created, is_main)),
        ]);
        if any_own_data {
            row.insert(Col::Mode, mode_cell(record));
        }
        let mut compact = BTreeMap::new();
        if any_shared {
            let share = record.and_then(|r| r.share.as_ref());
            let (full, short) = match share {
                Some(share) => (
                    Cell::new(&share.public_url, Some(Paint::Link)),
                    Cell::new("yes", Some(Paint::Good)),
                ),
                None => (Cell::dash(), Cell::dash()),
            };
            row.insert(Col::Public, full);
            compact.insert(Col::Public, short);
        }
        if any_renamed {
            let branch = match (named_for_branch(w) || is_main, &w.branch) {
                (true, _) => Cell::new("", None),
                (false, Some(branch)) => Cell::new(branch, None),
                (false, None) if w.is_mixed() => Cell::new(w.branch_shown(), None),
                (false, None) => Cell::new("(detached)", Some(Paint::Warn)),
            };
            row.insert(Col::Branch, branch);
        }
        if view.long {
            let head = match &w.head_sha {
                Some(sha) => Cell::new(sha, None),
                None => Cell::dash(),
            };
            row.insert(Col::Head, head);
            let path = view.style.tilde(&w.path.display().to_string());
            row.insert(Col::Path, Cell::new(path, None));
        }
        rows.push(row);
        compact_rows.push(compact);
    }

    let widths_of = |rows: &[BTreeMap<Col, Cell>]| {
        let mut widths: BTreeMap<Col, usize> = BTreeMap::new();
        for col in ORDER {
            let widest = rows
                .iter()
                .filter_map(|r| r.get(&col).map(Cell::width))
                .max();
            if let Some(widest) = widest {
                widths.insert(col, widest.max(crate::term::text_width(col.header())));
            }
        }
        widths
    };
    let Fit {
        kept,
        compacted,
        widths,
    } = fit(view.width, &widths_of(&rows), &widths_of(&compact_rows));
    for col in &compacted {
        for (row, compact) in rows.iter_mut().zip(&compact_rows) {
            row.insert(*col, compact[col].clone());
        }
    }
    // A name cut to fit is cut where it differs from its neighbours, so two
    // long branch names that share a prefix still read as two names. Never
    // when stdout is not a terminal, whose width is unbounded.
    let all_names: Vec<String> = rows.iter().map(|r| r[&Col::Name].text.clone()).collect();
    for row in &mut rows {
        let cell = row.get_mut(&Col::Name).expect("every row has a name");
        cell.text = ellipsize_distinct(&cell.text, &all_names, widths[&Col::Name]);
    }

    let header = |col: Col| Cell::new(col.header(), Some(Paint::Heading));
    writeln!(out, "{}", render_row(&kept, &widths, &view.style, header))?;
    for row in &rows {
        let line = render_row(&kept, &widths, &view.style, |col| row[&col].clone());
        writeln!(out, "{line}")?;
    }
    Ok(())
}

/// Pads every cell but the last, so a trailing column never carries spaces
/// to the end of the line. Padding is measured on the text and colour goes
/// around the text only, so an escape never skews a column.
fn render_row(
    kept: &[Col],
    widths: &BTreeMap<Col, usize>,
    style: &Style,
    cell: impl Fn(Col) -> Cell,
) -> String {
    let mut line = String::new();
    for (i, col) in kept.iter().enumerate() {
        let cell = cell(*col);
        match cell.paint {
            Some(paint) => line.push_str(&style.paint(&cell.text, paint)),
            None => line.push_str(&cell.text),
        }
        if i + 1 < kept.len() {
            let width = widths.get(col).copied().unwrap_or(0);
            line.push_str(&" ".repeat(width.saturating_sub(cell.width()) + COL_GAP));
        }
    }
    line.trim_end().to_string()
}

/// Whether the worktree's directory is its branch spelled as a directory:
/// then the branch *is* the name, and the listing says it once.
fn named_for_branch(w: &Worktree) -> bool {
    w.named_for_branch()
}

/// The name a person knows a worktree by — see [`Worktree::display_name`].
pub(super) fn display_name(w: &Worktree) -> String {
    w.display_name()
}

/// One word for a whole worktree, however many processes it runs: the
/// aggregate, so a row never reads `running` while one of its processes is
/// dead — and, for a failed one that runs several, which of them.
fn status_cell(record: Option<&WorktreeRecord>, aggregate: Option<&Aggregate>) -> Cell {
    match aggregate {
        None => Cell::new("stopped", Some(Paint::Faint)),
        Some(Aggregate::Running { .. }) => Cell::new("running", Some(Paint::Good)),
        Some(Aggregate::Starting { .. }) => Cell::new("starting", Some(Paint::Warn)),
        Some(Aggregate::Failed { process, .. }) => {
            match record.is_some_and(|r| r.processes.len() > 1) {
                true => Cell::new(format!("failed ({process})"), Some(Paint::Bad)),
                false => Cell::new("failed", Some(Paint::Bad)),
            }
        }
    }
}

/// The URL, while something is up to answer it. A stopped worktree keeps
/// its ports and so could name one, but a link that goes nowhere is not
/// worth the column — nor is one whose own process is stopped while a
/// sibling runs.
fn url_cell(record: Option<&WorktreeRecord>, aggregate: Option<&Aggregate>) -> Cell {
    let up = matches!(
        aggregate,
        Some(Aggregate::Running { .. } | Aggregate::Starting { .. })
    ) && record.is_some_and(|r| url_owner_not_running(r).is_none());
    match record.and_then(worktree_url) {
        Some(url) if up => Cell::new(url, Some(Paint::Link)),
        _ => Cell::dash(),
    }
}

/// The ports column: `role:port` for each, one or many — a lone bare
/// number beside another row's `api:29496 web:29497` read as a different
/// kind of thing. Faint when nothing runs on them: they are still this
/// worktree's, and will be again on the next start.
fn ports_cell(record: Option<&WorktreeRecord>, aggregate: Option<&Aggregate>) -> Cell {
    let Some(record) = record.filter(|r| !r.ports.is_empty()) else {
        return Cell::dash();
    };
    let text = record
        .ports
        .iter()
        .map(|(role, port)| format!("{role}:{port}"))
        .collect::<Vec<_>>()
        .join(" ");
    Cell::new(text, aggregate.is_none().then_some(Paint::Faint))
}

/// Which services the worktree talks to, in the words `start --isolated`,
/// `start --namespaced` and `start --shared` use.
fn mode_cell(record: Option<&WorktreeRecord>) -> Cell {
    match record.map(WorktreeRecord::mode) {
        Some(ServiceMode::Shared) => Cell::new("shared", Some(Paint::Faint)),
        Some(mode) => Cell::new(mode.word(), None),
        None => Cell::dash(),
    }
}

/// What git has to say, in git's own words, ordered by how much it should
/// stop you — a prunable or locked entry, then uncommitted changes — then how far it is ahead of and behind the
/// base branch, and
/// whether pando made it: `rm` asks before removing one it did not, and
/// never removes the main checkout, which the row says it is.
fn git_cell(w: &Worktree, created_by_pando: bool, main: bool) -> Cell {
    let (word, paint) = if w.prunable {
        ("prunable", Paint::Bad)
    } else if w.locked {
        ("locked", Paint::Bad)
    } else {
        match w.dirty {
            Some(true) => ("uncommitted", Paint::Warn),
            Some(false) => ("clean", Paint::Faint),
            None => ("?", Paint::Faint),
        }
    };
    let mut text = word.to_string();
    if let Some((ahead, behind)) = w.ahead_behind {
        if ahead > 0 {
            text.push_str(&format!(" ↑{ahead}"));
        }
        if behind > 0 {
            text.push_str(&format!(" ↓{behind}"));
        }
    }
    if main {
        text.push_str(" main checkout");
    } else if !created_by_pando {
        text.push_str(" adopted");
    }
    Cell::new(text, Some(paint))
}

#[derive(Serialize)]
struct LsOutput {
    version: u32,
    project: ProjectOut,
    worktrees: Vec<WorktreeOut>,
}

#[derive(Serialize)]
pub(super) struct ProjectOut {
    pub(super) id: String,
    pub(super) root: String,
    pub(super) name: String,
    /// A multi-repo project's repositories, by directory name under
    /// `root`; absent for a project that is one repository.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(super) repos: Vec<String>,
}

impl ProjectOut {
    pub(super) fn of(project: &crate::project::ProjectRef) -> Self {
        Self {
            id: project.id.clone(),
            root: project.root.display().to_string(),
            name: project.display_name.clone(),
            repos: project.repos.clone(),
        }
    }
}

#[derive(Serialize)]
struct WorktreeOut {
    name: String,
    /// The main checkout: always the first entry, never pando's to remove.
    main: bool,
    path: String,
    branch: Option<String>,
    head: Option<String>,
    detached: bool,
    /// `null` when git could not be asked.
    dirty: Option<bool>,
    ahead: Option<u32>,
    behind: Option<u32>,
    created_by_pando: bool,
    /// Which services it talks to: `shared`, `namespaced` or `isolated`.
    mode: ServiceMode,
    prunable: bool,
    /// `null` when unlocked; the lock reason, possibly empty, when locked.
    locked: Option<String>,
    pr: Option<PrOut>,
    /// A multi-repo project's checkout is a set: each repository's
    /// checkout in it. Absent for a project that is one repository.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    repos: Vec<RepoOut>,
}

/// One repository's checkout in a set, as `git` describes it.
#[derive(Serialize)]
struct RepoOut {
    name: String,
    path: String,
    branch: Option<String>,
    head: Option<String>,
    detached: bool,
    dirty: Option<bool>,
    ahead: Option<u32>,
    behind: Option<u32>,
    prunable: bool,
}

impl RepoOut {
    fn of(checkout: &Worktree) -> Self {
        Self {
            name: checkout.name.clone(),
            path: checkout.path.display().to_string(),
            branch: checkout.branch.clone(),
            head: short_head(checkout),
            detached: checkout.detached,
            dirty: checkout.dirty,
            ahead: checkout.ahead_behind.map(|(a, _)| a),
            behind: checkout.ahead_behind.map(|(_, b)| b),
            prunable: checkout.prunable,
        }
    }
}

#[derive(Serialize)]
struct PrOut {
    number: u32,
    state: PrState,
    url: String,
}

pub fn ls_json<W: Write>(paths: &PandoPaths, out: &mut W) -> Result<()> {
    // Side by side, as for the text.
    let (listing, refreshed) = std::thread::scope(|scope| {
        let refreshing = scope.spawn(|| actions::refresh(paths));
        let listing = actions::ls_all(paths);
        (listing, refreshing.join().unwrap_or_default())
    });
    let listing = listing?;
    // A share the refresh closed or a service it forgot is saved as gone,
    // so this is the one run that can say so.
    report_refresh(&refreshed);
    let owned = actions::ownership(&refreshed.state, &listing.worktrees);
    // The main checkout first, always: a program sees everything pando
    // can run, and `main` tells it which one is not a worktree.
    let main_name = listing.main.name.clone();
    let worktrees: Vec<Worktree> = std::iter::once(listing.main)
        .chain(listing.worktrees)
        .collect();
    // Cache only: the CLI never spawns `gh`, so `ls --json` stays fast and
    // works offline. The TUI is what refreshes this.
    let prs = cache::load_prs(&paths.pr_cache_file());

    let output = LsOutput {
        version: JSON_VERSION,
        project: ProjectOut::of(&paths.project),
        worktrees: worktrees
            .into_iter()
            .map(|w| {
                let head = short_head(&w);
                let pr = w
                    .branch
                    .as_deref()
                    .and_then(|b| prs.prs.get(b))
                    .map(|p| PrOut {
                        number: p.number,
                        state: p.state,
                        url: p.url.clone(),
                    });
                let mode = refreshed
                    .state
                    .worktrees
                    .get(&w.name)
                    .map(WorktreeRecord::mode)
                    .unwrap_or_default();
                WorktreeOut {
                    repos: w.repos.iter().map(RepoOut::of).collect(),
                    created_by_pando: owned.get(&w.name).copied().unwrap_or(false),
                    mode,
                    main: w.name == main_name,
                    name: w.name,
                    path: w.path.display().to_string(),
                    branch: w.branch,
                    head,
                    detached: w.detached,
                    dirty: w.dirty,
                    ahead: w.ahead_behind.map(|(a, _)| a),
                    behind: w.ahead_behind.map(|(_, b)| b),
                    prunable: w.prunable,
                    locked: if w.locked {
                        Some(w.lock_reason.unwrap_or_default())
                    } else {
                        None
                    },
                    pr,
                }
            })
            .collect(),
    };
    writeln!(out, "{}", serde_json::to_string_pretty(&output)?)?;
    Ok(())
}
