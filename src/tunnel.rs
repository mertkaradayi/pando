//! Publishing one local port at a public URL.
//!
//! One provider in v1, `cloudflared` quick tunnels, behind a trait so a
//! second one can be added without touching `actions`.
//!
//! The tunnel is a process like any other: detached, its own process group,
//! logged to `logs/<worktree>/tunnel.log`, recorded in state, and swept.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::paths::PandoPaths;
use crate::platform::files::is_executable;
use crate::process::{self, SpawnOptions};
use crate::state::ShareRecord;

/// The log source the tunnel writes to. Reserved in [`crate::paths`], so no
/// process, hook, or service can be given the same name.
pub const TUNNEL_LOG: &str = "tunnel";

/// How long a provider may take to publish a URL before the share fails.
pub const READY_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Matches `actions`' own grace: a tunnel is not special enough to wait
/// longer for.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// Lines of the log an error carries. Enough to see the provider's own
/// complaint, short enough to read on one screen.
const TAIL_LINES: usize = 5;

const URL_SCHEME: &str = "https://";
/// What every public host a quick tunnel hands out ends in, and so the
/// `Host` every visitor's request through the share proxy carries.
pub const URL_HOST: &str = ".trycloudflare.com";
/// Where cloudflared asks for a quick tunnel. It is in the installed
/// binary, and a run that is offline or rate limited logs
/// `Post "https://api.trycloudflare.com/tunnel": …` — which is a URL on a
/// log line and is never one that was published.
const API_HOST: &str = "api.trycloudflare.com";

/// The only value `[share].provider` takes in v1.
pub const DEFAULT_PROVIDER: &str = "cloudflared";

/// The host a tunnel straight onto a dev server is pointed at. A name, not
/// `127.0.0.1`: a dev server told `localhost` is on `[::1]` alone on macOS
/// — Vite's default — and cloudflared dials every address the name
/// resolves to, so a server on either loopback answers.
pub const DEV_SERVER_HOST: &str = "localhost";

/// Without `--config <path>`, cloudflared reads `~/.cloudflared/config.yml`
/// and applies its `ingress:` rules to every request — even to a quick
/// tunnel created with `--url`. A developer who already runs a named tunnel
/// with a catch-all `service: http_status:404` would get empty 404s instead
/// of their dev server. Pointing cloudflared at this file, which pando owns
/// and which defines no ingress, makes it fall back to `--url` for
/// everything.
const EMPTY_TUNNEL_CONFIG: &str = "\
# Written by pando. An empty cloudflared config, whose whole purpose is to
# shadow ~/.cloudflared/config.yml so its ingress rules are not inherited by
# a quick tunnel. Edit it if you want to (pando never rewrites it); just do
# not add ingress rules, or shared worktrees will stop answering.
no-autoupdate: true
";

/// A running tunnel, before it is written to state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TunnelSpawn {
    pub pid: u32,
    pub pgid: process::Group,
    pub public_url: String,
    pub log_path: PathBuf,
    /// The tail of its log when the deadline passed before the tunnel had
    /// connected to the provider's edge: the URL may not answer yet, and
    /// this is what it was doing instead. `None` once it has connected.
    pub unconnected: Option<String>,
}

/// One way of publishing a local port.
///
/// v1 has exactly one implementation. The trait is here so the second one
/// arrives as a new file rather than as a branch inside `actions`.
pub trait Provider {
    /// What `[share].provider` calls this one.
    fn name(&self) -> &'static str;

    /// Whether the machine can run it at all, with an install hint when it
    /// cannot. Asked after every refusal, so a worktree that was never
    /// going to be shared does not get told to install anything.
    fn ensure_present(&self, paths: &PandoPaths) -> Result<()>;

    /// Publishes `local_port` on `host` and returns once a public URL
    /// exists and the provider's edge has a connection to serve it on —
    /// or, with the URL, the reason it does not yet, once the deadline
    /// passes.
    ///
    /// `spawned` is told the tunnel's process group the moment there is
    /// one, before the wait, so the caller can write it down somewhere
    /// that outlives this process.
    fn start(
        &self,
        paths: &PandoPaths,
        name: &str,
        host: &str,
        local_port: u16,
        spawned: &dyn Fn(process::Group),
    ) -> Result<TunnelSpawn>;
}

/// The provider `[share].provider` names, or the default when it names
/// none. An unknown name is refused rather than silently defaulted: a typo
/// that quietly shares through something else is worse than a failure.
pub fn provider_for(configured: Option<&str>) -> Result<Box<dyn Provider>> {
    match configured.unwrap_or(DEFAULT_PROVIDER) {
        DEFAULT_PROVIDER => Ok(Box::new(Cloudflared)),
        other => bail!(
            "[share].provider is {other:?}, and pando only speaks {DEFAULT_PROVIDER:?} — remove \
             the line to use it"
        ),
    }
}

pub struct Cloudflared;

impl Provider for Cloudflared {
    fn name(&self) -> &'static str {
        DEFAULT_PROVIDER
    }

    fn ensure_present(&self, paths: &PandoPaths) -> Result<()> {
        ensure_runnable(
            &cloudflared_program(paths),
            &paths.home.join("bin").join(DEFAULT_PROVIDER),
        )
    }

    fn start(
        &self,
        paths: &PandoPaths,
        name: &str,
        host: &str,
        local_port: u16,
        spawned: &dyn Fn(process::Group),
    ) -> Result<TunnelSpawn> {
        start_tunnel_with(paths, name, host, local_port, spawned)
    }
}

/// The cloudflared executable pando runs.
///
/// `<home>/bin/cloudflared` when it is there and executable, else whatever
/// `cloudflared` resolves to on PATH — the same hook `docker` has, for the
/// same two reasons: a developer whose cloudflared is not on the PATH pando
/// inherits has somewhere to put a shim, and the tests drive a fake one per
/// test home without mutating the process environment.
pub fn cloudflared_program(paths: &PandoPaths) -> PathBuf {
    let shim = paths.home.join("bin").join(DEFAULT_PROVIDER);
    if is_executable(&shim) {
        return shim;
    }
    PathBuf::from(DEFAULT_PROVIDER)
}

/// Refuses when the provider cannot be run, naming both ways of fixing it.
///
/// Split from [`Provider::ensure_present`] so the refusal is testable on a
/// machine that does have cloudflared installed.
fn ensure_runnable(program: &Path, shim: &Path) -> Result<()> {
    if program_is_runnable(program) {
        return Ok(());
    }
    bail!(
        "cloudflared is not installed — install it with {}, or put a shim at {}",
        crate::catalog::tools::how_to_get("cloudflared").unwrap_or("its own package"),
        shim.display()
    )
}

/// Whether the program can be run: a shim is checked on disk, a bare name
/// is looked up the way the shell would.
fn program_is_runnable(program: &Path) -> bool {
    if program.components().count() > 1 {
        return is_executable(program);
    }
    let Ok(mut sh) = crate::platform::shell::posix() else {
        return false;
    };
    sh.arg("-c")
        .arg(format!(
            "command -v {} >/dev/null 2>&1",
            process::shell_quote(&program.to_string_lossy())
        ))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawns a quick tunnel onto `host:local_port` and waits for its public
/// URL.
pub fn start_tunnel(
    paths: &PandoPaths,
    name: &str,
    host: &str,
    local_port: u16,
) -> Result<TunnelSpawn> {
    start_tunnel_with(paths, name, host, local_port, &|_| {})
}

/// [`start_tunnel`], telling `spawned` the tunnel's process group before
/// the wait.
fn start_tunnel_with(
    paths: &PandoPaths,
    name: &str,
    host: &str,
    local_port: u16,
    spawned: &dyn Fn(process::Group),
) -> Result<TunnelSpawn> {
    let log_path = paths.log_file(name, TUNNEL_LOG);
    // `spawn_detached` opens the log with O_APPEND, so after
    // share → unshare → share the dead session's URL is still in the file
    // and the parser would hand it back instead of the live one.
    truncate_log(&log_path)?;
    let config_path = paths.tunnel_config_file();
    ensure_tunnel_config(&config_path)?;

    // `--no-autoupdate` keeps the background updater from restarting a
    // detached child mid-session. `--output json` asks for the machine
    // readable log — one object per line, with the published URL in a
    // field rather than loose in a sentence — and pins it against a
    // `TUNNEL_LOG_OUTPUT` in the inherited environment; a cloudflared too
    // old to honour it falls back to the banner parser. `--config` shadows
    // the user's own.
    let shell_cmd = format!(
        "exec {program} tunnel --no-autoupdate --output json --config {config} \
         --url http://{host}:{local_port}",
        program = process::shell_quote(&cloudflared_program(paths).to_string_lossy()),
        config = process::shell_quote(&config_path.to_string_lossy()),
    );
    // Not the worktree: a tunnel holding a directory open is one more
    // reason `rm` cannot remove it, and the tunnel needs nothing from there.
    let cwd = std::env::temp_dir();
    let spawn = process::spawn_detached(SpawnOptions {
        shell_cmd: &shell_cmd,
        cwd: &cwd,
        log_file: &log_path,
        env: &[],
        status_file: None,
    })
    .context("spawn cloudflared")?;
    spawned(spawn.pgid);

    match await_url(spawn.pid, &log_path) {
        Ok(published) => Ok(TunnelSpawn {
            pid: spawn.pid,
            pgid: spawn.pgid,
            public_url: published.url,
            log_path,
            unconnected: published.unconnected,
        }),
        Err(e) => {
            // Never leave the child behind on the way out: nothing has
            // recorded it yet, so this is the last moment anything knows
            // its process group.
            let _ = process::stop(spawn.pgid, STOP_GRACE);
            Err(e)
        }
    }
}

/// What the wait for a tunnel found.
#[derive(Debug)]
struct Published {
    url: String,
    /// The log's tail, when the deadline passed with no edge connection.
    unconnected: Option<String>,
}

/// Polls the log until a URL appears and the edge has a connection for
/// it, the process dies, or the timeout passes.
fn await_url(pid: u32, log_path: &Path) -> Result<Published> {
    await_url_until(pid, log_path, Instant::now() + READY_TIMEOUT)
}

/// [`await_url`] with the deadline given, so a test can drive the timeout
/// branch without sitting through it.
///
/// cloudflared prints its URL as soon as the quick-tunnel API answers,
/// before it has a single connection to the edge that serves it. On a
/// network that blocks the edge the URL never answers, and cloudflared
/// retries for a minute or two before it exits — so a tunnel that exits
/// first fails the share, with the tail that says why.
///
/// A deadline passed with the tunnel still trying is not a failure, since
/// a slow edge is not a dead one: the URL comes back with the tail, for
/// the caller to pass on.
fn await_url_until(pid: u32, log_path: &Path, deadline: Instant) -> Result<Published> {
    await_url_while(|| process::is_alive(pid), log_path, deadline)
}

/// [`await_url_until`], asking `is_alive` whether the tunnel still runs, so
/// a test can have it write its last lines and exit between two looks.
fn await_url_while(
    is_alive: impl Fn() -> bool,
    log_path: &Path,
    deadline: Instant,
) -> Result<Published> {
    loop {
        // Liveness first, then the log. A tunnel seen dead has written
        // all it ever will, so the log read after that is the whole of
        // what its exit means, however much of it was written between two
        // looks: one that published, failed to dial and exited while this
        // loop slept never connected, whether its URL is first seen with
        // it alive or with it gone. Read the other way round, the log can
        // predate the very lines that decide it.
        let alive = is_alive();
        let url = parse_url_from_log(log_path);
        let connected = url.is_some() && log_says_connected(log_path);
        match (url, alive) {
            (None, false) => bail!(
                "cloudflared exited before publishing a URL — tail: {}",
                tail_log(log_path)
            ),
            (Some(url), false) if connected => bail!(
                "cloudflared published {url} and then exited — tail: {}",
                tail_log(log_path)
            ),
            (Some(url), false) => bail!(
                "cloudflared published {url} but never connected to Cloudflare's edge — tail: {}",
                tail_log(log_path)
            ),
            (Some(url), true) if connected => {
                return Ok(Published {
                    url,
                    unconnected: None,
                });
            }
            (Some(url), true) if Instant::now() >= deadline => {
                return Ok(Published {
                    url,
                    unconnected: Some(tail_log(log_path)),
                });
            }
            (None, true) if Instant::now() >= deadline => bail!(
                "cloudflared published no URL within {}s — tail: {}",
                READY_TIMEOUT.as_secs(),
                tail_log(log_path)
            ),
            (_, true) => std::thread::sleep(POLL_INTERVAL),
        }
    }
}

fn log_says_connected(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|content| content.lines().any(connection_registered))
        .unwrap_or(false)
}

/// Whether one log line says cloudflared registered a connection with the
/// edge: `Registered tunnel connection` in 2025.11.1, in either log
/// format, and `Connection <id> registered` in older releases.
fn connection_registered(line: &str) -> bool {
    line.contains("Registered tunnel connection")
        || (line.contains("Connection ") && line.contains(" registered"))
}

/// Takes both halves of a share down: the tunnel first, so no new request
/// reaches a proxy that is about to go, then the proxy.
///
/// Both are always signalled, whatever the first one did — a stuck
/// cloudflared must not be the reason a proxy is orphaned. The first error
/// is what the caller sees.
pub fn stop_share(record: &ShareRecord) -> Result<()> {
    stop_share_with(record, |pgid| process::stop(pgid, STOP_GRACE))
}

/// [`stop_share`] with the signal injected, so a test can watch the order
/// without real process groups.
pub fn stop_share_with(
    record: &ShareRecord,
    stop: impl Fn(process::Group) -> Result<()>,
) -> Result<()> {
    let tunnel = stop(record.tunnel_pgid);
    let proxy = match record.proxy_pgid {
        Some(pgid) => stop(pgid),
        None => Ok(()),
    };
    tunnel.and(proxy)
}

/// Empties the log before a share, creating nothing that was not there.
fn truncate_log(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create log dir {}", parent.display()))?;
    }
    if path.exists() {
        std::fs::write(path, b"")
            .with_context(|| format!("truncate tunnel log {}", path.display()))?;
    }
    Ok(())
}

/// Writes pando's own cloudflared config once, and never again: a developer
/// who edited it — to add a metrics endpoint, say — keeps their edit.
fn ensure_tunnel_config(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if !path.exists() {
        std::fs::write(path, EMPTY_TUNNEL_CONFIG)
            .with_context(|| format!("write tunnel config {}", path.display()))?;
    }
    Ok(())
}

fn parse_url_from_log(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    content.lines().find_map(published_url)
}

/// The URL a provider says it *published*, on one log line — never a URL it
/// merely mentioned.
///
/// Two shapes, because `--output json` is what pando asks for and a
/// cloudflared too old to honour it logs the human format instead. The
/// structured form is tried first: it keeps the URL cloudflared published
/// in a field of its own, away from the URLs it names in errors.
fn published_url(line: &str) -> Option<String> {
    url_from_json_line(line).or_else(|| bordered_url(line))
}

/// The URL in one `--output json` log line.
///
/// `url`/`hostname` first, so a cloudflared that ever names the URL
/// outright is read straight. 2025.11.1 does not: it logs the same ASCII
/// box the human format shows, one line per `message`, so every string
/// field is offered to the banner rule — which means the `error` field of
/// a failed request, whose URL is not bordered, cannot be mistaken for one.
fn url_from_json_line(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    let object = value.as_object()?;
    for key in ["url", "hostname"] {
        if let Some(text) = object.get(key).and_then(|v| v.as_str())
            && let Some(url) = quick_tunnel_url(text.trim())
        {
            return Some(url);
        }
    }
    object
        .values()
        .filter_map(|v| v.as_str())
        .find_map(bordered_url)
}

/// The URL inside cloudflared's own banner, which is an ASCII box: the URL
/// is always alone between two pipes.
///
/// Anchoring on the box is the difference between "cloudflared published
/// this" and "cloudflared wrote this URL down". Its failure to *request* a
/// quick tunnel renders as `Post "https://api.trycloudflare.com/tunnel":
/// …`, which is a sentence, not a box.
fn bordered_url(line: &str) -> Option<String> {
    let open = line.find('|')?;
    let close = line.rfind('|')?;
    if close <= open {
        return None;
    }
    let inner = line[open + 1..close].trim();
    if inner.is_empty() || inner.contains(char::is_whitespace) {
        return None;
    }
    quick_tunnel_url(inner)
}

/// A quick-tunnel URL, host only, or nothing.
///
/// Everything past the host is dropped, so a future cloudflared that prints
/// a path or a query string still yields a URL that can be opened — and a
/// token in a query string never travels with the URL pando hands out.
fn quick_tunnel_url(candidate: &str) -> Option<String> {
    let rest = candidate.strip_prefix(URL_SCHEME)?;
    let host = match rest.find(['/', '?', '#']) {
        Some(end) => &rest[..end],
        None => rest,
    };
    if host.len() <= URL_HOST.len() || !host.ends_with(URL_HOST) {
        return None;
    }
    // The host cloudflared *asks* for a tunnel. It is in the error an
    // offline or rate-limited run logs, and it is never what was published.
    if host.eq_ignore_ascii_case(API_HOST) {
        return None;
    }
    Some(format!("{URL_SCHEME}{host}"))
}

fn tail_log(path: &Path) -> String {
    let Ok(content) = std::fs::read_to_string(path) else {
        return "(no log)".to_string();
    };
    let last: Vec<&str> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(TAIL_LINES)
        .collect();
    if last.is_empty() {
        return "(empty log)".to_string();
    }
    last.into_iter().rev().collect::<Vec<_>>().join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectRef;
    use crate::testutil::{
        FAKE_REGISTERED, FAKE_TUNNEL_URL, fake_cloudflared_failing, fake_cloudflared_publishing,
        fake_cloudflared_silent, fake_cloudflared_still_dialling,
        fake_cloudflared_unreachable_edge, wait_until,
    };
    use chrono::Utc;
    use std::sync::Mutex;
    use tempfile::{TempDir, tempdir};

    struct Fx {
        _dir: TempDir,
        paths: PandoPaths,
    }

    fn fixture() -> Fx {
        let dir = tempdir().unwrap();
        let root = dir.path().join("acme-shop");
        std::fs::create_dir_all(&root).unwrap();
        let paths = PandoPaths::new(
            dir.path().join("pando-home"),
            ProjectRef::from_root(&root).unwrap(),
        );
        Fx { paths, _dir: dir }
    }

    fn share_record(tunnel_pgid: i32, proxy_pgid: Option<i32>) -> ShareRecord {
        ShareRecord {
            tunnel_pid: tunnel_pgid as u32,
            tunnel_pgid: process::Group::from_raw(tunnel_pgid),
            public_url: "https://x.trycloudflare.com".into(),
            local_port: 17000,
            started_at: Utc::now(),
            log_path: PathBuf::from("tunnel.log"),
            proxy_pid: proxy_pgid.map(|p| p as u32),
            proxy_pgid: proxy_pgid.map(process::Group::from_raw),
            proxy_port: proxy_pgid.map(|_| 17001),
        }
    }

    #[test]
    fn the_isolated_config_is_written_under_the_home_and_defines_no_ingress() {
        let fx = fixture();
        let config = fx.paths.tunnel_config_file();
        assert!(config.starts_with(&fx.paths.home));
        assert!(!config.exists());

        ensure_tunnel_config(&config).unwrap();

        let content = std::fs::read_to_string(&config).unwrap();
        assert!(content.contains("no-autoupdate: true"), "{content}");
        assert!(
            !content.contains("ingress:"),
            "the whole point of this file is that it defines no ingress: {content}"
        );
    }

    #[test]
    fn the_isolated_config_is_never_rewritten() {
        let fx = fixture();
        let config = fx.paths.tunnel_config_file();
        let edited = "# mine\nno-autoupdate: true\nmetrics: 127.0.0.1:8081\n";
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, edited).unwrap();

        ensure_tunnel_config(&config).unwrap();
        ensure_tunnel_config(&config).unwrap();

        assert_eq!(std::fs::read_to_string(&config).unwrap(), edited);
    }

    #[test]
    fn a_url_is_extracted_from_the_line_cloudflared_really_prints() {
        assert_eq!(
            published_url(
                "2026-09-21T12:00:00Z INF |  https://quiet-aspen-grove-example.trycloudflare.com  |"
            ),
            Some("https://quiet-aspen-grove-example.trycloudflare.com".into())
        );
    }

    // The whole of finding 2. `api.trycloudflare.com` is the host
    // cloudflared *asks* for a quick tunnel, and a run that is offline or
    // rate limited logs it inside a Go `*url.Error`. Handing it back turns
    // a failed share into a successful one pointing at Cloudflare's API.
    #[test]
    fn the_quick_tunnel_api_host_is_never_a_published_url() {
        assert_eq!(
            published_url(
                "2026-09-21T12:00:00Z ERR failed to request quick Tunnel \
                 error=\"Post \\\"https://api.trycloudflare.com/tunnel\\\": dial tcp: lookup\""
            ),
            None
        );
        assert_eq!(
            published_url(
                "{\"level\":\"error\",\"error\":\"Post \\\"https://api.trycloudflare.com/tunnel\\\": \
                 dial tcp\",\"message\":\"failed to request quick Tunnel\"}"
            ),
            None,
            "and not in the structured form either"
        );
        assert_eq!(
            published_url("INF |  https://api.trycloudflare.com  |"),
            None,
            "not even bordered like the real banner"
        );
    }

    // The banner is an ASCII box cloudflared builds itself, so the URL is
    // always between two pipes. A URL anywhere else on a line is prose, an
    // error, or a request cloudflared is making — never what it published.
    #[test]
    fn a_url_outside_the_bordered_banner_is_not_a_published_url() {
        assert_eq!(
            published_url("Visit https://abc-123-xyz.trycloudflare.com to try it"),
            None
        );
        assert_eq!(
            published_url("INF connecting to https://abc.trycloudflare.com now"),
            None
        );
    }

    #[test]
    fn a_url_is_cut_at_the_host_so_nothing_after_it_is_carried() {
        assert_eq!(
            published_url("INF | https://named.trycloudflare.com/foo?token=secret |"),
            Some("https://named.trycloudflare.com".into()),
            "a path or query must never travel with the URL pando hands out"
        );
    }

    #[test]
    fn lines_without_a_quick_tunnel_url_yield_nothing() {
        assert_eq!(published_url("INF Starting tunnel..."), None);
        assert_eq!(published_url("INF | https://example.com |"), None);
        assert_eq!(published_url(""), None);
    }

    // `--output json` wraps every log line in an object; 2025.11.1 puts the
    // banner in `message`, and a later one may name the URL outright.
    #[test]
    fn a_url_is_read_out_of_a_json_log_line() {
        assert_eq!(
            published_url(
                "{\"level\":\"info\",\"time\":\"2026-09-21T12:00:00Z\",\
                 \"message\":\"|  https://json-shaped.trycloudflare.com  |\"}"
            ),
            Some("https://json-shaped.trycloudflare.com".into())
        );
        assert_eq!(
            published_url("{\"level\":\"info\",\"url\":\"https://a-field.trycloudflare.com\"}"),
            Some("https://a-field.trycloudflare.com".into()),
            "a future cloudflared that names the URL is read straight"
        );
        assert_eq!(
            published_url("{\"level\":\"info\",\"message\":\"Requesting new quick Tunnel\"}"),
            None
        );
    }

    #[test]
    fn a_provider_that_logs_json_publishes_its_url_just_the_same() {
        let fx = fixture();
        crate::testutil::fake_cloudflared_json_publishing(&fx.paths.home);

        let spawn = start_tunnel(&fx.paths, "feat+one", DEV_SERVER_HOST, 17000).unwrap();
        let _ = process::stop(spawn.pgid, STOP_GRACE);
        assert_eq!(spawn.public_url, FAKE_TUNNEL_URL);
    }

    #[test]
    fn a_provider_whose_request_for_a_tunnel_fails_is_not_a_published_url() {
        let fx = fixture();
        crate::testutil::fake_cloudflared_api_error(&fx.paths.home);

        let err = start_tunnel(&fx.paths, "feat+one", DEV_SERVER_HOST, 17000).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("exited before publishing") || message.contains("no URL"),
            "a failed request must fail the share: {message}"
        );
        assert!(
            message.contains("failed to request quick Tunnel"),
            "with the provider's own complaint in it: {message}"
        );
    }

    #[test]
    fn the_log_is_parsed_and_a_missing_or_urlless_file_says_so() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("tunnel.log");
        assert_eq!(parse_url_from_log(&log), None, "missing file");

        std::fs::write(&log, "INF Requesting new quick Tunnel...\n").unwrap();
        assert_eq!(parse_url_from_log(&log), None, "no URL yet");

        std::fs::write(
            &log,
            "INF requesting...\nINF |  https://aaa-bbb.trycloudflare.com  |\n",
        )
        .unwrap();
        assert_eq!(
            parse_url_from_log(&log),
            Some("https://aaa-bbb.trycloudflare.com".into())
        );
    }

    #[test]
    fn the_log_is_truncated_before_a_share_and_never_created() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("logs").join("tunnel.log");
        truncate_log(&log).unwrap();
        assert!(!log.exists(), "truncate must not create the file");

        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, b"INF | https://stale.trycloudflare.com |\n").unwrap();
        truncate_log(&log).unwrap();

        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
        assert_eq!(
            parse_url_from_log(&log),
            None,
            "a stale URL must not survive into the next share"
        );
    }

    #[test]
    fn a_tunnel_publishes_its_url_and_is_stoppable() {
        let fx = fixture();
        fake_cloudflared_publishing(&fx.paths.home);

        let spawn = start_tunnel(&fx.paths, "feat+one", DEV_SERVER_HOST, 17000).unwrap();
        assert_eq!(spawn.public_url, FAKE_TUNNEL_URL);
        assert_eq!(spawn.unconnected, None, "it registered a connection");
        assert_eq!(spawn.log_path, fx.paths.log_file("feat+one", TUNNEL_LOG));
        assert!(process::is_alive(spawn.pid), "the tunnel must still be up");

        process::stop(spawn.pgid, STOP_GRACE).unwrap();
        assert!(wait_until(Duration::from_secs(5), || !process::is_alive(
            spawn.pid
        )));
    }

    #[test]
    fn the_log_written_is_the_worktrees_own_tunnel_log() {
        let fx = fixture();
        fake_cloudflared_publishing(&fx.paths.home);
        let spawn = start_tunnel(&fx.paths, "feat+one", DEV_SERVER_HOST, 17000).unwrap();
        let _ = process::stop(spawn.pgid, STOP_GRACE);

        let log = std::fs::read_to_string(fx.paths.log_file("feat+one", TUNNEL_LOG)).unwrap();
        assert!(log.contains("trycloudflare.com"), "{log}");
        assert!(
            fx.paths
                .log_file("feat+one", TUNNEL_LOG)
                .starts_with(&fx.paths.home),
            "logs live under the home, never in the repository"
        );
    }

    /// The fake in the home, spawned as `start_tunnel` spawns it but not
    /// waited on, so the test drives the wait.
    fn spawn_fake(fx: &Fx) -> (process::SpawnResult, PathBuf) {
        let log = fx.paths.log_file("feat+one", TUNNEL_LOG);
        truncate_log(&log).unwrap();
        let spawn = process::spawn_detached(SpawnOptions {
            shell_cmd: &format!(
                "exec {}",
                process::shell_quote(&cloudflared_program(&fx.paths).to_string_lossy())
            ),
            cwd: &std::env::temp_dir(),
            log_file: &log,
            env: &[],
            status_file: None,
        })
        .unwrap();
        (spawn, log)
    }

    /// [`spawn_fake`], once its log says `said`: the timeout is 30s, far
    /// too long for a test to sit through. The wait for `said` has the
    /// budget the product gives a provider to say anything, since a login
    /// shell on a loaded machine can take seconds to start, and it returns
    /// the moment the line is there.
    fn spawn_unwaited(fx: &Fx, said: &str) -> (process::SpawnResult, PathBuf) {
        let (spawn, log) = spawn_fake(fx);
        assert!(wait_until(READY_TIMEOUT, || {
            std::fs::read_to_string(&log)
                .map(|s| s.contains(said))
                .unwrap_or(false)
        }));
        (spawn, log)
    }

    // The wait is driven directly with a child that never publishes.
    #[test]
    fn a_provider_that_never_publishes_fails_with_the_log_tail() {
        let fx = fixture();
        fake_cloudflared_silent(&fx.paths.home);
        let (spawn, log) = spawn_unwaited(&fx, "Requesting");

        // A deadline in the past, so the loop reports the timeout it would
        // report thirty seconds from now.
        let err = await_url_until(spawn.pid, &log, Instant::now()).unwrap_err();
        let _ = process::stop(spawn.pgid, STOP_GRACE);

        let message = format!("{err:#}");
        assert!(message.contains("no URL"), "{message}");
        assert!(
            message.contains("Requesting new quick Tunnel"),
            "the tail of the log is the whole diagnosis: {message}"
        );
    }

    // cloudflared prints its URL before it has one connection to the edge
    // that serves it. On a network that blocks the edge the URL never
    // answers, and the share was reported as a success all the same.
    #[test]
    fn a_tunnel_that_publishes_and_never_reaches_the_edge_fails_with_the_log_tail() {
        let fx = fixture();
        fake_cloudflared_unreachable_edge(&fx.paths.home);
        let err = start_tunnel(&fx.paths, "feat+one", DEV_SERVER_HOST, 17000).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("never connected"), "{message}");
        assert!(
            message.contains("Failed to dial"),
            "the dial error is the diagnosis: {message}"
        );
    }

    // The same tunnel, first looked at once it had already gone: what a
    // poller the machine did not run for half a second sees. It was
    // reported as one that published and then exited — true, and no help,
    // since the dial error in its tail explains a tunnel that never
    // reached the edge.
    #[test]
    fn a_tunnel_first_seen_after_it_exited_still_never_connected() {
        let fx = fixture();
        fake_cloudflared_unreachable_edge(&fx.paths.home);
        let (spawn, log) = spawn_fake(&fx);
        // The budget the wait itself gives a provider, since a start on a
        // loaded machine can take seconds: this waits for the exit, and
        // the fake always exits.
        assert!(wait_until(READY_TIMEOUT, || !process::is_alive(spawn.pid)));

        let err = await_url_until(spawn.pid, &log, Instant::now() + READY_TIMEOUT).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("never connected"), "{message}");
        assert!(message.contains("Failed to dial"), "{message}");
    }

    // Liveness is asked before the log is read, so a tunnel that writes its
    // last lines and exits between the two looks is judged on those lines.
    // Read the other way round, the log would predate them: the first case
    // would say it never connected, the second that it never published.
    #[test]
    fn a_tunnel_that_writes_its_last_lines_as_it_exits_is_judged_on_them() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("cloudflared.log");
        let append = |line: &str| {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
            writeln!(file, "{line}").unwrap();
        };
        let banner = format!("INF |  {FAKE_TUNNEL_URL}  |");

        std::fs::write(&log, format!("{banner}\n")).unwrap();
        let connects_and_exits = || {
            append(FAKE_REGISTERED);
            false
        };
        let err =
            await_url_while(connects_and_exits, &log, Instant::now() + READY_TIMEOUT).unwrap_err();
        assert!(format!("{err:#}").contains("and then exited"), "{err:#}");

        std::fs::write(&log, "").unwrap();
        let publishes_and_exits = || {
            append(&banner);
            false
        };
        let err =
            await_url_while(publishes_and_exits, &log, Instant::now() + READY_TIMEOUT).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains(&format!("published {FAKE_TUNNEL_URL}")),
            "{message}"
        );
    }

    // And one that did connect before it went is told apart by the same
    // look, not by which of the two the wait happened to see first.
    #[test]
    fn a_tunnel_that_connected_and_then_exited_says_so() {
        let fx = fixture();
        crate::testutil::fake_cloudflared(
            &fx.paths.home,
            &format!("echo 'INF |  {FAKE_TUNNEL_URL}  |'\necho '{FAKE_REGISTERED}'\nexit 1\n"),
        );
        let (spawn, log) = spawn_fake(&fx);
        assert!(wait_until(READY_TIMEOUT, || !process::is_alive(spawn.pid)));

        let err = await_url_until(spawn.pid, &log, Instant::now() + READY_TIMEOUT).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("and then exited"), "{message}");
        assert!(!message.contains("never connected"), "{message}");
    }

    // A slow edge is not a dead one: the URL comes back, with the reason
    // it may not answer yet.
    #[test]
    fn a_tunnel_still_dialling_at_the_deadline_publishes_with_the_reason() {
        let fx = fixture();
        fake_cloudflared_still_dialling(&fx.paths.home);
        let (spawn, log) = spawn_unwaited(&fx, "Retrying");

        let published = await_url_until(spawn.pid, &log, Instant::now());
        let _ = process::stop(spawn.pgid, STOP_GRACE);

        let published = published.unwrap();
        assert_eq!(published.url, FAKE_TUNNEL_URL);
        let tail = published.unconnected.expect("it never connected");
        assert!(tail.contains("Retrying connection"), "{tail}");
    }

    #[test]
    fn a_connection_is_registered_in_every_form_cloudflared_logs_it() {
        assert!(connection_registered(FAKE_REGISTERED));
        assert!(connection_registered(
            "{\"level\":\"info\",\"connIndex\":0,\"message\":\"Registered tunnel connection\"}"
        ));
        assert!(
            connection_registered(
                "INF Connection 25e2ee72-4f2c-4e5b-9b3c-0c0b1b7b3f1e registered connIndex=0 \
                 location=LHR"
            ),
            "an older release"
        );
        for line in [
            "INF Retrying connection in up to 2s",
            "ERR Failed to dial a quic connection",
            "ERR Unable to establish connection.",
            "ERR Register tunnel error from server side",
            "INF |  https://x.trycloudflare.com  |",
        ] {
            assert!(!connection_registered(line), "{line}");
        }
    }

    #[test]
    fn a_provider_that_exits_before_publishing_fails_with_the_log_tail() {
        let fx = fixture();
        fake_cloudflared_failing(&fx.paths.home);
        let err = start_tunnel(&fx.paths, "feat+one", DEV_SERVER_HOST, 17000).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("exited before publishing"), "{message}");
        assert!(message.contains("Too Many Requests"), "{message}");
    }

    #[test]
    fn stop_share_signals_the_tunnel_before_the_proxy() {
        let order = Mutex::new(Vec::new());
        let record = share_record(4242, Some(8484));
        stop_share_with(&record, |pgid| {
            order.lock().unwrap().push(pgid.as_raw());
            Ok(())
        })
        .unwrap();
        assert_eq!(
            order.into_inner().unwrap(),
            vec![4242, 8484],
            "the tunnel goes first so no new request reaches a proxy that is about to die"
        );
    }

    #[test]
    fn stop_share_signals_the_proxy_even_when_the_tunnel_refuses_to_die() {
        let signalled = Mutex::new(Vec::new());
        let record = share_record(4242, Some(8484));
        let result = stop_share_with(&record, |pgid| {
            signalled.lock().unwrap().push(pgid.as_raw());
            if pgid.as_raw() == 4242 {
                bail!("stuck")
            } else {
                Ok(())
            }
        });
        assert!(result.is_err(), "the caller still learns the tunnel stuck");
        assert_eq!(
            signalled.into_inner().unwrap(),
            vec![4242, 8484],
            "a stuck tunnel must never be the reason a proxy is orphaned"
        );
    }

    #[test]
    fn stop_share_without_a_proxy_signals_only_the_tunnel() {
        let signalled = Mutex::new(Vec::new());
        let record = share_record(4242, None);
        stop_share_with(&record, |pgid| {
            signalled.lock().unwrap().push(pgid.as_raw());
            Ok(())
        })
        .unwrap();
        assert_eq!(signalled.into_inner().unwrap(), vec![4242]);
    }

    // The `--config` is the whole reason a quick tunnel answers at all on a
    // machine whose owner already runs a named tunnel with catch-all
    // ingress rules.
    #[test]
    fn the_tunnel_shadows_the_user_config_and_targets_the_port_it_was_given() {
        let fx = fixture();
        fake_cloudflared_publishing(&fx.paths.home);
        let spawn = start_tunnel(&fx.paths, "feat+one", DEV_SERVER_HOST, 17042).unwrap();
        let _ = process::stop(spawn.pgid, STOP_GRACE);

        let log = std::fs::read_to_string(&spawn.log_path).unwrap();
        assert!(
            log.contains("--url http://localhost:17042"),
            "a name both loopbacks answer to, so a server on `[::1]` alone is reached: {log}"
        );
        assert!(
            log.contains(&format!(
                "--config {}",
                fx.paths.tunnel_config_file().display()
            )),
            "the user's own ~/.cloudflared/config.yml must be shadowed: {log}"
        );
        assert!(log.contains("--no-autoupdate"), "{log}");
        assert!(
            log.contains("--output json"),
            "the machine-readable log is what the URL is parsed from: {log}"
        );
    }

    #[test]
    fn the_program_is_the_home_shim_when_there_is_one() {
        let fx = fixture();
        assert_eq!(
            cloudflared_program(&fx.paths),
            PathBuf::from(DEFAULT_PROVIDER),
            "with no shim, whatever the shell would run"
        );
        fake_cloudflared_publishing(&fx.paths.home);
        assert_eq!(
            cloudflared_program(&fx.paths),
            fx.paths.home.join("bin").join(DEFAULT_PROVIDER)
        );
    }

    // Not through `ensure_present`: this machine has cloudflared installed,
    // so the only honest way to test the refusal is to hand it a program
    // that is definitely not there.
    #[test]
    fn a_missing_provider_is_refused_with_an_install_hint() {
        let fx = fixture();
        let shim = fx.paths.home.join("bin").join(DEFAULT_PROVIDER);
        let err = ensure_runnable(Path::new("/nonexistent/cloudflared"), &shim).unwrap_err();

        let message = format!("{err:#}");
        assert!(message.contains("not installed"), "{message}");
        assert!(message.contains("brew install cloudflared"), "{message}");
        assert!(
            message.contains(&shim.display().to_string()),
            "the message must say where a shim would go: {message}"
        );
    }

    #[test]
    fn a_shim_that_is_not_executable_is_not_installed() {
        let fx = fixture();
        let bin = fx.paths.home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let shim = bin.join(DEFAULT_PROVIDER);
        std::fs::write(&shim, "#!/bin/sh\n").unwrap();
        assert!(!program_is_runnable(&shim), "a file is not a program");
        assert!(ensure_runnable(&shim, &shim).is_err());

        fake_cloudflared_publishing(&fx.paths.home);
        Cloudflared.ensure_present(&fx.paths).unwrap();
    }

    #[test]
    fn only_cloudflared_is_a_provider_and_a_typo_is_refused() {
        assert_eq!(provider_for(None).unwrap().name(), DEFAULT_PROVIDER);
        assert_eq!(
            provider_for(Some("cloudflared")).unwrap().name(),
            DEFAULT_PROVIDER
        );
        // `map` first: a boxed trait object has no `Debug` for `unwrap_err`.
        let err = provider_for(Some("ngrok")).map(|_| ()).unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("ngrok"), "{message}");
        assert!(message.contains("cloudflared"), "{message}");
    }

    #[test]
    fn a_shim_path_with_a_space_in_it_is_quoted() {
        assert_eq!(
            process::shell_quote("/tmp/x y/cloudflared"),
            "'/tmp/x y/cloudflared'"
        );
        assert_eq!(process::shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn the_tail_says_so_when_there_is_nothing_to_say() {
        let dir = tempdir().unwrap();
        assert_eq!(tail_log(&dir.path().join("nope.log")), "(no log)");
        let empty = dir.path().join("empty.log");
        std::fs::write(&empty, "\n\n").unwrap();
        assert_eq!(tail_log(&empty), "(empty log)");
    }
}
