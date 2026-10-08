//! Workspaces: the apps of a monorepo, each with its own dev script — a
//! workspace's members, or the app directories below a root that is not
//! an app.

use std::collections::BTreeMap;
use std::path::Path;

use crate::catalog::frameworks;
use crate::catalog::frameworks::{FrameworkRule, PortMechanism};
use crate::catalog::package_managers::{self, Ecosystem};
use crate::config::{PortsSpec, ProcessConfig, ReadySpec};

use super::apply::DEV;
use super::dev::{
    dev_script, flag_reaches_server, is_multiplexer, own_port, script_args, script_runner,
};
use super::frameworks::{only_builds, runs, script_framework};
use super::proposal::{Candidate, Proposal, Slot};
use super::signals::{AppDir, LIBRARY_PARENT, Signals, env_file_value, parse_scripts, present};

/// One app of a workspace: a directory with its own manifest and its own
/// dev script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceApp {
    /// The directory's own name, which is the process's name and its role.
    pub name: String,
    /// Relative to the repository root, for example `apps/web`.
    pub dir: String,
    /// The script pando would run, already prefixed with the runner.
    pub cmd: String,
    /// The port this app listens on when nobody tells it otherwise. What
    /// makes a localhost URL in the env example resolvable to *this* app.
    pub default_port: Option<u16>,
    /// How this app takes a port, from its own framework rule.
    pub port: PortMechanism,
    /// The port its own dev script fixes, which no port pando hands it can
    /// move: `next dev --port 3000` binds 3000 whatever `PORT` says, and
    /// so does `PORT=3000 next dev`. Such an app owns no role.
    pub fixed_port: Option<u16>,
    /// The readiness wait its framework's rule proposes for its server:
    /// Expo's 90 seconds. `None` for an app whose script runs no rule's
    /// server, or a rule that keeps the default.
    pub ready_timeout_s: Option<u64>,
}

/// Fewer than this is not a workspace worth splitting up: one app with a
/// dev script is the single-process case pando already handles.
const MIN_WORKSPACE_APPS: usize = 2;

/// Where a workspace says its packages live.
///
/// pnpm keeps them in `pnpm-workspace.yaml`, npm, yarn and bun in the root
/// manifest. turbo and nx describe pipelines rather than membership, so
/// when one of those is the only marker the two conventional directories
/// are tried. Hand-parsed on purpose: one list of globs is not worth a YAML
/// dependency, and anything this cannot read simply is not a signal.
pub(super) fn workspace_globs(root: &Path) -> Vec<String> {
    let mut globs = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        globs.extend(yaml_string_list(&text, "packages"));
    }
    let manifest = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&manifest) {
        let workspaces = value.get("workspaces");
        let list = match workspaces {
            // Both shapes yarn and npm accept.
            Some(serde_json::Value::Array(list)) => Some(list.clone()),
            Some(serde_json::Value::Object(map)) => match map.get("packages") {
                Some(serde_json::Value::Array(list)) => Some(list.clone()),
                _ => None,
            },
            _ => None,
        };
        if let Some(list) = list {
            globs.extend(list.iter().filter_map(|v| v.as_str()).map(str::to_string));
        }
    }
    if globs.is_empty()
        && ["turbo.json", "nx.json"]
            .iter()
            .any(|f| root.join(f).exists())
    {
        globs.push("apps/*".to_string());
        globs.push("packages/*".to_string());
    }
    globs.sort();
    globs.dedup();
    globs
}

/// The string items of a top-level YAML list, for one key. Enough for the
/// ways a `packages:` list is written: `- 'apps/*'` items indented under
/// the key or at its own column, or a `['apps/*']` flow list on its line,
/// with comments after any of them. Nothing more.
fn yaml_string_list(text: &str, key: &str) -> Vec<String> {
    let item = |text: &str| text.trim().trim_matches(['"', '\'']).to_string();
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let line = without_comment(line);
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // A column-zero line is the next key, unless it is an item of this
        // one: YAML lets a list sit at its key's own column.
        let indented = line.starts_with([' ', '\t']);
        let own_item = inside && trimmed.starts_with('-');
        if !(indented || own_item) {
            let value = trimmed
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(':'))
                .map(str::trim);
            inside = value == Some("");
            if let Some(list) = value
                .and_then(|value| value.strip_prefix('['))
                .and_then(|value| value.strip_suffix(']'))
            {
                out.extend(list.split(',').map(item).filter(|glob| !glob.is_empty()));
            }
            continue;
        }
        if !inside {
            continue;
        }
        let Some(glob) = trimmed.strip_prefix('-').map(item) else {
            continue;
        };
        if !glob.is_empty() {
            out.push(glob);
        }
    }
    out
}

/// A YAML line without its comment. A `#` starts one only at the start of
/// a token, and never inside quotes: `- 'apps/#1'` is a glob.
fn without_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut previous = ' ';
    for (at, c) in line.char_indices() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && previous.is_whitespace() => return &line[..at],
            None => {}
        }
        previous = c;
    }
    line
}

/// The directories a workspace glob names.
///
/// Only a trailing `*` is expanded, which is the shape every workspace
/// glob in the wild has (`apps/*`, `packages/*`); anything else is treated
/// as a literal directory. A glob pando cannot read finds nothing, which
/// means one fewer proposal rather than a wrong one.
fn expand_glob(root: &Path, glob: &str) -> Vec<String> {
    let glob = glob.trim_end_matches('/');
    let Some(prefix) = glob.strip_suffix("/*") else {
        if glob.contains('*') || !root.join(glob).is_dir() {
            return Vec::new();
        }
        return vec![glob.to_string()];
    };
    if prefix.contains('*') {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root.join(prefix)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| Some(format!("{prefix}/{}", e.file_name().to_str()?)))
        .collect();
    out.sort();
    out
}

/// Whether a workspace glob names `dir`, a path relative to the root:
/// `*` within one directory name, `**` across any number of them.
pub(super) fn glob_matches(pattern: &str, dir: &str) -> bool {
    fn segments(path: &str) -> Vec<&str> {
        path.trim_start_matches("./")
            .split('/')
            .filter(|s| !s.is_empty())
            .collect()
    }
    fn name(pattern: &[u8], text: &[u8]) -> bool {
        match (pattern.first(), text.first()) {
            (None, None) => true,
            (Some(b'*'), _) => {
                name(&pattern[1..], text) || (!text.is_empty() && name(pattern, &text[1..]))
            }
            (Some(p), Some(t)) if p == t => name(&pattern[1..], &text[1..]),
            _ => false,
        }
    }
    fn path(pattern: &[&str], dir: &[&str]) -> bool {
        match (pattern.first(), dir.first()) {
            (None, None) => true,
            (Some(&"**"), _) => {
                path(&pattern[1..], dir) || (!dir.is_empty() && path(pattern, &dir[1..]))
            }
            (Some(p), Some(d)) if name(p.as_bytes(), d.as_bytes()) => {
                path(&pattern[1..], &dir[1..])
            }
            _ => false,
        }
    }
    path(&segments(pattern), &segments(dir))
}

/// Signals of one app directory: enough for its framework rule, and no
/// more. A full read would shell out to git once per app for files nothing
/// here looks at.
fn app_signals(dir: &Path) -> Signals {
    let manifest = std::fs::read_to_string(dir.join("package.json")).unwrap_or_default();
    Signals {
        scripts: parse_scripts(&manifest),
        lockfiles: present(dir, &package_managers::lockfiles()),
        markers: present(dir, &frameworks::marker_files()),
        ..Default::default()
    }
}

/// Where the apps are: a workspace's own globs, or, below a root that is
/// not an app, each app directory `signals` found there, as a literal
/// path. The two never meet: a root with a workspace marker or a
/// `package.json` has no app directories.
fn member_globs(root: &Path, signals: &Signals) -> Vec<String> {
    match signals.app_dirs.is_empty() {
        true => workspace_globs(root),
        false => signals.app_dirs.iter().map(|app| app.dir.clone()).collect(),
    }
}

/// The runner of an app directory below a root that is not an app, and
/// what goes before the arguments handed on to it: its own lockfile's,
/// the way it would run at a root of its own. `None` for a workspace
/// member, whose scripts run under the root's package manager, and for an
/// app directory with no lockfile to say.
fn own_runner(signals: &Signals, app: &Signals) -> Option<(&'static str, &'static str)> {
    let lockfiles = || app.lockfiles.iter().map(String::as_str);
    if signals.app_dirs.is_empty() {
        return None;
    }
    Some((
        package_managers::run_prefix(lockfiles(), Ecosystem::JavaScript)?,
        package_managers::script_args(lockfiles(), Ecosystem::JavaScript)?,
    ))
}

/// Every app of the workspace that has a dev script of its own — or, below
/// a root that is not an app, every app directory that has one.
///
/// The name is the directory's, which is also the role its port is
/// reserved under — so two apps with the same directory name would claim
/// one role, and `config::validate` would refuse the file pando had just
/// written. That is a workspace pando says nothing about.
pub fn workspace_apps(root: &Path, signals: &Signals) -> Vec<WorkspaceApp> {
    let runner = script_runner(signals);
    let args = script_args(signals);
    let mut apps: Vec<WorkspaceApp> = Vec::new();
    let globs = member_globs(root, signals);
    // `!apps/legacy`, `!**/test/**`: a member the workspace itself leaves
    // out is no app to propose.
    let (excluded, included): (Vec<&String>, Vec<&String>) =
        globs.iter().partition(|glob| glob.starts_with('!'));
    let excluded: Vec<&str> = excluded.iter().map(|glob| &glob[1..]).collect();
    for glob in included {
        for dir in expand_glob(root, glob) {
            if excluded.iter().any(|pattern| glob_matches(pattern, &dir)) {
                continue;
            }
            let path = root.join(&dir);
            let app = app_signals(&path);
            let Some((script_name, script)) = dev_script(&path, &app) else {
                continue;
            };
            // Not the production check: a script named `dev` is the one the
            // app develops with, even when it compiles and runs `dist/`.
            if is_multiplexer(script) {
                continue;
            }
            let Some(name) = Path::new(&dir).file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let (runner, args) = own_runner(signals, &app).unwrap_or((runner, args));
            let rule = script_framework(&path, &app, script);
            let fixed = own_port(script, rule);
            // The rule whose server the script runs, for what that rule
            // proposes beside the command.
            let serves = rule.filter(|rule| runs(rule, script) && !only_builds(rule, script));
            let mut cmd = format!("{runner}{script_name}");
            // A framework that takes its port on the command line gets the
            // flag appended to its own script: `pnpm dev --port 1234`, or
            // `npm run dev -- --port 1234`, runs what the app already runs,
            // on the port pando chose. Not to a script that already says
            // which port: that would be the flag twice. Nor to one whose
            // last command, the one the flag is handed to, is not the
            // framework's server: `vite & vite build --watch` hands it to
            // the build, which refuses it.
            let flag = rule
                .filter(|rule| {
                    fixed.is_none()
                        && rule.port == PortMechanism::InCommand
                        && flag_reaches_server(rule, script)
                })
                .and_then(|rule| rule.port_flag);
            if let Some(flag) = flag {
                cmd = format!(
                    "{cmd} {args}{}",
                    flag.replace("{port}", &format!("{{port:{name}}}"))
                );
            }
            apps.push(WorkspaceApp {
                default_port: fixed.or_else(|| app_default_port(signals, &path, name, rule)),
                port: match rule {
                    _ if fixed.is_some() => PortMechanism::Ask,
                    Some(rule) if rule.port == PortMechanism::InCommand && flag.is_none() => {
                        PortMechanism::Ask
                    }
                    Some(rule) => rule.port,
                    None => PortMechanism::Ask,
                },
                fixed_port: fixed,
                ready_timeout_s: serves.and_then(|rule| rule.ready_timeout_s),
                name: name.to_string(),
                dir,
                cmd,
            });
        }
    }
    apps.sort_by(|a, b| a.dir.cmp(&b.dir));
    apps.dedup_by(|a, b| a.dir == b.dir);
    let mut names: Vec<&str> = apps.iter().map(|a| a.name.as_str()).collect();
    names.sort_unstable();
    let unique = names.len();
    names.dedup();
    if names.len() != unique {
        // Two apps with the same directory name would claim the same role.
        return Vec::new();
    }
    // Two names that differ only in punctuation — `web-app` and `web.app` —
    // would read one `WEB_APP_PORT` between them.
    let mut vars: Vec<String> = apps.iter().map(|a| app_port_var(&a.name)).collect();
    vars.sort_unstable();
    vars.dedup();
    if vars.len() != unique {
        return Vec::new();
    }
    // And a name that cannot be a process and a role at all: one a
    // `{port:…}` placeholder cannot spell renders as literal braces, and a
    // name pando keeps for its own logs is refused by the loader — either
    // way the answer would fail once taken, rather than never be offered.
    if apps.iter().any(|a| !usable_as_role(&a.name)) {
        return Vec::new();
    }
    apps
}

/// The root's local `.env`, for each workspace app that has no env file of
/// its own: `(destination, source)`. See `Signals::workspace_env_links`.
pub(super) fn workspace_env_links(root: &Path, signals: &Signals) -> Vec<(String, String)> {
    const ROOT_ENV: &str = ".env";
    if !signals.ignored_present.iter().any(|path| path == ROOT_ENV) {
        return Vec::new();
    }
    let apps = workspace_apps(root, signals);
    if apps.len() < MIN_WORKSPACE_APPS {
        return Vec::new();
    }
    apps.iter()
        .filter(|app| !has_env_file(&root.join(&app.dir)))
        .map(|app| (format!("{}/{ROOT_ENV}", app.dir), ROOT_ENV.to_string()))
        .filter(|(destination, _)| super::signals::is_gitignored(root, destination))
        .collect()
}

/// Whether a directory has any env file of its own, local or example: an
/// app that has one has said which environment it reads, and it is not
/// the root's.
fn has_env_file(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().starts_with(".env"))
        })
        .unwrap_or(false)
}

/// The env variable an app's port is declared under in the env example:
/// `apps/admin-v2` reads `ADMIN_V2_PORT`. An env name is letters, digits
/// and underscores, so every other character becomes an underscore — a dot
/// as much as a dash.
fn app_port_var(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| match c.is_ascii_alphanumeric() {
            true => c.to_ascii_uppercase(),
            false => '_',
        })
        .collect();
    format!("{stem}_PORT")
}

/// Whether an app's directory name can be its process name and its role:
/// what `{port:<role>}` can spell, and not one of pando's own log names.
fn usable_as_role(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        && crate::paths::validate_owned_log_source("process name", name).is_ok()
}

/// The port an app listens on by default: what the root env example says
/// for it, else what its own env files say for a variable it takes its
/// port from, else what its framework does.
///
/// Its own env files before its framework's default: a `node server.js`
/// whose `.env` says `PORT=8787` listens on 8787, and a sibling told
/// `http://127.0.0.1:8787` is being told where that app is.
fn app_default_port(
    signals: &Signals,
    dir: &Path,
    name: &str,
    rule: Option<&'static FrameworkRule>,
) -> Option<u16> {
    let wanted = app_port_var(name);
    let from_example = signals
        .env_example
        .iter()
        .find(|(key, _)| *key == wanted)
        .and_then(|(_, value)| value.parse::<u16>().ok());
    let framework_var = rule.and_then(|rule| match rule.port {
        PortMechanism::Env(var) => Some(var),
        _ => None,
    });
    let from_own_files = || {
        std::iter::once(wanted.as_str())
            .chain(framework_var)
            .find_map(|var| env_file_value(dir, var)?.trim().parse::<u16>().ok())
    };
    from_example
        .or_else(from_own_files)
        .or_else(|| rule.map(|r| r.default_port))
}

/// The env variables an app reads its port from: a `<APP>_PORT` key the
/// env example declares for it, and its framework's own variable.
///
/// The env example first, as the single-process rule has it — a project
/// that wrote `WEB_PORT=3000` beside its web app has said how that app
/// takes its port, and handing it `PORT` alone ignores that. Both are
/// given when both exist: they carry the same port, so there is nothing
/// for them to disagree about, and an app that reads either one works.
fn app_port_env(signals: &Signals, app: &WorkspaceApp) -> Vec<String> {
    let wanted = app_port_var(&app.name);
    let declared = signals.env_keys().any(|key| key == wanted);
    let mut out: Vec<String> = Vec::new();
    // A port the app's own script fixes beats any variable: a role for it
    // would be a port reserved and waited on that nothing ever binds.
    if app.fixed_port.is_some() {
        return out;
    }
    match app.port {
        // The command already carries the port. A second way of saying it
        // is a second thing that can disagree.
        PortMechanism::InCommand => return out,
        PortMechanism::Env(name) => {
            if declared && wanted != name {
                out.push(wanted);
            }
            out.push(name.to_string());
        }
        PortMechanism::Ask => {
            if declared {
                out.push(wanted);
            }
        }
    }
    out
}

/// Cross-references between apps, read out of the root env example.
///
/// A value like `http://localhost:4000` is one app being told where
/// another one listens. When that port is one app's default and no other
/// app's, the value becomes a template pointing at that app's role — so
/// every worktree gets its own pair of ports and the two halves still
/// find each other. It is given to every process *except* the one it
/// points at, which is the only one that does not need to be told.
///
/// Only an app that owns a role can be pointed at: `{port:<role>}` for a
/// role nobody owns does not render, and an environment that cannot be
/// rendered is a start that refuses.
///
/// `env_example` is the root's, or one app's own: an Expo app's
/// `EXPO_PUBLIC_API_URL=http://localhost:4000` in `apps/mobile` is read
/// the same way, and [`processes_proposal`] gives it to that app alone.
fn cross_references(
    env_example: &[(String, String)],
    apps: &[WorkspaceApp],
    owns_role: &[bool],
) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (key, value) in env_example {
        let Some(port) = localhost_url_port(value) else {
            continue;
        };
        // Only a port that is one app's: two apps whose frameworks both
        // default to 3000 leave no telling which one the URL names, and
        // the wrong guess points a web app's own URL at the api.
        let mut listening = apps
            .iter()
            .zip(owns_role)
            .filter(|(app, owns)| **owns && app.default_port == Some(port))
            .map(|(app, _)| app);
        let (Some(target), None) = (listening.next(), listening.next()) else {
            continue;
        };
        let template = value.replacen(
            &format!(":{port}"),
            &format!(":{{port:{}}}", target.name),
            1,
        );
        out.push((target.name.clone(), key.clone(), template));
    }
    out
}

/// The bare-port twin of [`cross_references`]: an `<APP>_PORT` the env
/// example declares for an app that owns a role, as `(app, variable)`.
///
/// A project that writes `API_PORT=4000` in its env example has said, once
/// for every process, where the api listens. A gateway that proxies to the
/// api reads that same variable — and told nothing, falls back to the
/// default, which is the main checkout's api whenever one is running. So
/// the variable goes to every process, carrying the owner's role, and the
/// two halves of each worktree find each other rather than someone else's.
fn sibling_port_vars(
    signals: &Signals,
    apps: &[WorkspaceApp],
    owns_role: &[bool],
) -> Vec<(String, String)> {
    apps.iter()
        .zip(owns_role)
        .filter(|(_, owns)| **owns)
        .map(|(app, _)| (app.name.clone(), app_port_var(&app.name)))
        .filter(|(_, var)| signals.env_keys().any(|key| key == var))
        .collect()
}

/// The port of a URL that points at this machine, if that is what this is.
pub(super) fn localhost_url_port(value: &str) -> Option<u16> {
    let (_, after) = value.split_once("://")?;
    let host_port = after.split(['/', '?', '#']).next()?;
    // Credentials, as in `postgres://user:pass@localhost:5432/db`.
    let host_port = host_port.rsplit('@').next()?;
    let (host, port) = host_port.rsplit_once(':')?;
    if !matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]") {
        return None;
    }
    port.parse().ok()
}

/// Whether the root `dev` script is the project's own orchestration of its
/// apps rather than a generic fan-out over them: it has a `predev` step,
/// which running each app's own script would skip, or it runs a script
/// file of the project's (`node scripts/dev.mjs`), which is where a
/// project that wrote one says how its apps are started and told about
/// each other.
pub(super) fn root_orchestrates(signals: &Signals) -> bool {
    let Some(dev) = signals.scripts.get("dev") else {
        return false;
    };
    const SCRIPT_FILES: [&str; 6] = [".js", ".mjs", ".cjs", ".ts", ".mts", ".cts"];
    let runs_a_file = dev
        .split(|c: char| c.is_whitespace() || matches!(c, '&' | ';' | '|'))
        .any(|word| SCRIPT_FILES.iter().any(|ext| word.ends_with(ext)));
    signals.scripts.contains_key("predev") || runs_a_file
}

/// How the reason an app directory is left out of the per-app form ends:
/// the words `agent/json.md` quotes.
pub const UNSTARTED: &str = "nothing here starts it";

/// The app directories below a root that is not an app that no process of
/// the per-app form runs in, each as the reason it has none: `backend has
/// uv.lock but no dev script: nothing here starts it`.
///
/// The rules never invent a command, so a directory whose manifest names
/// no dev script — a Python api and its worker, a Go service — has no
/// process here, and a check of the others passes with it never run. Only
/// the project's docs or the developer know how it runs. The libraries
/// under `packages/` are left out: nothing runs one on its own.
fn unstarted(root: &Path, signals: &Signals, apps: &[WorkspaceApp]) -> Vec<String> {
    let library = format!("{LIBRARY_PARENT}/");
    signals
        .app_dirs
        .iter()
        .filter(|dir| !dir.dir.starts_with(&library))
        .filter(|dir| !apps.iter().any(|app| app.dir == dir.dir))
        .map(|dir| {
            let path = root.join(&dir.dir);
            match dev_script(&path, &app_signals(&path)) {
                // A fan-out over its own parts, which the per-app form
                // leaves out.
                Some(_) => format!(
                    "{}'s dev script starts several things at once: {UNSTARTED}",
                    dir.dir
                ),
                None => format!(
                    "{} has {} but no dev script: {UNSTARTED}",
                    dir.dir,
                    evidence(dir)
                ),
            }
        })
        .collect()
}

/// What makes a directory an app, named as its file: its lockfile, else
/// its marker, else its manifest.
fn evidence(dir: &AppDir) -> &str {
    dir.lockfiles
        .first()
        .or(dir.markers.first())
        .map_or("a package.json", String::as_str)
}

/// The multi-process form, for a workspace whose apps each have a dev
/// script — and the root script as the one-process form beside it.
///
/// Always a question: running two servers instead of one changes what
/// `start`, `stop` and the log tabs do, and that is the developer's call.
/// The first option is what is taken without asking: the per-app form,
/// unless the root script is the project's own orchestration of its apps
/// (see [`root_orchestrates`]), which then leads — with the ports the
/// project's apps read from the env example, so it needs nothing else.
///
/// Below a root that is not an app, a per-app form that leaves an app
/// directory unstarted is offered and never preselected: its evidence
/// names the directory ([`unstarted`]), `init --yes` has nothing to take
/// and exits 3, and only an answer that covers every process settles it.
/// `new` and `start` still take it, and say so, as they take any first
/// option.
pub(super) fn processes_proposal(root: &Path, signals: &Signals) -> Option<Proposal> {
    let apps = workspace_apps(root, signals);
    // Below a root that is not an app, one is enough: there is no root
    // command for the single-process form to run, so the per-app form,
    // with the directory it runs in, is the only way to say how it starts.
    let below_root = !signals.app_dirs.is_empty();
    let least = match below_root {
        true => 1,
        false => MIN_WORKSPACE_APPS,
    };
    if apps.len() < least {
        return None;
    }
    let unstarted = unstarted(root, signals, &apps);
    // A role is a port, and a port is only worth giving an app that has
    // some way of being told which one it got: its framework reads one
    // from the environment, or takes it on the command line (the flag is
    // already in `cmd` by now), or the env example has an `<APP>_PORT` key
    // for it. An app with none of those — a watcher, a codegen step, a
    // queue consumer, the `dev: "tsc -w"` of a `packages/*` library — gets
    // `ports = []` and no readiness rule. Given a role anyway it would be
    // handed a reserved port it never hears about, and `advance_phases`
    // would wait thirty seconds for it to bind before calling a perfectly
    // healthy process failed — and the whole worktree with it.
    let port_envs: Vec<Vec<String>> = apps.iter().map(|app| app_port_env(signals, app)).collect();
    let owns_role: Vec<bool> = apps
        .iter()
        .zip(&port_envs)
        .map(|(app, port_env)| app.port == PortMechanism::InCommand || !port_env.is_empty())
        .collect();
    let references = cross_references(&signals.env_example, &apps, &owns_role);
    let siblings = sibling_port_vars(signals, &apps, &owns_role);
    let mut processes: BTreeMap<String, ProcessConfig> = BTreeMap::new();
    for ((app, port_env), owns) in apps.iter().zip(&port_envs).zip(&owns_role) {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        for var in port_env {
            env.insert(var.clone(), format!("{{port:{}}}", app.name));
        }
        for (target, key, template) in &references {
            // The app a reference points at is the one that does not need
            // to be told where it is.
            if *target == app.name {
                continue;
            }
            env.insert(key.clone(), template.clone());
        }
        // The app's own env example says where it expects the others, and
        // says it to this app only: Expo inlines `EXPO_PUBLIC_*` from
        // Metro's own environment into the bundle, so the api's port has to
        // reach Metro as a variable. Over the root's, being nearer the app.
        let own = super::signals::env_example(&root.join(&app.dir));
        for (target, key, template) in cross_references(&own, &apps, &owns_role) {
            if target != app.name {
                env.insert(key, template);
            }
        }
        for (target, var) in &siblings {
            if *target == app.name {
                continue;
            }
            // Never over a variable the app already reads for itself: its
            // own port wins over a sibling's of the same spelling.
            env.entry(var.clone())
                .or_insert_with(|| format!("{{port:{target}}}"));
        }
        let roles = if *owns {
            vec![app.name.clone()]
        } else {
            Vec::new()
        };
        processes.insert(
            app.name.clone(),
            ProcessConfig {
                cmd: app.cmd.clone(),
                cwd: Some(app.dir.clone()),
                ports: Some(PortsSpec::List(roles)),
                env,
                ready: owns.then(|| ReadySpec {
                    role: Some(app.name.clone()),
                    timeout_s: app.ready_timeout_s,
                }),
                // Left to the catalog, as a written config leaves it.
                page: None,
            },
        );
    }
    let summary = apps
        .iter()
        .map(|app| format!("{}: {} in {}", app.name, app.cmd, app.dir))
        .collect::<Vec<_>>()
        .join("; ");
    // The env example's own port variables, named in the evidence, so the
    // question says where they came from.
    let declared: Vec<String> = apps
        .iter()
        .filter(|app| app.fixed_port.is_none())
        .map(|app| app_port_var(&app.name))
        .filter(|key| signals.env_keys().any(|k| k == key))
        .collect();
    let mut why = match below_root {
        true => format!(
            "a dev script in {}",
            super::dev::listed(&apps.iter().map(|app| app.dir.as_str()).collect::<Vec<_>>())
        ),
        false => format!("a dev script in each of {} workspace apps", apps.len()),
    };
    if !declared.is_empty() {
        why.push_str(&format!("; {} in the env example", declared.join(" and ")));
    }
    // Said, because such an app gets no port of its own in a worktree and
    // two worktrees will both want the one its script names.
    for app in &apps {
        if let Some(port) = app.fixed_port {
            why.push_str(&format!(
                "; {} fixes its own port {port} in its dev script",
                app.name
            ));
        }
    }
    for reason in &unstarted {
        why.push_str(&format!("; {reason}"));
    }
    let mut candidates = vec![Candidate {
        value: summary,
        why,
        processes: Some(processes),
        needs_a_human: !unstarted.is_empty(),
        ..Candidate::default()
    }];
    // The root script, as one process. Written as `[dev]`, because one
    // process is what that shorthand is for.
    if signals.scripts.contains_key("dev") {
        let value = format!("{}dev", script_runner(signals));
        let orchestrates = root_orchestrates(signals);
        // The ports the project's own apps read, `<APP>_PORT` in the env
        // example, each under its app's role: what the root script's own
        // orchestration hands on to them.
        let ports: BTreeMap<String, String> = if orchestrates {
            apps.iter()
                .map(|app| (app_port_var(&app.name), app.name.clone()))
                .filter(|(key, _)| signals.env_keys().any(|k| k == key))
                .collect()
        } else {
            BTreeMap::new()
        };
        let mut why = "package.json scripts.dev".to_string();
        if orchestrates {
            why.push_str(", which starts the workspace's apps itself");
        }
        if !ports.is_empty() {
            let keys: Vec<&str> = ports.keys().map(String::as_str).collect();
            why.push_str(&format!("; {} in the env example", keys.join(" and ")));
        }
        let root = Candidate {
            value: value.clone(),
            why,
            processes: Some(BTreeMap::from([(
                DEV.to_string(),
                ProcessConfig {
                    cmd: value,
                    ports: match ports.is_empty() {
                        true => None,
                        false => Some(PortsSpec::Map(ports)),
                    },
                    ..Default::default()
                },
            )])),
            ..Candidate::default()
        };
        // The project's own way of starting itself leads: what its
        // `predev` builds and what its script hands each app are what a
        // per-app split would leave out.
        if orchestrates {
            candidates.insert(0, root);
        } else {
            candidates.push(root);
        }
    }
    // Never decided: two processes instead of one is a change of shape,
    // and "ask just in time, once" is exactly what this is for.
    Some(Proposal::of(Slot::Processes, candidates, false))
}
