//! The share proxy as it really runs: a detached child of the pando binary,
//! reading its cookie from the environment.
//!
//! Everything here binds loopback ports and spawns the real binary, so each
//! test owns a guard that stops its child even when an assertion panics.

use crate::common;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use common::paths_for;
use pando::share_proxy;
use tempfile::TempDir;

const COOKIE: &str = "pando_session=top-secret-value; who=dev";

/// A spawned proxy that is always stopped, even when a test fails partway
/// through. A leaked proxy holds a port and a cookie.
struct Proxy {
    spawn: share_proxy::ProxySpawn,
}

impl Drop for Proxy {
    fn drop(&mut self) {
        let _ = pando::process::stop(self.spawn.pgid, Duration::from_secs(5));
    }
}

struct Env {
    _dir: TempDir,
    home: PathBuf,
    root: PathBuf,
}

fn env() -> Env {
    let dir = TempDir::new().unwrap();
    let root = common::fixture_repo(dir.path());
    Env {
        home: dir.path().join("pando-home"),
        root,
        _dir: dir,
    }
}

fn pando_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pando"))
}

/// A port nothing is listening on. Bound to learn the number, then released
/// — the proxy takes it a moment later and the test waits until it has.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn wait_until(timeout: Duration, ready: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if ready() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// An upstream that records the request headers it was sent and answers.
/// Serves `requests` connections, then stops.
fn upstream(requests: usize) -> (u16, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for _ in 0..requests {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 4096];
            let mut seen: Vec<u8> = Vec::new();
            loop {
                match socket.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        seen.extend_from_slice(&buf[..n]);
                        if seen.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            tx.send(String::from_utf8_lossy(&seen).into_owned()).ok();
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
            let _ = socket.shutdown(std::net::Shutdown::Write);
        }
    });
    (port, rx)
}

fn get_through(port: u16, path: &str) -> String {
    let mut client = TcpStream::connect(("127.0.0.1", port)).unwrap();
    client
        .write_all(
            format!(
                "GET {path} HTTP/1.1\r\nHost: abc.trycloudflare.com\r\nCookie: stale=1\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    response
}

/// Every `.log` under a worktree's log directory, concatenated.
fn all_logs(home: &Path, root: &Path, name: &str) -> String {
    let paths = paths_for(home, root);
    let dir = paths.logs_dir(name);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return String::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_hidden_subcommand_proxies_a_request_and_injects_the_cookie() {
    let e = env();
    let paths = paths_for(&e.home, &e.root);
    let (upstream_port, seen) = upstream(1);
    let listen_port = free_port();

    let proxy = Proxy {
        spawn: share_proxy::spawn_with(
            &paths,
            "feat+one",
            listen_port,
            upstream_port,
            COOKIE,
            &pando_bin(),
        )
        .expect("spawn the proxy"),
    };
    assert!(
        wait_until(Duration::from_secs(10), || TcpStream::connect((
            "127.0.0.1",
            listen_port
        ))
        .is_ok()),
        "the proxy never started listening; log: {}",
        all_logs(&e.home, &e.root, "feat+one")
    );

    let response = get_through(listen_port, "/hello");
    assert!(response.contains("200 OK"), "{response}");

    let head = seen.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        head.contains(&format!("Cookie: {COOKIE}")),
        "the upstream did not see the injected cookie:\n{head}"
    );
    assert!(!head.contains("stale=1"), "{head}");
    assert_eq!(proxy.spawn.listen_port, listen_port);
}

// The whole reason the cookie goes by environment. `ps` output is readable
// by every process on the machine.
#[test]
fn the_cookie_is_in_no_command_line_and_in_no_log() {
    let e = env();
    let paths = paths_for(&e.home, &e.root);
    let (upstream_port, seen) = upstream(1);
    let listen_port = free_port();

    let proxy = Proxy {
        spawn: share_proxy::spawn_with(
            &paths,
            "feat+one",
            listen_port,
            upstream_port,
            COOKIE,
            &pando_bin(),
        )
        .unwrap(),
    };
    assert!(wait_until(Duration::from_secs(10), || TcpStream::connect(
        ("127.0.0.1", listen_port)
    )
    .is_ok()));
    get_through(listen_port, "/x");
    seen.recv_timeout(Duration::from_secs(10)).unwrap();

    let ps = Command::new("ps")
        .args(["-eo", "pid,pgid,command"])
        .output()
        .expect("run ps");
    let listing = String::from_utf8_lossy(&ps.stdout);
    assert!(
        listing.contains(&proxy.spawn.pid.to_string()),
        "the proxy should be in ps output at all"
    );
    assert!(
        !listing.contains("top-secret-value"),
        "the cookie reached a command line"
    );

    let logs = all_logs(&e.home, &e.root, "feat+one");
    assert!(
        !logs.is_empty(),
        "the proxy should have logged that it is listening"
    );
    assert!(
        !logs.contains("top-secret-value"),
        "the cookie reached a log that outlives the share:\n{logs}"
    );
    assert!(
        logs.contains(&listen_port.to_string()),
        "the proxy log should say where it is listening:\n{logs}"
    );
}

#[test]
fn the_hidden_subcommand_refuses_to_run_without_its_cookie() {
    let e = env();
    // Not a repository: the proxy must not need one, and the refusal must
    // not be "not inside a git repository".
    let elsewhere = e.root.parent().unwrap().join("not-a-repo");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let out = Command::new(pando_bin())
        .env_remove(share_proxy::ENV_COOKIE)
        .env("PANDO_HOME", &e.home)
        .current_dir(&elsewhere)
        .args([
            share_proxy::SUBCOMMAND,
            "--listen",
            "17005",
            "--upstream",
            "17000",
        ])
        .output()
        .expect("run pando");

    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(share_proxy::ENV_COOKIE), "{stderr}");
    assert!(stderr.contains("pando share"), "{stderr}");
    assert!(
        !stderr.contains("git repository"),
        "the proxy runs outside a repository on purpose: {stderr}"
    );
}

// ---- the whole lifecycle through the binary --------------------------------

/// A fixture with a dev process that really binds a port and a fake
/// cloudflared in its home, run through the `pando` binary.
struct Cli {
    _dir: TempDir,
    home: PathBuf,
    root: PathBuf,
}

impl Drop for Cli {
    fn drop(&mut self) {
        // Every share, every process, whatever the test did.
        if self.home.exists() {
            let _ = self.run(&["stop"]);
        }
    }
}

impl Cli {
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(pando_bin())
            .env("PANDO_HOME", &self.home)
            .current_dir(&self.root)
            .args(args)
            .output()
            .expect("run pando")
    }

    fn state(&self) -> serde_json::Value {
        let out = self.run(&["status", "--json"]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "status failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("valid json")
    }

    /// Starts a worktree and waits until its process is really Running.
    ///
    /// `start` records `Starting` and returns; the phase advances on the
    /// next read path, once the listener has actually bound its port. A
    /// share of something still coming up is refused on purpose, so this is
    /// what a developer does too — they look at `status`.
    fn start_and_wait(&self, name: &str) {
        let out = self.run(&["start", name]);
        assert_eq!(code(&out), 0, "start failed: {}", stderr_of(&out));
        assert!(
            wait_until(Duration::from_secs(30), || {
                self.state()["worktrees"][0]["processes"]["dev"]["phase"] == "running"
            }),
            "the dev process never reached running: {}",
            self.state()
        );
    }
}

fn cli_env() -> Option<Cli> {
    if !common::python3_available() {
        eprintln!("skipping: python3 is needed for a process that really holds a port");
        return None;
    }
    let dir = TempDir::new().unwrap();
    let root = common::build(common::Kind::Plain, dir.path()).root;
    let home = dir.path().join("pando-home");
    common::fake_cloudflared(&home);
    common::write_listener_config(common::Kind::Plain, &home, &root);
    Some(Cli {
        home,
        root,
        _dir: dir,
    })
}

fn code(out: &std::process::Output) -> i32 {
    out.status.code().expect("pando exited via a signal")
}

fn stdout_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn share_publishes_a_url_and_unshare_takes_it_down() {
    let Some(cli) = cli_env() else { return };
    assert_eq!(code(&cli.run(&["new", "feat/one"])), 0);
    cli.start_and_wait("feat+one");

    let shared = cli.run(&["share", "feat+one"]);
    assert_eq!(code(&shared), 0, "{}", stderr_of(&shared));
    assert_eq!(
        stdout_of(&shared).trim(),
        common::FAKE_TUNNEL_URL,
        "stdout is the URL and nothing else, so `open \"$(pando share x)\"` works"
    );

    let share = cli.state()["worktrees"][0]["share"].clone();
    assert_eq!(share["url"], common::FAKE_TUNNEL_URL);
    assert_eq!(share["proxy_port"], serde_json::Value::Null);

    let text = cli.run(&["status", "feat+one"]);
    assert!(
        stdout_of(&text).contains(common::FAKE_TUNNEL_URL),
        "{}",
        stdout_of(&text)
    );

    let unshared = cli.run(&["unshare", "feat+one"]);
    assert_eq!(code(&unshared), 0, "{}", stderr_of(&unshared));
    assert_eq!(
        cli.state()["worktrees"][0]["share"],
        serde_json::Value::Null
    );
    assert_eq!(
        common::status_porcelain(&cli.root),
        "",
        "the fixture must stay clean throughout"
    );
}

#[test]
fn a_second_share_prints_the_url_it_already_has() {
    let Some(cli) = cli_env() else { return };
    cli.run(&["new", "feat/one"]);
    cli.start_and_wait("feat+one");
    let first = cli.run(&["share", "feat+one"]);
    assert_eq!(code(&first), 0, "{}", stderr_of(&first));

    let second = cli.run(&["share", "feat+one"]);
    assert_eq!(code(&second), 0);
    assert_eq!(stdout_of(&second).trim(), stdout_of(&first).trim());
    assert!(
        stderr_of(&second).contains("already shared"),
        "{}",
        stderr_of(&second)
    );
}

#[test]
fn sharing_a_worktree_that_is_not_running_exits_one() {
    let Some(cli) = cli_env() else { return };
    cli.run(&["new", "feat/one"]);
    let out = cli.run(&["share", "feat+one"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr_of(&out).contains("start it first"),
        "{}",
        stderr_of(&out)
    );
    assert!(stdout_of(&out).is_empty(), "nothing to pipe on a failure");
}

#[test]
fn unsharing_something_that_is_not_shared_exits_one() {
    let Some(cli) = cli_env() else { return };
    cli.run(&["new", "feat/one"]);
    let out = cli.run(&["unshare", "feat+one"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr_of(&out).contains("not shared"),
        "{}",
        stderr_of(&out)
    );
}

#[test]
fn stop_closes_the_public_url() {
    let Some(cli) = cli_env() else { return };
    cli.run(&["new", "feat/one"]);
    cli.start_and_wait("feat+one");
    cli.run(&["share", "feat+one"]);
    assert_ne!(
        cli.state()["worktrees"][0]["share"],
        serde_json::Value::Null
    );

    assert_eq!(code(&cli.run(&["stop", "feat+one"])), 0);
    assert_eq!(
        cli.state()["worktrees"][0]["share"],
        serde_json::Value::Null,
        "a stopped worktree never keeps a public URL"
    );
}

#[test]
fn rm_closes_the_public_url_and_removes_the_worktree() {
    let Some(cli) = cli_env() else { return };
    cli.run(&["new", "feat/one"]);
    cli.start_and_wait("feat+one");
    cli.run(&["share", "feat+one"]);

    let removed = cli.run(&["rm", "feat+one", "--force"]);
    assert_eq!(code(&removed), 0, "{}", stderr_of(&removed));
    assert_eq!(
        cli.state()["worktrees"].as_array().unwrap().len(),
        0,
        "the worktree is gone, and so is anything it was running"
    );
    assert_eq!(common::status_porcelain(&cli.root), "");
}

// `unshare` is one of the commands you need most when `pando.toml` is
// broken: the record holds both pgids, and taking a URL down needs nothing
// else.
#[test]
fn unshare_works_when_the_config_is_unreadable() {
    let Some(cli) = cli_env() else { return };
    cli.run(&["new", "feat/one"]);
    cli.start_and_wait("feat+one");
    cli.run(&["share", "feat+one"]);

    let project = pando::project::ProjectRef::from_root(&cli.root).unwrap();
    let config = cli
        .home
        .join("projects")
        .join(&project.id)
        .join("pando.toml");
    std::fs::write(&config, "this is not toml {{{").unwrap();

    let out = cli.run(&["unshare", "feat+one"]);
    assert_eq!(code(&out), 0, "{}", stderr_of(&out));
}

// It is hidden, not secret: it must not appear in `--help`, because nobody
// should ever type it.
#[test]
fn the_hidden_subcommand_is_not_advertised() {
    let out = Command::new(pando_bin())
        .arg("--help")
        .output()
        .expect("run pando --help");
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(help.contains("Commands:"), "{help}");
    assert!(
        !help.contains(share_proxy::SUBCOMMAND),
        "the proxy subcommand must stay out of help:\n{help}"
    );
}

// A share spawns its tunnel in a session of its own and records it only
// once the tunnel has published, up to thirty seconds later. A pando
// killed in between — a Ctrl-C, a TUI quit mid-share — left the tunnel
// running with nothing able to name it: `unshare` said "not shared", and
// neither `stop` nor `rm` ever saw it.
#[test]
fn a_share_killed_before_its_tunnel_is_up_leaves_nothing_running() {
    let Some(cli) = cli_env() else { return };
    // A tunnel that never publishes, so the share is still waiting on it
    // when it is killed.
    {
        use std::os::unix::fs::PermissionsExt;
        let fake = cli.home.join("bin").join("cloudflared");
        std::fs::write(
            &fake,
            "#!/bin/sh\necho 'INF Requesting new quick Tunnel on trycloudflare.com...'\n\
             exec sleep 300\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert_eq!(code(&cli.run(&["new", "feat/one"])), 0);
    cli.start_and_wait("feat+one");

    let mut sharing = Command::new(pando_bin())
        .env("PANDO_HOME", &cli.home)
        .current_dir(&cli.root)
        .args(["share", "feat+one"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run pando share");
    let paths = paths_for(&cli.home, &cli.root);
    let pending = || {
        pando::state::load(&paths.state_file())
            .ok()
            .and_then(|state| state.worktrees.get("feat+one").cloned())
            .map(|record| record.pending_shares)
            .unwrap_or_default()
    };
    let noted = wait_until(Duration::from_secs(20), || !pending().is_empty());
    // SIGKILL: nothing of the share's own clean-up runs.
    let _ = sharing.kill();
    let _ = sharing.wait();
    assert!(noted, "the tunnel was never written down while it came up");
    let groups = pending()[0].pgids.clone();
    assert!(
        groups.iter().any(|&pgid| pando::process::group_alive(pgid)),
        "the tunnel should still be up once the share that spawned it is gone"
    );

    // Any read path, as the next command anyone types would be.
    let status = cli.run(&["status"]);
    assert_eq!(code(&status), 0, "{}", stderr_of(&status));
    assert!(
        wait_until(Duration::from_secs(10), || groups
            .iter()
            .all(|&pgid| !pando::process::group_alive(pgid))),
        "a tunnel outlived the share that spawned it, with nothing left to name it"
    );
    assert!(pending().is_empty(), "{:?}", pending());
}
