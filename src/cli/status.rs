//! `status`: each worktree's processes, services and share, as text
//! or JSON.

use super::JSON_VERSION;
use super::ls::COL_GAP;
use super::ls::NAME_FLOOR;
use super::ls::ProjectOut;
use super::ls::display_name;
use super::ls::terminal_width;
use super::report_refresh;
use crate::actions;
use crate::actions::{url_owner_not_running, worktree_url};
use crate::paths::PandoPaths;
use crate::state::{Phase, ProcessRecord, WorktreeRecord};
use crate::term::{Paint, Style, ellipsize_distinct, ellipsize_end, text_width};
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::BTreeMap;
use std::io::Write;

// ---- status ---------------------------------------------------------------

/// One process, flattened for the machine-readable shape.
#[derive(Serialize)]
struct ProcessOut {
    pid: u32,
    /// `starting`, `running`, or `failed`.
    phase: &'static str,
    since: DateTime<Utc>,
    /// `null` unless the phase is `failed`.
    reason: Option<String>,
    log: String,
    /// How the app it serves is opened on a simulator or a device, for a
    /// process that runs a framework whose app runs on one; `null` for
    /// every other process.
    app: Option<AppOut>,
}

/// A process's app, opened on a simulator or a device: Expo's.
#[derive(Serialize)]
struct AppOut {
    /// What opens `url`: `Expo Go`, or `its development build` for an app
    /// that depends on `expo-dev-client`.
    client: &'static str,
    /// The URL `client` opens it at: `exp://…` in Expo Go.
    url: String,
    /// The command that opens `url` on the booted iOS simulator.
    simulator: String,
    /// The command that opens `url` on a connected Android device or
    /// emulator, after pointing its `127.0.0.1` port at this machine's.
    android: String,
    /// The URL a development build opens it at, its scheme `exp+` and the
    /// slug in `app.json` or a literal in `app.config.*`; `exp+<slug>`
    /// where neither says it.
    development_build: String,
    /// The app's native code this worktree's branch changes, which a
    /// build of another branch lacks; `null` when it changes none, and
    /// for the main checkout.
    native: Option<NativeOut>,
    /// The development build of this app installed on a booted iOS
    /// simulator, while the process runs; `null` when none is found.
    installed: Option<InstalledOut>,
}

/// A development build of the app on a booted simulator, and the SDK it
/// was made for against the one the worktree's JavaScript needs.
#[derive(Serialize)]
struct InstalledOut {
    /// The simulator's name.
    device: String,
    /// The SDK's major version the build was made with; `null` when its
    /// config does not say.
    sdk: Option<u32>,
    /// The SDK's major version the worktree needs, from its installed
    /// `expo` package, else its `package.json`; `null` when neither says.
    expected_sdk: Option<u32>,
    /// Run in the app's directory: builds and installs this worktree's own
    /// over it, pointed at its running Metro.
    build: String,
}

impl From<actions::InstalledBuild> for InstalledOut {
    fn from(build: actions::InstalledBuild) -> Self {
        Self {
            device: build.device,
            sdk: build.sdk,
            expected_sdk: build.expected_sdk,
            build: build.build,
        }
    }
}

/// What a branch changes of an app's native code, and how the worktree
/// gets a build of its own.
#[derive(Serialize)]
struct NativeOut {
    base: String,
    /// Relative to the worktree.
    changed: Vec<String>,
    /// Run in the app's directory: builds and installs this worktree's app
    /// on the iOS simulator, pointed at its running Metro. The same as
    /// `builds.ios`, kept for the readers written before `builds`.
    build: String,
    /// The same for every platform the app is built for, by platform:
    /// `ios`, `android`.
    builds: BTreeMap<&'static str, String>,
}

/// Each device app's native changes in a worktree, against the base its
/// branch forks from, where it has apps a device runs. None for the main
/// checkout, which is what the others are measured against.
fn native_of(
    paths: &PandoPaths,
    config: &crate::config::Config,
    w: &crate::worktree::Worktree,
    main: bool,
    record: &WorktreeRecord,
    apps: &BTreeMap<String, crate::catalog::frameworks::AppLinks>,
) -> BTreeMap<String, actions::NativeChanges> {
    if main || apps.is_empty() {
        return BTreeMap::new();
    }
    // Each checkout's base as the git menu reads it: the one recorded for
    // it, then config's, then its repository's own.
    let base_of = |repo: Option<&str>| {
        let root = crate::project::ProjectRef::checkout_of(paths.root(), repo);
        let branch = match repo {
            Some(repo) => w.repos.iter().find(|r| r.name == repo)?.branch.clone(),
            None => w.branch.clone(),
        };
        actions::git::base_for(&root, config, repo, branch.as_deref(), record.base_of(repo))
    };
    actions::native_changes(config, &paths.project, record, &base_of)
}

#[derive(Serialize)]
struct HookOut {
    fingerprint: Option<String>,
    ran_at: DateTime<Utc>,
}

/// One private service, flattened for the machine-readable shape.
#[derive(Serialize)]
struct ServiceOut {
    /// `compose` for now; `native` joins it in Phase 6.
    kind: &'static str,
    port: Option<u16>,
    /// Whether something answers on that port right now.
    up: bool,
    /// Whether a log pump is running in front of it. `false` while the
    /// worktree is stopped, and `false` with `up` true when the pump died:
    /// the log tab has stopped filling, and the next `start` or `restart`
    /// puts it back.
    logging: bool,
    /// The compose project the container belongs to, which is what `rm`
    /// takes down.
    project: Option<String>,
}

/// A worktree's public URL, when it has one.
///
/// No cookie, ever: the value `auth_cmd` produced lives in the proxy's
/// environment and nowhere else, and this shape is printed, logged, and
/// piped into things.
#[derive(Serialize)]
struct ShareOut {
    url: String,
    /// The port being published — the application's own.
    local_port: u16,
    /// The proxy in front of it, when `auth_cmd` put one there.
    proxy_port: Option<u16>,
    since: DateTime<Utc>,
}

#[derive(Serialize)]
struct StatusWorktreeOut {
    name: String,
    /// The main checkout, listed first once pando has run it.
    main: bool,
    branch: Option<String>,
    path: String,
    ports: BTreeMap<String, u16>,
    observed_ports: Vec<u16>,
    /// The readiness role's URL, when this worktree has one.
    url: Option<String>,
    /// Which services it talks to: `shared`, `namespaced` or `isolated`.
    mode: crate::state::ServiceMode,
    /// Whether this worktree runs private copies of the project's
    /// services: `mode` is `isolated`. Published before `mode` was, and
    /// kept for every program that reads it.
    isolated: bool,
    /// `null` when the worktree is not shared.
    share: Option<ShareOut>,
    processes: BTreeMap<String, ProcessOut>,
    services: BTreeMap<String, ServiceOut>,
    /// What it holds in the main checkout's own servers: a database or a
    /// slot of its own, one per service.
    namespaces: Vec<NamespaceOut>,
    hooks: BTreeMap<String, HookOut>,
}

/// One namespace a worktree holds. Never its login: that is not in state,
/// and this shape is printed, logged and piped into things.
#[derive(Serialize)]
struct NamespaceOut {
    service: String,
    /// The database's name, for a database.
    #[serde(skip_serializing_if = "Option::is_none")]
    database: Option<String>,
    /// The slot's number, for a slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    slot: Option<u32>,
    host: String,
    port: u16,
    /// Whether the worktree runs on it now. `false` for one kept through a
    /// switch to another mode, which `rm` still drops.
    in_use: bool,
}

#[derive(Serialize)]
struct StatusOutput {
    version: u32,
    project: ProjectOut,
    worktrees: Vec<StatusWorktreeOut>,
}

pub(super) fn phase_word(phase: &Phase) -> &'static str {
    match phase {
        Phase::Starting { .. } => "starting",
        Phase::Running { .. } => "running",
        Phase::Failed { .. } => "failed",
    }
}

fn phase_since(phase: &Phase) -> DateTime<Utc> {
    match phase {
        Phase::Starting { since } | Phase::Running { since } => *since,
        Phase::Failed { at, .. } => *at,
    }
}

fn phase_reason(phase: &Phase) -> Option<String> {
    match phase {
        Phase::Failed { reason, .. } => Some(reason.clone()),
        _ => None,
    }
}

pub fn status_json<W: Write>(paths: &PandoPaths, only: Option<&str>, out: &mut W) -> Result<()> {
    let refreshed = actions::refresh(paths);
    report_refresh(&refreshed);
    // The listing alone: the shape has no git fields, and enriching would
    // be a `git status` in every worktree for nothing.
    let (worktrees, main_name) = shown_checkouts(paths, only, &refreshed.state)?;
    if let Some(name) = only
        && worktrees.is_empty()
    {
        return Err(not_listed(name, refreshed.state.worktrees.get(name)));
    }
    // For the processes whose app a device runs; a config that does not
    // load only costs their links.
    let config = crate::config::load(paths)
        .map(|loaded| loaded.config)
        .unwrap_or_default();
    // Looked at once, and only for a worktree whose device app runs.
    let simulators = actions::Simulators::new(paths);
    let output = StatusOutput {
        version: JSON_VERSION,
        project: ProjectOut::of(&paths.project),
        worktrees: worktrees
            .into_iter()
            .map(|w| {
                let record = refreshed.state.worktrees.get(&w.name);
                let empty = WorktreeRecord::new(&w.path, false);
                let record = record.unwrap_or(&empty);
                let mut installed = actions::installed_builds(&config, record, &simulators);
                let mut apps = actions::app_links_installed(&config, record, &installed);
                let mut native = native_of(paths, &config, &w, w.name == main_name, record, &apps);
                StatusWorktreeOut {
                    name: w.name.clone(),
                    main: w.name == main_name,
                    branch: w.branch.clone(),
                    path: w.path.display().to_string(),
                    ports: record.ports.clone(),
                    observed_ports: record.observed_ports.clone(),
                    url: worktree_url(record),
                    mode: record.mode(),
                    isolated: record.mode() == crate::state::ServiceMode::Isolated,
                    share: record.share.as_ref().map(|share| ShareOut {
                        url: share.public_url.clone(),
                        local_port: share.local_port,
                        proxy_port: share.proxy_port,
                        since: share.started_at,
                    }),
                    services: actions::recorded_service_statuses(record)
                        .into_iter()
                        .map(|status| {
                            let recorded = record.services.iter().find(|s| s.name == status.name);
                            (
                                status.name,
                                ServiceOut {
                                    kind: match recorded.map(|s| s.kind) {
                                        Some(crate::state::ServiceKind::Native) => "native",
                                        _ => "compose",
                                    },
                                    port: status.port,
                                    up: status.up,
                                    logging: status.logging,
                                    project: recorded.and_then(|s| s.compose_project.clone()),
                                },
                            )
                        })
                        .collect(),
                    processes: record
                        .processes
                        .iter()
                        .map(|(name, p)| {
                            (
                                name.clone(),
                                ProcessOut {
                                    pid: p.pid,
                                    phase: phase_word(&p.phase),
                                    since: phase_since(&p.phase),
                                    reason: phase_reason(&p.phase),
                                    log: p.log_path.display().to_string(),
                                    app: apps.remove(name).map(|links| AppOut {
                                        client: links.client,
                                        url: links.url,
                                        simulator: links.simulator,
                                        android: links.android,
                                        development_build: links.development_build,
                                        native: native.remove(name).map(|n| NativeOut {
                                            base: n.base,
                                            changed: n.changed,
                                            build: n
                                                .builds
                                                .first()
                                                .map(|(_, build)| build.clone())
                                                .unwrap_or_default(),
                                            builds: n.builds.into_iter().collect(),
                                        }),
                                        installed: installed.remove(name).map(InstalledOut::from),
                                    }),
                                },
                            )
                        })
                        .collect(),
                    namespaces: record
                        .namespaces
                        .iter()
                        .map(|ns| NamespaceOut {
                            service: ns.service.clone(),
                            database: (ns.kind == crate::state::NamespaceKind::Database)
                                .then(|| ns.name.clone()),
                            slot: match ns.kind {
                                crate::state::NamespaceKind::Slot => ns.name.parse().ok(),
                                crate::state::NamespaceKind::Database => None,
                            },
                            host: ns.host.clone(),
                            port: ns.port,
                            in_use: record.mode() == crate::state::ServiceMode::Namespaced,
                        })
                        .collect(),
                    hooks: record
                        .hooks
                        .iter()
                        .map(|(name, h)| {
                            (
                                name.clone(),
                                HookOut {
                                    fingerprint: h.fingerprint.clone(),
                                    ran_at: h.ran_at,
                                },
                            )
                        })
                        .collect(),
                }
            })
            .collect(),
    };
    writeln!(out, "{}", serde_json::to_string_pretty(&output)?)?;
    Ok(())
}

pub fn status_text<W: Write>(paths: &PandoPaths, only: Option<&str>, out: &mut W) -> Result<()> {
    status_text_with(paths, only, out, terminal_width(), &Style::for_stdout())
}

/// [`status_text`] at a given terminal width, so the shedding is testable
/// without a terminal — the shape `ls_text_at` already has.
pub fn status_text_at<W: Write>(
    paths: &PandoPaths,
    only: Option<&str>,
    out: &mut W,
    width: usize,
) -> Result<()> {
    status_text_with(paths, only, out, width, &Style::plain())
}

/// The phase words, and what each looks like on a terminal. Painted after
/// a line is fitted, so colour never counts against its width.
const PAINTED_WORDS: [(&str, Paint); 7] = [
    ("running", Paint::Good),
    ("up", Paint::Good),
    ("public", Paint::Good),
    ("starting", Paint::Warn),
    ("failed", Paint::Bad),
    ("down", Paint::Bad),
    ("stopped", Paint::Faint),
];

/// `line` with its phase words and its URLs painted, token by token.
fn paint_line(line: &str, style: &Style) -> String {
    if !style.color() {
        return line.to_string();
    }
    line.split(' ')
        .map(|token| {
            if token.starts_with("http://") || token.starts_with("https://") {
                return style.paint(token, Paint::Link);
            }
            match PAINTED_WORDS.iter().find(|(word, _)| *word == token) {
                Some((_, paint)) => style.paint(token, *paint),
                None => token.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn status_text_with<W: Write>(
    paths: &PandoPaths,
    only: Option<&str>,
    out: &mut W,
    width: usize,
    style: &Style,
) -> Result<()> {
    let mut plain: Vec<u8> = Vec::new();
    status_lines(paths, only, &mut plain, width)?;
    for line in String::from_utf8_lossy(&plain).lines() {
        writeln!(out, "{}", paint_line(line, style))?;
    }
    Ok(())
}

fn status_lines<W: Write>(
    paths: &PandoPaths,
    only: Option<&str>,
    out: &mut W,
    width: usize,
) -> Result<()> {
    let refreshed = actions::refresh(paths);
    report_refresh(&refreshed);
    // For the services a namespaced worktree leaves shared, and why; a
    // config that does not load only costs those lines.
    let config = crate::config::load(paths).ok().map(|loaded| loaded.config);
    // Looked at once, and only for a worktree whose device app runs.
    let simulators = actions::Simulators::new(paths);
    // The listing alone, as for the JSON: nothing here reads a git field.
    let (shown, main_name) = shown_checkouts(paths, only, &refreshed.state)?;
    if shown.is_empty() {
        if let Some(name) = only {
            return Err(not_listed(name, refreshed.state.worktrees.get(name)));
        }
        writeln!(out, "no worktrees — `pando new <branch>` creates one")?;
        return Ok(());
    }
    // Named as `ls` names them: by branch, where the directory is the
    // branch spelled for a filesystem. Measured in columns, and cut only as
    // far as leaves the phase word room, never below `ls`'s floor.
    let all_names: Vec<String> = shown
        .iter()
        .map(|w| match (w.name == main_name, &w.branch) {
            // By its branch, as `ls` and the TUI name it, and said to be
            // the main checkout: the line is about it, not a worktree.
            (true, Some(branch)) => format!("{branch} (main checkout)"),
            (true, None) => format!("{} (main checkout)", display_name(w)),
            (false, _) => display_name(w),
        })
        .collect();
    let widest = all_names.iter().map(|n| text_width(n)).max().unwrap_or(0);
    let names = widest.min(width.saturating_sub(COL_GAP + PHASE_CELL).max(NAME_FLOOR));
    for (w, shown_name) in shown.iter().zip(&all_names) {
        let record = refreshed.state.worktrees.get(&w.name);
        writeln!(
            out,
            "{}  {}",
            pad(&ellipsize_distinct(shown_name, &all_names, names), names),
            worktree_line(record, width.saturating_sub(names + COL_GAP))
        )?;
        // One line per process under it, so a worktree that is `failed`
        // says which of its processes is, and each one's pid is reachable.
        let Some(record) = record else { continue };
        let installed = config
            .as_ref()
            .map(|config| actions::installed_builds(config, record, &simulators))
            .unwrap_or_default();
        let apps = config
            .as_ref()
            .map(|config| actions::app_links_installed(config, record, &installed))
            .unwrap_or_default();
        let native = config
            .as_ref()
            .map(|config| native_of(paths, config, w, w.name == main_name, record, &apps))
            .unwrap_or_default();
        let process_width = record
            .processes
            .keys()
            .map(|name| text_width(name))
            .max()
            .unwrap_or(0);
        for (name, p) in &record.processes {
            // The same shape as the worktree line above, and the same
            // degradation: a reason or an uptime is truncated rather than
            // allowed to wrap the row under it.
            let row = format!("  {}  {}", pad(name, process_width), process_line(p));
            writeln!(out, "{}", ellipsize_end(&row, width))?;
            // Under a running bundler whose app a device runs, the command
            // that opens it on the simulator, first, so a narrow terminal
            // cuts the development build's form rather than the command.
            if matches!(p.phase, Phase::Running { .. })
                && let Some(links) = apps.get(name)
            {
                // The development build's link only where it is not the
                // one the command already opens.
                let other = match links.url == links.development_build {
                    true => String::new(),
                    false => format!(
                        "; for a development build, open {}",
                        links.development_build
                    ),
                };
                let row = format!(
                    "  {}  {:<PHASE_CELL$}  {} — opens it in {} on the simulator{other}",
                    pad(name, process_width),
                    "app",
                    links.simulator,
                    links.client,
                );
                writeln!(out, "{}", ellipsize_end(&row, width))?;
                // And under it, the same on an Android device or emulator.
                let row = format!(
                    "  {}  {:<PHASE_CELL$}  {} — opens it on an Android device or emulator",
                    pad(name, process_width),
                    "app",
                    links.android,
                );
                writeln!(out, "{}", ellipsize_end(&row, width))?;
            }
            // A build on the simulator made for another SDK: the bundle
            // this Metro serves calls into native code it lacks.
            if let Some(mismatch) = installed.get(name).and_then(|build| build.mismatch()) {
                let row = format!(
                    "  {}  {:<PHASE_CELL$}  {mismatch}",
                    pad(name, process_width),
                    "build",
                );
                writeln!(out, "{}", ellipsize_end(&row, width))?;
            }
            // Whatever its phase: a build is made before its bundler runs.
            if let Some(changes) = native.get(name) {
                let [first, rest @ ..] = changes.changed.as_slice() else {
                    continue;
                };
                let more = match rest.len() {
                    0 => String::new(),
                    n => format!(" and {n} more"),
                };
                // One command per platform, the iOS simulator's first.
                let builds: Vec<String> = changes
                    .builds
                    .iter()
                    .map(|(platform, build)| format!("`{build}` ({platform})"))
                    .collect();
                let row = format!(
                    "  {}  {:<PHASE_CELL$}  this branch changes native code against {} ({first}{more}) \
                     — a build of another branch lacks it; {} in the app's directory builds its own",
                    pad(name, process_width),
                    "native",
                    changes.base,
                    builds.join(" or "),
                );
                writeln!(out, "{}", ellipsize_end(&row, width))?;
            }
        }
        // A port the worktree's processes were given and nothing listens
        // on: the api half of a root script whose web half is up.
        for (role, port) in crate::state::silent_ports(record, Utc::now()) {
            let row = format!(
                "  {}  {:<PHASE_CELL$}  nothing listens on {port} — the log says why",
                pad(&role, process_width),
                "silent",
            );
            writeln!(out, "{}", ellipsize_end(&row, width))?;
        }
        // And one per private service, so a worktree whose database is
        // down says which one rather than only that its app failed.
        let services = actions::service_statuses(record);
        let namespaces = actions::namespace_lines(paths, config.as_ref(), &w.name, record);
        let service_width = services
            .iter()
            .map(|s| text_width(&s.name))
            .chain(namespaces.iter().map(|(service, _, _)| text_width(service)))
            .max()
            .unwrap_or(0)
            .max(process_width);
        for service in &services {
            let port = match service.port {
                Some(port) => port.to_string(),
                None => "-".to_string(),
            };
            // Only for a service that is *up*: a stopped worktree has no
            // pump by design, and saying so on every line of every stopped
            // service would bury the one case that matters — a container
            // answering while nothing fills its log tab. `start` and
            // `restart` put the pump back; no read path ever does.
            let pump = match service.up && !service.logging {
                true => ", no log pump",
                false => "",
            };
            let row = format!(
                "  {}  {:<PHASE_CELL$}  service on {port}{pump}",
                pad(&service.name, service_width),
                if service.up { "up" } else { "down" },
            );
            writeln!(out, "{}", ellipsize_end(&row, width))?;
        }
        // And what it holds in the main checkout's own servers: its own
        // database and slot, ones kept for the way back from another mode,
        // and the services a namespaced worktree leaves on main's data.
        for (service, word, what) in &namespaces {
            let row = format!(
                "  {}  {word:<PHASE_CELL$}  {what}",
                pad(service, service_width)
            );
            writeln!(out, "{}", ellipsize_end(&row, width))?;
        }
        // And the public URL, last, because it is the line somebody is
        // most often here to copy.
        if let Some(share) = &record.share {
            let through = match share.proxy_port {
                Some(port) => format!(" through a proxy on {port}"),
                None => String::new(),
            };
            let row = format!(
                "  {}  {:<PHASE_CELL$}  {}{through}",
                pad("share", service_width),
                "public",
                share.public_url,
            );
            writeln!(out, "{}", ellipsize_end(&row, width))?;
        }
    }
    Ok(())
}

/// `text` padded with spaces to `width` columns. `{:<width$}` counts
/// characters, and a wide one takes two columns.
fn pad(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(text_width(text)))
    )
}

/// What `status` lists, and the main checkout's name: the main checkout
/// first, once pando has anything recorded for it or when it is the one
/// asked about, then every worktree but a check's — or, with `only`, the
/// one it names.
///
/// A main checkout pando never ran is not a row: a project with no
/// worktree still reads "no worktrees", and one with some lists them.
fn shown_checkouts(
    paths: &PandoPaths,
    only: Option<&str>,
    state: &crate::state::State,
) -> Result<(Vec<crate::worktree::Worktree>, String)> {
    let found = crate::worktree::discover_all(&paths.project)?;
    let main_name = found.main.name.clone();
    let main = match only {
        Some(name) => name == main_name,
        None => state.worktrees.contains_key(&main_name),
    };
    let shown = main
        .then_some(found.main)
        .into_iter()
        .chain(found.worktrees.into_iter().filter(|w| {
            !crate::worktree::is_check(&w.name) && only.is_none_or(|name| w.name == name)
        }))
        .collect();
    Ok((shown, main_name))
}

/// The error for a name `status` was given that git does not list.
///
/// Such a name resolves because pando still has a record or logs of it,
/// so `stop` can clean up after a worktree removed outside pando. Its
/// processes may still be up and holding their ports, and then that is
/// what the error says.
fn not_listed(name: &str, record: Option<&WorktreeRecord>) -> anyhow::Error {
    let up: Vec<&str> = record
        .into_iter()
        .flat_map(|r| &r.processes)
        .filter(|(_, p)| !matches!(p.phase, Phase::Failed { .. }))
        .map(|(process, _)| process.as_str())
        .collect();
    if up.is_empty() {
        return anyhow::anyhow!("no worktree named \"{name}\" — git does not list it");
    }
    anyhow::anyhow!(
        "git no longer lists {name}, and pando still runs {} for it — `pando stop {name}` \
         stops what is left",
        up.join(", ")
    )
}

/// Width the phase word is padded to, so the cell after it lines up
/// whichever of the four words is printed.
const PHASE_CELL: usize = 8;

/// The worktree's own line: its aggregate phase, its ports, and the one URL
/// it serves on, fitted to `room` characters.
fn worktree_line(record: Option<&WorktreeRecord>, room: usize) -> String {
    let Some(record) = record else {
        return "stopped".to_string();
    };
    let ports = ports_text(&record.ports);
    let Some(aggregate) = crate::state::aggregate_phase(record) else {
        if record.ports.is_empty() {
            return "stopped".to_string();
        }
        // The ports survive a stop, and saying so is how a developer knows
        // the URL they bookmarked will still be theirs.
        return fit_line("stopped", &ports, None, None, room);
    };
    let age = human_duration(Utc::now().signed_duration_since(aggregate.since()));
    // Not while the process it points at is stopped and a sibling runs:
    // nothing answers it then. `--json` still carries it, as documented.
    let url = worktree_url(record).filter(|_| url_owner_not_running(record).is_none());
    match aggregate {
        crate::state::Aggregate::Running { .. } => fit_line(
            "running",
            &ports,
            url.as_deref(),
            Some(&format!("up {age}")),
            room,
        ),
        crate::state::Aggregate::Starting { .. } => {
            fit_line("starting", &ports, None, Some(&format!("for {age}")), room)
        }
        // Named: `failed` on a worktree running three processes is a
        // question until it says which one.
        crate::state::Aggregate::Failed { .. } => {
            fit_line("failed", &ports, None, aggregate.reason().as_deref(), room)
        }
    }
}

/// Assembles a worktree's status line and fits it into `room` characters.
///
/// The TUI is used in tmux splits and `pando status` is read in the same
/// ones, so this degrades rather than wraps. The URL goes first: it is the
/// longest cell by far and `--json` still carries it. The ports cell is
/// truncated after that, because a developer who can see three roles and
/// two numbers knows more than one looking at a line that wrapped.
fn fit_line(word: &str, ports: &str, url: Option<&str>, tail: Option<&str>, room: usize) -> String {
    let assemble = |ports: &str, url: Option<&str>| {
        let mut parts = vec![format!("{word:<PHASE_CELL$}"), ports.to_string()];
        parts.extend(url.map(str::to_string));
        parts.extend(tail.map(str::to_string));
        parts.join(&" ".repeat(COL_GAP))
    };
    let full = assemble(ports, url);
    if text_width(&full) <= room {
        return full;
    }
    let without_url = assemble(ports, None);
    if text_width(&without_url) <= room {
        return without_url;
    }
    let over = text_width(&without_url) - room;
    let keep = text_width(ports).saturating_sub(over);
    // And if even an empty ports cell will not fit — a split narrow enough
    // that the phase word and the age are already too much — the line is
    // truncated rather than left to wrap onto the process rows below it.
    ellipsize_end(&assemble(&ellipsize_end(ports, keep), None), room)
}

fn process_line(p: &ProcessRecord) -> String {
    match &p.phase {
        Phase::Running { since } => format!(
            "running   pid {}  up {}",
            p.pid,
            human_duration(Utc::now().signed_duration_since(*since))
        ),
        Phase::Starting { since } => format!(
            "starting  pid {}  for {}",
            p.pid,
            human_duration(Utc::now().signed_duration_since(*since))
        ),
        Phase::Failed { reason, .. } => format!("failed    {reason}"),
    }
}

fn ports_text(ports: &BTreeMap<String, u16>) -> String {
    if ports.is_empty() {
        return "-".to_string();
    }
    ports
        .iter()
        .map(|(role, port)| format!("{role} {port}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Uptime in the shortest form that is still precise enough to be useful.
pub(super) fn human_duration(d: chrono::TimeDelta) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}
