//! Helpers shared by the unit tests of several modules. Compiled only under
//! `cfg(test)`; integration tests use `tests/common/mod.rs` instead, since
//! they cannot see a test-gated module of the library.

use std::path::Path;
use std::process::{Command, Stdio};

/// Runs git with a fixture identity so committing works on any machine and
/// never picks up (or depends on) the operator's own git config. These flags
/// belong to generated fixture repositories only.
const FIXTURE_IDENTITY: [&str; 10] = [
    "-c",
    "user.name=t",
    "-c",
    "user.email=t@t",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "tag.gpgSign=false",
    "-c",
    "init.defaultBranch=main",
];

pub fn git(cwd: &Path, args: &[&str]) {
    no_auto_maintenance();
    let out = Command::new("git")
        .args(FIXTURE_IDENTITY)
        .current_dir(cwd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

/// Turns git's automatic maintenance off for every git this test binary
/// starts — the fixtures', the library's, and any a process it runs
/// starts — once, before its first fixture: every fixture repository is
/// made through [`git`], which calls this.
///
/// A commit, a merge, a rebase or a fetch ends by starting `git maintenance
/// run --auto`, as does the receive-pack a push starts in the repository
/// it pushes to, unless `maintenance.auto` is false. Since git 2.47 that
/// run detaches, and since 2.55 its detached half holds
/// `.git/objects/maintenance.lock` until it is done, so a repository goes
/// on changing after the command that changed it has returned, and the
/// detached half can outlive the test that started it. No test here is
/// about git's housekeeping.
///
/// In the environment rather than in a config file: git reads
/// `GIT_CONFIG_COUNT` pairs (2.31 and later) as it reads `-c`, over every
/// config file, so neither the developer's own config nor a repository's
/// turns it back on, and every child inherits them. Added after any pairs
/// the environment already had. The one git they do not reach is the
/// `receive-pack` a push starts in a bare origin, because git clears them
/// for a command it runs in another repository, so a test's origin turns
/// maintenance off in its own config.
///
/// The one other test that writes the environment is
/// `paths::tests::default_home_honours_pando_home_then_falls_back`, on
/// PANDO_HOME (fakes go in pando's own `bin`, see [`fake_cloudflared`]).
/// This is set once, before any fixture repository exists, and to the same
/// value for every test, so unlike a PATH one test sets for itself there
/// is nothing for two tests to disagree about.
pub fn no_auto_maintenance() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let n: usize = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|count| count.parse().ok())
            .unwrap_or(0);
        // SAFETY: std's own environment lock orders these with every other
        // std::env read and with Command's spawn, and nothing in these
        // tests reads the environment through libc behind std's back. The
        // pair goes in before the count that reaches it, so a git spawned
        // in between never reads a count past its pairs.
        unsafe {
            std::env::set_var(format!("GIT_CONFIG_KEY_{n}"), "maintenance.auto");
            std::env::set_var(format!("GIT_CONFIG_VALUE_{n}"), "false");
            std::env::set_var("GIT_CONFIG_COUNT", (n + 1).to_string());
        }
    });
}

/// The HOME every login shell pando starts under test is given: empty, and
/// the same one for the whole run. See `platform::shell::login`.
pub fn shell_home() -> &'static Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("pando-test-home")
            .tempdir()
            .expect("a HOME for the shells under test")
    })
    .path()
}

/// A repository with one commit on `main`, for tests that need a real repo
/// but no particular contents.
pub fn init_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "--initial-branch=main"]);
    git(path, &["commit", "--quiet", "--allow-empty", "-m", "root"]);
}

/// A repository whose origin/HEAD, `origin/develop`, is one commit made
/// `days` before the branch the main checkout is on, `work`, which has
/// `ahead` commits it lacks. `file`, when named, is committed on `work`
/// only, with the dirs it names.
///
/// Written by one `git fast-import`, so a hundred commits cost one
/// process, and with no remote at all: origin/HEAD is only a symbolic ref.
pub fn drifted_repo(path: &Path, ahead: u32, days: i64, file: Option<&str>) {
    use std::fmt::Write as _;
    use std::io::Write as _;
    std::fs::create_dir_all(path).unwrap();
    git(path, &["init", "--quiet", "--initial-branch=main"]);
    let then: i64 = 1_700_000_000;
    let now = then + days * 86_400;
    let mut stream = format!(
        "commit refs/heads/develop\nmark :1\ncommitter t <t@t> {then} +0000\ndata 4\nroot\n\
         M 644 inline README\ndata 2\nr\n\n\
         reset refs/remotes/origin/develop\nfrom :1\n\n"
    );
    for n in 0..ahead {
        let _ = write!(
            stream,
            "commit refs/heads/work\ncommitter t <t@t> {now} +0000\ndata 2\nc\n"
        );
        if n == 0 {
            stream.push_str("from :1\n");
            if let Some(file) = file {
                let _ = write!(stream, "M 644 inline {file}\ndata 2\nl\n");
            }
        }
        stream.push('\n');
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .expect("spawn git fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "git fast-import failed");
    git(
        path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/develop",
        ],
    );
    git(path, &["checkout", "--quiet", "work"]);
}

/// A detached child that is always stopped, even when a test fails partway
/// through. Every test in this crate that forks a real process holds one of
/// these: an assertion that panics mid-test must not leave a `sleep` or a
/// listener behind.
pub struct Detached {
    pub pid: u32,
    pub pgid: crate::process::Group,
}

impl Drop for Detached {
    fn drop(&mut self) {
        let _ = crate::process::stop(self.pgid, std::time::Duration::from_secs(5));
    }
}

/// Spawns a detached shell command under a guard, as `actions::start` does.
pub fn spawn_guarded(shell_cmd: &str, cwd: &Path, log_file: &Path) -> Detached {
    let r = crate::process::spawn_detached(crate::process::SpawnOptions {
        shell_cmd,
        cwd,
        log_file,
        env: &[],
        status_file: None,
    })
    .expect("spawn detached");
    Detached {
        pid: r.pid,
        pgid: r.pgid,
    }
}

/// Whether `python3` is on PATH. Tests that need a process which binds a
/// port skip with a message rather than failing on a machine without it.
pub fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A process that binds `port` and then does nothing, for readiness and
/// observed-port tests.
///
/// Deliberately brace-free: `{` is pando's template syntax, and a command
/// carrying one would have to be escaped everywhere this string is used.
pub fn python_listener(port: u16) -> String {
    format!(
        "python3 -u -c \"import socket,time;s=socket.socket();\
         s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);\
         s.bind(('127.0.0.1',{port}));s.listen(5);print('listening on {port}');time.sleep(300)\""
    )
}

/// A listener that binds whatever port the `{port:<role>}` template
/// resolves to, for tests that start a process which really holds a port.
pub fn python_listener_for_role(role: &str) -> String {
    format!(
        "python3 -u -c \"import socket,time;s=socket.socket();\
         s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);\
         s.bind(('127.0.0.1',{{port:{role}}}));s.listen(5);\
         print('listening');time.sleep(300)\""
    )
}

/// A process that binds `port` on the IPv6 loopback and nowhere else.
///
/// Not exotic: `listen(port, "localhost")` in Node on macOS resolves to
/// `::1` first, and `runserver [::1]:8000` does the same. Both IPv4
/// addresses stay bindable, so a probe that only tries those never sees it.
pub fn python_listener_v6(port: u16) -> String {
    format!(
        "python3 -u -c \"import socket,time;s=socket.socket(socket.AF_INET6,socket.SOCK_STREAM);\
         s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);\
         s.bind(('::1',{port}));s.listen(5);print('listening');time.sleep(300)\""
    )
}

/// Whether this machine has an IPv6 loopback to bind at all. A test about
/// IPv6 behaviour skips with a message rather than failing where there is
/// none.
pub fn ipv6_loopback_available() -> bool {
    std::net::TcpListener::bind(("::1", 0)).is_ok()
}

/// A loopback port nothing answers on, held for as long as this lives.
///
/// Bound on 127.0.0.1, and on ::1 where there is one, and never listened
/// on: a connection to it is refused, and nothing else can take it in the
/// meantime — neither another test's listener nor the OS handing out an
/// ephemeral port — which a port bound once and let go cannot promise.
/// Under a full run another test bound such a port between the two, and
/// "nothing answers" failed for want of anything wrong.
pub struct RefusedPort {
    pub port: u16,
    _held: Vec<std::os::fd::OwnedFd>,
}

pub fn refused_port() -> RefusedPort {
    for _ in 0..32 {
        let (v4, port) = bound_unlistened(false, 0).expect("bind a socket on 127.0.0.1");
        match bound_unlistened(true, port) {
            Ok((v6, _)) => {
                return RefusedPort {
                    port,
                    _held: vec![v4, v6],
                };
            }
            // Taken on ::1 alone: another port.
            Err(e) if e.raw_os_error() == Some(libc::EADDRINUSE) => continue,
            // No IPv6 loopback, so no connection over it to refuse.
            Err(_) => {
                return RefusedPort {
                    port,
                    _held: vec![v4],
                };
            }
        }
    }
    panic!("no loopback port could be held on both 127.0.0.1 and ::1");
}

#[test]
fn a_refused_port_is_refused_and_nobody_else_can_take_it() {
    let refused = refused_port();
    assert!(!crate::ports::something_is_listening(refused.port));
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", refused.port)).is_err(),
        "held"
    );
}

/// A TCP socket bound to `port` (0: any) on the IPv4 or IPv6 loopback and
/// not listening, with the port it got. `std` binds only to listen.
fn bound_unlistened(v6: bool, port: u16) -> std::io::Result<(std::os::fd::OwnedFd, u16)> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let family = if v6 { libc::AF_INET6 } else { libc::AF_INET };
    // SAFETY: plain socket calls on a descriptor this function owns; every
    // address handed over is a zeroed struct of the size passed with it.
    unsafe {
        let fd = libc::socket(family, libc::SOCK_STREAM, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let owned = OwnedFd::from_raw_fd(fd);
        // Never inherited: a process another test starts would hold the
        // port for as long as it runs.
        if libc::fcntl(owned.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut storage: libc::sockaddr_storage = std::mem::zeroed();
        let len = if v6 {
            let addr = &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in6);
            addr.sin6_family = libc::AF_INET6 as libc::sa_family_t;
            addr.sin6_port = port.to_be();
            addr.sin6_addr.s6_addr = std::net::Ipv6Addr::LOCALHOST.octets();
            std::mem::size_of::<libc::sockaddr_in6>()
        } else {
            let addr = &mut *(&mut storage as *mut _ as *mut libc::sockaddr_in);
            addr.sin_family = libc::AF_INET as libc::sa_family_t;
            addr.sin_port = port.to_be();
            addr.sin_addr.s_addr = u32::from(std::net::Ipv4Addr::LOCALHOST).to_be();
            std::mem::size_of::<libc::sockaddr_in>()
        } as libc::socklen_t;
        let address = &storage as *const _ as *const libc::sockaddr;
        if libc::bind(owned.as_raw_fd(), address, len) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut got: libc::sockaddr_storage = std::mem::zeroed();
        let mut got_len = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        if libc::getsockname(
            owned.as_raw_fd(),
            &mut got as *mut _ as *mut libc::sockaddr,
            &mut got_len,
        ) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let port = if v6 {
            (*(&got as *const _ as *const libc::sockaddr_in6)).sin6_port
        } else {
            (*(&got as *const _ as *const libc::sockaddr_in)).sin_port
        };
        Ok((owned, u16::from_be(port)))
    }
}

/// The quick-tunnel URL the fake provider publishes.
pub const FAKE_TUNNEL_URL: &str = "https://fake-tunnel-for-tests.trycloudflare.com";

/// Installs a fake `cloudflared` at `<home>/bin/cloudflared`.
///
/// The same hook a developer would use for a real shim, so no test has to
/// put anything on PATH: `std::env::set_var` is unsafe in this edition and
/// racy across parallel tests, and a child's `PATH` is not reliably what
/// program lookup uses.
///
/// Every fake echoes its own arguments first, so a test can assert what
/// pando asked the provider for — which port it tunnelled, and that it
/// shadowed the user's config.
pub fn fake_cloudflared(home: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).expect("create the shim directory");
    let path = bin.join("cloudflared");
    std::fs::write(&path, format!("#!/bin/sh\necho \"ARGS: $*\"\n{body}"))
        .expect("write the fake cloudflared");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make the fake cloudflared executable");
}

/// A fake that publishes a URL in cloudflared's own bordered format,
/// registers a connection with the edge, and then stays up, as a tunnel
/// does. `exec` so the pid pando records is the one that has to be killed.
pub fn fake_cloudflared_publishing(home: &Path) {
    fake_cloudflared(
        home,
        &format!(
            "echo 'INF Requesting new quick Tunnel on trycloudflare.com...'\n\
             echo 'INF +----------------------------------------------------+'\n\
             echo 'INF |  {FAKE_TUNNEL_URL}  |'\n\
             echo 'INF +----------------------------------------------------+'\n\
             echo '{FAKE_REGISTERED}'\n\
             exec sleep 300\n"
        ),
    );
}

/// The line cloudflared logs once the edge has a connection for its
/// tunnel, as 2025.11.1 prints it.
pub const FAKE_REGISTERED: &str = "INF Registered tunnel connection connIndex=0 \
     connection=00000000-0000-0000-0000-000000000000 event=0 ip=198.41.200.13 location=tst01 \
     protocol=quic";

/// A fake that publishes its URL and then never reaches the edge, the way
/// cloudflared does on a network that blocks it: dial errors, and then an
/// exit.
pub fn fake_cloudflared_unreachable_edge(home: &Path) {
    fake_cloudflared(
        home,
        &format!(
            "echo 'INF |  {FAKE_TUNNEL_URL}  |'\n\
             echo 'ERR Failed to dial a quic connection error=\"timeout: no recent network \
             activity\"' >&2\n\
             sleep 0.3\n\
             echo 'ERR no more connections active and exiting' >&2\n\
             exit 1\n"
        ),
    );
}

/// A fake that publishes its URL and then neither connects nor exits.
pub fn fake_cloudflared_still_dialling(home: &Path) {
    fake_cloudflared(
        home,
        &format!(
            "echo 'INF |  {FAKE_TUNNEL_URL}  |'\n\
             echo 'INF Retrying connection in up to 2s' >&2\n\
             exec sleep 300\n"
        ),
    );
}

/// A fake that publishes the same URL the way `--output json` logs it: one
/// JSON object per line, with the bordered banner inside `message`, which
/// is where cloudflared 2025.11.1 really puts it.
pub fn fake_cloudflared_json_publishing(home: &Path) {
    fake_cloudflared(
        home,
        &format!(
            "echo '{{\"level\":\"info\",\"message\":\"Requesting new quick Tunnel on \
             trycloudflare.com...\"}}'\n\
             echo '{{\"level\":\"info\",\"message\":\"+---------------------+\"}}'\n\
             echo '{{\"level\":\"info\",\"message\":\"|  {FAKE_TUNNEL_URL}  |\"}}'\n\
             echo '{{\"level\":\"info\",\"message\":\"+---------------------+\"}}'\n\
             echo '{{\"level\":\"info\",\"connIndex\":0,\"event\":0,\"location\":\"tst01\",\
             \"protocol\":\"quic\",\"message\":\"Registered tunnel connection\"}}'\n\
             exec sleep 300\n"
        ),
    );
}

/// A fake that fails the way an offline or rate-limited cloudflared does:
/// a Go `*url.Error` naming the quick-tunnel API host, and then a shutdown
/// that is not instantaneous, because a real binary's is not either.
pub fn fake_cloudflared_api_error(home: &Path) {
    fake_cloudflared(
        home,
        "echo 'INF Requesting new quick Tunnel on trycloudflare.com...'\n\
         echo 'ERR failed to request quick Tunnel: Post \
         \"https://api.trycloudflare.com/tunnel\": dial tcp: lookup api.trycloudflare.com: \
         no such host' >&2\n\
         sleep 0.4\nexit 1\n",
    );
}

/// A fake that starts, says so, and never publishes anything.
pub fn fake_cloudflared_silent(home: &Path) {
    fake_cloudflared(
        home,
        "echo 'INF Requesting new quick Tunnel on trycloudflare.com...'\nexec sleep 300\n",
    );
}

/// A fake that fails the way a rate-limited cloudflared does.
pub fn fake_cloudflared_failing(home: &Path) {
    fake_cloudflared(
        home,
        "echo 'ERR failed to request quick Tunnel: 429 Too Many Requests' >&2\nexit 1\n",
    );
}

/// Polls `ready` until it is true or the deadline passes. Returns whether it
/// became true. Fixed sleeps make process tests flaky on a loaded machine;
/// this makes them fast when the machine is idle and patient when it is not.
pub fn wait_until(timeout: std::time::Duration, ready: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if ready() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// Makes `system` look like WSL's: its kernel release and its mount table
/// under `proc/`, where [`crate::platform::Host::at`] reads them.
pub fn wsl_system(system: &Path, release: &str, mounts: &str) {
    let kernel = system.join("proc/sys/kernel");
    std::fs::create_dir_all(&kernel).unwrap();
    std::fs::write(kernel.join("osrelease"), release).unwrap();
    std::fs::write(system.join("proc/mounts"), mounts).unwrap();
}

/// A WSL 2 mount table with drive C at `/mnt/c`, as Ubuntu reads it.
pub const WSL_MOUNTS: &str = "\
drivers /usr/lib/wsl/drivers 9p ro,nosuid,nodev,noatime,aname=drivers;fmask=222;dmask=222,cache=0x5,access=client,msize=65536,trans=fd,rfd=8,wfd=8 0 0
/dev/sdc / ext4 rw,relatime,discard,errors=remount-ro,data=ordered 0 0
C:\\134 /mnt/c 9p rw,noatime,aname=drvfs;path=C:\\;uid=1000;gid=1000;symlinkroot=/mnt/,cache=0x5,access=client,msize=65536,trans=fd,rfd=6,wfd=6 0 0
";

/// The kernel release WSL 2 reports.
pub const WSL_RELEASE: &str = "6.18.40.1-microsoft-standard-WSL2\n";
