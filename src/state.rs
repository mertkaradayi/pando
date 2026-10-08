//! Persistent state: what pando started, what it owns, and what it observed.
//!
//! Two rules are inherited from the origin tool unchanged: every mutation
//! holds the flock, and every read path reconciles before trusting what is
//! in the map. State is v2 from the first commit — there is no v1 to migrate.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::platform::boot::Boot;
use crate::platform::process::Group;

pub const STATE_VERSION: u32 = 2;

/// How long a process may sit in `Starting` before it is called failed.
pub const START_TIMEOUT_SECS: i64 = 30;

/// What one look at a watched port could say.
///
/// Three answers, not two. "Nothing is bound there" and "pando could not
/// find out" are different facts: the scan of a group's sockets runs
/// behind a deadline, and on a loaded machine it can miss it again and
/// again while the server is perfectly healthy. Folding the second into
/// the first failed healthy servers with "nothing bound port N" — the
/// opposite of what had happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortCheck {
    Bound,
    NotBound,
    /// The scan could not run, or ran out of time, and nothing else
    /// could say either way.
    Unknown,
}

impl From<bool> for PortCheck {
    fn from(bound: bool) -> Self {
        match bound {
            true => PortCheck::Bound,
            false => PortCheck::NotBound,
        }
    }
}

/// How much longer than its own window a process may stay `Starting`
/// while pando cannot get an answer about its port: the window again, and
/// never less than [`START_TIMEOUT_SECS`]. Bounded, because a machine
/// with no working scan at all must still end the wait some time.
pub fn unconfirmed_grace_secs(timeout_secs: i64) -> i64 {
    timeout_secs.max(START_TIMEOUT_SECS)
}

/// The longest a process with this readiness window can stay `Starting`:
/// its window, plus the grace an unanswerable port scan earns it.
///
/// What anything that *waits* for readiness — `start --wait`, `share` —
/// has to wait for. A wait bounded by the window alone gives up while the
/// phase machine is still, correctly, waiting on a healthy server.
pub fn longest_starting_secs(timeout_secs: i64) -> i64 {
    timeout_secs.saturating_add(unconfirmed_grace_secs(timeout_secs))
}

/// The reason written for a process that is simply gone.
///
/// A constant because it is half a sentence: this is what the phase knows,
/// and `actions::explain_new_failures` — which can read the log and the
/// status the shell recorded — finishes it. Matching on the wrong spelling
/// there would silently stop every failure being explained.
pub const EXITED: &str = "process exited";

/// How the reason for a process that outlived its readiness wait with
/// nothing bound begins: the same half-sentence, finished by
/// `actions::explain_new_failures`, which may say to wait longer.
pub const NOTHING_BOUND: &str = "timeout: nothing bound port";

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub version: u32,
    pub worktrees: BTreeMap<String, WorktreeRecord>,
}

/// Everything pando knows about one worktree. Records exist for worktrees
/// pando created even when nothing is running — `created_by_pando` is what
/// lets `rm` tell an adopted worktree from one of its own.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRecord {
    pub path: PathBuf,
    #[serde(default)]
    pub created_by_pando: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub processes: BTreeMap<String, ProcessRecord>,
    /// Role name to allocated port. Survives a stop: a stopped worktree
    /// still owns its ports.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ports: BTreeMap<String, u16>,
    /// Which process owns which roles, as config declared them at the last
    /// start. Process name to its roles, in the order it declared them.
    ///
    /// Written for every process of the worktree, whatever `--only` asked
    /// for, and it survives a stop exactly as `ports` does. Without it the
    /// URL rule — the `web` role, else the first role of the alphabetically
    /// first process — is only answerable while something is running, and
    /// `start` and a later `status` would disagree about a worktree that
    /// has since been stopped.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub roles: BTreeMap<String, Vec<String>>,
    /// The processes whose port no browser opens, as config said at the
    /// last start (`ProcessConfig::serves_page`): the URL is never one of
    /// theirs. Written and kept beside `roles`, for the same reason.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub pageless: std::collections::BTreeSet<String>,
    /// Every listening socket seen across this worktree's process groups:
    /// the union of the per-process lists below.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_ports: Vec<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceRecord>,
    /// Which services this worktree's processes talk to: the mode it runs
    /// in, or last ran in once stopped. `None` for a worktree never
    /// started — and for one 0.3.0 started shared, which it did not write
    /// down. [`WorktreeRecord::mode`] reads it with that default.
    ///
    /// Per start and remembered per worktree: `start --isolated` sets it,
    /// a later plain `start` keeps the services already up, and only `rm`
    /// forgets it. Without the memory, restarting an isolated worktree
    /// would hand its processes the *shared* database's address while its
    /// own database was still running beside it.
    ///
    /// Read from `isolated` too, which is what every 0.3.0 state file
    /// says instead: `true` is isolated, and `false` is nothing written.
    #[serde(
        default,
        alias = "isolated",
        deserialize_with = "mode_or_isolated",
        skip_serializing_if = "Option::is_none"
    )]
    pub mode: Option<ServiceMode>,
    /// The namespaces pando made for this worktree in the project's own
    /// servers: a database, a numbered slot. Recorded the moment a server
    /// made one, and kept through a switch to another mode, so switching
    /// back finds its data; only `rm` drops them, through
    /// `namespace::may_drop`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub namespaces: Vec<NamespaceRecord>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hooks: BTreeMap<String, HookRecord>,
    /// The port this worktree's share proxy listens on, once it has needed
    /// one. Kept beside `ports` rather than in it, and it survives an
    /// unshare exactly as they do, so the number is stable.
    ///
    /// Not a role in `ports`, deliberately. `ports::assign` reuses a
    /// worktree's window only when the recorded roles are exactly the ones
    /// being asked for, so a `share` key would make every later start
    /// decide the window had changed and move every port — under a live
    /// application. It is excluded from the URL rule for the same reason:
    /// no process owns it, and a proxy is not what a worktree serves on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<ShareRecord>,
    /// Shares of this worktree that are still coming up, one per pando
    /// waiting on one. See [`PendingShare`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_shares: Vec<PendingShare>,
    /// When a start last spawned one of its processes. Kept through a
    /// stop, which clears the process records and their times with them,
    /// so the TUI can still order its list by what ran last.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started: Option<DateTime<Utc>>,
    /// The branch this one was made to go onto, when `new` knew it: the
    /// base it forked a new branch from (`origin/release` for `--base
    /// release`), or a pull request's target. What the git menu rebases
    /// onto and merges in, before the config's rule: a branch cut from
    /// `release` rebased onto `main` takes every commit of `release` along.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
}

impl WorktreeRecord {
    /// A record for a worktree with nothing running yet.
    pub fn new(path: impl Into<PathBuf>, created_by_pando: bool) -> Self {
        Self {
            path: path.into(),
            created_by_pando,
            processes: BTreeMap::new(),
            ports: BTreeMap::new(),
            roles: BTreeMap::new(),
            pageless: Default::default(),
            observed_ports: Vec::new(),
            services: Vec::new(),
            mode: None,
            namespaces: Vec::new(),
            hooks: BTreeMap::new(),
            share_port: None,
            share: None,
            pending_shares: Vec::new(),
            last_started: None,
            base: None,
        }
    }

    /// When this worktree last ran: the start that last spawned a process,
    /// or, for a record written before pando kept that, its processes'.
    pub fn last_run(&self) -> Option<DateTime<Utc>> {
        self.processes
            .values()
            .map(|p| p.started_at)
            .chain(self.last_started)
            .max()
    }

    /// The mode this worktree runs in, or last ran in: shared when nothing
    /// says otherwise, because that is where a plain start puts it.
    pub fn mode(&self) -> ServiceMode {
        self.mode.unwrap_or_default()
    }

    /// Whether anything of this worktree's is up: a process that has not
    /// failed, a service with a process behind it, or a tunnel. A crashed
    /// dev server's database counts; a record left behind by a stop, one
    /// whose only process has exited, or the compose records a stopped
    /// isolated worktree keeps for `rm` do not.
    pub fn is_live(&self) -> bool {
        self.processes
            .values()
            .any(|p| !matches!(p.phase, Phase::Failed { .. }))
            || self.services.iter().any(|s| s.pid.is_some())
            || self.share.is_some()
    }
}

/// How a worktree's processes reach the project's stateful services.
///
/// Code, processes, dependencies, ports and logs are the worktree's own in
/// every mode; a mode only decides how the databases and caches are
/// separated.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMode {
    /// The main checkout's servers and the main checkout's data.
    #[default]
    Shared,
    /// The main checkout's servers, and a namespace of its own in each: a
    /// database, a numbered slot.
    Namespaced,
    /// Servers of its own, on ports of its own.
    Isolated,
}

impl ServiceMode {
    /// Every mode, in the order a chooser offers them.
    pub const ALL: [ServiceMode; 3] = [
        ServiceMode::Shared,
        ServiceMode::Namespaced,
        ServiceMode::Isolated,
    ];

    /// The word for it everywhere pando prints one: `status`, `ls`, the
    /// TUI, and `--json`.
    pub fn word(self) -> &'static str {
        match self {
            ServiceMode::Shared => "shared",
            ServiceMode::Namespaced => "namespaced",
            ServiceMode::Isolated => "isolated",
        }
    }
}

/// `mode` as this version writes it, or `isolated` as 0.3.0 wrote it.
///
/// One field under two names rather than two fields, so nothing past the
/// parser ever has to reconcile them.
fn mode_or_isolated<'de, D>(deserializer: D) -> Result<Option<ServiceMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Written {
        Mode(ServiceMode),
        Isolated(bool),
    }
    Ok(match Written::deserialize(deserializer)? {
        Written::Mode(mode) => Some(mode),
        Written::Isolated(true) => Some(ServiceMode::Isolated),
        Written::Isolated(false) => None,
    })
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProcessRecord {
    pub pid: u32,
    pub pgid: Group,
    pub started_at: DateTime<Utc>,
    pub log_path: PathBuf,
    /// The port `advance_phases` watches for the Starting → Running
    /// transition. `None` means "running once alive".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_port: Option<u16>,
    /// How long this process may sit in `Starting`. `None` is
    /// [`START_TIMEOUT_SECS`]; a slow first build sets its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_timeout_s: Option<u64>,
    /// The listening sockets seen in *this* process's own group, sorted.
    ///
    /// Per process, not per worktree: a port an api opened for its
    /// debugger is indistinguishable from one the web server opened once
    /// the two are merged into one list, and the URL a worktree hands out
    /// then follows whichever of them nothing claimed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_ports: Vec<u16>,
    /// Whether the orphan sweep has already signalled this record's
    /// process group.
    ///
    /// A record whose leader is dead is signalled once, because a dead
    /// leader is not a dead group. A `Failed` record then lives on until
    /// its own worktree is started, stopped or removed, and re-signalling
    /// its pgid on every later mutation is how a pid that has since
    /// wrapped around onto an unrelated session leader gets killed. Those
    /// three clear a swept record without signalling it again. A group
    /// already empty when its record is seen to fail is marked swept then:
    /// nothing is left to signal, and a signal days later finds only
    /// whoever has the number now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub swept: bool,
    pub phase: Phase,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "phase")]
pub enum Phase {
    Starting { since: DateTime<Utc> },
    Running { since: DateTime<Utc> },
    Failed { at: DateTime<Utc>, reason: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceKind {
    Compose,
    Native,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ServiceRecord {
    pub name: String,
    pub kind: ServiceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Native services run as pando's own detached children.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<Group>,
    /// Compose services are addressed by their compose project name instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_project: Option<String>,
}

/// A namespace pando made for one worktree inside a server the main
/// checkout runs: a database of its own, or a numbered slot of its own.
///
/// A record here is pando's word that pando made it. It is written only
/// once the server has made it for pando — a database that was already
/// there is never recorded — and it is the only thing `rm` will ever drop.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRecord {
    /// The `[[services]]` entry it belongs to: `mariadb`, `redis`.
    pub service: String,
    /// The recipe that knows how to make and drop it. `rm` never loads
    /// config, so it finds the commands by this name.
    pub recipe: String,
    pub kind: NamespaceKind,
    /// The server, as the main checkout's env files named it when this was
    /// made. What `rm` drops it on, even if those files have moved on.
    pub host: String,
    pub port: u16,
    /// The database, or the slot's number, as the server names it.
    pub name: String,
    /// The main checkout's own database, or slot, on that server: what the
    /// name was derived from, and what it must never be.
    pub main: String,
    /// Every one the main checkout's env files named on that server when a
    /// start last used this, `main` first: a queue's slot beside a
    /// cache's. What another project on the same server knows of this
    /// project's main checkout is these, so none of them is ever given out
    /// or emptied there. Empty in a record written before they were kept,
    /// which [`NamespaceRecord::every_main`] reads as `main` alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mains: Vec<String>,
    /// The env keys the app found the service by when this was made,
    /// which is where its login is read from: `rm` reads it there to drop
    /// this without loading config.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    /// When a start of this worktree last used it.
    pub used_at: DateTime<Utc>,
    /// Which server process a slot was last kept on, as its recipe's
    /// `server_id` says it — a Redis's `run_id`. A slot is known by host
    /// and port alone, and another Redis on the same port has its own
    /// slot of that number: this, with the mark pando writes into the
    /// slot, tells the two apart. `None` in a record written before it
    /// was kept, which is trusted as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

impl NamespaceRecord {
    /// Every name the main checkout's own has on that server, as this
    /// record knows them: `main`, then the rest of
    /// [`NamespaceRecord::mains`].
    pub fn every_main(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.main.as_str()).chain(
            self.mains
                .iter()
                .map(String::as_str)
                .filter(|main| *main != self.main),
        )
    }
}

/// What a namespace is on its server.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NamespaceKind {
    /// A database of its own name: `CREATE DATABASE`, `DROP DATABASE`.
    Database,
    /// A numbered slot the server always has: allocated, emptied, never
    /// created. Redis's `SELECT n`.
    Slot,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct HookRecord {
    /// Content hash of the hook's fingerprint globs at the last run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    pub ran_at: DateTime<Utc>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ShareRecord {
    pub tunnel_pid: u32,
    pub tunnel_pgid: Group,
    pub public_url: String,
    pub local_port: u16,
    pub started_at: DateTime<Utc>,
    pub log_path: PathBuf,
    /// Optional local proxy that injects an auth header into tunnel traffic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_pgid: Option<Group>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_port: Option<u16>,
}

/// A share whose tunnel is still coming up, written down before anything
/// waits on it.
///
/// A share spawns its proxy and its tunnel in sessions of their own and
/// records them as a [`ShareRecord`] only once the tunnel has published,
/// up to thirty seconds later. A pando that died in between — a Ctrl-C, a
/// TUI quit mid-share — left both running with no record: a public URL,
/// and a proxy holding the auth cookie, that nothing could name again.
/// This names them for that window. The share that owns it drops it once
/// it is recorded or stopped, and a sweep stops what it names once the
/// pando that owns it is gone.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PendingShare {
    /// The pando process waiting on the share.
    pub owner_pid: u32,
    /// When it was written down. A share pending for longer than any share
    /// waits is not one `owner_pid` is still waiting on: that pid has been
    /// handed to another process since. Read as now from a state file
    /// without it, which only moves the deadline later.
    #[serde(default = "Utc::now")]
    pub since: DateTime<Utc>,
    /// Every process group spawned for it so far.
    pub pgids: Vec<Group>,
}

impl State {
    pub fn new() -> Self {
        Self {
            version: STATE_VERSION,
            worktrees: BTreeMap::new(),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

pub fn load(path: &Path) -> Result<State> {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::new()),
        Err(e) => {
            return Err(
                anyhow::Error::from(e).context(format!("read state file {}", path.display()))
            );
        }
    };
    // The version first, on its own: a file another version of pando wrote
    // may not have this version's shape at all, and "could not parse" is
    // not the sentence that says what to do about it.
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    if let Ok(Versioned { version }) = serde_json::from_str::<Versioned>(&text)
        && version != STATE_VERSION
    {
        anyhow::bail!(
            "state file {} is version {version}, this pando speaks version {STATE_VERSION} — \
             upgrade pando, or move that file aside to start over (worktrees pando created will \
             then read as adopted)",
            path.display(),
        );
    }
    let OnDisk {
        mut state,
        boot,
        boot_pids_since,
    } = serde_json::from_str(&text)
        .with_context(|| format!("parse state file {}", path.display()))?;
    let written = boot.as_deref().map(|id| Boot {
        id,
        pids_since: boot_pids_since,
    });
    forget_previous_boot(&mut state, written, crate::platform::boot::now());
    Ok(state)
}

/// The state file as it is written: the state, and the boot it was
/// written during, in two fields: `boot` is compared whole by every pando
/// before `boot_pids_since` was recorded, so it stays the system's id
/// alone.
#[derive(Deserialize)]
struct OnDisk {
    #[serde(flatten)]
    state: State,
    #[serde(default)]
    boot: Option<String>,
    #[serde(default)]
    boot_pids_since: Option<u64>,
}

#[derive(Serialize)]
struct Saving<'a> {
    #[serde(flatten)]
    state: &'a State,
    #[serde(skip_serializing_if = "Option::is_none")]
    boot: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    boot_pids_since: Option<u64>,
}

/// Forgets every pid a state file recorded during an earlier boot.
///
/// Nothing pando started survives a restart of the machine, but the file
/// does — and the numbers in it are handed out again from the bottom on
/// the next boot. Trusted, a dev server's pid then names whatever process
/// got that number: `status` calls it running, `start` leaves it "already
/// running", and `stop` sends SIGTERM and then SIGKILL to the process group
/// of a stranger (a terminal's shell leads a group of its own). So a file
/// written during another boot keeps what outlives a restart — ports,
/// roles, hooks, the compose projects that name containers and volumes,
/// the mode — and loses every pid, the process groups of a share still
/// coming up among them.
///
/// A file with no boot recorded, or a system that cannot say, keeps them:
/// not knowing is not evidence of a restart.
fn forget_previous_boot(state: &mut State, written: Option<Boot<'_>>, now: Option<Boot<'_>>) {
    let (Some(written), Some(now)) = (written, now) else {
        return;
    };
    if crate::platform::boot::same(written, now) {
        return;
    }
    for record in state.worktrees.values_mut() {
        record.processes.clear();
        record.observed_ports.clear();
        record.share = None;
        record.pending_shares.clear();
        for service in record.services.iter_mut() {
            service.pid = None;
            service.pgid = None;
        }
    }
}

pub fn save(path: &Path, state: &State) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let now = crate::platform::boot::now();
    let json = serde_json::to_string_pretty(&Saving {
        state,
        boot: now.map(|boot| boot.id),
        boot_pids_since: now.and_then(|boot| boot.pids_since),
    })
    .context("serialize state")?;
    // On disk before the rename, not merely in the page cache: a crash
    // after a rename whose data never landed leaves an empty state file,
    // which every command then refuses to read.
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        std::io::Write::write_all(&mut file, json.as_bytes())?;
        file.sync_all()
    });
    written.with_context(|| format!("write tmp state {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename tmp → {}", path.display()))?;
    Ok(())
}

/// Drops what is no longer running: process records whose pid is gone,
/// native service records whose pid is gone, and a share whose tunnel or
/// proxy has exited. The worktree record itself survives — `created_by_pando`
/// and the port assignment outlive any process.
///
/// One exception: a `Failed` record stays. It is the one phase whose whole
/// purpose is to outlive its process — `refresh` advances phases rather
/// than reconciling precisely so a crashed dev server stays visible until
/// the developer acts on it — and dropping it here erased that from
/// `status` the moment anything else in the project was started or stopped.
/// The mutations that *are* about those processes clear them explicitly:
/// `start` for the ones it starts, `stop` for the ones it stops, `rm` for
/// all of a worktree's.
///
/// Every read path calls this before trusting the map. Returns whether
/// anything changed, so a caller holding the lock knows to save.
/// Whether a process record still has something behind it.
///
/// The leader first, because that is the question `stop` and the Phase 2b
/// orphan handling were both designed around: a `sleep 300 & exit 0` leaves
/// a child holding the group open, and pando calls that record Failed so it
/// survives `reconcile` and stays stoppable.
///
/// The group is consulted for **one** shape: a process that owns no port.
/// A dev command that backgrounds its server and returns leaves exactly the
/// same trace as an orphan — leader gone, group alive — and when a port is
/// configured pando can tell them apart by asking whether anything bound
/// it. When no port is configured there is no such question to ask, and
/// reading the leader alone reports a dead worktree over a desktop
/// application that is running. Measured on a real project: the `bash -lc`
/// leader lives about half a second after backgrounding, and every read
/// after that called it dead.
fn record_alive(
    pid: u32,
    pgid: Group,
    ready_port: Option<u16>,
    is_alive: &impl Fn(u32) -> bool,
    group_alive: &impl Fn(Group) -> bool,
) -> bool {
    // Reaps a zombie when we are the parent, which is what keeps the group
    // probe below from seeing a corpse as a member.
    if is_alive(pid) {
        return true;
    }
    ready_port.is_none() && group_alive(pgid)
}

impl ProcessRecord {
    /// Whether this process still has something behind it, by the rule
    /// [`reconcile`] and [`advance_phases`] read it with.
    ///
    /// For the paths that act on the answer: the orphan sweep, and a start
    /// deciding what is already up. They have to agree with `status`,
    /// which calls a portless process whose leader backgrounded it and
    /// returned Running — and a mutation that asked the leader alone
    /// killed that process as an orphan.
    pub fn alive(
        &self,
        is_alive: impl Fn(u32) -> bool,
        group_alive: impl Fn(Group) -> bool,
    ) -> bool {
        record_alive(
            self.pid,
            self.pgid,
            self.ready_port,
            &is_alive,
            &group_alive,
        )
    }
}

pub fn reconcile(
    state: &mut State,
    is_alive: impl Fn(u32) -> bool,
    group_alive: impl Fn(Group) -> bool,
) -> bool {
    let mut changed = false;
    for rec in state.worktrees.values_mut() {
        let before = rec.processes.len();
        rec.processes.retain(|_, p| {
            record_alive(p.pid, p.pgid, p.ready_port, &is_alive, &group_alive)
                || matches!(p.phase, Phase::Failed { .. })
        });
        changed |= rec.processes.len() != before;

        // A native service *is* its process, so a dead pid is a dead
        // service. A compose service is a container, and the pid on its
        // record is only the log pump in front of it: dropping the record
        // because the pump died would leave a running container — with its
        // volume — that nothing in pando could ever find again, let alone
        // take down. So the pump is forgotten and the service is kept.
        let before = rec.services.len();
        rec.services
            .retain(|s| s.kind != ServiceKind::Native || s.pid.map(&is_alive).unwrap_or(true));
        changed |= rec.services.len() != before;
        for service in rec.services.iter_mut() {
            if service.kind == ServiceKind::Compose && service.pid.is_some_and(|pid| !is_alive(pid))
            {
                service.pid = None;
                service.pgid = None;
                changed = true;
            }
        }

        // Nothing is up, whatever records are left, so the worktree is not
        // listening on anything.
        if rec
            .processes
            .values()
            .any(|p| matches!(p.phase, Phase::Starting { .. } | Phase::Running { .. }))
        {
            continue;
        }
        if !rec.observed_ports.is_empty() {
            rec.observed_ports.clear();
            changed = true;
        }
    }
    changed |= sweep_dead_shares(state, &is_alive);
    changed
}

/// Forgets every native service whose server is gone, group and all,
/// returning `(worktree, service)` for each one forgotten.
///
/// The read path's half of what [`reconcile`] does for native services.
/// A native service *is* its process, so a dead server is a service that
/// is not there — and between it dying and the next mutation, `status`
/// and the TUI used to go on reporting a database that was gone, on a
/// pid the kernel is free to hand to something else.
///
/// Only once the whole group is gone. A leader that died while its group
/// lives on — a database's own children still holding the socket — is the
/// orphan sweep's to signal, on the next mutation, and dropping the record
/// here would leave that group with nothing able to find it again.
pub fn forget_dead_native_services(
    state: &mut State,
    is_alive: impl Fn(u32) -> bool,
    group_alive: impl Fn(Group) -> bool,
) -> Vec<(String, String)> {
    let mut forgotten = Vec::new();
    for (name, rec) in state.worktrees.iter_mut() {
        rec.services.retain(|service| {
            let gone = service.kind == ServiceKind::Native
                && service.pid.is_some_and(|pid| !is_alive(pid))
                && !service.pgid.is_some_and(&group_alive);
            if gone {
                forgotten.push((name.clone(), service.name.clone()));
            }
            !gone
        });
    }
    forgotten
}

/// A share is only useful while both halves live: a tunnel whose proxy died
/// serves the wrong thing, and a proxy whose tunnel died is unreachable.
fn sweep_dead_shares(state: &mut State, is_alive: &impl Fn(u32) -> bool) -> bool {
    let mut changed = false;
    for rec in state.worktrees.values_mut() {
        let Some(share) = &rec.share else { continue };
        let tunnel_dead = !is_alive(share.tunnel_pid);
        let proxy_dead = share.proxy_pid.map(|p| !is_alive(p)).unwrap_or(false);
        if tunnel_dead || proxy_dead {
            rec.share = None;
            changed = true;
        }
    }
    changed
}

/// Moves each process through Starting → Running → Failed.
///
/// `port_bound(pgid, port)` answers "has this process group opened that
/// port yet?" and is injected: the honest answer comes from scanning the
/// group's own sockets, and the one thing it must never do is *bind* the
/// port to find out, which would hand the server being waited for an
/// `EADDRINUSE`. A process that declared no `ready_port` is Running as soon
/// as it is alive.
pub fn advance_phases<R: Into<PortCheck>>(
    state: &mut State,
    is_alive: impl Fn(u32) -> bool,
    group_alive: impl Fn(Group) -> bool,
    port_bound: impl Fn(Group, u16) -> R,
) -> bool {
    let now = Utc::now();
    let mut changed = false;
    for rec in state.worktrees.values_mut() {
        for proc in rec.processes.values_mut() {
            match &proc.phase {
                Phase::Starting { since } => {
                    let timeout = proc
                        .ready_timeout_s
                        .map(|s| i64::try_from(s).unwrap_or(i64::MAX))
                        .unwrap_or(START_TIMEOUT_SECS);
                    if !record_alive(
                        proc.pid,
                        proc.pgid,
                        proc.ready_port,
                        &is_alive,
                        &group_alive,
                    ) {
                        proc.phase = Phase::Failed {
                            at: now,
                            reason: EXITED.into(),
                        };
                        proc.swept |= !group_alive(proc.pgid);
                        changed = true;
                    } else {
                        let check = proc
                            .ready_port
                            .map(|p| port_bound(proc.pgid, p).into())
                            .unwrap_or(PortCheck::Bound);
                        let elapsed = now.signed_duration_since(*since).num_seconds();
                        // The watched port is the whole content of this
                        // failure: "timeout" alone tells nobody what pando
                        // was waiting for.
                        let port = proc
                            .ready_port
                            .map(|p| p.to_string())
                            .unwrap_or_else(|| "?".to_string());
                        let failed = match check {
                            PortCheck::Bound => {
                                proc.phase = Phase::Running { since: now };
                                changed = true;
                                None
                            }
                            PortCheck::NotBound if elapsed > timeout => {
                                Some(format!("{NOTHING_BOUND} {port} in {timeout}s"))
                            }
                            // Not an answer, so not a reason to give up at
                            // the window's end: the wait goes on while the
                            // next scan might still say. Past the grace, the
                            // failure says what really happened.
                            PortCheck::Unknown if elapsed > longest_starting_secs(timeout) => {
                                Some(format!(
                                    "timeout: pando could not confirm port {port} was bound in \
                                     {elapsed}s — the scan of the process's sockets kept failing \
                                     or timing out, and nothing answered on the port"
                                ))
                            }
                            PortCheck::NotBound | PortCheck::Unknown => None,
                        };
                        if let Some(reason) = failed {
                            proc.phase = Phase::Failed { at: now, reason };
                            changed = true;
                        }
                    }
                }
                Phase::Running { .. } => {
                    if !record_alive(
                        proc.pid,
                        proc.pgid,
                        proc.ready_port,
                        &is_alive,
                        &group_alive,
                    ) {
                        proc.phase = Phase::Failed {
                            at: now,
                            reason: EXITED.into(),
                        };
                        proc.swept |= !group_alive(proc.pgid);
                        changed = true;
                    }
                }
                Phase::Failed { .. } => {}
            }
        }
    }
    changed |= sweep_dead_shares(state, &is_alive);
    changed
}

/// A worktree's phase, aggregated across every process it runs.
///
/// A worktree is one thing in a list and one row in the TUI, however many
/// processes it has, so the several phases have to become one. Failed wins,
/// because a half-running worktree is not running; Starting comes next,
/// because something is still on its way up; only when every process is
/// Running is the worktree Running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Aggregate {
    /// Since the *first* process started coming up: that is how long the
    /// worktree as a whole has been starting.
    Starting { since: DateTime<Utc> },
    /// Since the *last* process reached Running: that is the moment the
    /// worktree as a whole became ready.
    Running { since: DateTime<Utc> },
    /// Named, because "failed" without which process is a question rather
    /// than an answer — and carrying that process's own reason, including
    /// the hint the classifier wrote for it.
    Failed {
        process: String,
        at: DateTime<Utc>,
        reason: String,
    },
}

impl Aggregate {
    /// The one word a listing shows.
    pub fn word(&self) -> &'static str {
        match self {
            Aggregate::Starting { .. } => "starting",
            Aggregate::Running { .. } => "running",
            Aggregate::Failed { .. } => "failed",
        }
    }

    /// When this phase began.
    pub fn since(&self) -> DateTime<Utc> {
        match self {
            Aggregate::Starting { since } | Aggregate::Running { since } => *since,
            Aggregate::Failed { at, .. } => *at,
        }
    }

    /// Why a worktree is failed, with the process it happened to in front
    /// of it. `None` for anything that has not failed.
    pub fn reason(&self) -> Option<String> {
        match self {
            Aggregate::Failed {
                process, reason, ..
            } => Some(format!("{process}: {reason}")),
            _ => None,
        }
    }
}

/// The phase of a whole worktree, or `None` when it is running nothing.
///
/// The failure reported is the *earliest* one: with a web process that died
/// because its api never came up, the api's failure is the one that
/// explains the worktree, and the classifier's hint for it is the one worth
/// showing. Ties go to the first name in order, so the answer is stable
/// across reads.
/// How long a worktree has to have been up before a port of its processes
/// that nothing listens on is worth saying: long enough for a second
/// server the ready one started to bind.
pub const SILENT_PORT_GRACE: chrono::Duration = chrono::Duration::seconds(15);

/// The ports a running worktree's processes were given and nothing
/// listens on, as `(role, port)`, once it has been up for
/// [`SILENT_PORT_GRACE`].
///
/// Readiness waits for one role. A process that owns two — a root script
/// that starts a web server and an api — reads as running when the web
/// server binds, and the api can have died at startup behind it. Only
/// process roles are looked at, since a service's port is its own
/// process's; and nothing is said before the last scan saw anything at
/// all, because an empty scan is not an answer.
pub fn silent_ports(record: &WorktreeRecord, now: DateTime<Utc>) -> Vec<(String, u16)> {
    let Some(Aggregate::Running { since }) = aggregate_phase(record) else {
        return Vec::new();
    };
    if now - since < SILENT_PORT_GRACE || record.observed_ports.is_empty() {
        return Vec::new();
    }
    let mut silent: Vec<(String, u16)> = record
        .roles
        .values()
        .flatten()
        .filter_map(|role| Some((role.clone(), *record.ports.get(role)?)))
        .filter(|(_, port)| !record.observed_ports.contains(port))
        .collect();
    silent.sort();
    silent.dedup();
    silent
}

pub fn aggregate_phase(record: &WorktreeRecord) -> Option<Aggregate> {
    if record.processes.is_empty() {
        return None;
    }
    let mut failed: Option<(&str, DateTime<Utc>, &str)> = None;
    let mut earliest_start: Option<DateTime<Utc>> = None;
    let mut latest_running: Option<DateTime<Utc>> = None;
    for (name, proc) in &record.processes {
        match &proc.phase {
            Phase::Failed { at, reason } => {
                if failed.is_none_or(|(_, first, _)| *at < first) {
                    failed = Some((name.as_str(), *at, reason.as_str()));
                }
            }
            Phase::Starting { since } => {
                earliest_start = Some(earliest_start.map_or(*since, |e| e.min(*since)));
            }
            Phase::Running { since } => {
                latest_running = Some(latest_running.map_or(*since, |l| l.max(*since)));
            }
        }
    }
    if let Some((process, at, reason)) = failed {
        return Some(Aggregate::Failed {
            process: process.to_string(),
            at,
            reason: reason.to_string(),
        });
    }
    if let Some(since) = earliest_start {
        return Some(Aggregate::Starting { since });
    }
    Some(Aggregate::Running {
        since: latest_running.expect("a record with processes is in one of the three phases"),
    })
}

pub struct StateLock {
    _file: std::fs::File,
}

pub fn lock(lock_path: &Path) -> Result<StateLock> {
    let file = open_lock_file(lock_path)?;
    crate::platform::files::lock_exclusive(&file)
        .with_context(|| format!("flock on {}", lock_path.display()))?;
    Ok(StateLock { _file: file })
}

pub fn try_lock(lock_path: &Path) -> Result<Option<StateLock>> {
    let file = open_lock_file(lock_path)?;
    let held = crate::platform::files::try_lock_exclusive(&file)
        .with_context(|| format!("flock (try) on {}", lock_path.display()))?;
    Ok(held.then_some(StateLock { _file: file }))
}

fn open_lock_file(lock_path: &Path) -> Result<std::fs::File> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .with_context(|| format!("open lock {}", lock_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn at(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 20, hour, 0, 0).unwrap()
    }

    fn process(pid: u32, phase: Phase) -> ProcessRecord {
        ProcessRecord {
            pid,
            pgid: Group::from_raw(pid as i32),
            started_at: at(9),
            log_path: PathBuf::from("logs/feat+x/dev.log"),
            ready_port: Some(17_000),
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase,
        }
    }

    fn running(pid: u32) -> ProcessRecord {
        process(pid, Phase::Running { since: at(9) })
    }

    fn starting(pid: u32, since: DateTime<Utc>) -> ProcessRecord {
        process(pid, Phase::Starting { since })
    }

    fn record_with(pid: u32) -> WorktreeRecord {
        let mut rec = WorktreeRecord::new("/abs/feat+x", true);
        rec.processes.insert("dev".into(), running(pid));
        rec.ports.insert("web".into(), 17_000);
        rec
    }

    // ---- silent ports ----------------------------------------------------

    /// One process owning `web` and `api`, up since nine, with a private
    /// database beside it; `observed` is what the last scan saw.
    fn two_server_script(observed: &[u16]) -> WorktreeRecord {
        let mut rec = record_with(1);
        rec.ports.insert("api".into(), 17_001);
        rec.ports.insert("db".into(), 17_002);
        rec.roles
            .insert("dev".into(), vec!["api".into(), "web".into()]);
        rec.observed_ports = observed.to_vec();
        rec
    }

    // The api died at startup behind a web server that is up: named, once
    // the worktree has been up long enough for it to have bound. The
    // database's port is its own process's, so it is never named.
    #[test]
    fn a_port_nothing_listens_on_is_named_after_the_grace() {
        let rec = two_server_script(&[17_000]);
        assert_eq!(
            silent_ports(&rec, at(9) + SILENT_PORT_GRACE),
            vec![("api".to_string(), 17_001)]
        );
        assert!(silent_ports(&rec, at(9)).is_empty(), "too early to say");
        assert!(silent_ports(&two_server_script(&[17_000, 17_001]), at(10)).is_empty());
        assert!(
            silent_ports(&two_server_script(&[]), at(10)).is_empty(),
            "an empty scan is not an answer"
        );
    }

    // ---- the aggregate phase ---------------------------------------------

    /// A worktree running `web` and `api` in the phases given.
    fn two_processes(web: Phase, api: Phase) -> WorktreeRecord {
        let mut rec = WorktreeRecord::new("/abs/feat+x", true);
        rec.processes.insert("web".into(), process(1, web));
        rec.processes.insert("api".into(), process(2, api));
        rec
    }

    fn failed(at_hour: u32, reason: &str) -> Phase {
        Phase::Failed {
            at: at(at_hour),
            reason: reason.to_string(),
        }
    }

    #[test]
    fn a_worktree_running_nothing_has_no_phase() {
        let rec = WorktreeRecord::new("/abs/feat+x", true);
        assert_eq!(aggregate_phase(&rec), None);
    }

    #[test]
    fn one_process_is_its_own_aggregate() {
        let rec = record_with(1);
        assert_eq!(
            aggregate_phase(&rec),
            Some(Aggregate::Running { since: at(9) })
        );
    }

    /// Every combination of two processes' phases, and what the worktree
    /// reads as. Failed beats Starting beats Running: a worktree with a
    /// dead api is not "running", and one still bringing a process up is
    /// not ready.
    #[test]
    fn the_aggregate_of_two_phases_is_the_worst_of_them() {
        let starting = || Phase::Starting { since: at(10) };
        let running = || Phase::Running { since: at(11) };
        let broken = || failed(12, "process exited");
        let cases: [(Phase, Phase, &str); 9] = [
            (running(), running(), "running"),
            (running(), starting(), "starting"),
            (running(), broken(), "failed"),
            (starting(), running(), "starting"),
            (starting(), starting(), "starting"),
            (starting(), broken(), "failed"),
            (broken(), running(), "failed"),
            (broken(), starting(), "failed"),
            (broken(), broken(), "failed"),
        ];
        for (web, api, expected) in cases {
            let rec = two_processes(web.clone(), api.clone());
            let aggregate = aggregate_phase(&rec).expect("two processes have a phase");
            assert_eq!(
                aggregate.word(),
                expected,
                "web {web:?} and api {api:?} should read as {expected}"
            );
        }
    }

    #[test]
    fn a_failed_aggregate_names_the_process_that_failed() {
        let rec = two_processes(
            Phase::Running { since: at(11) },
            failed(12, "process exited — the command was not found"),
        );
        let aggregate = aggregate_phase(&rec).expect("a phase");
        assert_eq!(
            aggregate,
            Aggregate::Failed {
                process: "api".into(),
                at: at(12),
                reason: "process exited — the command was not found".into(),
            }
        );
        assert_eq!(
            aggregate.reason().as_deref(),
            Some("api: process exited — the command was not found"),
            "the hint belongs to the process it was read from"
        );
        assert_eq!(aggregate.since(), at(12));
    }

    // The classifier runs per log, so the reason shown must be the failing
    // process's own — not the first one's in name order.
    #[test]
    fn the_earliest_failure_is_the_one_that_explains_the_worktree() {
        let rec = two_processes(
            failed(13, "timeout: nothing bound port 17000 in 30s"),
            failed(12, "process exited — port 17001 is already in use"),
        );
        let aggregate = aggregate_phase(&rec).expect("a phase");
        assert_eq!(
            aggregate.reason().as_deref(),
            Some("api: process exited — port 17001 is already in use"),
            "the api died first and took the web process with it"
        );
    }

    #[test]
    fn starting_dates_from_the_first_process_and_running_from_the_last() {
        let rec = two_processes(
            Phase::Starting { since: at(9) },
            Phase::Starting { since: at(11) },
        );
        assert_eq!(
            aggregate_phase(&rec),
            Some(Aggregate::Starting { since: at(9) }),
            "the worktree has been coming up since the first one started"
        );

        let rec = two_processes(
            Phase::Running { since: at(9) },
            Phase::Running { since: at(11) },
        );
        assert_eq!(
            aggregate_phase(&rec),
            Some(Aggregate::Running { since: at(11) }),
            "and it was only ready when the last one was"
        );
    }

    #[test]
    fn a_process_still_starting_holds_the_whole_worktree_back() {
        let rec = two_processes(
            Phase::Running { since: at(9) },
            Phase::Starting { since: at(11) },
        );
        assert_eq!(
            aggregate_phase(&rec),
            Some(Aggregate::Starting { since: at(11) })
        );
        assert_eq!(aggregate_phase(&rec).unwrap().reason(), None);
    }

    fn share(tunnel_pid: u32) -> ShareRecord {
        ShareRecord {
            tunnel_pid,
            tunnel_pgid: Group::from_raw(tunnel_pid as i32),
            public_url: "https://x.trycloudflare.com".into(),
            local_port: 17_000,
            started_at: at(9),
            log_path: PathBuf::from("logs/feat+x/tunnel.log"),
            proxy_pid: None,
            proxy_pgid: None,
            proxy_port: None,
        }
    }

    fn full_state() -> State {
        let mut rec = record_with(4242);
        rec.roles.insert("dev".to_string(), vec!["web".to_string()]);
        rec.observed_ports = vec![17_000, 17_001];
        if let Some(dev) = rec.processes.get_mut("dev") {
            dev.observed_ports = vec![17_000, 17_001];
        }
        rec.services = vec![
            ServiceRecord {
                name: "postgres".into(),
                kind: ServiceKind::Compose,
                port: Some(17_002),
                pid: None,
                pgid: None,
                compose_project: Some("pando-acme-feat+x".into()),
            },
            ServiceRecord {
                name: "redis".into(),
                kind: ServiceKind::Native,
                port: Some(17_003),
                pid: Some(5150),
                pgid: Some(Group::from_raw(5150)),
                compose_project: None,
            },
        ];
        rec.hooks.insert(
            "migrate".into(),
            HookRecord {
                fingerprint: Some("sha256:abc".into()),
                ran_at: at(10),
            },
        );
        rec.share = Some(share(7000));
        rec.namespaces = vec![NamespaceRecord {
            service: "mariadb".into(),
            recipe: "mariadb".into(),
            kind: NamespaceKind::Database,
            host: "localhost".into(),
            port: 3306,
            name: "shop__feat_x".into(),
            main: "shop".into(),
            mains: vec!["shop".into(), "shop_jobs".into()],
            keys: vec!["DATABASE_PORT".into()],
            used_at: at(10),
            server: None,
        }];
        let mut state = State::new();
        state.worktrees.insert("feat+x".into(), rec);
        state
    }

    #[test]
    fn a_new_state_is_version_two() {
        assert_eq!(State::new().version, 2);
        assert_eq!(State::default(), State::new());
    }

    #[test]
    fn v2_round_trips_through_json_with_every_record_type() {
        let state = full_state();
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"version\":2"));
        let back: State = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }

    // A namespace recorded before every name of the main checkout's own was
    // kept names only `main`, and reads as that alone.
    #[test]
    fn a_namespace_recorded_with_its_main_alone_reads_as_that_main() {
        let mut state = full_state();
        let namespace = &mut state.worktrees.get_mut("feat+x").unwrap().namespaces[0];
        namespace.mains.clear();
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("\"mains\""), "{json}");
        let back: State = serde_json::from_str(&json).unwrap();
        let namespace = &back.worktrees["feat+x"].namespaces[0];
        assert_eq!(namespace.every_main().collect::<Vec<_>>(), vec!["shop"]);
        let kept = &full_state().worktrees["feat+x"].namespaces[0];
        assert_eq!(
            kept.every_main().collect::<Vec<_>>(),
            vec!["shop", "shop_jobs"]
        );
    }

    // Every 0.3.0 state file says `isolated` and never `mode`. Read wrong,
    // an isolated worktree's next plain start would hand its processes the
    // shared database while its own was still running beside it.
    #[test]
    fn a_state_file_from_before_modes_reads_its_isolated_flag() {
        let with = |field: &str| -> WorktreeRecord {
            serde_json::from_str(&format!(r#"{{"path":"/abs/w"{field}}}"#)).unwrap()
        };
        assert_eq!(
            with(r#","isolated":true"#).mode,
            Some(ServiceMode::Isolated)
        );
        assert_eq!(with(r#","isolated":false"#).mode, None);
        assert_eq!(with("").mode, None);
        assert_eq!(with("").mode(), ServiceMode::Shared, "unsaid is shared");
        for mode in ServiceMode::ALL {
            let rec = with(&format!(r#","mode":"{}""#, mode.word()));
            assert_eq!(rec.mode, Some(mode));
        }
    }

    #[test]
    fn a_mode_is_written_as_its_word_and_never_as_the_old_flag() {
        let mut rec = WorktreeRecord::new("/abs/w", true);
        let json = serde_json::to_value(&rec).unwrap();
        assert!(json.get("mode").is_none(), "never started says nothing");
        rec.mode = Some(ServiceMode::Namespaced);
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(json["mode"], "namespaced");
        assert!(json.get("isolated").is_none(), "{json}");
        let back: WorktreeRecord = serde_json::from_value(json).unwrap();
        assert_eq!(back, rec);
        for mode in ServiceMode::ALL {
            assert_eq!(serde_json::to_value(mode).unwrap(), mode.word());
        }
    }

    #[test]
    fn phases_round_trip_through_json() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", false);
        rec.processes.insert("s".into(), starting(1, at(9)));
        rec.processes.insert("r".into(), running(2));
        rec.processes.insert(
            "f".into(),
            process(
                3,
                Phase::Failed {
                    at: at(11),
                    reason: "timeout".into(),
                },
            ),
        );
        state.worktrees.insert("w".into(), rec);
        let back: State = serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert_eq!(state, back);
    }

    #[test]
    fn load_of_a_missing_file_is_an_empty_state() {
        let dir = tempdir().unwrap();
        assert_eq!(load(&dir.path().join("nope.json")).unwrap(), State::new());
    }

    #[test]
    fn save_then_load_preserves_state_and_creates_parents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("projects").join("p").join("state.json");
        let state = full_state();
        save(&path, &state).unwrap();
        assert!(path.exists());
        assert_eq!(load(&path).unwrap(), state);
    }

    #[test]
    fn save_overwrites_without_leaking_the_temp_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut a = State::new();
        a.worktrees.insert("one".into(), record_with(1));
        save(&path, &a).unwrap();

        let mut b = State::new();
        b.worktrees.insert("two".into(), record_with(2));
        save(&path, &b).unwrap();

        assert_eq!(load(&path).unwrap(), b);
        assert!(
            !path.with_extension("json.tmp").exists(),
            "the temp file must not survive the rename"
        );
    }

    #[test]
    fn load_rejects_malformed_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{not json").unwrap();
        assert!(load(&path).is_err());
    }

    // pando starts at v2; a file from a future (or imagined past) version is
    // an error rather than something to guess at.
    #[test]
    fn load_rejects_another_state_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(&path, r#"{"version":1,"worktrees":{}}"#).unwrap();
        let err = load(&path).unwrap_err();
        assert!(
            format!("{err:#}").contains("version 1"),
            "unexpected error: {err:#}"
        );
    }

    // A version this pando does not speak may not have this version's shape
    // either, and the parse error it used to get said nothing about what to
    // do. The version is read first, whatever else the file holds.
    #[test]
    fn another_versions_file_of_another_shape_still_says_which_version_it_is() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"version":3,"worktrees":{"w":{"somewhere":"else"}},"new":true}"#,
        )
        .unwrap();
        let err = format!("{:#}", load(&path).unwrap_err());
        assert!(err.contains("version 3"), "{err}");
    }

    // A state file as pando 0.8.3 wrote it, before a process group had a
    // type of its own: every group a bare number. Typed as `Group`, they
    // must read from such a file and be written back byte for byte, or an
    // upgrade loses track of everything running.
    #[test]
    fn a_state_file_written_before_groups_had_a_type_reads_and_writes_the_same() {
        const WRITTEN: &str = r#"{
  "version": 2,
  "worktrees": {
    "feat+x": {
      "path": "/abs/feat+x",
      "created_by_pando": true,
      "processes": {
        "dev": {
          "pid": 4242,
          "pgid": 4242,
          "started_at": "2026-09-20T09:00:00Z",
          "log_path": "logs/feat+x/dev.log",
          "ready_port": 17000,
          "observed_ports": [
            17000,
            17001
          ],
          "phase": {
            "phase": "Running",
            "since": "2026-09-20T09:00:00Z"
          }
        }
      },
      "ports": {
        "web": 17000
      },
      "roles": {
        "dev": [
          "web"
        ]
      },
      "observed_ports": [
        17000,
        17001
      ],
      "services": [
        {
          "name": "postgres",
          "kind": "compose",
          "port": 17002,
          "compose_project": "pando-acme-feat+x"
        },
        {
          "name": "redis",
          "kind": "native",
          "port": 17003,
          "pid": 5150,
          "pgid": 5150
        }
      ],
      "namespaces": [
        {
          "service": "mariadb",
          "recipe": "mariadb",
          "kind": "database",
          "host": "localhost",
          "port": 3306,
          "name": "shop__feat_x",
          "main": "shop",
          "mains": [
            "shop",
            "shop_jobs"
          ],
          "keys": [
            "DATABASE_PORT"
          ],
          "used_at": "2026-09-20T10:00:00Z"
        }
      ],
      "hooks": {
        "migrate": {
          "fingerprint": "sha256:abc",
          "ran_at": "2026-09-20T10:00:00Z"
        }
      },
      "share": {
        "tunnel_pid": 7000,
        "tunnel_pgid": 7000,
        "public_url": "https://x.trycloudflare.com",
        "local_port": 17000,
        "started_at": "2026-09-20T09:00:00Z",
        "log_path": "logs/feat+x/tunnel.log"
      },
      "pending_shares": [
        {
          "owner_pid": 4343,
          "since": "2023-11-14T22:13:30Z",
          "pgids": [
            7100,
            7101
          ]
        }
      ]
    }
  }
}"#;
        let state: State = serde_json::from_str(WRITTEN).unwrap();
        assert_eq!(serde_json::to_string_pretty(&state).unwrap(), WRITTEN);
        let rec = &state.worktrees["feat+x"];
        assert_eq!(rec.processes["dev"].pgid, Group::from_raw(4242));
        assert_eq!(rec.services[1].pgid, Some(Group::from_raw(5150)));
        let share = rec.share.as_ref().unwrap();
        assert_eq!(share.tunnel_pgid, Group::from_raw(7000));
        assert_eq!(
            rec.pending_shares[0].pgids,
            vec![Group::from_raw(7100), Group::from_raw(7101)]
        );
        assert_eq!(
            rec.processes["dev"].pgid.to_string(),
            "4242",
            "as messages name it"
        );
    }

    // After a restart every pid in the file names whatever process the new
    // boot handed that number to. Trusted, `stop` signals a stranger's
    // process group and `start` calls a dead dev server "already running".
    #[test]
    fn a_state_file_from_an_earlier_boot_keeps_what_outlives_a_restart_and_no_pid() {
        let Some(now) = crate::platform::boot::now().map(|boot| boot.id) else {
            return; // A system that cannot say which boot this is.
        };
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = full_state();
        if let Some(rec) = state.worktrees.get_mut("feat+x") {
            rec.pending_shares.push(PendingShare {
                owner_pid: 4343,
                since: at(10),
                pgids: vec![Group::from_raw(7100), Group::from_raw(7101)],
            });
        }
        save(&path, &state).unwrap();
        assert_eq!(load(&path).unwrap(), state, "this boot's pids are kept");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(now), "the boot is written down: {text}");
        std::fs::write(&path, text.replace(now, "an-earlier-boot")).unwrap();
        let loaded = load(&path).unwrap();
        let rec = &loaded.worktrees["feat+x"];
        assert!(rec.processes.is_empty(), "{:?}", rec.processes);
        assert!(rec.share.is_none());
        assert!(
            rec.pending_shares.is_empty(),
            "a share still coming up names groups this boot gave to others"
        );
        assert!(rec.observed_ports.is_empty());
        assert!(
            rec.services
                .iter()
                .all(|s| s.pid.is_none() && s.pgid.is_none())
        );
        let before = &state.worktrees["feat+x"];
        assert_eq!(rec.ports, before.ports);
        assert_eq!(rec.hooks, before.hooks);
        assert_eq!(
            rec.namespaces, before.namespaces,
            "a database on the developer's server outlives a restart of this one"
        );
        assert_eq!(rec.services.len(), before.services.len());
        assert_eq!(
            rec.services[0].compose_project, before.services[0].compose_project,
            "the compose project still names the volumes"
        );

        // A file that never recorded a boot is not evidence of a restart.
        let mut unmarked: serde_json::Value = serde_json::from_str(&text).unwrap();
        unmarked.as_object_mut().unwrap().remove("boot");
        std::fs::write(&path, unmarked.to_string()).unwrap();
        assert_eq!(load(&path).unwrap(), state);
    }

    // A WSL 2 distro restarts on a kernel that keeps running, and so does
    // a container: the kernel's boot id is the same, and every pid is
    // handed out again from the bottom. Measured on WSL 2: the boot id
    // read the same before and after the distro was stopped and started.
    #[test]
    fn a_restart_under_a_kernel_that_kept_running_forgets_every_pid() {
        let boot = |pids_since| {
            Some(Boot {
                id: "kernel-a",
                pids_since,
            })
        };
        let mut state = full_state();
        forget_previous_boot(&mut state, boot(Some(100)), boot(Some(250)));
        let rec = &state.worktrees["feat+x"];
        assert!(rec.processes.is_empty(), "{:?}", rec.processes);
        assert!(rec.share.is_none());
        assert!(
            rec.services
                .iter()
                .all(|s| s.pid.is_none() && s.pgid.is_none())
        );
        assert_eq!(rec.ports, full_state().worktrees["feat+x"].ports);

        let mut same = full_state();
        forget_previous_boot(&mut same, boot(Some(100)), boot(Some(100)));
        assert_eq!(same, full_state(), "the same init: the same boot");

        let mut upgraded = full_state();
        forget_previous_boot(&mut upgraded, boot(None), boot(Some(250)));
        assert_eq!(
            upgraded,
            full_state(),
            "worktrees running when pando is upgraded are not forgotten"
        );
    }

    // Every pando before `boot_pids_since` compares `boot` whole. Folded
    // into it as `<id>:<ticks>`, a worktree started by this pando read as
    // stopped to 0.9.0, whose `stop` then left its listeners running.
    #[test]
    fn the_boot_an_older_pando_compares_is_the_systems_id_alone() {
        let Some(now) = crate::platform::boot::now() else {
            return; // A system that cannot say which boot this is.
        };
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.json");
        save(&path, &full_state()).unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(json["boot"], now.id);
        assert_eq!(
            json.get("boot_pids_since")
                .and_then(serde_json::Value::as_u64),
            now.pids_since
        );
    }

    #[test]
    fn reconcile_drops_dead_process_records_and_keeps_live_ones() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        rec.processes.insert("dev".into(), running(100));
        rec.processes.insert("api".into(), running(200));
        state.worktrees.insert("w".into(), rec);

        assert!(reconcile(&mut state, |pid| pid == 100, |_| false));
        let rec = state.worktrees.get("w").unwrap();
        assert!(rec.processes.contains_key("dev"));
        assert!(
            !rec.processes.contains_key("api"),
            "a dead process record must be dropped"
        );
    }

    // Phase 2b review, finding 7. `Failed` is the one phase whose whole
    // purpose is to outlive its process — `refresh` deliberately advances
    // phases rather than reconciling so a crash stays visible — and
    // `reconcile` threw it away on the next mutation of *any* worktree.
    #[test]
    fn reconcile_keeps_a_failed_record_and_drops_the_merely_dead() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        rec.processes.insert("web".into(), running(100));
        rec.processes
            .insert("api".into(), process(200, failed(12, "process exited")));
        state.worktrees.insert("w".into(), rec);

        assert!(reconcile(&mut state, |_| false, |_| false));
        let rec = state.worktrees.get("w").unwrap();
        assert!(
            !rec.processes.contains_key("web"),
            "a dead record nobody was told about is bookkeeping"
        );
        assert!(
            rec.processes.contains_key("api"),
            "a crash has to stay visible until the developer acts on it"
        );
        // And the worktree stops claiming to be listening on anything.
        assert!(rec.observed_ports.is_empty());
    }

    #[test]
    fn reconcile_keeps_the_worktree_record_and_its_ports() {
        let mut state = State::new();
        state.worktrees.insert("w".into(), record_with(999_999));
        reconcile(&mut state, |_| false, |_| false);

        let rec = state.worktrees.get("w").expect("the record survives");
        assert!(rec.processes.is_empty());
        assert!(
            rec.created_by_pando,
            "created_by_pando outlives any process"
        );
        assert_eq!(
            rec.ports.get("web"),
            Some(&17_000),
            "a stopped worktree still owns its ports"
        );
    }

    #[test]
    fn reconcile_is_a_no_op_when_everything_is_alive() {
        let mut state = full_state();
        let before = state.clone();
        assert!(!reconcile(&mut state, |_| true, |_| false));
        assert_eq!(state, before);
    }

    #[test]
    fn reconcile_drops_native_services_whose_process_died_and_keeps_compose() {
        let mut state = full_state();
        reconcile(&mut state, |pid| pid != 5150, |_| false);
        let services = &state.worktrees.get("feat+x").unwrap().services;
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "postgres");
    }

    // A compose service's pid is the log pump in front of the container,
    // not the container. Dropping the record when the pump dies would
    // orphan a container and its volume with nothing able to name them.
    #[test]
    fn reconcile_forgets_a_dead_log_pump_and_keeps_the_service() {
        let mut state = full_state();
        if let Some(rec) = state.worktrees.get_mut("feat+x") {
            rec.services[0].pid = Some(9001);
            rec.services[0].pgid = Some(Group::from_raw(9001));
        }
        assert!(reconcile(&mut state, |pid| pid != 9001, |_| false));
        let services = &state.worktrees.get("feat+x").unwrap().services;
        let compose = services.iter().find(|s| s.name == "postgres").unwrap();
        assert_eq!(compose.pid, None, "the pump is forgotten");
        assert_eq!(compose.pgid, None);
        assert_eq!(
            compose.compose_project.as_deref(),
            Some("pando-acme-feat+x"),
            "what `rm` needs to take it down survives"
        );
        assert_eq!(compose.port, Some(17_002));
    }

    #[test]
    fn a_read_path_forgets_a_native_service_whose_whole_group_is_gone() {
        let mut state = full_state();
        // Alive: nothing happens.
        assert!(forget_dead_native_services(&mut state, |_| true, |_| true).is_empty());
        // Leader dead, group alive: the sweep's, not this function's.
        assert!(forget_dead_native_services(&mut state, |_| false, |_| true).is_empty());
        assert_eq!(state.worktrees["feat+x"].services.len(), 2);
        // Gone entirely: forgotten, and said.
        let forgotten = forget_dead_native_services(&mut state, |pid| pid != 5150, |_| false);
        assert_eq!(forgotten, vec![("feat+x".to_string(), "redis".to_string())]);
        let services = &state.worktrees["feat+x"].services;
        assert_eq!(
            services.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["postgres"],
            "a compose record is a container, not a process, and stays"
        );
    }

    #[test]
    fn reconcile_clears_observed_ports_once_nothing_runs() {
        let mut state = full_state();
        reconcile(&mut state, |_| false, |_| false);
        assert!(
            state
                .worktrees
                .get("feat+x")
                .unwrap()
                .observed_ports
                .is_empty()
        );
    }

    #[test]
    fn reconcile_clears_a_share_whose_tunnel_died() {
        let mut state = full_state();
        assert!(reconcile(&mut state, |pid| pid != 7000, |_| false));
        assert!(state.worktrees.get("feat+x").unwrap().share.is_none());
    }

    #[test]
    fn reconcile_clears_a_share_whose_proxy_died_even_when_the_tunnel_lives() {
        let mut state = full_state();
        if let Some(share) = state.worktrees.get_mut("feat+x").unwrap().share.as_mut() {
            share.proxy_pid = Some(999_999);
            share.proxy_pgid = Some(Group::from_raw(999_999));
            share.proxy_port = Some(17_500);
        }
        reconcile(&mut state, |pid| pid != 999_999, |_| false);
        assert!(
            state.worktrees.get("feat+x").unwrap().share.is_none(),
            "a tunnel without its proxy serves the wrong thing"
        );
    }

    #[test]
    fn advance_phases_moves_starting_to_running_when_the_ready_port_binds() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        rec.processes
            .insert("dev".into(), starting(100, Utc::now()));
        state.worktrees.insert("w".into(), rec);

        assert!(advance_phases(&mut state, |_| true, |_| false, |_, _| true));
        let phase = &state.worktrees["w"].processes["dev"].phase;
        assert!(matches!(phase, Phase::Running { .. }), "got {phase:?}");
    }

    #[test]
    fn a_process_with_no_ready_port_is_running_once_alive() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        let mut proc = starting(100, Utc::now());
        proc.ready_port = None;
        rec.processes.insert("worker".into(), proc);
        state.worktrees.insert("w".into(), rec);

        assert!(advance_phases(
            &mut state,
            |_| true,
            |_| false,
            |_, _| false
        ));
        let phase = &state.worktrees["w"].processes["worker"].phase;
        assert!(matches!(phase, Phase::Running { .. }), "got {phase:?}");
    }

    #[test]
    fn advance_phases_moves_starting_to_failed_when_the_process_exits() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        rec.processes
            .insert("dev".into(), starting(100, Utc::now()));
        state.worktrees.insert("w".into(), rec);

        assert!(advance_phases(
            &mut state,
            |_| false,
            |_| false,
            |_, _| false
        ));
        let phase = &state.worktrees["w"].processes["dev"].phase;
        assert!(
            matches!(phase, Phase::Failed { reason, .. } if reason == "process exited"),
            "got {phase:?}"
        );
    }

    #[test]
    fn advance_phases_moves_starting_to_failed_on_timeout() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        let long_ago = Utc::now() - chrono::Duration::seconds(START_TIMEOUT_SECS + 1);
        rec.processes.insert("dev".into(), starting(100, long_ago));
        state.worktrees.insert("w".into(), rec);

        assert!(advance_phases(
            &mut state,
            |_| true,
            |_| false,
            |_, _| false
        ));
        let phase = &state.worktrees["w"].processes["dev"].phase;
        // The watched port is the content of this failure: "timeout" alone
        // does not say what pando was waiting for.
        assert!(
            matches!(phase, Phase::Failed { reason, .. }
                if reason.contains("timeout") && reason.contains("17000")),
            "got {phase:?}"
        );
    }

    // A window past `i64::MAX` seconds wrapped to a negative one when it was
    // cast, failing the process at once with "in -1s"; and the grace on top
    // of a large window overflowed, a panic in a debug build. Both saturate:
    // a window that large is "never", not "already over".
    #[test]
    fn a_window_too_large_to_count_is_never_rather_than_already_over() {
        assert_eq!(longest_starting_secs(i64::MAX), i64::MAX);
        assert_eq!(longest_starting_secs(i64::MAX / 2 + 1), i64::MAX);
        for check in [PortCheck::NotBound, PortCheck::Unknown] {
            let mut state = State::new();
            let mut rec = WorktreeRecord::new("/abs/w", true);
            let mut proc = starting(100, Utc::now() - chrono::Duration::seconds(3600));
            proc.ready_port = Some(17_000);
            proc.ready_timeout_s = Some(u64::MAX);
            rec.processes.insert("dev".into(), proc);
            state.worktrees.insert("w".into(), rec);
            advance_phases(&mut state, |_| true, |_| false, |_, _| check);
            assert!(
                matches!(
                    state.worktrees["w"].processes["dev"].phase,
                    Phase::Starting { .. }
                ),
                "{check:?}: {:?}",
                state.worktrees["w"].processes["dev"].phase
            );
        }
    }

    // A first build can take minutes; a project that says so must not be
    // called failed after the default thirty seconds.
    #[test]
    fn a_process_with_its_own_timeout_is_given_it() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        let elapsed = Utc::now() - chrono::Duration::seconds(START_TIMEOUT_SECS + 5);
        let mut proc = starting(100, elapsed);
        proc.ready_timeout_s = Some(300);
        rec.processes.insert("dev".into(), proc);
        state.worktrees.insert("w".into(), rec);

        assert!(
            !advance_phases(&mut state, |_| true, |_| false, |_, _| false),
            "still inside its own window, so nothing changes"
        );
        assert!(matches!(
            state.worktrees["w"].processes["dev"].phase,
            Phase::Starting { .. }
        ));

        state
            .worktrees
            .get_mut("w")
            .unwrap()
            .processes
            .get_mut("dev")
            .unwrap()
            .ready_timeout_s = Some(1);
        assert!(advance_phases(
            &mut state,
            |_| true,
            |_| false,
            |_, _| false
        ));
        let phase = &state.worktrees["w"].processes["dev"].phase;
        assert!(
            matches!(phase, Phase::Failed { reason, .. } if reason.contains("1s")),
            "the reason names the window it ran out of: {phase:?}"
        );
    }

    // A scan that cannot run is not a scan that found nothing. Under load
    // `lsof` can miss its deadline tick after tick while the server is
    // fine, and that used to fail it with "nothing bound port N".
    #[test]
    fn a_port_pando_cannot_check_is_waited_on_past_the_window_and_then_named_honestly() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        let just_past = Utc::now() - chrono::Duration::seconds(START_TIMEOUT_SECS + 1);
        rec.processes.insert("dev".into(), starting(100, just_past));
        state.worktrees.insert("w".into(), rec);

        assert!(
            !advance_phases(&mut state, |_| true, |_| false, |_, _| PortCheck::Unknown),
            "no answer is not a failure yet"
        );
        assert!(matches!(
            state.worktrees["w"].processes["dev"].phase,
            Phase::Starting { .. }
        ));
        // And a scan that does work, a tick later, is believed at once.
        assert!(advance_phases(
            &mut state,
            |_| true,
            |_| false,
            |_, _| PortCheck::Bound
        ));
        assert!(matches!(
            state.worktrees["w"].processes["dev"].phase,
            Phase::Running { .. }
        ));

        let mut rec = WorktreeRecord::new("/abs/w", true);
        let long_ago = Utc::now()
            - chrono::Duration::seconds(
                START_TIMEOUT_SECS + unconfirmed_grace_secs(START_TIMEOUT_SECS) + 1,
            );
        rec.processes.insert("dev".into(), starting(100, long_ago));
        state.worktrees.insert("w".into(), rec);
        assert!(advance_phases(
            &mut state,
            |_| true,
            |_| false,
            |_, _| PortCheck::Unknown
        ));
        let phase = &state.worktrees["w"].processes["dev"].phase;
        assert!(
            matches!(phase, Phase::Failed { reason, .. }
                if reason.contains("could not confirm port 17000")
                    && !reason.contains("nothing bound")),
            "the failure says pando could not tell, not that nothing bound: {phase:?}"
        );
    }

    #[test]
    fn advance_phases_moves_running_to_failed_when_the_process_dies() {
        let mut state = State::new();
        state.worktrees.insert("w".into(), record_with(100));
        assert!(advance_phases(
            &mut state,
            |_| false,
            |_| false,
            |_, _| false
        ));
        let phase = &state.worktrees["w"].processes["dev"].phase;
        assert!(
            matches!(phase, Phase::Failed { reason, .. } if reason == "process exited"),
            "got {phase:?}"
        );
    }

    #[test]
    fn advance_phases_leaves_running_and_failed_alone() {
        let mut state = State::new();
        state.worktrees.insert("alive".into(), record_with(100));
        let mut failed = WorktreeRecord::new("/abs/f", true);
        failed.processes.insert(
            "dev".into(),
            process(
                200,
                Phase::Failed {
                    at: at(9),
                    reason: "timeout".into(),
                },
            ),
        );
        state.worktrees.insert("failed".into(), failed);

        let before = state.clone();
        assert!(!advance_phases(
            &mut state,
            |_| true,
            |_| false,
            |_, _| true
        ));
        assert_eq!(state, before);
    }

    #[test]
    fn advance_phases_decides_per_process_not_per_worktree() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        rec.processes.insert("dev".into(), running(100));
        rec.processes.insert("api".into(), running(200));
        state.worktrees.insert("w".into(), rec);

        advance_phases(&mut state, |pid| pid == 100, |_| false, |_, _| true);
        let procs = &state.worktrees["w"].processes;
        assert!(matches!(procs["dev"].phase, Phase::Running { .. }));
        assert!(matches!(procs["api"].phase, Phase::Failed { .. }));
    }

    #[test]
    fn try_lock_is_exclusive_and_releases_on_drop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sub").join("state.lock");
        let held = lock(&path).unwrap();
        assert!(
            try_lock(&path).unwrap().is_none(),
            "a contended try_lock must not hand out the lock"
        );
        drop(held);
        assert!(
            reacquired(&path),
            "the lock must become available once the holder drops it"
        );
    }

    /// A `fork` anywhere in the test process duplicates every open
    /// descriptor, so a sibling test spawning git can hold a copy of this
    /// lock's fd for the few microseconds before it `exec`s and CLOEXEC
    /// closes it. Retrying briefly tests the release, not that race.
    fn reacquired(path: &Path) -> bool {
        for _ in 0..50 {
            if try_lock(path).unwrap().is_some() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn try_lock_succeeds_when_uncontended() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("state.lock");
        assert!(try_lock(&path).unwrap().is_some());
    }

    #[test]
    fn a_process_whose_group_is_gone_when_it_fails_is_never_signalled_later() {
        let mut state = State::new();
        let mut rec = WorktreeRecord::new("/abs/w", true);
        rec.processes.insert("gone".into(), running(100));
        rec.processes.insert("lingering".into(), running(200));
        rec.processes
            .insert("starting".into(), starting(300, Utc::now()));
        state.worktrees.insert("w".into(), rec);

        // Every leader is dead; only `lingering`'s group has a member
        // left, which the sweep still has to signal once.
        let lingering = Group::from_raw(200);
        assert!(advance_phases(
            &mut state,
            |_| false,
            |group| group == lingering,
            |_, _| false
        ));
        let procs = &state.worktrees["w"].processes;
        for name in ["gone", "lingering", "starting"] {
            assert!(
                matches!(procs[name].phase, Phase::Failed { .. }),
                "{name}: {:?}",
                procs[name].phase
            );
        }
        assert!(procs["gone"].swept, "nothing left to signal");
        assert!(procs["starting"].swept, "nothing left to signal");
        assert!(!procs["lingering"].swept, "its group still has a member");
    }
}
