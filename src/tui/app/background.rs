//! Work that leaves the UI thread: the events it reports back with, the
//! repository snapshot, and the refresh, discovery and fetch workers.

use anyhow::Result;
use ratatui::crossterm::event::Event;
use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::actions;
use crate::cache;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::state::{self, State};
use crate::worktree::{self, BranchEntry, EnrichUpdate, GhAccount, PrInfo, Worktree};

use super::{App, ServiceHealth};

/// Everything the event loop can deliver. One enum so the loop blocks on a
/// single receive and redraws once per batch.
pub enum AppEvent {
    Input(Event),
    Tick,
    FsChange,
    Discovered(Box<Result<Snapshot, String>>),
    Enrich(EnrichUpdate),
    /// One read of git state is over: a quiet re-read, or an announced one.
    EnrichDone {
        quiet: bool,
    },
    BranchesReady(Vec<BranchEntry>),
    PrsReady(Result<Vec<PrInfo>, String>),
    /// Which GitHub account `gh` acts as for this project.
    GhAccountReady(GhAccount),
    /// Process state, advanced and saved off the UI thread, with what the
    /// refresh had to say beside it: why the state file could not be used,
    /// and each share or service it found dead.
    Refreshed(Box<QuickRefresh>),
    /// Whether the project's services are answering — the shared ones for
    /// the header chips, each worktree's private ones for its detail rows.
    /// Probed on the refresh worker: it is a TCP connect apiece.
    ServiceHealth(Box<ServiceHealth>),
    /// A worker needs an answer before it can go on. It is blocked on the
    /// other end of this channel until one arrives — or until the channel
    /// is dropped, which is how quitting aborts it.
    AskQuestion(Box<(actions::Question, Sender<Result<actions::Answer, String>>)>),
    /// Detection settled something, and this is the config with it applied.
    /// Sent as soon as it is resolved rather than with the outcome: the
    /// answer is on disk by then, so the UI thread's copy has to match
    /// whether or not the start that followed worked.
    ConfigResolved(Box<Config>),
    /// A shell or editor handed to tmux or the desktop did not start.
    LaunchFailed(String),
    /// What opening an app on a simulator or a device is waiting for.
    AppOpening(String),
    /// Where the app opened, or why it did not.
    AppOpened(Result<String, String>),
    /// Something a worker decided without asking, worth a line in the
    /// header and in `m`: the first choice it took for a question.
    Notice(String),
    /// The theme to paint with changed: the terminal's theme switcher
    /// wrote a new name, or the system went dark or light.
    Theme(Box<crate::theme::Resolved>),
    /// What pando's own detection proposes for the project, for the setup
    /// screen.
    SetupDetected(Box<Vec<crate::detect::Proposal>>),
    /// The setup screen's re-read after a config file changed: the config
    /// as it loads now and the setup against it, or why it does not load.
    SetupRead(Box<Result<(Config, crate::setup::Setup), String>>),
    /// pando's own guess, worked out for the setup screen's `⏎`.
    SetupTried(Box<Result<super::setup::Tried, String>>),
    /// Where a checkout stands, for the git menu opened on it.
    GitRead(Box<(String, actions::git::GitRead)>),
}

/// One consistent read of the repository, taken off the UI thread.
pub struct Snapshot {
    pub main: Worktree,
    pub worktrees: Vec<Worktree>,
    pub created_by_pando: BTreeMap<String, bool>,
    /// Process state as of the same read, so the list and the detail pane
    /// never disagree about what is running.
    pub state: State,
    /// Why the ownership map may be wrong — the same one line `ls` prints
    /// and `rm` refuses with, so the TUI never shows "adopted" about a
    /// state file it could not read without saying so.
    pub warning: Option<String>,
    /// What the refresh itself had to do — a share whose tunnel died, most
    /// often. Shown once, as a status line.
    pub notices: Vec<String>,
    pub default_base: Option<String>,
}

/// What the one-second refresh found.
pub struct QuickRefresh {
    pub refreshed: actions::Refreshed,
    /// Whether [`actions::refresh`] ran. The read [`refresh_if_needed`]
    /// takes instead when it cannot matter takes no lock and saves
    /// nothing, so a warning it did not give is not one it cleared.
    pub ran: bool,
}

/// Sends a question to the UI thread and waits for the answer.
///
/// Called from a worker, never from the UI thread. A dropped receiver — the
/// app quitting while the modal is open — comes back as an error, so the
/// worker unwinds instead of waiting for an answer nobody will give.
pub(super) fn ask_through_ui(
    tx: &Sender<AppEvent>,
    question: &actions::Question,
) -> Result<actions::Answer> {
    // The rules' first choice is taken, not asked about — see
    // `actions::recommended` — and said where `m` keeps it.
    if let Some((answer, line)) = actions::recommended(question) {
        let _ = tx.send(AppEvent::Notice(line));
        return Ok(answer);
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    tx.send(AppEvent::AskQuestion(Box::new((
        question.clone(),
        reply_tx,
    ))))
    .map_err(|_| anyhow::anyhow!("pando is shutting down"))?;
    match reply_rx.recv() {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(reason)) => Err(anyhow::anyhow!(reason)),
        Err(_) => Err(anyhow::anyhow!("cancelled")),
    }
}

/// The config as it is on disk now, for a worker about to resolve and act
/// on it.
///
/// Read again rather than taken from the session. Another pando — an
/// agent running `init --answers`, or the developer's own edit — may have
/// written `pando.toml` since the TUI opened, and resolving the copy from
/// before that recorded an override nobody made, wrote this session's
/// pick over the other writer's answer, and started on the old command.
/// A file that does not load is one somebody is editing, so it stops the
/// worker rather than falling back to the copy it would have overwritten.
///
/// And guarded as `main` guards the copy it loads. `config::load` checks
/// `worktrees_dir` against the repository root and nothing else, so one
/// moved into a linked worktree since the TUI opened is where `n` would
/// make the next worktree: inside another checkout.
pub(super) fn config_now(paths: &PandoPaths) -> Result<Config, String> {
    let config = crate::config::load(paths)
        .map(|loaded| loaded.config)
        .map_err(|e| format!("{e:#}"))?;
    actions::guard_write_locations(paths, &config).map_err(|e| format!("{e:#}"))?;
    Ok(config)
}

/// One consistent read of the repository: the worktrees, who owns them, and
/// the base a new branch would fork from. Runs off the UI thread, except
/// once before the first frame.
///
/// `known_base` is the base the last snapshot resolved: three git calls
/// to find again an answer that almost never changes, so it is reused and
/// re-resolved only on the slow git tick. `scan` asks for the full refresh,
/// sockets and all; without it the refresh is [`refresh_if_needed`]'s,
/// which is what keeps the first frame from waiting on a socket scan of
/// every running worktree.
pub fn snapshot(paths: &PandoPaths, known_base: Option<String>, scan: bool) -> Result<Snapshot> {
    thread::scope(|scope| {
        // Beside the listing rather than after it: on the first frame the
        // two are most of the wait.
        let resolving = known_base
            .is_none()
            .then(|| scope.spawn(|| worktree::resolve_base_branch(paths.root())));
        let mut discovery = worktree::discover_all(&paths.project)?;
        // A check's throwaway worktree is not a row: it comes and goes on
        // its own, and nothing a row offers is for it.
        discovery.worktrees.retain(|w| !worktree::is_check(&w.name));
        // One read of state for both answers, so the list's ownership dots
        // and its status column cannot come from two different moments.
        let refreshed = if scan {
            actions::refresh(paths)
        } else {
            refresh_if_needed(paths).refreshed
        };
        let default_base =
            known_base.or_else(|| resolving.and_then(|handle| handle.join().ok().flatten()));
        Ok(Snapshot {
            created_by_pando: actions::ownership(&refreshed.state, &discovery.worktrees),
            main: discovery.main,
            worktrees: discovery.worktrees,
            state: refreshed.state,
            warning: refreshed.warning,
            notices: refreshed.notices,
            default_base,
        })
    })
}

/// The one-second refresh, without the socket scan when it cannot matter.
///
/// [`actions::refresh`] scans every live process group's listening sockets
/// (a `ps` and an `lsof` apiece), which with a few worktrees running costs
/// far more than everything else the idle TUI does. The scan only moves a
/// process forward while it is `Starting`; a `Running` one changes phase
/// when its process dies, which a signal probe sees without spawning
/// anything. So when nothing is starting and every running process is
/// alive, the state file is read as it stands. The full refresh still runs
/// with every discovery, so ports a running server opens later are
/// captured within one slow tick.
pub(super) fn refresh_if_needed(paths: &PandoPaths) -> QuickRefresh {
    match state::load(&paths.state_file()) {
        Ok(store)
            if !needs_advance(
                &store,
                crate::process::is_alive,
                crate::process::group_alive,
            ) =>
        {
            QuickRefresh {
                refreshed: actions::Refreshed {
                    state: store,
                    ..Default::default()
                },
                ran: false,
            }
        }
        _ => QuickRefresh {
            refreshed: actions::refresh(paths),
            ran: true,
        },
    }
}

/// Whether a refresh could change anything: a process still starting, a
/// running one advancing would call dead, or a share's or native service's
/// pid gone.
///
/// A running process is asked by the rule advancing reads it with,
/// [`state::ProcessRecord::alive`], not by its leader alone. A portless
/// process whose command backgrounded its work and returned has a dead
/// leader for as long as it runs, and advancing keeps it Running all that
/// time: asking the leader sent every tick down the full path, a `ps` and
/// an `lsof` a second, to change nothing. The group probe is a signal, so
/// it spawns nothing either.
///
/// A service counts only when it is native, the one kind the refresh
/// forgets: a compose service's pid is its log pump, which nothing on the
/// read path clears, so a dead one would take the full path every second
/// until the next mutation and change nothing.
pub(super) fn needs_advance(
    store: &State,
    is_alive: impl Fn(u32) -> bool,
    group_alive: impl Fn(crate::process::Group) -> bool,
) -> bool {
    store.worktrees.values().any(|record| {
        let process = record.processes.values().any(|p| match p.phase {
            state::Phase::Starting { .. } => true,
            state::Phase::Running { .. } => !p.alive(&is_alive, &group_alive),
            state::Phase::Failed { .. } => false,
        });
        let share = record
            .share
            .as_ref()
            .is_some_and(|s| !is_alive(s.tunnel_pid) || s.proxy_pid.is_some_and(|p| !is_alive(p)));
        let service = record
            .services
            .iter()
            .any(|s| s.kind == state::ServiceKind::Native && s.pid.is_some_and(|p| !is_alive(p)));
        process || share || service
    })
}

impl App {
    /// Replaces the list, preserving enrichment already collected for names
    /// that are still there. Returns the names that are new, so only those
    /// get enriched.
    pub(super) fn apply_snapshot(&mut self, snapshot: Snapshot) -> Vec<String> {
        // Captured before the list is replaced: after that, the old row
        // index points at whatever worktree the refresh moved into it.
        let keep = self.selected_worktree().map(|w| w.name.clone());
        let row = self.list_state.selected().unwrap_or(0);
        let known: HashMap<String, Worktree> = self
            .worktrees
            .drain(..)
            .map(|w| (w.name.clone(), w))
            .collect();
        let mut fresh = Vec::new();
        // The main checkout is the first row, enriched like any other.
        // Whether it shows is `refilter`'s to say: see `App::main_row_shown`.
        self.worktrees = std::iter::once(snapshot.main.clone())
            .chain(snapshot.worktrees)
            .map(|mut w| {
                match known.get(&w.name) {
                    Some(old) if old.head == w.head => {
                        w.head_sha = old.head_sha.clone();
                        w.head_subject = old.head_subject.clone();
                        w.head_age = old.head_age.clone();
                        w.dirty = old.dirty;
                        w.ahead_behind = old.ahead_behind;
                    }
                    _ => fresh.push(w.name.clone()),
                }
                w
            })
            .collect();
        self.main = Some(snapshot.main);
        self.default_base = snapshot.default_base;
        self.created_by_pando = snapshot.created_by_pando;
        // Through the same door as the refresh: the full refresh a
        // discovery runs is as likely as the quick one to be the read
        // that first finds a process dead.
        self.adopt_state(snapshot.state);
        self.report_refresh(snapshot.warning, snapshot.notices);
        let keep = self.take_arrival().or(keep);
        self.refilter_keeping(keep.clone(), row);
        // The worktree under the cursor went away, and the cursor is on
        // its neighbour now: whose tail it shows starts over, as a move
        // would start it.
        let after = self.selected_worktree().map(|w| w.name.clone());
        if keep.is_some() && after != keep {
            self.tail_index = 0;
            self.tail_scroll = 0;
        }
        fresh
    }

    /// What a refresh said beside the state, from either door: the quick
    /// refresh or the full one a discovery runs. Returns whether it said
    /// anything.
    ///
    /// A warning is said when it appears or changes. A state file pando
    /// cannot read stays that way, and saying so on every quick refresh
    /// fills `m` with that one line within a minute.
    pub(super) fn report_refresh(&mut self, warning: Option<String>, notices: Vec<String>) -> bool {
        let mut said = false;
        if warning != self.state_warning {
            if let Some(message) = warning.clone() {
                self.set_error(message);
                said = true;
            }
            self.state_warning = warning;
        }
        // Shares and services that died since the last refresh. Every one
        // of them, once: the records are already gone, so the next refresh
        // has nothing to repeat, and showing only the first means the
        // second worktree's public URL closed in silence. The status line
        // truncates rather than wraps, like every other row.
        if !notices.is_empty() {
            self.set_error(notices.join(" · "));
            said = true;
        }
        said
    }

    /// The worktree `n` just made, once a discovery has listed it: it takes
    /// the cursor, and a filter that would hide it is cleared, or the
    /// cursor has nowhere to go.
    ///
    /// Not while the filter line has the keyboard. Out of filter mode, the
    /// rest of what is being typed lands on the list as commands — `d` ⏎
    /// removes the worktree just made — so the arrival waits for the
    /// filter to be done with.
    pub(super) fn take_arrival(&mut self) -> Option<String> {
        if self.typing_filter() {
            return None;
        }
        let name = self
            .select_on_arrival
            .take_if(|name| self.worktrees.iter().any(|w| w.name == *name))?;
        self.filter.clear();
        self.mode = super::Mode::Normal;
        self.tail_index = 0;
        self.tail_scroll = 0;
        Some(name)
    }

    pub(super) fn hydrate_from_cache(&mut self) {
        let cached = cache::load(&self.paths.enrich_cache_file());
        for wt in self.worktrees.iter_mut() {
            if wt.head_sha.is_some() {
                continue;
            }
            if let Some(entry) = cached.entries.get(&wt.name) {
                cache::apply_to_worktree(wt, entry);
            }
        }
    }

    pub(super) fn save_enrich_cache(&self) {
        let mut file = cache::CacheFile::new();
        for wt in &self.worktrees {
            file.entries.insert(
                wt.name.clone(),
                cache::CachedEnrich {
                    branch: wt.branch.clone(),
                    prunable: wt.prunable,
                    head_sha: wt.head_sha.clone(),
                    head_subject: wt.head_subject.clone(),
                    head_age: wt.head_age.clone(),
                    dirty: wt.dirty,
                    ahead_behind: wt.ahead_behind,
                },
            );
        }
        let _ = cache::save(&self.paths.enrich_cache_file(), &file);
    }

    pub(super) fn save_pr_cache(&self) {
        let mut file = cache::PrCacheFile::new();
        file.prs = self
            .prs
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let _ = cache::save_prs(&self.paths.pr_cache_file(), &file);
    }

    pub fn pr_for(&self, wt: &Worktree) -> Option<&PrInfo> {
        self.prs.get(wt.branch.as_deref()?)
    }

    // ---- background work -------------------------------------------------
    //
    // Everything below leaves the UI thread immediately. A key handler that
    // shells out to git freezes the frame for as long as git takes.

    /// Advances phases, captures observed ports, and saves — off the UI
    /// thread, because it takes the state lock and scans listening sockets.
    pub fn spawn_refresh(&mut self) {
        if self.refreshing {
            return;
        }
        self.refreshing = true;
        let paths = self.paths.clone();
        let config = self.config.clone();
        let tx = self.event_tx.clone();
        // Only the selected worktree's private services are probed: its
        // detail pane is the one place they are shown, and probing every
        // worktree's was a connect per service a second, nearly all of it
        // thrown away.
        let selected = self.selected_worktree().map(|w| w.name.clone());
        thread::spawn(move || {
            let quick = refresh_if_needed(&paths);
            // Before the state is handed over: probing is a TCP connect
            // per service, and it has no business on the UI thread or in
            // a paint.
            let health = ServiceHealth {
                shared: actions::shared_service_statuses(
                    &paths,
                    &config,
                    &actions::env_dirs(&config),
                ),
                worktrees: selected
                    .and_then(|name| {
                        let record = quick.refreshed.state.worktrees.get(&name)?;
                        (!record.services.is_empty())
                            .then(|| (name, actions::service_statuses(record)))
                    })
                    .into_iter()
                    .collect(),
            };
            let _ = tx.send(AppEvent::Refreshed(Box::new(quick)));
            // Sent when empty too: a worktree whose private services were
            // just taken down must lose their rows, and an answer that is
            // the same as the last one costs no repaint.
            let _ = tx.send(AppEvent::ServiceHealth(Box::new(health)));
        });
    }

    /// A fresh listing and a full refresh, off the UI thread. One at a
    /// time: asked for while one runs, it runs again once that one lands,
    /// rather than stacking up behind the state lock.
    pub fn spawn_discovery(&mut self) {
        // Kept with the request rather than read off the tick it runs on:
        // the slow git tick's discovery, put off behind another, runs
        // once the tick has moved on.
        self.resolve_base |= self.tick.is_multiple_of(super::GIT_ALL_EVERY);
        if self.discovering {
            self.discover_again = true;
            return;
        }
        self.discovering = true;
        let paths = self.paths.clone();
        let tx = self.event_tx.clone();
        let known_base = if std::mem::take(&mut self.resolve_base) {
            None
        } else {
            self.default_base.clone()
        };
        thread::spawn(move || {
            let result = snapshot(&paths, known_base, true).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::Discovered(Box::new(result)));
        });
    }

    /// Enriches `only` when given, every worktree otherwise. A `quiet` read
    /// is not announced in the header.
    pub(super) fn spawn_enrichment(&mut self, only: Option<Vec<String>>, quiet: bool) {
        let items: Vec<(String, std::path::PathBuf)> = self
            .worktrees
            .iter()
            .filter(|w| only.as_ref().is_none_or(|names| names.contains(&w.name)))
            .map(|w| (w.name.clone(), w.path.clone()))
            .collect();
        if items.is_empty() {
            return;
        }
        if quiet {
            self.git_refreshing = true;
        } else {
            self.enriching += 1;
        }
        let root = self.paths.root().to_path_buf();
        let tx = self.event_tx.clone();
        // A few rows are measured against the base the detail pane names,
        // which discovery keeps current; the full pass resolves its own.
        let known_base = only.and(self.default_base.clone());
        thread::spawn(move || {
            let (etx, erx) = mpsc::channel();
            let forward = tx.clone();
            let pump = thread::spawn(move || {
                while let Ok(update) = erx.recv() {
                    if forward.send(AppEvent::Enrich(update)).is_err() {
                        break;
                    }
                }
            });
            // Narrow pool: every job forks `git status` against a full tree,
            // and startup enrichment must not starve the UI.
            worktree::enrich_stream(&root, items, known_base, etx, 4);
            let _ = pump.join();
            let _ = tx.send(AppEvent::EnrichDone { quiet });
        });
    }

    /// One at a time: `p` pressed twice does not ask GitHub twice. One
    /// asked for while another is out runs when that one lands: the one
    /// out may have left before the merge it was asked about.
    pub fn spawn_pr_fetch(&mut self) {
        if self.pr_fetching {
            self.pr_fetch_again = true;
            return;
        }
        // Tests press `p` like any other key; they must not reach GitHub.
        if cfg!(test) {
            return;
        }
        self.pr_fetching = true;
        let root = self.paths.root().to_path_buf();
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let result = worktree::list_prs(&root).map_err(|e| format!("{e:#}"));
            let _ = tx.send(AppEvent::PrsReady(result));
        });
    }

    /// The fetch asked for while the last one was out.
    pub(super) fn fetch_prs_again(&mut self) {
        if std::mem::take(&mut self.pr_fetch_again) {
            self.spawn_pr_fetch();
        }
    }

    /// Asks `gh` which account it is, from the project's own directory —
    /// a network round trip, so never on the UI thread.
    pub fn spawn_gh_account_check(&self) {
        // The key tests press `R` like any other key; they must neither
        // reach the network nor learn whose laptop they run on.
        if cfg!(test) {
            return;
        }
        let root = self.paths.root().to_path_buf();
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let _ = tx.send(AppEvent::GhAccountReady(worktree::gh_account(&root)));
        });
    }

    pub(super) fn spawn_branch_fetch(&self) {
        let root = self.paths.root().to_path_buf();
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let _ = tx.send(AppEvent::BranchesReady(worktree::list_branches(&root)));
        });
    }
}
