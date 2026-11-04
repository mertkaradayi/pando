//! What each known project has up, read the way every reader reads
//! state: nothing saved, nothing signalled, no lock waited for or made.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::time::SystemTime;

use crate::state::{self, Aggregate, Phase};

use super::known::{KnownProject, known};
use crate::actions::refresh::{observe, scan_all};

/// One project, as a view of every project shows it.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    /// A multi-repo project's repositories; empty for one repository.
    pub repos: Vec<String>,
    /// Its repository is gone or now another project's: never opened or
    /// acted on, though what of it still runs is counted as running.
    pub moved: bool,
    /// Worktrees whose every process is running.
    pub up: usize,
    /// Worktrees with a process still coming up, none failed.
    pub starting: usize,
    /// Worktrees with a process that failed.
    pub failed: usize,
    /// Recorded worktrees running nothing.
    pub stopped: usize,
    /// Every worktree with something recorded running or failed, by name.
    pub worktrees: Vec<LiveWorktree>,
    /// When its state was last written: the last time pando did something
    /// in it. `None` for a project with no state file.
    pub last_used: Option<SystemTime>,
    /// Why its state could not be read, when it could not.
    pub warning: Option<String>,
}

impl ProjectSummary {
    /// Whether anything of it is up or coming up.
    pub fn is_live(&self) -> bool {
        self.up + self.starting > 0
    }

    /// Where it sits in a list of every project: live, moved or not, then
    /// failed, then idle, then moved. A moved project's processes run on
    /// in its worktrees, which are under pando's home, not its repository.
    pub fn rank(&self) -> u8 {
        match () {
            _ if self.is_live() => 0,
            _ if self.moved => 3,
            _ if self.failed > 0 => 1,
            _ => 2,
        }
    }
}

/// A worktree with something recorded up, coming up, or failed.
#[derive(Debug, Clone)]
pub struct LiveWorktree {
    pub name: String,
    pub path: PathBuf,
    /// Its processes as a whole: starting, running or failed.
    pub phase: Aggregate,
    /// Each process, by name.
    pub processes: BTreeMap<String, LiveProcess>,
    /// The ports pando gave it, by role.
    pub ports: BTreeMap<String, u16>,
    /// Every port it claims: its roles' ports, what its processes were
    /// seen listening on, and its own services'.
    pub claimed: BTreeSet<u16>,
}

impl LiveWorktree {
    /// Whether its processes are up or coming up, so its ports are taken.
    pub fn is_up(&self) -> bool {
        !matches!(self.phase, Aggregate::Failed { .. })
    }
}

/// One process of a live worktree.
#[derive(Debug, Clone)]
pub struct LiveProcess {
    pub pid: u32,
    pub phase: Phase,
}

/// Every project under `home`, each with what it has up, in the order a
/// list of them is shown: live first (moved or not), then failed, then
/// idle, then moved;
/// within each the most recently used first, then by id.
///
/// The live process groups of every project are scanned in one pass.
pub fn summaries(home: &std::path::Path) -> Vec<ProjectSummary> {
    let projects = known(home);
    let loaded: Vec<(KnownProject, Option<Result<state::State, String>>)> = projects
        .into_iter()
        .map(|project| {
            let file = project.state_file();
            let store = file
                .is_file()
                .then(|| state::load(&file).map_err(|e| format!("{e:#}")));
            (project, store)
        })
        .collect();
    let stores: Vec<&state::State> = loaded
        .iter()
        .filter_map(|(_, store)| store.as_ref()?.as_ref().ok())
        .collect();
    let scans = scan_all(&stores);
    let mut out: Vec<ProjectSummary> = loaded
        .into_iter()
        .map(|(project, store)| summarise(project, store, &scans))
        .collect();
    out.sort_by(|a, b| {
        a.rank()
            .cmp(&b.rank())
            .then(b.last_used.cmp(&a.last_used))
            .then(a.id.cmp(&b.id))
    });
    out
}

fn summarise(
    project: KnownProject,
    store: Option<Result<state::State, String>>,
    scans: &crate::actions::Scans,
) -> ProjectSummary {
    let last_used = std::fs::metadata(project.state_file())
        .and_then(|meta| meta.modified())
        .ok();
    let mut summary = ProjectSummary {
        name: project.registered.name.clone(),
        root: project.registered.root.clone(),
        repos: project.registered.repos.clone(),
        moved: project.moved,
        up: 0,
        starting: 0,
        failed: 0,
        stopped: 0,
        worktrees: Vec::new(),
        last_used,
        warning: None,
        id: project.id.clone(),
    };
    let seen = match store {
        None => return summary,
        Some(Err(warning)) => {
            summary.warning = Some(warning);
            return summary;
        }
        Some(Ok(seen)) => seen,
    };
    let observed = observe(seen, &project.state_file(), &project.lock_file(), scans);
    for (name, record) in observed.state.worktrees {
        // `pando check`'s throwaway worktree is no worktree of the
        // project's.
        if name == crate::paths::CHECK_WORKTREE {
            continue;
        }
        let Some(phase) = state::aggregate_phase(&record) else {
            summary.stopped += 1;
            continue;
        };
        match phase {
            Aggregate::Running { .. } => summary.up += 1,
            Aggregate::Starting { .. } => summary.starting += 1,
            Aggregate::Failed { .. } => summary.failed += 1,
        }
        let mut claimed: BTreeSet<u16> = record.ports.values().copied().collect();
        claimed.extend(record.observed_ports.iter().copied());
        claimed.extend(record.services.iter().filter_map(|s| s.port));
        summary.worktrees.push(LiveWorktree {
            name,
            path: record.path,
            phase,
            processes: record
                .processes
                .into_iter()
                .map(|(name, p)| {
                    (
                        name,
                        LiveProcess {
                            pid: p.pid,
                            phase: p.phase,
                        },
                    )
                })
                .collect(),
            ports: record.ports,
            claimed,
        });
    }
    summary
}

/// The ports live worktrees of two or more projects claim, each with
/// every `(project id, worktree)` claiming it. Shown, never resolved. A
/// moved project's live worktrees count: a process holds its port
/// wherever its repository went.
pub fn port_clashes(summaries: &[ProjectSummary]) -> BTreeMap<u16, Vec<(String, String)>> {
    let mut claims: BTreeMap<u16, Vec<(String, String)>> = BTreeMap::new();
    for summary in summaries {
        for worktree in summary.worktrees.iter().filter(|w| w.is_up()) {
            for port in &worktree.claimed {
                claims
                    .entry(*port)
                    .or_default()
                    .push((summary.id.clone(), worktree.name.clone()));
            }
        }
    }
    claims.retain(|_, by| {
        let projects: BTreeSet<&str> = by.iter().map(|(id, _)| id.as_str()).collect();
        projects.len() > 1
    });
    claims
}
