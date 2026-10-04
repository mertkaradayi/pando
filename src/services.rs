//! Private copies of the project's services, one set per worktree.
//!
//! The compose adapter, and for now the only one: a project that already
//! has a compose file gets isolation with no new configuration beyond
//! which services to include. Native recipes are Phase 6 and land beside
//! this, behind the same shape.
//!
//! Everything here goes through one compose *project name* per worktree,
//! `pando-<project id>-<worktree>`, which is what isolates containers, the
//! network, and — because compose prefixes named volumes with it — the
//! data. Nothing is ever written into the repository: the override that
//! remaps the ports lives under pando's home and is passed with `-f`.
//!
//! Readiness never binds a port. A bind probe would take the port from the
//! server being waited for; a service with a compose `healthcheck` is
//! asked through `docker compose ps`, and one without is asked with a
//! connect.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::paths::PandoPaths;
use crate::ports;

/// How long one service gets to become ready before the start is failed.
pub const DEFAULT_READY_TIMEOUT_S: u64 = 60;

/// How often readiness is re-checked. Short enough that a redis that comes
/// up in half a second is not waited on for two.
const POLL: Duration = Duration::from_millis(250);

/// How many poll rounds between `docker compose ps` calls when nothing is
/// healthchecked. Every round would be a process spawn four times a
/// second for a whole minute; this is often enough to notice a container
/// that died without making the wait itself expensive.
const CHECK_EVERY: u32 = 8;

/// What docker says when the binary is present and the daemon is not. Two
/// spellings, because the classic Unix-socket message and the newer one
/// differ, and both mean the same thing to the developer.
const DAEMON_DOWN: [&str; 3] = [
    "Cannot connect to the Docker daemon",
    "docker daemon is not running",
    // Newer clients, and OrbStack's socket: "failed to connect to the
    // docker API at unix://…; check if the path is correct and if the
    // daemon is running".
    "failed to connect to the docker API",
];

/// Docker answered, and the answer was that its daemon is not running.
///
/// Its own type because the right response depends on who is asking. A
/// start that needs containers has to stop and say so; a `stop` or an
/// `rm` of containers a daemon that is down cannot be running has nothing
/// left to do, and failing there turns "Docker is off" into "pando cannot
/// stop my worktree".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonDown;

impl std::fmt::Display for DaemonDown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "isolated mode needs Docker, and the Docker daemon is not running — start Docker, \
             or {}",
            crate::remedy::SHARED
        )
    }
}

impl std::error::Error for DaemonDown {}

/// How long a compose question that only reads may take: `ps`, and
/// `config`. Docker Desktop can wedge with its socket still accepting, and
/// then `docker compose ps` waits for ever — while `rm` holds the state
/// lock around it, which freezes `pando ls` and the TUI's tick with it.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

thread_local! {
    static PROBE_TIMEOUT_HERE: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

/// [`PROBE_TIMEOUT`], unless [`with_probe_timeout`] is running on this
/// thread.
fn probe_timeout() -> Duration {
    PROBE_TIMEOUT_HERE
        .with(|t| t.get())
        .unwrap_or(PROBE_TIMEOUT)
}

/// Runs `f` with the probes it makes on this thread bounded by `timeout`
/// instead of [`PROBE_TIMEOUT`]. For tests: a hung daemon is waited out
/// for one whole probe, and twenty seconds of that was the slowest test
/// in the suite. Per thread, so the tests running beside it keep the
/// real bound.
#[doc(hidden)]
pub fn with_probe_timeout<R>(timeout: Duration, f: impl FnOnce() -> R) -> R {
    let before = PROBE_TIMEOUT_HERE.with(|t| t.replace(Some(timeout)));
    // Put back on unwind too, so a failing test leaves nothing behind on
    // a thread the harness may reuse.
    struct Restore(Option<Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROBE_TIMEOUT_HERE.with(|t| t.set(self.0));
        }
    }
    let _restore = Restore(before);
    f()
}

/// How long `stop` and `down -v` may take. Generous, because compose gives
/// each container its own grace period and a project can have several;
/// bounded, because `rm` runs them under the state lock too.
pub const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(300);

/// Docker was asked and did not answer in time: a daemon that is up but
/// wedged, which is not the same as one that is not running. Its
/// containers may well be running, so nothing that acts on "Docker is
/// off" may act on this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonHung {
    pub verb: String,
    pub timeout: Duration,
}

impl std::fmt::Display for DaemonHung {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Docker did not answer `docker compose {}` in {}s — it looks hung; restarting \
             Docker usually clears it",
            self.verb,
            self.timeout.as_secs()
        )
    }
}

impl std::error::Error for DaemonHung {}

/// Whether an error is, underneath its context, a daemon that did not
/// answer in time.
pub fn is_daemon_hung(e: &anyhow::Error) -> bool {
    e.downcast_ref::<DaemonHung>().is_some()
}

/// Whether an error is, underneath whatever context it gathered, a
/// daemon that is not running.
pub fn is_daemon_down(e: &anyhow::Error) -> bool {
    e.downcast_ref::<DaemonDown>().is_some()
}

/// Whether an error is, underneath its context, a docker that could not
/// be run at all: nothing called `docker` on the PATH, or a shim whose
/// interpreter is gone. Unlike a daemon that is down, there is no Docker
/// here for a container to come back with.
pub fn is_docker_missing(e: &anyhow::Error) -> bool {
    e.downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

/// The docker executable pando runs.
///
/// `<home>/bin/docker` when it is there and executable, else whatever
/// `docker` resolves to on PATH. Two reasons for the hook: a developer
/// whose docker is not on the PATH pando inherits has somewhere to put a
/// shim, and the tests drive a fake docker per test home without mutating
/// the process environment — `std::env::set_var` is unsafe and racy with
/// tests running in parallel, and a child `PATH` is not reliably what
/// program lookup uses.
pub fn docker_program(paths: &PandoPaths) -> PathBuf {
    let shim = paths.home.join("bin").join("docker");
    if is_executable(&shim) {
        return shim;
    }
    PathBuf::from("docker")
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// One worktree's compose project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compose {
    program: PathBuf,
    project: String,
    /// The project's own file first, pando's override second. Empty for
    /// the by-project form.
    files: Vec<PathBuf>,
    /// The directory compose resolves relative paths against, which is the
    /// worktree. `None` for the by-project form.
    dir: Option<PathBuf>,
}

impl Compose {
    pub fn new(
        program: impl Into<PathBuf>,
        project: impl Into<String>,
        files: Vec<PathBuf>,
        dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            program: program.into(),
            project: project.into(),
            files,
            dir: Some(dir.into()),
        }
    }

    /// The form `stop` and `rm` use: they never load config, so they have
    /// no compose file to name — and compose does not need one, because it
    /// labels every container it created with its project.
    pub fn by_project(program: impl Into<PathBuf>, project: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            project: project.into(),
            files: Vec::new(),
            dir: None,
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    /// `compose -p <project> [-f <file>…] <rest…>`, which every invocation
    /// starts with.
    pub fn args(&self, rest: &[&str]) -> Vec<String> {
        let mut args = vec![
            "compose".to_string(),
            "-p".to_string(),
            self.project.clone(),
        ];
        for file in &self.files {
            args.push("-f".to_string());
            args.push(file.display().to_string());
        }
        args.extend(rest.iter().map(|a| (*a).to_string()));
        args
    }

    /// Runs one compose verb, bounded by `timeout` when there is one. `up`
    /// has none: pulling an image takes as long as the network does, and
    /// a start is never run under the state lock.
    fn run(&self, rest: &[&str], timeout: Option<Duration>) -> Result<String> {
        self.run_with(&[], rest, timeout)
    }

    /// [`Self::run`] with global flags ahead of the verb, which is the only
    /// place compose reads them.
    fn run_with(
        &self,
        globals: &[&str],
        rest: &[&str],
        timeout: Option<Duration>,
    ) -> Result<String> {
        let args = self.args(&[globals, rest].concat());
        let mut command = Command::new(&self.program);
        command.args(&args).stdin(Stdio::null());
        if let Some(dir) = &self.dir {
            command.current_dir(dir);
        }
        let out = match timeout {
            Some(timeout) => crate::project::output_within(command, timeout),
            None => command.output(),
        };
        if let (Err(e), Some(timeout)) = (&out, timeout)
            && e.kind() == std::io::ErrorKind::TimedOut
        {
            return Err(anyhow::Error::new(DaemonHung {
                verb: rest.first().copied().unwrap_or_default().to_string(),
                timeout,
            }));
        }
        let out = out.with_context(|| {
            format!(
                "run {} {} — isolated mode needs Docker; install it, or {}",
                self.program.display(),
                args.join(" "),
                crate::remedy::SHARED
            )
        })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            // The binary is there and the daemon is not. That is one thing
            // to do about it, and a compose command line with two `-f`
            // paths in it is not how to say so.
            if DAEMON_DOWN.iter().any(|needle| stderr.contains(needle)) {
                return Err(anyhow::Error::new(DaemonDown));
            }
            let reason = stderr
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("no output")
                .trim();
            bail!(
                "`{} {}` exited {}: {reason}",
                self.program.display(),
                args.join(" "),
                out.status.code().unwrap_or(-1)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// The fully resolved file, as compose itself reads it.
    ///
    /// `extends:`, a top-level `include:` and YAML aliases are followed and
    /// every default is filled in — none of which pando's own reader does. Nothing is
    /// created or started: `config` only prints.
    ///
    /// Every profile is on. Compose leaves a service whose profile is not
    /// active out of `config`, while `up -d <service>` turns on the profile
    /// of a service it is given by name, so without them a profiled
    /// service pando would bring up is one this file says is not there.
    pub fn config(&self) -> Result<crate::compose::ComposeFile> {
        let text = self.run_with(
            &["--profile", "*"],
            &["config", "--format", "json"],
            Some(probe_timeout()),
        )?;
        crate::compose::parse_config_json(&text)
    }

    /// Brings the included services up in the background. Idempotent:
    /// compose leaves a container that is already running and matches its
    /// configuration exactly as it is.
    pub fn up(&self, services: &[String]) -> Result<()> {
        let mut rest = vec!["up", "-d"];
        rest.extend(services.iter().map(String::as_str));
        self.run(&rest, None)?;
        Ok(())
    }

    /// Whether the daemon answers at all, asked the cheapest way compose
    /// has: `ps` of this project, which creates and starts nothing.
    ///
    /// What an isolated start asks *before* it stops anything, so "Docker
    /// is not running" is a refusal rather than the end of a start that
    /// already took the running environment down.
    pub fn reachable(&self) -> Result<()> {
        self.run(&["ps", "--all", "--format", "json"], Some(probe_timeout()))?;
        Ok(())
    }

    /// What compose says about every container of this project.
    pub fn ps(&self) -> Result<Vec<Status>> {
        let text = self.run(&["ps", "--all", "--format", "json"], Some(probe_timeout()))?;
        parse_ps(&text)
    }

    /// Stops the containers and leaves the volumes. What `stop` does: the
    /// data survives, and the next start brings the same database back.
    pub fn stop(&self) -> Result<()> {
        self.run(&["stop"], Some(TEARDOWN_TIMEOUT))?;
        Ok(())
    }

    /// Stops only these services' containers, and leaves the rest of the
    /// project running and every volume where it is.
    pub fn stop_services(&self, services: &[String]) -> Result<()> {
        let mut rest = vec!["stop"];
        rest.extend(services.iter().map(String::as_str));
        self.run(&rest, Some(TEARDOWN_TIMEOUT))?;
        Ok(())
    }

    /// Removes the containers, the network, and the named volumes. What
    /// `rm` does: the worktree is going, and its database goes with it.
    pub fn down_with_volumes(&self) -> Result<()> {
        self.run(&["down", "-v"], Some(TEARDOWN_TIMEOUT))?;
        Ok(())
    }

    /// The shell command that pumps one service's container log into a
    /// file, run detached through [`crate::process::spawn_detached`] so it
    /// is a process group pando can sweep like any other.
    pub fn logs_shell_cmd(&self, service: &str) -> String {
        let mut parts = vec![shell_quote(&self.program.display().to_string())];
        for arg in self.args(&["logs", "-f", "--no-color", service]) {
            parts.push(shell_quote(&arg));
        }
        parts.join(" ")
    }
}

/// Single quotes, with any single quote inside closed, escaped, reopened.
/// A worktree path can hold a space, and a docker shim path can hold both.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// One container of the compose project, as `docker compose ps` reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub service: String,
    /// `running`, `exited`, `created`, …
    pub state: String,
    /// `starting`, `healthy`, `unhealthy`, or empty when the service
    /// declares no healthcheck.
    pub health: String,
    /// Published host ports, as `(host, container)`.
    pub published: Vec<(u16, u16)>,
}

impl Status {
    pub fn running(&self) -> bool {
        self.state == "running"
    }

    /// A container that will never become ready on its own.
    pub fn dead(&self) -> bool {
        matches!(self.state.as_str(), "exited" | "dead" | "removing")
    }
}

/// Compose 5 emits one JSON object per line. Older ones emit a single JSON
/// array, so both are accepted: a developer on an older compose should get
/// a working pando, not a parse error with a version number in it.
pub fn parse_ps(text: &str) -> Result<Vec<Status>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('[') {
        let values: Vec<serde_json::Value> =
            serde_json::from_str(trimmed).context("parse `docker compose ps --format json`")?;
        return Ok(values.iter().map(status_from).collect());
    }
    let mut out = Vec::new();
    for line in trimmed.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("parse a `docker compose ps` line: {line}"))?;
        out.push(status_from(&value));
    }
    Ok(out)
}

fn status_from(value: &serde_json::Value) -> Status {
    let string = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let published = value
        .get("Publishers")
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let host = entry.get("PublishedPort")?.as_u64()? as u16;
                    let container = entry.get("TargetPort")?.as_u64()? as u16;
                    (host != 0).then_some((host, container))
                })
                .collect()
        })
        .unwrap_or_default();
    Status {
        service: string("Service"),
        state: string("State"),
        health: string("Health"),
        published,
    }
}

// ---- telling the app where its services are -------------------------------

/// Where a value is looked up, in order. `.env` first because it holds the
/// real credentials for this project; the example files are the fallback
/// for a worktree that has none.
const ENV_FILES: [&str; 4] = [".env", ".env.example", ".env.sample", ".env.template"];

/// The environment that points an app at *this* worktree's services.
///
/// For each `ENV_KEY = "service"` in a `[[services]]` entry, the value the
/// project already uses is read from the worktree's own env files and its
/// port replaced with the one pando allocated. A URL keeps its
/// credentials, its database name, and its query string; a bare number
/// becomes the port. A value that holds a reference nothing sets is an
/// error of [`Unresolved`] underneath: rewritten around it, the app logged
/// in as a user called `${DB_USER}`.
///
/// This is what replaces materialising a rewritten `.env` inside the
/// worktree, which Invariant 1 forbids unless the project ignores it. The
/// same map reaches the processes, the hooks, and `pando status --env`.
pub fn app_env(
    worktree: &Path,
    mapping: &std::collections::BTreeMap<String, String>,
    ports: &std::collections::BTreeMap<String, u16>,
) -> Result<std::collections::BTreeMap<String, String>> {
    let files = read_env_files(worktree);
    let mut out = std::collections::BTreeMap::new();
    for (key, service) in mapping {
        let port = *ports.get(service).with_context(|| {
            format!("no port was allocated for the service {service:?} (env.{key})")
        })?;
        let found = value_in_files(&files, key).map_err(|unresolved| {
            anyhow::Error::new(unresolved).context(format!(
                "env.{key} points at the service {service:?}, but pando cannot tell what {key} \
                 will be"
            ))
        })?;
        let Some((source, value)) = found else {
            bail!(
                "env.{key} points at the service {service:?}, but nothing in this worktree says \
                 what {key} normally looks like — add it to .env.example (or .env), or drop it \
                 from the [[services]] env map"
            );
        };
        out.insert(key.clone(), rewrite(key, value, source, service, port)?);
    }
    Ok(out)
}

fn rewrite(key: &str, value: &str, source: &str, service: &str, port: u16) -> Result<String> {
    let trimmed = value.trim();
    if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Ok(port.to_string());
    }
    if let Some(rewritten) = rewrite_url(trimmed, service, port) {
        return Ok(rewritten);
    }
    bail!(
        "{key}={trimmed:?} in {source} is neither a URL nor a port number, so pando cannot point \
         it at {service:?} — make it a URL or a bare port, or drop {key} from the [[services]] \
         env map"
    )
}

/// A URL with its port replaced, and its host replaced too when the host
/// is the compose service's own name: inside the compose network a service
/// is reachable as `postgres`, and from the host it is `localhost`.
fn rewrite_url(value: &str, service: &str, port: u16) -> Option<String> {
    let after_scheme = value.find("://")? + 3;
    let (head, rest) = value.split_at(after_scheme);
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(at) => authority.split_at(at + 1),
        None => ("", authority),
    };
    // An IPv6 host is bracketed and full of colons; only the one after the
    // closing bracket separates the port.
    let host = if hostport.starts_with('[') {
        match hostport.find(']') {
            Some(close) => &hostport[..=close],
            None => hostport,
        }
    } else {
        match hostport.rfind(':') {
            Some(colon) => &hostport[..colon],
            None => hostport,
        }
    };
    if host.is_empty() {
        return None;
    }
    let host = if host == service { "localhost" } else { host };
    Some(format!("{head}{userinfo}{host}:{port}{tail}"))
}

/// The user and the database name a connection URL carries, if it carries
/// them.
///
/// A native service has to *create* what the app's own URL asks for: a
/// Postgres cluster that initdb made has one database called `postgres`
/// and nothing called `acme_dev`. The port is rewritten by [`app_env`];
/// these two are what a recipe's `create` command needs on top of it.
///
/// Percent-escapes are not decoded. A user name with a `%40` in it is
/// vanishingly rare in a development URL, and a wrong guess here would be
/// spliced into a SQL statement — so the caller checks the shape of what
/// comes back and refuses anything that is not a plain identifier.
pub fn url_identity(value: &str) -> (Option<String>, Option<String>) {
    let Some(after_scheme) = value.find("://").map(|at| at + 3) else {
        return (None, None);
    };
    let rest = &value[after_scheme..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let user = authority
        .rfind('@')
        .map(|at| &authority[..at])
        .map(|userinfo| match userinfo.find(':') {
            Some(colon) => &userinfo[..colon],
            None => userinfo,
        })
        .filter(|user| !user.is_empty())
        .map(str::to_string);
    let database = tail
        .strip_prefix('/')
        .map(|path| path.split(['?', '#']).next().unwrap_or_default())
        .filter(|database| !database.is_empty())
        .map(str::to_string);
    (user, database)
}

/// Who the app connects as, and to which database, from keys beside the
/// one that addresses the service: `(user, database)`.
///
/// Many projects address a database as parts — `DB_HOST`, `DB_PORT`,
/// `DB_NAME`, `DB_USER` — rather than as one URL, and only a URL carries a
/// database name. So for each addressing key `<P>_PORT`, `<P>_HOST` or
/// `<P>_URL`, the siblings `<P>_NAME`, `<P>_DATABASE` or `<P>_DB` and
/// `<P>_USER` or `<P>_USERNAME` are read, first found wins. A database
/// "name" that is all digits is a numbered database — redis's `REDIS_DB=2`
/// — and not a name anything creates.
pub fn sibling_identity<'a>(
    worktree: &Path,
    keys: impl IntoIterator<Item = &'a str>,
) -> (Option<String>, Option<String>) {
    let files = read_env_files(worktree);
    // As written: a reference nothing sets is no plain identifier, and the
    // create step refuses it by name.
    let lookup = |key: &str| {
        files
            .iter()
            .find_map(|(_, env)| env.values.get(key))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let mut user = None;
    let mut database = None;
    for key in keys {
        let Some(prefix) = ["_PORT", "_HOST", "_URL"]
            .iter()
            .find_map(|suffix| key.strip_suffix(suffix))
        else {
            continue;
        };
        if database.is_none() {
            database = ["_NAME", "_DATABASE", "_DB"]
                .iter()
                .filter_map(|suffix| lookup(&format!("{prefix}{suffix}")))
                .find(|value| !value.chars().all(|c| c.is_ascii_digit()));
        }
        if user.is_none() {
            user = ["_USER", "_USERNAME"]
                .iter()
                .find_map(|suffix| lookup(&format!("{prefix}{suffix}")));
        }
    }
    (user, database)
}

/// Something an app keeps beside a service's address, by the same rule
/// [`sibling_identity`] reads a database's name by: for each addressing
/// key `<P>_PORT`, `<P>_HOST` or `<P>_URL` among `keys`, the first of
/// `<P><suffix>` this directory's env files set to anything — as the key
/// it was found under, and its value.
///
/// `DATABASE_PASSWORD` beside `DATABASE_PORT`, `REDIS_DB` beside
/// `REDIS_PORT`. [`Unresolved`] when the first one set holds a reference
/// nothing sets, as [`value_in_env`] is.
pub fn sibling_value<'a>(
    dir: &Path,
    keys: impl IntoIterator<Item = &'a str>,
    suffixes: &[&str],
) -> Result<Option<(String, String)>, Unresolved> {
    EnvFiles::read(dir, &[]).sibling(keys, suffixes)
}

/// The env files of the main checkout, read once, in the order a key is
/// looked up in: the root's, then those of each of `dirs` below it — the
/// directories the processes run in — the order [`env_value_below`] reads
/// them in. The first file that sets a key wins.
///
/// What namespaced mode reads a server's address, its database and its
/// login from. A project whose root has no manifest keeps them beside its
/// apps, in `backend/.env`, where a lookup at the root alone found no
/// port and left the database shared.
///
/// No `Debug`: the values are passwords as often as not.
#[derive(Default)]
pub struct EnvFiles {
    files: Vec<(String, ParsedEnv)>,
}

impl EnvFiles {
    pub fn read(root: &Path, dirs: &[String]) -> EnvFiles {
        let mut files = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for dir in std::iter::once("").chain(dirs.iter().map(String::as_str)) {
            let dir = dir.trim_start_matches("./").trim_end_matches('/');
            if seen.contains(&dir) {
                continue;
            }
            seen.push(dir);
            for (name, env) in read_env_files(&root.join(dir)) {
                let name = match dir {
                    "" | "." => name,
                    dir => format!("{dir}/{name}"),
                };
                files.push((name, env));
            }
        }
        EnvFiles { files }
    }

    /// A key's value, as [`value_in_env`] reads it.
    pub fn value(&self, key: &str) -> Result<Option<String>, Unresolved> {
        Ok(value_in_files(&self.files, key)?.map(|(_, value)| value.to_string()))
    }

    /// Something kept beside a service's address, as [`sibling_value`]
    /// reads it.
    pub fn sibling<'a>(
        &self,
        keys: impl IntoIterator<Item = &'a str>,
        suffixes: &[&str],
    ) -> Result<Option<(String, String)>, Unresolved> {
        for key in keys {
            let Some(prefix) = ["_PORT", "_HOST", "_URL"]
                .iter()
                .find_map(|suffix| key.strip_suffix(suffix))
            else {
                continue;
            };
            for suffix in suffixes {
                let sibling = format!("{prefix}{suffix}");
                let Some((_, value)) = value_in_files(&self.files, &sibling)? else {
                    continue;
                };
                let value = value.trim();
                if !value.is_empty() {
                    return Ok(Some((sibling, value.to_string())));
                }
            }
        }
        Ok(None)
    }
}

/// The user and the password a connection URL carries before its `@`,
/// percent-escapes decoded: a password with an `@` or a `:` in it can only
/// be written in a URL escaped, and the client that is handed it wants
/// the characters, not the escapes.
pub fn url_userinfo(value: &str) -> (Option<String>, Option<String>) {
    let Some(after_scheme) = value.find("://").map(|at| at + 3) else {
        return (None, None);
    };
    let rest = &value[after_scheme..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let Some(at) = authority.rfind('@') else {
        return (None, None);
    };
    let userinfo = &authority[..at];
    let (user, password) = match userinfo.split_once(':') {
        Some((user, password)) => (user, Some(password)),
        None => (userinfo, None),
    };
    let decoded = |text: &str| Some(percent_decoded(text)).filter(|t| !t.is_empty());
    (decoded(user), password.and_then(decoded))
}

/// The host of a URL, without its port, brackets and all for IPv6.
pub fn url_host(value: &str) -> Option<String> {
    let after_scheme = value.find("://")? + 3;
    let rest = &value[after_scheme..];
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let hostport = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    let host = match hostport.starts_with('[') {
        true => &hostport[..=hostport.find(']')?],
        false => hostport.split(':').next().unwrap_or(hostport),
    };
    (!host.is_empty()).then(|| host.to_string())
}

/// A URL with its path — the database a connection URL names, or the
/// slot of a Redis URL — replaced, and everything else as it was: the
/// login, the host, the port, and the query string.
pub fn with_url_path(value: &str, path: &str) -> Option<String> {
    let after_scheme = value.find("://")? + 3;
    let rest = &value[after_scheme..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let query = match tail.starts_with('/') {
        true => &tail[tail.find(['?', '#']).unwrap_or(tail.len())..],
        false => tail,
    };
    Some(format!(
        "{}{authority}/{path}{query}",
        &value[..after_scheme]
    ))
}

/// Every value a URL's query string gives the parameter `name`, as
/// written: `2` for `db` in `redis://localhost:6379?db=2`. Empty when the
/// URL has no query, or none by that name.
pub fn url_query_values(value: &str, name: &str) -> Vec<String> {
    let Some((start, end)) = query_span(value) else {
        return Vec::new();
    };
    value[start..end]
        .split('&')
        .filter_map(|pair| pair.split_once('=').filter(|(key, _)| *key == name))
        .map(|(_, value)| value.to_string())
        .collect()
}

/// A URL with every value its query string gives the parameter `name`
/// replaced by `new`, and everything else as it was. A URL with no such
/// parameter comes back as it was.
pub fn with_url_query_value(value: &str, name: &str, new: &str) -> String {
    let Some((start, end)) = query_span(value) else {
        return value.to_string();
    };
    let query: Vec<String> = value[start..end]
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) if key == name => format!("{key}={new}"),
            _ => pair.to_string(),
        })
        .collect();
    format!("{}{}{}", &value[..start], query.join("&"), &value[end..])
}

/// Where a URL's query string is, between its `?` and its `#` or its end.
fn query_span(value: &str) -> Option<(usize, usize)> {
    let at = value.find(['?', '#'])?;
    if !value[at..].starts_with('?') {
        return None;
    }
    let start = at + 1;
    let end = value[start..]
        .find('#')
        .map_or(value.len(), |at| start + at);
    Some((start, end))
}

/// `%40` as `@`; anything that is not a valid escape stays as written.
fn percent_decoded(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .filter(|pair| pair.iter().all(u8::is_ascii_hexdigit))
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The port an env key names in this directory's env files: the port of a
/// URL, or a bare number.
///
/// What shared mode probes. The services there are the ones the developer
/// already runs, on the ports the project's own files say, so pando reads
/// rather than assigns.
///
/// A value that holds a reference nothing sets still names its port when
/// the port is written out: the server is where it says.
pub fn port_in_env(dir: &Path, key: &str) -> Option<u16> {
    let files = read_env_files(dir);
    let value = files.iter().find_map(|(_, env)| env.values.get(key))?;
    port_of_value(value)
}

/// Where an env key is set in the main checkout: its root's env files,
/// then those of each of `dirs` below it, in that order, and the first
/// file that sets it wins. The file, relative to `root` — `.env`,
/// `backend/.env` — and the value as written.
///
/// A project whose root has no manifest keeps its env files beside its
/// apps: the database an api reads from `backend/.env` is where that file
/// says, and a status that looked at the root alone said it had no port.
pub fn env_value_below(root: &Path, dirs: &[String], key: &str) -> Option<(String, String)> {
    std::iter::once("")
        .chain(dirs.iter().map(String::as_str))
        .find_map(|dir| {
            read_env_files(&root.join(dir))
                .into_iter()
                .find_map(|(name, mut env)| {
                    let value = env.values.remove(key)?;
                    let file = match dir {
                        "" => name,
                        dir => format!("{}/{name}", dir.trim_end_matches('/')),
                    };
                    Some((file, value))
                })
        })
}

/// [`port_in_env`] over [`env_value_below`]'s files: the port, and the
/// file it was read from.
pub fn port_in_env_below(root: &Path, dirs: &[String], key: &str) -> Option<(u16, String)> {
    let (file, value) = env_value_below(root, dirs, key)?;
    Some((port_of_value(&value)?, file))
}

/// The value an env key has in this directory's env files, in lookup order.
///
/// [`Unresolved`] when the value holds a reference nothing sets: handed on
/// as written, ahead of the app's own loader, the app logged in as a user
/// called `${DB_USER}`.
pub fn value_in_env(dir: &Path, key: &str) -> Result<Option<String>, Unresolved> {
    EnvFiles::read(dir, &[]).value(key)
}

/// An env key whose value holds a reference, as [`parse_env`] tells one
/// from a `$` in a password, that neither pando's environment nor an
/// earlier line of its file sets: a variable of a shell pando was not
/// started from, or of a file it does not read, like `.env.local`. What
/// the app's own loader makes of it is not something pando can know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub key: String,
    /// The file it was read from: `.env`.
    pub file: String,
    /// The reference, as written: `${DB_USER}`.
    pub reference: String,
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} in {} holds {}, which neither pando's environment nor an earlier line of {} sets",
            self.key, self.file, self.reference, self.file
        )
    }
}

impl std::error::Error for Unresolved {}

/// A key's value in `files`, and the name of the file it is in: the first
/// file that sets it wins, and when that one's value holds a reference
/// nothing sets, the key is [`Unresolved`] rather than the text as
/// written. A later file is not asked: an example's value is not the real
/// one.
fn value_in_files<'a>(
    files: &'a [(String, ParsedEnv)],
    key: &str,
) -> Result<Option<(&'a str, &'a str)>, Unresolved> {
    for (name, env) in files {
        if let Some(reference) = env.unresolved.get(key) {
            return Err(Unresolved {
                key: key.to_string(),
                file: name.clone(),
                reference: reference.clone(),
            });
        }
        if let Some(value) = env.values.get(key) {
            return Ok(Some((name, value)));
        }
    }
    Ok(None)
}

/// The port a value names: the port of a URL, or a bare number.
pub fn port_of_value(value: &str) -> Option<u16> {
    let value = value.trim();
    if let Ok(port) = value.parse::<u16>() {
        return Some(port);
    }
    let after_scheme = value.find("://")? + 3;
    let rest = &value[after_scheme..];
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let hostport = match authority.rfind('@') {
        Some(at) => &authority[at + 1..],
        None => authority,
    };
    // An IPv6 host is bracketed; only the colon after `]` is the port's.
    let colon = if hostport.starts_with('[') {
        hostport.find(']').map(|close| close + 1)?
    } else {
        hostport.rfind(':')?
    };
    hostport.get(colon + 1..)?.parse().ok()
}

/// Every env file the worktree has, in lookup order, each by its name.
fn read_env_files(worktree: &Path) -> Vec<(String, ParsedEnv)> {
    let mut out = Vec::new();
    for name in ENV_FILES {
        let Ok(text) = std::fs::read_to_string(worktree.join(name)) else {
            continue;
        };
        out.push((
            name.to_string(),
            parse_env_in(&text, &|name| std::env::var(name).ok()),
        ));
    }
    out
}

/// An env file's keys, as [`parse_env`] reads them.
#[derive(Debug, Default)]
struct ParsedEnv {
    values: std::collections::BTreeMap<String, String>,
    /// Each key whose value holds a reference nothing sets, with the first
    /// such reference as written: `${DB_USER}`. Its value keeps it as
    /// written.
    unresolved: std::collections::BTreeMap<String, String>,
}

/// `KEY=value` lines, with `export`, surrounding quotes and a trailing
/// comment dropped, and references expanded. Not a full dotenv
/// implementation: it reads what a key's value is, and pando swaps a
/// number inside it or hands it to the app as it is.
///
/// A value in quotes ends at its closing quote, and whatever follows is
/// dropped. An unquoted one ends at a `#` that starts a word — at the
/// start, or after a space or a tab — which is how compose and
/// python-dotenv read it; a `#` inside a password or a URL is kept. The
/// value goes into the app's environment, ahead of the app's own dotenv
/// file, so a comment read into it breaks an app that connects fine
/// without pando.
///
/// For the same reason, a value that is not in single quotes has its
/// `$NAME` and `${NAME}` references expanded, the way dotenv loaders
/// expand them: from the process environment first, then from a key
/// earlier in the same file, which is the order a loader that does not
/// override the environment resolves them in. `${NAME:-default}` and
/// `${NAME-default}` take their default as a shell does, references in
/// it and all, and `\$` is a literal `$`. The app's own loader would have
/// expanded `postgres://${POSTGRES_USER}@…`; handed it unexpanded, ahead
/// of that loader, the app logged in as a user called `${POSTGRES_USER}`.
///
/// A reference nothing defines is left as written. A braced `${NAME}`
/// makes the key it is in, and any key built from that one, known to hold
/// it: [`value_in_env`] says so rather than hand it on. So does a bare
/// `$NAME` whose name is written the way environment variables are, in
/// capitals, digits and underscores: dotenv-expand and the Ruby dotenv gem
/// read `$DB_USER` from a file pando does not read, like `.env.local`. Any
/// other bare `$word` that names nothing is a `$` and the word after it,
/// handed on as written: `pa$word` is a password, and python-dotenv, which
/// expands only braces, reads it as written too.
pub fn parse_env(text: &str) -> std::collections::BTreeMap<String, String> {
    parse_env_in(text, &|name| std::env::var(name).ok()).values
}

/// [`parse_env`], with the process environment as `environment` answers
/// for it, and the keys that hold a reference nothing sets.
fn parse_env_in(text: &str, environment: &dyn Fn(&str) -> Option<String>) -> ParsedEnv {
    let mut out = ParsedEnv::default();
    for (key, value, quote) in text.lines().filter_map(env_entry) {
        let expanded = match quote {
            Some('\'') => Expanded {
                text: value.to_string(),
                unresolved: None,
            },
            _ => expand(value, &|name| match environment(name) {
                Some(text) => Some(Expanded {
                    text,
                    unresolved: None,
                }),
                None => out.values.get(name).map(|text| Expanded {
                    text: text.clone(),
                    unresolved: out.unresolved.get(name).cloned(),
                }),
            }),
        };
        match expanded.unresolved {
            Some(reference) => out.unresolved.insert(key.to_string(), reference),
            None => out.unresolved.remove(key),
        };
        out.values.insert(key.to_string(), expanded.text);
    }
    out
}

/// One line of an env file as [`parse_env`] reads it, before any reference
/// in it is expanded: `None` for a blank line, a comment, or a line with
/// no key.
pub fn parse_env_line(line: &str) -> Option<(String, String)> {
    let (key, value, _) = env_entry(line)?;
    Some((key.to_string(), value.to_string()))
}

/// A line's key, its value, and the quote the value was closed in, if it
/// was.
fn env_entry(line: &str) -> Option<(&str, &str, Option<char>)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let value = value.trim();
    Some(match value.chars().next() {
        // An unclosed quote is kept as written: there is no telling
        // where the value was meant to end.
        Some(quote @ ('"' | '\'')) => match closing_quote(&value[1..], quote) {
            Some(end) => (key, &value[1..1 + end], Some(quote)),
            None => (key, value, None),
        },
        _ => (key, without_comment(value), None),
    })
}

/// A value with its references expanded, as [`expand`] makes it.
struct Expanded {
    text: String,
    /// The first reference in it that nothing sets, as written.
    unresolved: Option<String>,
}

/// A value with its `$NAME`, `${NAME}`, `${NAME:-default}` and
/// `${NAME-default}` references replaced by what `lookup` says, as
/// [`parse_env`] describes.
fn expand(value: &str, lookup: &dyn Fn(&str) -> Option<Expanded>) -> Expanded {
    let mut out = String::with_capacity(value.len());
    let mut unresolved = None;
    let mut rest = value;
    while let Some(at) = rest.find(['$', '\\']) {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        if let Some(after) = tail.strip_prefix("\\$") {
            out.push('$');
            rest = after;
            continue;
        }
        let Some(reference) = tail.strip_prefix('$').and_then(reference) else {
            // A backslash before anything else, or a `$` that starts no
            // name, is itself.
            out.push_str(&tail[..1]);
            rest = &tail[1..];
            continue;
        };
        // A default is expanded only when it is taken, as a shell does.
        let taken = match (lookup(reference.name), reference.default) {
            (Some(found), Some(default)) if found.text.is_empty() && reference.or_empty => {
                expand(default, lookup)
            }
            (Some(found), _) => found,
            (None, Some(default)) => expand(default, lookup),
            (None, None) => {
                let written = &tail[..1 + reference.len];
                Expanded {
                    text: written.to_string(),
                    unresolved: reference.names_a_variable().then(|| written.to_string()),
                }
            }
        };
        out.push_str(&taken.text);
        unresolved = unresolved.or(taken.unresolved);
        rest = &tail[1 + reference.len..];
    }
    out.push_str(rest);
    Expanded {
        text: out,
        unresolved,
    }
}

/// A reference, as [`expand`] reads the text after its `$`.
struct Reference<'a> {
    /// How many bytes of that text it takes.
    len: usize,
    name: &'a str,
    /// Whether it is written in braces: `${NAME}` rather than `$NAME`.
    braced: bool,
    /// What stands in for the name when it is unset.
    default: Option<&'a str>,
    /// Whether the default stands in for an empty value too: `:-` rather
    /// than `-`.
    or_empty: bool,
}

impl Reference<'_> {
    /// Whether it names a variable even when nothing sets it, rather than
    /// being a `$` and the word after it: braced, or a bare name written
    /// the way environment variables are, `$DB_USER` but not `$word`.
    fn names_a_variable(&self) -> bool {
        self.braced
            || self
                .name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    }
}

/// The reference at the start of `text`, the text after a `$`, if one is
/// there.
fn reference(text: &str) -> Option<Reference<'_>> {
    let name_len = |text: &str| {
        let len = text
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(text.len());
        (len > 0 && !text.starts_with(|c: char| c.is_ascii_digit())).then_some(len)
    };
    let Some(braced) = text.strip_prefix('{') else {
        let len = name_len(text)?;
        return Some(Reference {
            len,
            name: &text[..len],
            braced: false,
            default: None,
            or_empty: false,
        });
    };
    let close = closing_brace(braced)?;
    let inner = &braced[..close];
    let (name, after) = inner.split_at(name_len(inner)?);
    let (default, or_empty) = match after.strip_prefix(":-") {
        Some(default) => (Some(default), true),
        None if after.is_empty() => (None, false),
        None => (Some(after.strip_prefix('-')?), false),
    };
    Some(Reference {
        len: close + 2,
        name,
        braced: true,
        default,
        or_empty,
    })
}

/// Where the `}` that closes a braced reference is, in the text after its
/// `{`: past each `${…}` its default holds, so `${A:-${B}/x}` ends at the
/// last `}` and not at B's. A `\$` opens nothing.
fn closing_brace(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], bytes.get(i + 1)) {
            (b'\\', Some(b'$')) => i += 1,
            (b'$', Some(b'{')) => {
                depth += 1;
                i += 1;
            }
            (b'}', _) if depth == 0 => return Some(i),
            (b'}', _) => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Where the quote that opened a value closes, in the text after it. A
/// `\"` inside double quotes does not close them.
fn closing_quote(text: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quote == '"' => escaped = true,
            c if c == quote => return Some(i),
            _ => {}
        }
    }
    None
}

/// An unquoted value up to the `#` that starts its comment, if it has one.
fn without_comment(value: &str) -> &str {
    let mut previous = ' ';
    for (i, c) in value.char_indices() {
        if c == '#' && matches!(previous, ' ' | '\t') {
            return value[..i].trim_end();
        }
        previous = c;
    }
    value
}

/// A service being waited on: which port it was given, and whether its
/// compose entry declares a healthcheck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    pub service: String,
    pub port: u16,
    pub healthcheck: bool,
}

/// Waits until every service is ready, or fails naming the first one that
/// was not and how long it was given.
///
/// Health when the service declares it, because a connect succeeding says
/// the socket is open and not that the database will answer a query —
/// Postgres in particular accepts connections while it is still
/// initialising. A connect otherwise, and never a bind.
pub fn wait_ready(
    compose: &Compose,
    wanted: &[Wanted],
    timeout: Duration,
    progress: &dyn Fn(&str),
) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    let deadline = Instant::now() + timeout;
    let mut pending: Vec<&Wanted> = wanted.iter().collect();
    let watched = pending.iter().any(|w| w.healthcheck);
    for service in &pending {
        progress(&format!("waiting for {}", service.service));
    }
    let mut round = 0u32;
    loop {
        // One `ps` per round, not one per service: it is a process spawn,
        // and it answers for every container of the project at once.
        //
        // Every round when a healthcheck is the answer; otherwise on a
        // slower beat, purely so a container that *exited* is noticed at
        // once. A postgres that refuses to start without a password dies
        // in a second, and waiting the whole minute to say "did not
        // become ready" hides the one line that explains it.
        let statuses = if watched || round.is_multiple_of(CHECK_EVERY) {
            compose.ps()?
        } else {
            Vec::new()
        };
        round += 1;
        pending.retain(|want| !is_ready(want, &statuses));
        if pending.is_empty() {
            return Ok(());
        }
        // A container that has exited is never going to be ready, and
        // waiting the whole timeout to say so wastes a minute of the
        // developer's time for an answer already on disk.
        if let Some(want) = pending.iter().find(|want| {
            statuses
                .iter()
                .any(|s| s.service == want.service && s.dead())
        }) {
            bail!(
                "the service {:?} exited before it was ready — `docker compose -p {} logs {}` \
                 says why",
                want.service,
                compose.project(),
                want.service
            );
        }
        if Instant::now() >= deadline {
            let names: Vec<&str> = pending.iter().map(|w| w.service.as_str()).collect();
            bail!(
                "the service{} {} did not become ready in {}s",
                if names.len() == 1 { "" } else { "s" },
                names.join(", "),
                // Rounded up, never down: "did not become ready in 0s" is
                // a sentence that makes pando look broken.
                timeout.as_millis().div_ceil(1000)
            );
        }
        std::thread::sleep(POLL);
    }
}

/// Compose health when the service declares a healthcheck, because that is
/// the project's own answer to "is this up" and nothing pando can do from
/// outside beats it.
///
/// Otherwise a connect that also proves something is *behind* the port.
/// Docker's published port is a proxy that completes the handshake as soon
/// as the container is running, so a plain connect would declare a postgres
/// ready while it is still running `initdb` — and the detected `migrate`
/// hook would then run against a database refusing connections. See
/// [`ports::something_is_serving`].
fn is_ready(want: &Wanted, statuses: &[Status]) -> bool {
    if want.healthcheck {
        return statuses
            .iter()
            .any(|s| s.service == want.service && s.health == "healthy");
    }
    ports::something_is_serving(want.port)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectRef;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn home_with_shim(body: &str) -> (TempDir, PandoPaths) {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let home = dir.path().join("pando-home");
        std::fs::create_dir_all(home.join("bin")).unwrap();
        let shim = home.join("bin").join("docker");
        std::fs::write(&shim, body).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let paths = PandoPaths::new(home, ProjectRef::from_root(&root).unwrap());
        (dir, paths)
    }

    #[test]
    fn a_service_addressed_in_parts_names_its_database_and_user_in_siblings() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(".env.example"),
            "DB_HOST=localhost\nDB_PORT=3306\nDB_NAME=shop\nDB_USERNAME=shop_user\n\
             REDIS_PORT=6379\nREDIS_DB=2\n",
        )
        .unwrap();
        assert_eq!(
            sibling_identity(dir.path(), ["DB_PORT"]),
            (Some("shop_user".to_string()), Some("shop".to_string()))
        );
        assert_eq!(
            sibling_identity(dir.path(), ["REDIS_PORT"]),
            (None, None),
            "a numbered database is not a name anything creates"
        );
        assert_eq!(sibling_identity(dir.path(), ["PORT"]), (None, None));
        // `.env` is read before the example, as everywhere else.
        std::fs::write(dir.path().join(".env"), "DB_NAME=shop_local\n").unwrap();
        assert_eq!(
            sibling_identity(dir.path(), ["DB_PORT"]).1.as_deref(),
            Some("shop_local")
        );
    }

    #[test]
    fn a_urls_login_is_read_with_only_real_escapes_undone() {
        assert_eq!(
            url_userinfo("mysql://a%40b:c%3Ad@h:1/x"),
            (Some("a@b".to_string()), Some("c:d".to_string()))
        );
        // Not escapes: left exactly as written.
        assert_eq!(
            url_userinfo("mysql://u:%zz%+1%@h/x").1.as_deref(),
            Some("%zz%+1%")
        );
        assert_eq!(url_userinfo("mysql://h:1/x"), (None, None));
        assert_eq!(url_userinfo("mysql://:@h:1/x"), (None, None));
        assert_eq!(url_userinfo("not a url"), (None, None));
        // The `@` that ends the login is the last one before the host.
        assert_eq!(
            url_userinfo("redis://:p@ss@h:6379/0").1.as_deref(),
            Some("p@ss")
        );
    }

    #[test]
    fn a_value_beside_an_address_is_found_by_its_prefix() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join(".env.example"),
            "DB_PORT=1\nDB_PASS=example\nREDIS_URL=redis://h\nREDIS_DB=2\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".env"), "DB_PASSWORD=\nDB_PWD=real\n").unwrap();
        // An empty `.env` value is nothing, and the next spelling is read.
        assert_eq!(
            sibling_value(dir.path(), ["DB_PORT"], &["_PASSWORD", "_PWD", "_PASS"]),
            Ok(Some(("DB_PWD".to_string(), "real".to_string())))
        );
        assert_eq!(
            sibling_value(dir.path(), ["REDIS_URL"], &["_DB"]),
            Ok(Some(("REDIS_DB".to_string(), "2".to_string())))
        );
        assert_eq!(sibling_value(dir.path(), ["PORT"], &["_DB"]), Ok(None));
    }

    #[test]
    fn docker_comes_from_the_path_unless_a_shim_is_in_pandos_home() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let paths = PandoPaths::new(dir.path().join("h"), ProjectRef::from_root(&root).unwrap());
        assert_eq!(docker_program(&paths), PathBuf::from("docker"));

        // Present but not executable is not a shim; it is a file somebody
        // left there, and running it would fail in a confusing way.
        std::fs::create_dir_all(paths.home.join("bin")).unwrap();
        std::fs::write(paths.home.join("bin").join("docker"), "x").unwrap();
        assert_eq!(docker_program(&paths), PathBuf::from("docker"));

        let shim = paths.home.join("bin").join("docker");
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(docker_program(&paths), shim);
    }

    #[test]
    fn every_invocation_names_the_project_and_both_files() {
        let compose = Compose::new(
            "docker",
            "pando-acme-feat-one",
            vec![
                PathBuf::from("/wt/docker-compose.yml"),
                PathBuf::from("/h/o.yml"),
            ],
            "/wt",
        );
        assert_eq!(
            compose.args(&["up", "-d", "postgres"]),
            vec![
                "compose",
                "-p",
                "pando-acme-feat-one",
                "-f",
                "/wt/docker-compose.yml",
                "-f",
                "/h/o.yml",
                "up",
                "-d",
                "postgres"
            ]
        );
    }

    #[test]
    fn the_by_project_form_names_no_file_because_stop_has_no_config() {
        let compose = Compose::by_project("docker", "pando-acme-feat-one");
        assert_eq!(
            compose.args(&["down", "-v"]),
            vec!["compose", "-p", "pando-acme-feat-one", "down", "-v"]
        );
    }

    #[test]
    fn the_log_pump_command_quotes_every_path_it_names() {
        let compose = Compose::new(
            "/pando home/bin/docker",
            "pando-p-feat-one",
            vec![PathBuf::from("/a b/docker-compose.yml")],
            "/a b",
        );
        assert_eq!(
            compose.logs_shell_cmd("postgres"),
            "'/pando home/bin/docker' 'compose' '-p' 'pando-p-feat-one' '-f' \
             '/a b/docker-compose.yml' 'logs' '-f' '--no-color' 'postgres'"
        );
    }

    // Compose 5.0.1 on the development machine: one object per line.
    #[test]
    fn ps_reads_the_ndjson_compose_five_emits() {
        let text = concat!(
            r#"{"Service":"postgres","State":"running","Health":"healthy","Publishers":[{"URL":"127.0.0.1","TargetPort":5432,"PublishedPort":17004,"Protocol":"tcp"}]}"#,
            "\n",
            r#"{"Service":"redis","State":"running","Health":"","Publishers":[{"URL":"","TargetPort":6379,"PublishedPort":0,"Protocol":"tcp"}]}"#,
            "\n"
        );
        let statuses = parse_ps(text).unwrap();
        assert_eq!(statuses.len(), 2);
        assert_eq!(statuses[0].service, "postgres");
        assert_eq!(statuses[0].health, "healthy");
        assert_eq!(statuses[0].published, vec![(17_004, 5432)]);
        assert!(statuses[0].running());
        assert!(!statuses[0].dead());
        // An unpublished port is not a port anything on the host can reach.
        assert!(statuses[1].published.is_empty());
    }

    #[test]
    fn ps_also_reads_the_json_array_an_older_compose_emits() {
        let text = r#"[{"Service":"db","State":"exited","Health":""}]"#;
        let statuses = parse_ps(text).unwrap();
        assert_eq!(statuses[0].service, "db");
        assert!(statuses[0].dead());
        assert!(!statuses[0].running());
        assert!(parse_ps("").unwrap().is_empty());
        assert!(parse_ps("  \n ").unwrap().is_empty());
    }

    #[test]
    fn a_docker_that_fails_is_reported_with_its_last_line() {
        let (_dir, paths) =
            home_with_shim("#!/bin/sh\necho 'no such service: nope' >&2\nexit 14\n");
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let err = format!("{:#}", compose.up(&["nope".to_string()]).unwrap_err());
        assert!(err.contains("exited 14"), "{err}");
        assert!(err.contains("no such service: nope"), "{err}");
        assert!(err.contains("compose -p pando-x-y up -d nope"), "{err}");
    }

    // A daemon that is down is its own error, so a `stop` can tell it from
    // a failure without reading the sentence.
    #[test]
    fn a_daemon_that_is_down_is_recognisable_through_any_context() {
        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\necho 'Cannot connect to the Docker daemon at unix:///var/run/docker.sock.' >&2\nexit 1\n",
        );
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let err = compose.stop().unwrap_err().context("stopping x");
        assert!(is_daemon_down(&err), "{err:#}");
        assert!(format!("{err:#}").contains("Docker daemon is not running"));
        let err = compose.reachable().unwrap_err();
        assert!(is_daemon_down(&err), "{err:#}");
    }

    // OrbStack's socket, and newer docker clients, say it another way; read
    // as a generic failure it made `stop` and `rm` fail where a daemon that
    // is down has nothing to stop.
    // Compose drops a service whose profile is not active from `config`,
    // and a file with an `include:` is read through `config`: an included
    // `db` with `profiles: [db]` was refused as one the file did not
    // declare, while `up -d db` would have started it.
    #[test]
    fn composes_own_reading_keeps_a_profiled_service() {
        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\nall=\nprev=\nfor a in \"$@\"; do\n  \
             [ \"$a\" = config ] && break\n  \
             [ \"$prev\" = --profile ] && [ \"$a\" = '*' ] && all=1\n  \
             prev=$a\ndone\n\
             if [ -n \"$all\" ]; then\n  \
             echo '{\"name\":\"p\",\"services\":{\"cache\":{\"image\":\"redis:7\"},\
             \"db\":{\"image\":\"postgres:16\",\"profiles\":[\"db\"]}}}'\n\
             else\n  \
             echo '{\"name\":\"p\",\"services\":{\"cache\":{\"image\":\"redis:7\"}}}'\n\
             fi\n",
        );
        let compose = Compose::new(
            docker_program(&paths),
            "p",
            vec![paths.root().join("compose.yaml")],
            paths.root(),
        );
        let file = compose.config().unwrap();
        assert_eq!(
            file.services.keys().collect::<Vec<_>>(),
            vec!["cache", "db"]
        );
    }

    #[test]
    fn a_daemon_down_in_the_newer_wording_is_recognised() {
        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\necho 'failed to connect to the docker API at unix:///x/docker.sock; check if the path is correct and if the daemon is running: dial unix /x/docker.sock: connect: no such file or directory' >&2\nexit 1\n",
        );
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let err = compose.reachable().unwrap_err();
        assert!(is_daemon_down(&err), "{err:#}");
    }

    // A wedged Docker Desktop accepts the connection and never answers, and
    // `rm` asked it under the state lock: every other pando froze with it.
    // The read-only probe is bounded, and a timeout is its own error, told
    // apart from a daemon that is down because its containers may be up.
    #[test]
    fn a_docker_that_never_answers_is_given_up_on() {
        let (_dir, paths) = home_with_shim("#!/bin/sh\nexec sleep 30\n");
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let began = Instant::now();
        let err = compose
            .run(
                &["ps", "--all", "--format", "json"],
                Some(Duration::from_millis(300)),
            )
            .unwrap_err()
            .context("asking about x");
        assert!(began.elapsed() < Duration::from_secs(10), "it waited");
        assert!(is_daemon_hung(&err), "{err:#}");
        assert!(!is_daemon_down(&err), "{err:#}");
        assert!(format!("{err:#}").contains("docker compose ps"), "{err:#}");
    }

    #[test]
    fn a_docker_that_is_not_there_says_isolated_mode_needs_it() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        let paths = PandoPaths::new(dir.path().join("h"), ProjectRef::from_root(&root).unwrap());
        let compose = Compose::by_project(paths.home.join("bin").join("docker"), "pando-x-y");
        let err = format!("{:#}", compose.stop().unwrap_err());
        assert!(err.contains("needs Docker"), "{err}");
    }

    // A switch to a native service stops the compose container first, and
    // a docker that cannot be run failed that stop for ever: there is no
    // Docker left for the container to come back with, where a daemon
    // that is only down still has one.
    #[test]
    fn a_docker_that_cannot_be_run_is_told_from_one_whose_daemon_is_down() {
        let (_dir, paths) = home_with_shim("#!/nonexistent/interpreter\n");
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let err = compose.ps().unwrap_err().context("asking about x");
        assert!(is_docker_missing(&err), "{err:#}");
        assert!(!is_daemon_down(&err), "{err:#}");

        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\necho 'Cannot connect to the Docker daemon at unix:///var/run/docker.sock.' >&2\nexit 1\n",
        );
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let err = compose.ps().unwrap_err();
        assert!(!is_docker_missing(&err), "{err:#}");
    }

    #[test]
    fn readiness_by_health_waits_for_healthy_and_not_merely_running() {
        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\n\
             f=$(dirname \"$0\")/round\n\
             n=$(cat \"$f\" 2>/dev/null || echo 0)\n\
             echo $((n + 1)) > \"$f\"\n\
             if [ \"$n\" -lt 2 ]; then h=starting; else h=healthy; fi\n\
             echo \"{\\\"Service\\\":\\\"db\\\",\\\"State\\\":\\\"running\\\",\\\"Health\\\":\\\"$h\\\"}\"\n",
        );
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let wanted = vec![Wanted {
            service: "db".into(),
            port: 1,
            healthcheck: true,
        }];
        wait_ready(&compose, &wanted, Duration::from_secs(10), &|_| {}).unwrap();
    }

    #[test]
    fn a_service_that_exits_fails_the_start_at_once_rather_than_at_the_timeout() {
        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\necho '{\"Service\":\"db\",\"State\":\"exited\",\"Health\":\"\"}'\n",
        );
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let wanted = vec![Wanted {
            service: "db".into(),
            port: 1,
            healthcheck: true,
        }];
        let started = Instant::now();
        let err = format!(
            "{:#}",
            wait_ready(&compose, &wanted, Duration::from_secs(30), &|_| {}).unwrap_err()
        );
        assert!(err.contains("\"db\""), "{err}");
        assert!(err.contains("exited"), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it did not wait"
        );
    }

    // Without a healthcheck the answer is a connect, and a connect to a
    // container that died looks exactly like one to a container still
    // starting. Docker is asked on a slow beat so the difference is
    // noticed in a second rather than at the end of the timeout.
    #[test]
    fn a_service_with_no_healthcheck_that_exits_is_noticed_without_waiting() {
        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\necho '{\"Service\":\"db\",\"State\":\"exited\",\"Health\":\"\"}'\n",
        );
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let wanted = vec![Wanted {
            service: "db".into(),
            port: 1,
            healthcheck: false,
        }];
        let started = Instant::now();
        let err = format!(
            "{:#}",
            wait_ready(&compose, &wanted, Duration::from_secs(30), &|_| {}).unwrap_err()
        );
        assert!(err.contains("exited before it was ready"), "{err}");
        assert!(err.contains("logs db"), "and where to read why: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "it did not wait"
        );
    }

    #[test]
    fn a_service_that_never_becomes_ready_fails_with_its_name_and_the_timeout() {
        let (_dir, paths) = home_with_shim(
            "#!/bin/sh\necho '{\"Service\":\"db\",\"State\":\"running\",\"Health\":\"starting\"}'\n",
        );
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let wanted = vec![Wanted {
            service: "db".into(),
            port: 1,
            healthcheck: true,
        }];
        let err = format!(
            "{:#}",
            wait_ready(&compose, &wanted, Duration::from_millis(400), &|_| {}).unwrap_err()
        );
        assert!(err.contains("db"), "{err}");
        assert!(err.contains("did not become ready"), "{err}");
    }

    // Readiness by connect, against a real listener rather than a mock:
    // the one thing it must never do is bind the port itself.
    #[test]
    fn readiness_by_connect_waits_for_something_to_be_listening() {
        let (_dir, paths) = home_with_shim("#!/bin/sh\nexit 0\n");
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let wanted = vec![Wanted {
            service: "cache".into(),
            port,
            healthcheck: false,
        }];
        wait_ready(&compose, &wanted, Duration::from_secs(5), &|_| {}).unwrap();
        // Held to the end on purpose. A port freed mid-test is a port
        // another test running in parallel can bind, and then "nothing is
        // listening" is not a thing this process can assert.
        drop(listener);
    }

    #[test]
    fn readiness_by_connect_gives_up_when_nothing_ever_answers() {
        let (_dir, paths) = home_with_shim("#!/bin/sh\nexit 0\n");
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        // Port 1 is privileged and unbindable by a test, so a refused
        // connection here is a fact rather than a race.
        let wanted = vec![Wanted {
            service: "cache".into(),
            port: 1,
            healthcheck: false,
        }];
        let err = format!(
            "{:#}",
            wait_ready(&compose, &wanted, Duration::from_millis(400), &|_| {}).unwrap_err()
        );
        assert!(err.contains("cache"), "{err}");
        assert!(err.contains("did not become ready"), "{err}");
    }

    // ---- env rewriting ---------------------------------------------------

    fn worktree_with(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (name, body) in files {
            std::fs::write(dir.path().join(name), body).unwrap();
        }
        dir
    }

    fn map(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn ports(pairs: &[(&str, u16)]) -> std::collections::BTreeMap<String, u16> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn a_url_keeps_everything_but_its_port() {
        let dir = worktree_with(&[(
            ".env",
            "DATABASE_URL=postgres://acme:secret@localhost:5432/acme?sslmode=disable\n\
             REDIS_URL=redis://localhost:6379\n",
        )]);
        let env = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres"), ("REDIS_URL", "redis")]),
            &ports(&[("postgres", 17_004), ("redis", 17_006)]),
        )
        .unwrap();
        assert_eq!(
            env["DATABASE_URL"],
            "postgres://acme:secret@localhost:17004/acme?sslmode=disable"
        );
        assert_eq!(env["REDIS_URL"], "redis://localhost:17006");
    }

    #[test]
    fn a_query_parameter_is_read_and_replaced_and_nothing_else_is() {
        let url = "redis://:pw@localhost:6379/1?ssl=true&db=2#main";
        assert_eq!(url_query_values(url, "db"), vec!["2".to_string()]);
        assert!(url_query_values(url, "ssl_db").is_empty());
        assert_eq!(
            with_url_query_value(url, "db", "7"),
            "redis://:pw@localhost:6379/1?ssl=true&db=7#main"
        );
        assert!(url_query_values("redis://localhost:6379/2", "db").is_empty());
        assert!(url_query_values("redis://localhost:6379#?db=2", "db").is_empty());
        assert_eq!(
            with_url_query_value("redis://localhost:6379/2", "db", "7"),
            "redis://localhost:6379/2"
        );
    }

    #[test]
    fn a_commented_value_is_rewritten_without_its_comment() {
        let dir = worktree_with(&[(
            ".env",
            "DATABASE_URL=postgres://app:app@localhost:5432/app  # local docker\n\
             DB_PORT=5432 # default\n",
        )]);
        let env = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres"), ("DB_PORT", "postgres")]),
            &ports(&[("postgres", 17_004)]),
        )
        .unwrap();
        assert_eq!(
            env["DATABASE_URL"],
            "postgres://app:app@localhost:17004/app"
        );
        assert_eq!(env["DB_PORT"], "17004", "a commented port is still a port");
    }

    #[test]
    fn a_url_that_names_the_compose_service_as_its_host_is_pointed_at_localhost() {
        let dir = worktree_with(&[(
            ".env.example",
            "DATABASE_URL=postgres://app@postgres:5432/app\nOTHER_URL=redis://cache-a:6379\n",
        )]);
        let env = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres"), ("OTHER_URL", "redis")]),
            &ports(&[("postgres", 17_004), ("redis", 17_006)]),
        )
        .unwrap();
        assert_eq!(env["DATABASE_URL"], "postgres://app@localhost:17004/app");
        assert_eq!(
            env["OTHER_URL"], "redis://cache-a:17006",
            "a host that is not the service's name is the developer's and is kept"
        );
    }

    #[test]
    fn a_url_with_no_port_gains_one() {
        let dir = worktree_with(&[(".env", "REDIS_URL=redis://localhost\n")]);
        let env = app_env(
            dir.path(),
            &map(&[("REDIS_URL", "redis")]),
            &ports(&[("redis", 17_006)]),
        )
        .unwrap();
        assert_eq!(env["REDIS_URL"], "redis://localhost:17006");
    }

    #[test]
    fn an_ipv6_host_keeps_its_brackets() {
        let dir = worktree_with(&[(".env", "DB=postgres://[::1]:5432/app\n")]);
        let env = app_env(
            dir.path(),
            &map(&[("DB", "postgres")]),
            &ports(&[("postgres", 17_004)]),
        )
        .unwrap();
        assert_eq!(env["DB"], "postgres://[::1]:17004/app");
    }

    #[test]
    fn a_bare_number_becomes_the_port() {
        let dir = worktree_with(&[(".env", "DB_HOST=localhost\nDB_PORT=5432\n")]);
        let env = app_env(
            dir.path(),
            &map(&[("DB_PORT", "db")]),
            &ports(&[("db", 17_004)]),
        )
        .unwrap();
        assert_eq!(env["DB_PORT"], "17004");
    }

    // The real credentials live in `.env`; the example is the fallback.
    #[test]
    fn the_worktrees_own_env_wins_over_the_example() {
        let dir = worktree_with(&[
            (
                ".env",
                "DATABASE_URL=postgres://real:pw@localhost:5432/real\n",
            ),
            (
                ".env.example",
                "DATABASE_URL=postgres://user:pass@localhost:5432/db\nEXTRA_URL=redis://localhost:6379\n",
            ),
        ]);
        let env = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres"), ("EXTRA_URL", "redis")]),
            &ports(&[("postgres", 17_004), ("redis", 17_006)]),
        )
        .unwrap();
        assert_eq!(
            env["DATABASE_URL"],
            "postgres://real:pw@localhost:17004/real"
        );
        assert_eq!(
            env["EXTRA_URL"], "redis://localhost:17006",
            "a key only the example has still resolves"
        );
    }

    #[test]
    fn a_key_nothing_in_the_worktree_sets_is_an_error_naming_it() {
        let dir = worktree_with(&[(".env", "SOMETHING_ELSE=1\n")]);
        let err = format!(
            "{:#}",
            app_env(
                dir.path(),
                &map(&[("DATABASE_URL", "postgres")]),
                &ports(&[("postgres", 17_004)]),
            )
            .unwrap_err()
        );
        assert!(err.contains("DATABASE_URL"), "{err}");
        assert!(
            err.contains(".env.example"),
            "it says where to put it: {err}"
        );
    }

    #[test]
    fn a_value_that_is_neither_a_url_nor_a_port_is_an_error_naming_it() {
        let dir = worktree_with(&[(".env", "DB=localhost\n")]);
        let err = format!(
            "{:#}",
            app_env(
                dir.path(),
                &map(&[("DB", "postgres")]),
                &ports(&[("postgres", 17_004)]),
            )
            .unwrap_err()
        );
        assert!(err.contains("DB=\"localhost\""), "{err}");
        assert!(err.contains(".env"), "{err}");
    }

    #[test]
    fn env_files_are_read_the_way_a_shell_would_read_them() {
        let parsed = parse_env(
            "# a comment\n\nexport A=1\nB = \"two\"\nC='three'\nD=four=five\nbroken\n=nokey\n",
        );
        assert_eq!(parsed["A"], "1");
        assert_eq!(parsed["B"], "two");
        assert_eq!(parsed["C"], "three");
        assert_eq!(parsed["D"], "four=five");
        assert_eq!(parsed.len(), 4);
    }

    // An inline comment is the developer's note, not part of the value.
    // Read into it, the URL pando puts in the app's environment, ahead of
    // the app's own dotenv file, names a database called `app  # local
    // docker`.
    #[test]
    fn an_inline_comment_is_not_part_of_the_value() {
        let parsed = parse_env(
            "URL=postgres://app:app@localhost:5432/app  # local docker\n\
             E=5432 # default\nF=\"x\" # c\nG=postgres://u:p#w@h:1/d\nH=a#b\n\
             I='y'# c\nJ=\"a \\\" # b\"\nK=\"open\n",
        );
        assert_eq!(parsed["URL"], "postgres://app:app@localhost:5432/app");
        assert_eq!(parsed["E"], "5432");
        assert_eq!(parsed["F"], "x");
        assert_eq!(
            parsed["G"], "postgres://u:p#w@h:1/d",
            "a `#` in a word stays"
        );
        assert_eq!(parsed["H"], "a#b");
        assert_eq!(parsed["I"], "y");
        assert_eq!(
            parsed["J"], "a \\\" # b",
            "an escaped quote does not close it"
        );
        assert_eq!(
            parsed["K"], "\"open",
            "an unclosed quote is kept as written"
        );
    }

    // A URL built from the file's own keys is what the app's loader
    // would have made of it; unexpanded, the app logged in as a user
    // called `${POSTGRES_USER}`.
    #[test]
    fn a_reference_is_expanded_from_the_environment_then_from_the_file() {
        let environment = |name: &str| (name == "FROM_SHELL").then(|| "shell".to_string());
        let parsed = parse_env_in(
            "POSTGRES_USER=app\nPOSTGRES_PASSWORD=\"s3cret\"\nPOSTGRES_DB=shop\n\
             DATABASE_URL=postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@localhost:5432/${POSTGRES_DB}\n\
             QUOTED=\"$POSTGRES_USER-${FROM_SHELL}\"\n\
             FROM_SHELL=file\nSHADOWED=${FROM_SHELL}\n",
            &environment,
        )
        .values;
        assert_eq!(
            parsed["DATABASE_URL"],
            "postgres://app:s3cret@localhost:5432/shop"
        );
        assert_eq!(parsed["QUOTED"], "app-shell");
        assert_eq!(
            parsed["SHADOWED"], "shell",
            "the environment wins, as a loader that does not override it reads it"
        );
        // And the environment is the process's own.
        let path = std::env::var("PATH").unwrap();
        assert_eq!(parse_env("P=${PATH}")["P"], path);
    }

    #[test]
    fn a_default_is_taken_and_a_literal_or_an_unknown_reference_is_left_as_written() {
        let parsed = parse_env_in(
            "EMPTY=\nDB_PORT=${PG_PORT:-5432}\nA=${EMPTY:-x}\nB=${EMPTY-x}\nC=${UNSET-x}\n\
             LITERAL='${DB_PORT}'\nESCAPED=\"a\\$DB_PORT\"\nUNKNOWN=${NOPE}/$NOPE\n\
             DOLLARS=pa$$word$ $1\nBROKEN=${DB_PORT\nLATER=${AFTER}\nAFTER=1\n",
            &|_| None,
        )
        .values;
        assert_eq!(parsed["DB_PORT"], "5432");
        assert_eq!(parsed["A"], "x", "`:-` stands in for an empty value");
        assert_eq!(parsed["B"], "", "`-` only for an unset one");
        assert_eq!(parsed["C"], "x");
        assert_eq!(parsed["LITERAL"], "${DB_PORT}", "single quotes are literal");
        assert_eq!(parsed["ESCAPED"], "a$DB_PORT");
        assert_eq!(parsed["UNKNOWN"], "${NOPE}/$NOPE");
        assert_eq!(parsed["DOLLARS"], "pa$$word$ $1");
        assert_eq!(parsed["BROKEN"], "${DB_PORT");
        assert_eq!(parsed["LATER"], "${AFTER}", "only a key read before it");
    }

    // A compose-style file defaults a URL to one built from its own keys;
    // closed at the inner `}`, the value was neither the default nor the
    // text as written.
    #[test]
    fn a_default_with_references_in_it_is_expanded_whole() {
        let environment = |name: &str| (name == "SET").then(|| "set".to_string());
        let parsed = parse_env_in(
            "DB_USER=app\n\
             DATABASE_URL=${DATABASE_URL:-postgres://${DB_USER}@localhost:5432/app}\n\
             DEEP=${UNSET:-${ALSO_UNSET:-$DB_USER}-x}\n\
             TAKEN=${SET:-${DB_USER}}\n\
             ESCAPED=${UNSET:-\\${DB_USER}}\n",
            &environment,
        )
        .values;
        assert_eq!(parsed["DATABASE_URL"], "postgres://app@localhost:5432/app");
        assert_eq!(parsed["DEEP"], "app-x");
        assert_eq!(
            parsed["TAKEN"], "set",
            "a name that is set wins, and its whole reference goes"
        );
        assert_eq!(
            parsed["ESCAPED"], "${DB_USER}",
            "`\\$` in a default opens nothing"
        );
    }

    #[test]
    fn an_isolated_app_env_is_built_from_the_expanded_value() {
        let dir = worktree_with(&[(
            ".env",
            "PANDO_TEST_PG_USER=app\nPANDO_TEST_PG_DB=shop\n\
             DATABASE_URL=postgres://${PANDO_TEST_PG_USER}@localhost:5432/${PANDO_TEST_PG_DB}\n\
             DB_PORT=${PANDO_TEST_PG_PORT:-5432}\n",
        )]);
        let env = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres"), ("DB_PORT", "postgres")]),
            &ports(&[("postgres", 17_004)]),
        )
        .unwrap();
        assert_eq!(env["DATABASE_URL"], "postgres://app@localhost:17004/shop");
        assert_eq!(env["DB_PORT"], "17004");
        assert_eq!(
            value_in_env(dir.path(), "DATABASE_URL").unwrap().as_deref(),
            Some("postgres://app@localhost:5432/shop"),
            "and a shared or namespaced start reads it expanded"
        );
    }

    #[test]
    fn a_reference_nothing_sets_is_known_and_so_is_a_key_built_from_it() {
        let parsed = parse_env_in(
            "USER=${NOPE}\nURL=postgres://${USER}@localhost/shop\nDEFAULTED=${NOPE:-app}\n\
             LITERAL='${NOPE}'\nESCAPED=\\$NOPE\nREDONE=${NOPE}\nREDONE=app\n\
             DEEP=${UNSET:-${NOPE}}\n",
            &|_| None,
        );
        let unresolved = |key: &str| parsed.unresolved.get(key).map(String::as_str);
        assert_eq!(unresolved("USER"), Some("${NOPE}"));
        assert_eq!(
            unresolved("URL"),
            Some("${NOPE}"),
            "built from a key that holds one"
        );
        assert_eq!(parsed.values["URL"], "postgres://${NOPE}@localhost/shop");
        assert_eq!(unresolved("DEEP"), Some("${NOPE}"), "a default taken");
        for key in ["DEFAULTED", "LITERAL", "ESCAPED", "REDONE"] {
            assert_eq!(unresolved(key), None, "{key}");
        }
    }

    // A password with a `$` in it failed an isolated start that had worked:
    // `$word` was taken for a variable nothing set. python-dotenv expands
    // only braces, and reads it as the password it is.
    #[test]
    fn a_bare_dollar_word_that_names_nothing_is_itself_and_handed_on() {
        let parsed = parse_env_in(
            "PASSWORD=pa$word\nURL=postgres://app:${PASSWORD}@localhost/shop\n\
             DEFAULTED=${NOPE:-pa$word}\nMIXED=pa$Word\nDIGITS=pa$x1y\n",
            &|_| None,
        );
        assert_eq!(parsed.values["PASSWORD"], "pa$word");
        assert_eq!(
            parsed.values["URL"],
            "postgres://app:pa$word@localhost/shop"
        );
        assert_eq!(parsed.values["DEFAULTED"], "pa$word");
        assert_eq!(parsed.values["MIXED"], "pa$Word");
        assert_eq!(parsed.values["DIGITS"], "pa$x1y");
        assert!(parsed.unresolved.is_empty(), "{:?}", parsed.unresolved);

        let dir = worktree_with(&[(
            ".env",
            "DATABASE_URL=postgres://app:pa$pando_test_unset_word@localhost:5432/shop\n",
        )]);
        let env = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres")]),
            &ports(&[("postgres", 17_004)]),
        )
        .unwrap();
        assert_eq!(
            env["DATABASE_URL"],
            "postgres://app:pa$pando_test_unset_word@localhost:17004/shop"
        );
        assert_eq!(
            value_in_env(dir.path(), "DATABASE_URL").unwrap().as_deref(),
            Some("postgres://app:pa$pando_test_unset_word@localhost:5432/shop")
        );
    }

    // Taken for a `$` in a password, a bare `$DB_USER` that `.env.local`
    // sets was handed on as written, ahead of the app's own loader, which
    // would have expanded it: the app logged in as a user called
    // `$DB_USER`.
    #[test]
    fn a_bare_name_written_as_a_variable_is_one_even_when_nothing_sets_it() {
        let parsed = parse_env_in(
            "URL=postgres://$DB_USER:$DB_PASSWORD@localhost:5432/app\nBUILT=${URL}\n\
             ONE=$_\nSET=$APP_HOME/data\n",
            &|name| (name == "APP_HOME").then(|| "/srv/app".to_string()),
        );
        let unresolved = |key: &str| parsed.unresolved.get(key).map(String::as_str);
        assert_eq!(unresolved("URL"), Some("$DB_USER"));
        assert_eq!(
            unresolved("BUILT"),
            Some("$DB_USER"),
            "built from a key that holds one"
        );
        assert_eq!(unresolved("ONE"), Some("$_"));
        assert_eq!(unresolved("SET"), None);
        assert_eq!(
            parsed.values["URL"],
            "postgres://$DB_USER:$DB_PASSWORD@localhost:5432/app"
        );
        assert_eq!(parsed.values["SET"], "/srv/app/data");

        let dir = worktree_with(&[
            (
                ".env",
                "DATABASE_URL=postgres://$PANDO_TEST_UNSET_USER@localhost:5432/app\n",
            ),
            (".env.local", "PANDO_TEST_UNSET_USER=app\n"),
        ]);
        let unresolved = Unresolved {
            key: "DATABASE_URL".into(),
            file: ".env".into(),
            reference: "$PANDO_TEST_UNSET_USER".into(),
        };
        assert_eq!(
            value_in_env(dir.path(), "DATABASE_URL"),
            Err(unresolved.clone())
        );
        let e = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres")]),
            &ports(&[("postgres", 17_004)]),
        )
        .unwrap_err();
        assert_eq!(e.downcast_ref::<Unresolved>(), Some(&unresolved));
    }

    // Handed on as written, ahead of the app's own loader, `${DB_USER}` was
    // the app's login; rewritten around it, an isolated app's too.
    #[test]
    fn a_value_holding_a_reference_nothing_sets_is_never_handed_on_as_written() {
        let dir = worktree_with(&[
            (
                ".env",
                "DATABASE_URL=postgres://${PANDO_TEST_UNSET_USER}@localhost:5432/shop\n",
            ),
            (
                ".env.example",
                "DATABASE_URL=postgres://app@localhost:5432/shop\n",
            ),
        ]);
        let unresolved = Unresolved {
            key: "DATABASE_URL".into(),
            file: ".env".into(),
            reference: "${PANDO_TEST_UNSET_USER}".into(),
        };
        assert_eq!(
            value_in_env(dir.path(), "DATABASE_URL"),
            Err(unresolved.clone()),
            "and the example's value is not the real one"
        );
        let e = app_env(
            dir.path(),
            &map(&[("DATABASE_URL", "postgres")]),
            &ports(&[("postgres", 17_004)]),
        )
        .unwrap_err();
        assert_eq!(e.downcast_ref::<Unresolved>(), Some(&unresolved));
        let e = format!("{e:#}");
        assert!(
            e.contains("env.DATABASE_URL")
                && e.contains("DATABASE_URL in .env holds ${PANDO_TEST_UNSET_USER}"),
            "{e}"
        );
        assert_eq!(
            port_in_env(dir.path(), "DATABASE_URL"),
            Some(5432),
            "the port is still where it says"
        );
        std::fs::write(
            dir.path().join(".env"),
            "DB_PORT=3306\nDB_PASSWORD=${PANDO_TEST_UNSET_SECRET}\nDB_PASS=other\n",
        )
        .unwrap();
        assert!(
            sibling_value(dir.path(), ["DB_PORT"], &["_PASSWORD", "_PASS"]).is_err(),
            "the first spelling set is the one read"
        );
    }

    // A project whose root has no manifest keeps its env files beside its
    // apps. The root still comes first, and the file a port was read from
    // is named, relative to the root.
    #[test]
    fn a_port_is_read_below_the_root_and_its_file_named() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for sub in ["backend", "frontend"] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }
        std::fs::write(root.join("frontend/.env"), "POSTGRES_PORT=6000\n").unwrap();
        std::fs::write(root.join("backend/.env.example"), "POSTGRES_PORT=5432\n").unwrap();
        std::fs::write(root.join("backend/.env"), "POSTGRES_PORT=5433\n").unwrap();
        let dirs = ["backend".to_string(), "frontend".to_string()];
        assert_eq!(port_in_env(root, "POSTGRES_PORT"), None);
        assert_eq!(
            port_in_env_below(root, &dirs, "POSTGRES_PORT"),
            Some((5433, "backend/.env".to_string())),
            "the first directory's local file, over its example and the next directory"
        );
        assert_eq!(port_in_env_below(root, &[], "POSTGRES_PORT"), None);

        std::fs::write(root.join(".env"), "POSTGRES_PORT=5434\n").unwrap();
        assert_eq!(
            port_in_env_below(root, &dirs, "POSTGRES_PORT"),
            Some((5434, ".env".to_string())),
            "the root's own first, as it always was"
        );
    }

    #[test]
    fn waiting_for_nothing_asks_docker_nothing() {
        // The shim would fail the test if it ran at all.
        let (_dir, paths) = home_with_shim("#!/bin/sh\nexit 3\n");
        let compose = Compose::by_project(docker_program(&paths), "pando-x-y");
        wait_ready(&compose, &[], Duration::from_secs(1), &|_| {}).unwrap();
    }
}
