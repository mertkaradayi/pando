//! The local proxy that sits between a tunnel and a dev server and injects
//! a `Cookie` header, so a visitor lands already authenticated.
//!
//! It runs as `pando __share-proxy`, a hidden subcommand of the same binary,
//! spawned detached like any other process. Two rules govern it:
//!
//! - **The cookie travels by environment, never by argv.** Anything in argv
//!   is in `ps` output, readable by every process on the machine.
//! - **Nothing it prints carries the cookie.** Its log is a log like any
//!   other; a credential in it would outlive the share.
//!
//! It is a hand-written HTTP/1.1 header rewriter, ported from the origin
//! tool where it has run in anger. It is deliberately not extended.

use anyhow::{Context, Result, bail};
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::paths::PandoPaths;
use crate::ports;
use crate::process::{self, SpawnOptions};

/// The log source the proxy writes to. Reserved in [`crate::paths`].
pub const PROXY_LOG: &str = "proxy";

/// How the cookie reaches the proxy. Never an argument.
pub const ENV_COOKIE: &str = "PANDO_SHARE_COOKIE";

/// The hidden subcommand that runs the proxy in this same binary.
pub const SUBCOMMAND: &str = "__share-proxy";

/// The one address the proxy listens on, and so the one a tunnel onto it
/// is pointed at. Not `localhost`: cloudflared tries `[::1]` first for
/// that name, and whatever binds `[::1]` on the proxy's port — which `ps`
/// shows — would be handed every visitor.
pub const LISTEN_HOST: &str = "127.0.0.1";

/// How long a spawned proxy has to start listening. Binding is the first
/// thing it does, so this is one exec on a loaded machine.
pub const LISTEN_TIMEOUT: Duration = Duration::from_secs(5);
const LISTEN_POLL: Duration = Duration::from_millis(50);

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a client may take to finish sending its request headers.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the upstream may take to start answering. Only until its
/// first byte: a server-sent event stream or a long poll is silent for as
/// long as it likes after that.
const UPSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(60);
/// A request whose headers are bigger than this is refused rather than
/// buffered: the proxy holds the whole header block in memory.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// A running proxy, before it is written to state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpawn {
    pub pid: u32,
    pub pgid: process::Group,
    pub listen_port: u16,
    pub log_path: PathBuf,
}

/// Spawns the proxy as a detached `pando __share-proxy`.
///
/// The same binary that is running now, so a `cargo run`, a
/// `./target/debug/pando`, and a PATH-installed `pando` all work without
/// anything having to know where it lives.
pub fn spawn(
    paths: &PandoPaths,
    name: &str,
    listen_port: u16,
    upstream_port: u16,
    cookie: &str,
) -> Result<ProxySpawn> {
    let exe = std::env::current_exe().context("resolve the running pando executable")?;
    spawn_with(paths, name, listen_port, upstream_port, cookie, &exe)
}

/// [`spawn`] with the executable given, so a test can point it at a binary
/// of its choosing.
pub fn spawn_with(
    paths: &PandoPaths,
    name: &str,
    listen_port: u16,
    upstream_port: u16,
    cookie: &str,
    exe: &Path,
) -> Result<ProxySpawn> {
    let log_path = paths.log_file(name, PROXY_LOG);
    // `spawn_detached` opens the log with O_APPEND, so without this every
    // share → unshare → share cycle keeps the dead sessions' bytes.
    truncate_log(&log_path)?;
    let spawn = process::spawn_detached(SpawnOptions {
        shell_cmd: &proxy_command(exe, listen_port, upstream_port),
        // Not the worktree: the proxy needs nothing from it, and a process
        // holding it open is one more reason `rm` cannot remove it.
        cwd: &std::env::temp_dir(),
        log_file: &log_path,
        env: &proxy_env(cookie),
        status_file: None,
    })
    .context("spawn the share proxy")?;
    Ok(ProxySpawn {
        pid: spawn.pid,
        pgid: spawn.pgid,
        listen_port,
        log_path,
    })
}

/// Waits until a spawned proxy is listening, and fails with the last line
/// of its log when it exits first or never binds.
///
/// [`spawn`] returns before the child has bound anything, so a proxy that
/// could not — its port taken, its binary replaced — was found only by the
/// first visitor, through a tunnel already published onto it.
///
/// Alive *and* listening, because the port can answer for a proxy that is
/// about to exit: another share of the same worktree started a moment
/// earlier holds it, and this one's bind fails a moment later. That one
/// is caught again when the share is recorded.
pub fn await_listening(proxy: &ProxySpawn) -> Result<()> {
    let deadline = Instant::now() + LISTEN_TIMEOUT;
    loop {
        let listening = ports::something_is_listening(proxy.listen_port);
        if !process::is_alive(proxy.pid) {
            bail!(
                "the share proxy exited before it was listening — {}",
                last_words(proxy)
            );
        }
        if listening {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "the share proxy was not listening on port {} within {}s — {}",
                proxy.listen_port,
                LISTEN_TIMEOUT.as_secs(),
                last_words(proxy)
            );
        }
        thread::sleep(LISTEN_POLL);
    }
}

/// The last line a proxy logged, for an error about it. Never the cookie:
/// nothing the proxy prints carries it.
pub fn last_words(proxy: &ProxySpawn) -> String {
    std::fs::read_to_string(&proxy.log_path)
        .ok()
        .and_then(|log| {
            log.lines()
                .rev()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("nothing in {}", proxy.log_path.display()))
}

/// The command line the proxy is spawned with. Ports only — the cookie is
/// in the environment, and this string ends up in `ps`.
fn proxy_command(exe: &Path, listen_port: u16, upstream_port: u16) -> String {
    format!(
        "exec {exe} {SUBCOMMAND} --listen {listen_port} --upstream {upstream_port}",
        exe = process::shell_quote(&exe.to_string_lossy()),
    )
}

/// The environment the proxy is spawned with: the cookie, and nothing else.
fn proxy_env(cookie: &str) -> Vec<(String, String)> {
    vec![(ENV_COOKIE.to_string(), cookie.to_string())]
}

/// The hidden subcommand's entry point. Binds [`LISTEN_HOST`]`:listen_port`
/// and forwards every connection to `upstream_port`, on whichever loopback
/// answers, with the cookie injected.
///
/// Loopback only. The proxy exists to be reached by a tunnel running on
/// this machine, and a bind on `0.0.0.0` would publish an
/// already-authenticated door onto the local network.
pub fn run_in_process(listen_port: u16, upstream_port: u16, cookie: &str) -> Result<()> {
    let listener = TcpListener::bind((LISTEN_HOST, listen_port))
        .with_context(|| format!("bind the share proxy on {LISTEN_HOST}:{listen_port}"))?;
    // Ports, never the cookie: this goes into the proxy log.
    eprintln!("pando share proxy: {LISTEN_HOST}:{listen_port} -> localhost:{upstream_port}");
    let cookie = cookie.to_string();
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let cookie = cookie.clone();
                thread::spawn(move || {
                    if let Err(e) = handle_connection(stream, upstream_port, &cookie) {
                        // `e` is about sockets and never holds a header.
                        eprintln!("pando share proxy: connection failed: {e:#}");
                    }
                });
            }
            Err(e) => eprintln!("pando share proxy: accept failed: {e}"),
        }
    }
    Ok(())
}

/// The deadlines one connection runs under.
#[derive(Debug, Clone, Copy)]
struct Limits {
    headers: Duration,
    upstream: Duration,
}

const LIMITS: Limits = Limits {
    headers: HEADER_READ_TIMEOUT,
    upstream: UPSTREAM_READ_TIMEOUT,
};

fn handle_connection(client: TcpStream, upstream_port: u16, cookie: &str) -> Result<()> {
    handle_connection_with(client, upstream_port, cookie, LIMITS)
}

/// [`handle_connection`] with its deadlines given, so a test can watch
/// what happens once one passes without sitting through it.
fn handle_connection_with(
    mut client: TcpStream,
    upstream_port: u16,
    cookie: &str,
    limits: Limits,
) -> Result<()> {
    // A connection that closes before sending a byte asked nothing, and
    // nothing failed: it is a port probe, `await_listening`'s among them.
    // Logged, it left a false error in every share's proxy log.
    let deadline = Instant::now() + limits.headers;
    let Some((head, leftover)) = read_until_headers_end(&mut client, deadline)? else {
        return Ok(());
    };
    // The deadline was for the headers, and they are here. A request whose
    // body is already sent — which is every GET — then has nothing more to
    // say for as long as the response takes, and a deadline left on turns
    // that ordinary silence into a failed read, a half-closed application,
    // and an SSE or streaming response cut off mid-flight.
    client.set_read_timeout(None).ok();
    let rewritten = rewrite_headers(&head, cookie);

    // Either loopback, per connection: a dev server told `localhost` is on
    // `[::1]` alone on macOS, and one that restarts may come back on the
    // other.
    let mut upstream = ports::connect_loopback(upstream_port, UPSTREAM_CONNECT_TIMEOUT)
        .with_context(|| format!("connect to the upstream on localhost:{upstream_port}"))?;
    upstream.set_read_timeout(Some(limits.upstream)).ok();

    upstream
        .write_all(rewritten.as_bytes())
        .context("write the rewritten headers")?;
    if !leftover.is_empty() {
        upstream
            .write_all(&leftover)
            .context("write the rest of the request body")?;
    }

    // `Connection: close` went upstream, so every request is its own
    // socket: pipe both ways until one side hangs up.
    let mut up_read = upstream.try_clone().context("clone the upstream socket")?;
    let mut cli_write = client.try_clone().context("clone the client socket")?;
    let back = thread::spawn(move || {
        // The deadline is for an upstream that never answers. Once it has,
        // it is cleared — on the socket, which both halves share — or an
        // event stream quiet for a minute is cut off mid-flight.
        let mut first = [0u8; 16 * 1024];
        if let Ok(n @ 1..) = up_read.read(&mut first) {
            up_read.set_read_timeout(None).ok();
            if cli_write.write_all(&first[..n]).is_ok() {
                let _ = std::io::copy(&mut up_read, &mut cli_write);
            }
        }
        let _ = cli_write.shutdown(std::net::Shutdown::Write);
    });
    // Half-closed only at a real end of file: "the client is done sending"
    // is something an `Ok` says and an `Err` does not. A copy that ended in
    // an error — a reset, a timeout — says nothing about the request, and
    // passing that on as a FIN tells the application the body is complete
    // when it may not be.
    if std::io::copy(&mut client, &mut upstream).is_ok() {
        let _ = upstream.shutdown(std::net::Shutdown::Write);
    }
    let _ = back.join();
    Ok(())
}

/// Reads until the end-of-headers marker, returning the header block and
/// whatever body bytes arrived in the same packet — or `None` when the
/// client closed without sending anything at all.
///
/// By `deadline` for the whole block, not per read: a client sending a
/// byte every little while would otherwise hold its thread for good.
fn read_until_headers_end(
    client: &mut TcpStream,
    deadline: Instant,
) -> Result<Option<(String, Vec<u8>)>> {
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("the client took too long to send its headers");
        }
        client.set_read_timeout(Some(left)).ok();
        let n = match client.read(&mut chunk) {
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                bail!("the client took too long to send its headers")
            }
            read => read.context("read from the client")?,
        };
        if n == 0 && buf.is_empty() {
            return Ok(None);
        }
        if n == 0 {
            bail!("the client closed before finishing its headers");
        }
        buf.extend_from_slice(&chunk[..n]);
        // After the read, not before it: checked first, the buffer could
        // reach the limit plus one chunk before anything objected.
        if buf.len() > MAX_HEADER_BYTES {
            bail!("the request headers were longer than {MAX_HEADER_BYTES} bytes");
        }
        if let Some((end_head, start_body)) = find_headers_end(&buf) {
            let head = String::from_utf8(buf[..end_head].to_vec())
                .context("the request headers were not valid UTF-8")?;
            return Ok(Some((head, buf[start_body..].to_vec())));
        }
    }
}

/// Where the headers end: `(end of the header block, start of the body)`.
/// The header block keeps the last header's own `\r\n` and excludes the
/// blank line. `\n\n` is accepted as well as `\r\n\r\n`, for tolerance with
/// hand-written traffic.
///
/// Whichever spelling comes *first*, not whichever is looked for first: a
/// request written with bare newlines whose body happens to contain
/// `\r\n\r\n` would otherwise be split at the body.
fn find_headers_end(buf: &[u8]) -> Option<(usize, usize)> {
    let crlf = (0..buf.len().saturating_sub(3)).find(|&i| &buf[i..i + 4] == b"\r\n\r\n");
    let lf = (0..buf.len().saturating_sub(1)).find(|&i| &buf[i..i + 2] == b"\n\n");
    match (crlf, lf) {
        (Some(c), Some(l)) if l < c => Some((l + 1, l + 2)),
        (Some(c), _) => Some((c + 2, c + 4)),
        (None, Some(l)) => Some((l + 1, l + 2)),
        (None, None) => None,
    }
}

/// Replaces the `Cookie` header with the one the auth command produced, and
/// forces `Connection: close` so each request is a fresh hop through the
/// proxy. Every other header is passed through byte for byte, obs-fold
/// continuations included.
pub fn rewrite_headers(head: &str, cookie: &str) -> String {
    let mut lines = split_header_lines(head);
    let request_line = lines.next().unwrap_or("").to_string();
    let mut kept: Vec<String> = Vec::new();
    let mut dropping = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        // An obs-fold continuation belongs to the header line above it, so
        // it leaves with that line or not at all. Keeping the continuation
        // of a dropped header folds it onto the header *before* that one —
        // a visitor's `Cookie:` line whose second half becomes part of
        // `Host:`.
        if line.starts_with([' ', '\t']) {
            if !dropping {
                kept.push(line.to_string());
            }
            continue;
        }
        let lower = line.to_ascii_lowercase();
        dropping = lower.starts_with("cookie:")
            || lower.starts_with("connection:")
            || lower.starts_with("keep-alive:")
            || lower.starts_with("proxy-connection:");
        if dropping {
            continue;
        }
        kept.push(line.to_string());
    }
    kept.push(format!("Cookie: {cookie}"));
    kept.push("Connection: close".to_string());

    let mut out = String::with_capacity(head.len() + cookie.len() + 64);
    out.push_str(&request_line);
    out.push_str("\r\n");
    for header in kept {
        out.push_str(&header);
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    out
}

fn split_header_lines(head: &str) -> impl Iterator<Item = &str> {
    head.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l))
}

fn truncate_log(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create log dir {}", parent.display()))?;
    }
    if path.exists() {
        std::fs::write(path, b"")
            .with_context(|| format!("truncate the proxy log {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::ProjectRef;
    use std::sync::mpsc;
    use tempfile::{TempDir, tempdir};

    const COOKIE: &str = "session=abc123; user=someone; flag=1";

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

    #[test]
    fn the_cookie_is_never_in_the_command_line() {
        let command = proxy_command(Path::new("/usr/local/bin/pando"), 17005, 17000);
        assert!(
            !command.contains(COOKIE) && !command.contains("session="),
            "anything in argv is in `ps` output: {command}"
        );
        assert!(command.contains(SUBCOMMAND), "{command}");
        assert!(command.contains("--listen 17005"), "{command}");
        assert!(command.contains("--upstream 17000"), "{command}");
        assert!(
            command.starts_with("exec '/usr/local/bin/pando'"),
            "the path is quoted and exec'd, so the recorded pid is the proxy: {command}"
        );
    }

    #[test]
    fn a_binary_path_with_a_space_in_it_is_quoted() {
        let command = proxy_command(Path::new("/Users/a b/pando"), 1, 2);
        assert!(command.starts_with("exec '/Users/a b/pando'"), "{command}");
    }

    #[test]
    fn the_cookie_travels_in_the_environment_under_one_name() {
        let env = proxy_env(COOKIE);
        assert_eq!(env, vec![(ENV_COOKIE.to_string(), COOKIE.to_string())]);
        assert_eq!(ENV_COOKIE, "PANDO_SHARE_COOKIE");
    }

    #[test]
    fn rewriting_replaces_the_cookie_and_forces_connection_close() {
        let head = "GET /foo HTTP/1.1\r\n\
                    Host: example\r\n\
                    Cookie: stale=1\r\n\
                    Connection: keep-alive\r\n\
                    Keep-Alive: timeout=5\r\n\
                    Proxy-Connection: keep-alive\r\n\
                    User-Agent: test/1\r\n";
        let out = rewrite_headers(head, COOKIE);

        assert!(out.starts_with("GET /foo HTTP/1.1\r\n"), "{out}");
        assert!(!out.contains("stale=1"), "the visitor's cookie goes: {out}");
        assert!(!out.to_lowercase().contains("keep-alive"), "{out}");
        assert!(!out.to_lowercase().contains("proxy-connection"), "{out}");
        assert_eq!(out.matches("Connection:").count(), 1, "{out}");
        assert!(out.contains("Connection: close\r\n"), "{out}");
        assert!(out.contains(&format!("Cookie: {COOKIE}\r\n")), "{out}");
        assert!(out.contains("User-Agent: test/1\r\n"), "{out}");
        assert!(out.ends_with("\r\n\r\n"), "{out}");
    }

    #[test]
    fn rewriting_adds_a_cookie_where_the_request_had_none() {
        let head = "GET / HTTP/1.1\r\nHost: x\r\n";
        let out = rewrite_headers(head, "k=v");
        assert!(out.contains("Cookie: k=v\r\n"), "{out}");
        assert!(out.contains("Host: x\r\n"), "{out}");
    }

    #[test]
    fn rewriting_leaves_every_other_header_byte_identical() {
        let head = "POST /api/x HTTP/1.1\r\n\
                    Host: a.b\r\n\
                    Content-Type: application/json; charset=UTF-8\r\n\
                    Content-Length: 42\r\n\
                    X-Odd-Casing: KeEp\r\n\
                    Authorization: Bearer abc.def\r\n";
        let out = rewrite_headers(head, "c=1");
        for line in [
            "POST /api/x HTTP/1.1\r\n",
            "Host: a.b\r\n",
            "Content-Type: application/json; charset=UTF-8\r\n",
            "Content-Length: 42\r\n",
            "X-Odd-Casing: KeEp\r\n",
            "Authorization: Bearer abc.def\r\n",
        ] {
            assert!(out.contains(line), "missing {line:?} in {out}");
        }
    }

    #[test]
    fn rewriting_tolerates_lf_only_line_endings_and_normalises_them() {
        let head = "GET / HTTP/1.1\nHost: x\nCookie: old=1\n";
        let out = rewrite_headers(head, "k=v");
        assert!(out.starts_with("GET / HTTP/1.1\r\n"), "{out}");
        assert!(out.contains("Host: x\r\n"), "{out}");
        assert!(!out.contains("old=1"), "{out}");
        assert!(out.contains("Cookie: k=v\r\n"), "{out}");
    }

    // Finding 7. `split_header_lines` splits on `\n`, so an obs-fold
    // continuation of a dropped header used to survive on its own — and a
    // line that begins with whitespace folds onto whatever header came
    // before it, which here is `Host`.
    #[test]
    fn a_folded_header_is_dropped_with_the_line_it_belongs_to() {
        let head = "GET / HTTP/1.1\r\n\
                    Host: h\r\n\
                    Cookie: a=1\r\n\
                    \x20b=2\r\n\
                    X-Other: z\r\n";
        let out = rewrite_headers(head, "k=v");

        assert!(
            !out.contains("b=2"),
            "the visitor's folded cookie became part of Host: {out}"
        );
        assert!(out.contains("Host: h\r\n"), "{out}");
        assert!(out.contains("X-Other: z\r\n"), "{out}");
        assert!(out.contains("Cookie: k=v\r\n"), "{out}");
    }

    #[test]
    fn a_folded_connection_header_goes_the_same_way() {
        let head = "GET / HTTP/1.1\r\nHost: h\r\nConnection: keep-alive,\r\n\tUpgrade\r\n";
        let out = rewrite_headers(head, "k=v");
        assert!(!out.contains("Upgrade"), "{out}");
        assert_eq!(out.matches("Connection:").count(), 1, "{out}");
    }

    // …and a fold on a header that is *kept* stays exactly where it was:
    // the contract is "every other header is passed through byte for byte".
    #[test]
    fn a_folded_header_that_is_kept_is_left_alone() {
        let head = "GET / HTTP/1.1\r\nHost: h\r\nX-Long: a,\r\n\tb\r\nCookie: old=1\r\n";
        let out = rewrite_headers(head, "k=v");
        assert!(out.contains("X-Long: a,\r\n\tb\r\n"), "{out}");
        assert!(!out.contains("old=1"), "{out}");
    }

    #[test]
    fn the_end_of_the_headers_is_found_in_both_spellings() {
        let crlf = b"GET / HTTP/1.1\r\nHost: x\r\n\r\nbody-bytes";
        let (end_head, start_body) = find_headers_end(crlf).unwrap();
        assert!(crlf[..end_head].ends_with(b"\r\n"));
        assert_eq!(&crlf[end_head..start_body], b"\r\n");
        assert_eq!(&crlf[start_body..], b"body-bytes");

        let lf = b"GET / HTTP/1.1\nHost: x\n\nbody";
        let (end_head, start_body) = find_headers_end(lf).unwrap();
        assert!(lf[..end_head].ends_with(b"\n"));
        assert_eq!(&lf[start_body..], b"body");

        assert!(find_headers_end(b"GET / HTTP/1.1\r\nHost: x\r\n").is_none());
    }

    /// An upstream that records the headers it was sent and answers 200.
    fn upstream_that_records() -> (u16, mpsc::Receiver<String>) {
        upstream_that_records_on(TcpListener::bind("127.0.0.1:0").unwrap())
    }

    /// [`upstream_that_records`], on a listener the test bound itself.
    fn upstream_that_records_on(listener: TcpListener) -> (u16, mpsc::Receiver<String>) {
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                match socket.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        if find_headers_end(&acc).is_some() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            tx.send(String::from_utf8_lossy(&acc).into_owned()).ok();
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
            let _ = socket.shutdown(std::net::Shutdown::Write);
        });
        (port, rx)
    }

    #[test]
    fn a_request_round_trips_and_the_upstream_sees_the_injected_cookie() {
        let (upstream_port, seen) = upstream_that_records();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((stream, _)) = proxy.accept() {
                let _ = handle_connection(stream, upstream_port, COOKIE);
            }
        });

        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        client
            .write_all(b"GET /hello HTTP/1.1\r\nHost: tunnel.example\r\nCookie: stale=1\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.contains("200 OK"), "{response}");

        let head = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(head.contains(&format!("Cookie: {COOKIE}")), "{head}");
        assert!(!head.contains("stale=1"), "{head}");
        assert!(head.contains("Connection: close"), "{head}");
        assert!(head.contains("Host: tunnel.example"), "{head}");
    }

    // A dev server told `localhost` is on `[::1]` alone on macOS, which is
    // Vite's default, and a proxy that dialled `127.0.0.1` only dropped
    // every request the tunnel handed it.
    #[test]
    fn a_request_reaches_an_upstream_on_the_ipv6_loopback_alone() {
        let Ok(listener) = TcpListener::bind(("::1", 0)) else {
            eprintln!("skipping: no IPv6 loopback on this machine");
            return;
        };
        let (upstream_port, seen) = upstream_that_records_on(listener);
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((stream, _)) = proxy.accept() {
                let _ = handle_connection(stream, upstream_port, COOKIE);
            }
        });

        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: tunnel.example\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.contains("200 OK"), "{response}");
        let head = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(head.contains(&format!("Cookie: {COOKIE}")), "{head}");
    }

    #[test]
    fn a_request_body_arriving_with_the_headers_reaches_the_upstream() {
        let (upstream_port, seen) = upstream_that_records();
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((stream, _)) = proxy.accept() {
                let _ = handle_connection(stream, upstream_port, COOKIE);
            }
        });

        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        client
            .write_all(b"POST /x HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.contains("200 OK"), "{response}");
        let head = seen.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(head.contains("Content-Length: 5"), "{head}");
    }

    /// An upstream that answers and then keeps writing to the response,
    /// reporting which chunk it was on when the proxy half-closed the
    /// connection at it — or `None` if it never did.
    fn streaming_upstream(chunks: usize, every: Duration) -> (u16, mpsc::Receiver<Option<usize>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<Option<usize>>();
        thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let mut acc: Vec<u8> = Vec::new();
            loop {
                match socket.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        acc.extend_from_slice(&buf[..n]);
                        if find_headers_end(&acc).is_some() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = socket.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            );
            // Short, so watching for the end of the client's half costs
            // almost nothing between chunks.
            let _ = socket.set_read_timeout(Some(Duration::from_millis(10)));
            let mut fin_at = None;
            for i in 0..chunks {
                if socket
                    .write_all(format!("data: tick-{i}\n\n").as_bytes())
                    .is_err()
                {
                    break;
                }
                thread::sleep(every);
                if fin_at.is_none() {
                    let mut probe = [0u8; 1];
                    if let Ok(0) = socket.read(&mut probe) {
                        fin_at = Some(i);
                    }
                }
            }
            tx.send(fin_at).ok();
        });
        (port, rx)
    }

    // Finding 3. The read timeout is for the *headers*; a request whose
    // body is already sent — every GET — then says nothing for as long as
    // the response takes. Leaving the timeout on made the client-to-upstream
    // copy fail at thirty seconds and the proxy send FIN to the
    // application, cutting every SSE, long-poll and streaming response.
    //
    // 200 ms stands in for the 30 s: the defect is that the deadline was
    // never cleared, not what it was set to.
    #[test]
    fn a_streaming_response_survives_a_client_with_nothing_more_to_say() {
        let tick = Duration::from_millis(60);
        let (upstream_port, fin) = streaming_upstream(12, tick);
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let header_timeout = Duration::from_millis(200);
        thread::spawn(move || {
            if let Ok((stream, _)) = proxy.accept() {
                let limits = Limits {
                    headers: header_timeout,
                    upstream: UPSTREAM_READ_TIMEOUT,
                };
                let _ = handle_connection_with(stream, upstream_port, COOKIE, limits);
            }
        });

        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        client
            .write_all(b"GET /stream HTTP/1.1\r\nHost: h\r\nAccept: text/event-stream\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();

        assert_eq!(
            fin.recv_timeout(Duration::from_secs(10)).unwrap(),
            None,
            "the proxy half-closed the application while the response was still streaming, \
             {}ms in",
            header_timeout.as_millis()
        );
        assert!(
            response.contains("tick-11"),
            "the stream was cut short: {response}"
        );
    }

    // The header deadline is for the whole block: a client trickling a
    // byte at a time, each well inside it, is cut off once it passes.
    #[test]
    fn a_client_trickling_its_headers_is_cut_off_at_the_deadline() {
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        thread::spawn(move || {
            if let Ok((stream, _)) = proxy.accept() {
                let limits = Limits {
                    headers: Duration::from_millis(300),
                    upstream: UPSTREAM_READ_TIMEOUT,
                };
                let handled = handle_connection_with(stream, 1, COOKIE, limits);
                tx.send(handled.map_err(|e| format!("{e:#}"))).ok();
            }
        });
        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        let began = Instant::now();
        for byte in b"GET / HTTP/1.1\r\nHost: h\r\n".iter().cycle().take(40) {
            if client.write_all(&[*byte]).is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
            if let Ok(result) = rx.try_recv() {
                let err = result.unwrap_err();
                assert!(err.contains("too long to send its headers"), "{err}");
                assert!(began.elapsed() < Duration::from_millis(1500));
                return;
            }
        }
        panic!("a trickle of 50 ms a byte held the connection past its 300 ms deadline");
    }

    // The upstream deadline is for a server that never starts answering.
    // Left on for the whole response, it cut a stream whose events came
    // further apart than it: 300 ms stands in for the 60 s.
    #[test]
    fn a_stream_quieter_than_the_upstream_deadline_is_not_cut() {
        let tick = Duration::from_millis(450);
        let (upstream_port, fin) = streaming_upstream(3, tick);
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        thread::spawn(move || {
            if let Ok((stream, _)) = proxy.accept() {
                let limits = Limits {
                    headers: HEADER_READ_TIMEOUT,
                    upstream: Duration::from_millis(300),
                };
                let _ = handle_connection_with(stream, upstream_port, COOKIE, limits);
            }
        });

        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        client
            .write_all(b"GET /stream HTTP/1.1\r\nHost: h\r\nAccept: text/event-stream\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(fin.recv_timeout(Duration::from_secs(10)).is_ok());
        assert!(
            response.contains("tick-2"),
            "the stream was cut between two events: {response}"
        );
    }

    // `await_listening` connects and closes at once to see the proxy is
    // up, and the proxy logged that probe as a failed connection: a false
    // error in every share's log, and in a race of two shares the line
    // `last_words` quoted for the other proxy's bind failure.
    #[test]
    fn a_connection_that_sends_nothing_is_not_a_failure() {
        let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_port = proxy.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel::<Result<(), String>>();
        thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((stream, _)) = proxy.accept() {
                    // No upstream: neither connection gets as far as one.
                    let handled = handle_connection(stream, 1, COOKIE);
                    tx.send(handled.map_err(|e| format!("{e:#}"))).ok();
                }
            }
        });

        drop(TcpStream::connect(("127.0.0.1", proxy_port)).unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Ok(()));

        let mut client = TcpStream::connect(("127.0.0.1", proxy_port)).unwrap();
        client.write_all(b"GET / HTTP/1.1\r\nHost: h\r\n").unwrap();
        drop(client);
        let err = rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap_err();
        assert!(
            err.contains("closed before finishing its headers"),
            "a request cut off part-way is still one: {err}"
        );
    }

    #[test]
    fn the_proxy_log_is_emptied_before_a_share_and_never_created() {
        let fx = fixture();
        let log = fx.paths.log_file("feat+one", PROXY_LOG);
        truncate_log(&log).unwrap();
        assert!(!log.exists(), "truncate must not create the file");

        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, b"bytes from a dead session\n").unwrap();
        truncate_log(&log).unwrap();
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
    }

    #[test]
    fn the_proxy_log_lives_under_the_home_beside_every_other_log() {
        let fx = fixture();
        let log = fx.paths.log_file("feat+one", PROXY_LOG);
        assert!(log.starts_with(&fx.paths.home));
        assert_eq!(log.file_name().unwrap(), "proxy.log");
    }

    // The proxy's own startup line goes into its log; a cookie there would
    // outlive the share it belonged to.
    #[test]
    fn nothing_the_proxy_announces_carries_the_cookie() {
        let announcement = format!(
            "pando share proxy: 127.0.0.1:{} -> localhost:{}",
            17005, 17000
        );
        assert!(!announcement.contains("session="), "{announcement}");
        assert!(!announcement.contains(COOKIE), "{announcement}");
    }
}
