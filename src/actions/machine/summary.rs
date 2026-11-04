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
    /// pando may not read its directory under the home: `moved` too, so
    /// never acted on, and said as unreadable, since it may not have moved.
    pub unreadable: bool,
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
    /// Whether pando knows where its repository is, or was: a project
    /// recovered from a state whose every checkout is gone has no root.
    pub fn located(&self) -> bool {
        !self.root.as_os_str().is_empty()
    }

    /// What gives an unreadable project back: its directory under the
    /// home made the developer's to read again — `chmod` when its mode
    /// took that away, `chown` when a run under `sudo` left it root's.
    /// Both are named, the one that applies plain to whoever reads it.
    pub fn read_again(&self) -> String {
        format!(
            "pando may not read projects/{} in pando's home — `chmod u+rwx` it there, or \
             `chown` it back to you if a run under `sudo` left it root's",
            self.id
        )
    }

    /// What its line says instead of where it is: why it is not located.
    /// How to bring it back is [`Self::find_again`].
    pub fn not_located(&self) -> &'static str {
        match self.unreadable {
            true => "cannot be read",
            false => "not located",
        }
    }

    /// What brings back a project nothing locates, or one pando may not
    /// read: its repository is registered by any pando run there while it
    /// is where it was, and adopted where it moved to as
    /// [`Self::adopt_hint`] says; an unreadable one as [`Self::read_again`] does.
    pub fn find_again(&self) -> String {
        match self.unreadable {
            true => self.read_again(),
            false => format!(
                "run pando in its repository; if it moved, {}",
                self.adopt_hint()
            ),
        }
    }

    /// What adopts a moved project where it is now. `doctor --adopt`
    /// takes a repository whose folder kept its name, never a renamed
    /// one nor a folder of repositories: pando run there starts those
    /// afresh.
    pub fn adopt_hint(&self) -> String {
        if !self.repos.is_empty() {
            return "a moved folder of repositories is not adopted: pando run where it is now \
                    starts it afresh"
                .to_string();
        }
        let folder = self
            .id
            .rsplit_once('-')
            .map_or(self.id.as_str(), |(n, _)| n);
        format!(
            "`pando doctor --adopt {}` run where it is now adopts it while its folder is still \
             called {folder}; renamed, pando there starts it afresh",
            self.id
        )
    }

    /// The word for a project nothing is done to: `unreadable`, or `moved`.
    pub fn hands_off_word(&self) -> &'static str {
        match self.unreadable {
            true => "unreadable",
            false => "moved",
        }
    }

    /// The folder its repository sits in, when another of `all` has its
    /// name: two clones of one repository, told apart in a list too
    /// narrow for their paths. `None` when its name is its own, or
    /// nothing locates it.
    pub fn twin_folder(&self, all: &[ProjectSummary]) -> Option<TwinFolder> {
        let folder_of = |s: &ProjectSummary| -> Option<String> {
            Some(s.root.parent()?.file_name()?.to_string_lossy().into_owned())
        };
        let twins: Vec<&ProjectSummary> = all
            .iter()
            .filter(|other| other.id != self.id && other.name == self.name)
            .collect();
        if twins.is_empty() {
            return None;
        }
        let folder = folder_of(self)?;
        let shared = twins
            .iter()
            .filter_map(|other| folder_of(other))
            .map(|other| {
                folder
                    .chars()
                    .zip(other.chars())
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .max()
            .unwrap_or(0);
        Some(TwinFolder { folder, shared })
    }

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

/// The folder a project's repository sits in, where another project has
/// its name, and how far it reads the same as theirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TwinFolder {
    pub folder: String,
    /// The characters its start shares with the most alike of the other
    /// projects' folders: where it first tells itself apart.
    pub shared: usize,
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
/// The live process groups of every project are scanned in one pass. An
/// error when the home's `projects/` cannot be read; a project that cannot
/// be is listed with a warning.
pub fn summaries(home: &std::path::Path) -> anyhow::Result<Vec<ProjectSummary>> {
    let projects = known(home)?;
    let loaded: Vec<(KnownProject, Option<Result<state::State, String>>)> = projects
        .into_iter()
        .map(|project| {
            let file = project.state_file();
            // Not there is nothing recorded; anything else, a file that
            // cannot be read, is said.
            let store = match std::fs::metadata(&file) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Ok(meta) if !meta.is_file() => None,
                _ => Some(state::load(&file).map_err(|e| format!("{e:#}"))),
            };
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
    Ok(out)
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
        unreadable: project.unreadable,
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

/// The ids of the projects one of whose ports another project claims
/// too ([`port_clashes`]).
pub fn clashing_projects(summaries: &[ProjectSummary]) -> BTreeSet<String> {
    port_clashes(summaries)
        .into_values()
        .flatten()
        .map(|(id, _)| id)
        .collect()
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
