//! Deterministic port allocation.
//!
//! Each worktree gets a base port derived from a hash of the project id plus
//! the worktree name, then one consecutive port per role. Bases step by
//! [`BASE_STEP`] so a worktree that grows from one role to four rarely walks
//! into a neighbour's base, and the project id is in the hash so two repos
//! with the same branch name do not collide by default.

pub const PORT_MIN: u16 = 17_000;
/// The top of the range, kept below every ephemeral floor pando has to
/// share a machine with: macOS allocates outgoing ports from 49152 and
/// Linux from 32768. A base above that can be taken by an unrelated
/// outgoing connection between the moment pando promises it to a worktree
/// and the moment the dev server binds it. Measured over 20 000 derived
/// bases, the old ceiling put 19.5% of them above macOS's floor and 60.5%
/// above Linux's; this leaves 1971 bases, which is still hundreds of times
/// more worktrees than anyone has.
pub const PORT_MAX: u16 = 32_767;

/// Gap between consecutive bases: enough room for a web port plus an api,
/// database, and cache port without reaching the next worktree's base.
pub const BASE_STEP: u16 = 8;

/// How many bases fit in the range with a full [`BASE_STEP`] window each, so
/// every port a base can hand out stays inside `PORT_MIN..=PORT_MAX`.
pub const BASE_COUNT: u32 = (PORT_MAX as u32 - PORT_MIN as u32 + 1) / BASE_STEP as u32;

/// Bases tried before giving up. A caller that exhausts this has ~400 ports
/// bound in its neighbourhood and wants a real error, not a longer walk.
const MAX_PROBE_ATTEMPTS: u32 = 50;

/// Separator between the hash inputs, so `("ab", "c")` and `("a", "bc")`
/// cannot hash to the same base.
const HASH_SEPARATOR: u8 = 0x1f;

/// Whether nothing is listening on `port`.
///
/// Three addresses, because one bind answers only part of the question. A
/// listener on `127.0.0.1` leaves `0.0.0.0` bindable and the other way
/// round — verified on macOS — and a listener on `[::1]` leaves both IPv4
/// addresses bindable, which is where `listen(port, "localhost")` on Node
/// and `runserver [::1]:8000` land.
///
/// A server bound to one non-loopback interface is still missed; the
/// observed-port scan is what catches those.
///
/// This binds the port for an instant, so it is only ever used where taking
/// the port is the point: reserving one. Readiness is answered by
/// [`something_is_listening`] and by scanning the process group, because a
/// probe that takes the port can hand the server it is waiting for an
/// `EADDRINUSE`.
pub fn is_port_free(port: u16) -> bool {
    can_bind("0.0.0.0", port)
        && can_bind("127.0.0.1", port)
        && v6_free("::1", port)
        && v6_free("::", port)
}

fn can_bind(host: &str, port: u16) -> bool {
    std::net::TcpListener::bind((host, port)).is_ok()
}

/// Whether `[host]:port` is free, on a machine that has IPv6.
///
/// `[::]` as well as `[::1]`: a listener on `[::]` with `IPV6_V6ONLY` set
/// — uvicorn's `--host ::`, nginx's `listen [::]:p`, Go's `tcp6` — leaves
/// both IPv4 addresses and, on macOS, `[::1]` bindable too.
///
/// Only `AddrInUse` counts as taken: a host without IPv6 answers every bind
/// with `AddrNotAvailable` or `AfNoSupport`, and reading that as "occupied"
/// would leave pando with no ports at all.
fn v6_free(host: &str, port: u16) -> bool {
    match std::net::TcpListener::bind((host, port)) {
        Ok(_) => true,
        Err(e) => e.kind() != std::io::ErrorKind::AddrInUse,
    }
}

/// How long a readiness connection waits before calling the port closed.
/// Local, so anything slower than this is not a listener that is up.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// The two loopbacks a server on this machine can be listening on, IPv4
/// first. Both, because a server told `localhost` lands on `[::1]` alone
/// wherever that resolves first — Node's `listen(port, "localhost")` on
/// macOS, and so Vite's default — and dialling `127.0.0.1` there is refused.
pub const LOOPBACKS: [std::net::IpAddr; 2] = [
    std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
    std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
];

/// A connection to `port` over whichever loopback accepts one, tried in
/// [`LOOPBACKS`] order, or the last refusal when neither does.
pub fn connect_loopback(
    port: u16,
    timeout: std::time::Duration,
) -> std::io::Result<std::net::TcpStream> {
    use std::net::{SocketAddr, TcpStream};
    let mut refused = std::io::ErrorKind::AddrNotAvailable.into();
    for host in LOOPBACKS {
        match TcpStream::connect_timeout(&SocketAddr::new(host, port), timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => refused = e,
        }
    }
    Err(refused)
}

/// Whether something accepts a connection on `port`, over either loopback.
///
/// The readiness question asked without taking anything: a refused
/// connection means not ready, and a successful one is released at once.
/// Used only where the process-group scan could not run — there is no
/// `lsof`, or it was denied — since a connection says nothing about *whose*
/// listener answered.
pub fn something_is_listening(port: u16) -> bool {
    connect_loopback(port, CONNECT_TIMEOUT).is_ok()
}

/// How long the readiness probe waits for a byte once it is connected.
/// A server that has nothing to say holds the socket open; that silence is
/// the answer, so this is the cost of one ready service, paid once.
const SERVING_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(200);

/// Whether something on `port` behaves like a server rather than a proxy
/// with nothing behind it.
///
/// Docker publishes a container port by putting a proxy in front of it, and
/// that proxy completes the handshake as soon as the *container* is
/// running — whatever is, or is not, listening inside. So [`something_is_listening`]
/// answers "the container exists", which is not the question readiness
/// asks: a database that takes two seconds to open its socket would be
/// declared ready at once, and the migration hook behind it would run
/// against nothing.
///
/// One `recv` tells the two apart. A real server holds the connection open
/// and says nothing until it is asked something — postgres and redis both
/// do — or sends a banner unprompted, as MySQL does. The proxy-only case
/// closes the connection immediately, which arrives as an end of file.
///
/// Still never a bind: taking the port would hand the server being waited
/// for an `EADDRINUSE`.
pub fn something_is_serving(port: u16) -> bool {
    use std::io::Read;
    use std::net::{SocketAddr, TcpStream};
    for host in LOOPBACKS {
        let Ok(mut stream) =
            TcpStream::connect_timeout(&SocketAddr::new(host, port), CONNECT_TIMEOUT)
        else {
            continue;
        };
        if stream.set_read_timeout(Some(SERVING_TIMEOUT)).is_err() {
            continue;
        }
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            // A banner: MySQL and a few others greet the client.
            Ok(n) if n > 0 => return true,
            // Nothing said, and the connection is still open — a server
            // waiting to be asked something.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return true;
            }
            // `Ok(0)` is an immediate end of file, which is the shape of a
            // published port with nothing behind it. Anything else is a
            // connection that broke, which is not readiness either.
            _ => continue,
        }
    }
    false
}

/// Whether `host`, as an env file names a server, is this machine: the
/// name `localhost`, an IPv4 loopback, or `::1`. The only hosts pando asks
/// anything of on its own — a server elsewhere is not the machine's to
/// answer for, and a name that has to be resolved has no bound on how long
/// that takes.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// What a local server said when asked for its first page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageAnswer {
    /// An HTTP response, by its status code.
    Status(u16),
    /// Bytes, and not HTTP's: a TLS alert from a server that wants HTTPS,
    /// or another protocol altogether. It answers; its page is not known.
    NotHttp,
    /// Nothing accepted a connection, on either loopback.
    Refused,
    /// Something accepted the connection and said nothing before the wait
    /// was over, or closed it without a byte every time.
    Silent,
}

/// How long `pando check` waits for a dev server's first page. A framework
/// that compiles a page on its first request — Next.js does — can take a
/// minute and more on a cold cache, and that is a server working, not one
/// that failed.
pub const PAGE_WAIT: std::time::Duration = std::time::Duration::from_secs(90);

/// How often a page wait looks up from its socket: to try again after a
/// refusal, and to let its caller end it.
const PAGE_POLL: std::time::Duration = std::time::Duration::from_millis(250);

thread_local! {
    static PAGE_WAIT_HERE: std::cell::Cell<Option<std::time::Duration>> =
        const { std::cell::Cell::new(None) };
}

/// [`PAGE_WAIT`], unless [`with_page_wait`] is running on this thread.
pub fn page_wait() -> std::time::Duration {
    PAGE_WAIT_HERE.with(|t| t.get()).unwrap_or(PAGE_WAIT)
}

/// Runs `f` with [`page_wait`] at `wait` on this thread. For tests: a
/// server that never answers is waited out for the whole of it, and a
/// minute and a half of that is not a test anyone runs. Per thread, so the
/// tests beside it keep the real wait.
#[doc(hidden)]
pub fn with_page_wait<R>(wait: std::time::Duration, f: impl FnOnce() -> R) -> R {
    let before = PAGE_WAIT_HERE.with(|t| t.replace(Some(wait)));
    struct Restore(Option<std::time::Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PAGE_WAIT_HERE.with(|t| t.set(self.0));
        }
    }
    let _restore = Restore(before);
    f()
}

/// Asks the server on `port` for `/`, as a browser at
/// `http://localhost:<port>` would — `Host: localhost:<port>`, since a dev
/// server that checks the host refuses any other — and says what came
/// back, waiting up to `wait` for it.
///
/// A refusal, or a connection closed without a byte, is tried again until
/// the wait is over: a dev server restarts itself while it starts. A
/// connection that stays silent is waited on, because that is a page being
/// compiled. `keep_waiting` is asked about every quarter second with the
/// time spent so far, and ends the wait when it says no. Written over a
/// plain socket: one request and one status line are not worth a client.
pub fn ask_for_page(
    port: u16,
    wait: std::time::Duration,
    keep_waiting: &mut dyn FnMut(std::time::Duration) -> bool,
) -> PageAnswer {
    let began = std::time::Instant::now();
    let mut last;
    loop {
        match connect_loopback(port, CONNECT_TIMEOUT) {
            Err(_) => last = PageAnswer::Refused,
            Ok(stream) => match read_page(stream, port, began, wait, keep_waiting) {
                Reading::Answered(answer) => return answer,
                Reading::GaveUp => return PageAnswer::Silent,
                Reading::Closed => last = PageAnswer::Silent,
            },
        }
        if began.elapsed() >= wait || !keep_waiting(began.elapsed()) {
            return last;
        }
        std::thread::sleep(PAGE_POLL);
    }
}

/// How one connection of [`ask_for_page`] ended.
enum Reading {
    Answered(PageAnswer),
    /// Closed, or broken, before a single byte: worth another try.
    Closed,
    /// The wait was over, or its caller ended it, with nothing said.
    GaveUp,
}

fn read_page(
    mut stream: std::net::TcpStream,
    port: u16,
    began: std::time::Instant,
    wait: std::time::Duration,
    keep_waiting: &mut dyn FnMut(std::time::Duration) -> bool,
) -> Reading {
    use std::io::{Read, Write};
    let request = format!(
        "GET / HTTP/1.1\r\nHost: localhost:{port}\r\nUser-Agent: pando-check\r\n\
         Accept: */*\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err()
        || stream.set_read_timeout(Some(PAGE_POLL)).is_err()
    {
        return Reading::Closed;
    }
    let mut got: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        match stream.read(&mut buf) {
            Ok(0) if got.is_empty() => return Reading::Closed,
            Ok(0) => {
                return Reading::Answered(page_answer(&got, true).unwrap_or(PageAnswer::NotHttp));
            }
            Ok(n) => {
                got.extend_from_slice(&buf[..n]);
                if let Some(answer) = page_answer(&got, false) {
                    return Reading::Answered(answer);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if began.elapsed() >= wait || !keep_waiting(began.elapsed()) {
                    return Reading::GaveUp;
                }
            }
            Err(_) if got.is_empty() => return Reading::Closed,
            Err(_) => {
                return Reading::Answered(page_answer(&got, true).unwrap_or(PageAnswer::NotHttp));
            }
        }
    }
}

/// What the bytes a server sent so far say, or `None` while they could
/// still be the start of an HTTP status line. `ended` is whether no more
/// are coming.
pub fn page_answer(got: &[u8], ended: bool) -> Option<PageAnswer> {
    const PREFIX: &[u8] = b"HTTP/";
    let shared = got.len().min(PREFIX.len());
    if got[..shared] != PREFIX[..shared] {
        return Some(PageAnswer::NotHttp);
    }
    let line_end = got.iter().position(|b| *b == b'\n');
    // `HTTP/1.1 200` is the shortest line that says a status.
    if line_end.is_none() && got.len() < 12 && !ended {
        return None;
    }
    let line = String::from_utf8_lossy(&got[..line_end.unwrap_or(got.len())]);
    let status = line
        .split_whitespace()
        .nth(1)
        .filter(|code| code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|code| code.parse().ok());
    match status {
        Some(code) => Some(PageAnswer::Status(code)),
        None if line_end.is_none() && !ended && got.len() < 64 => None,
        None => Some(PageAnswer::NotHttp),
    }
}

/// The base port for a worktree, before any occupancy probing.
pub fn derive_base(project_id: &str, name: &str) -> u16 {
    let mut input = Vec::with_capacity(project_id.len() + name.len() + 1);
    input.extend_from_slice(project_id.as_bytes());
    input.push(HASH_SEPARATOR);
    input.extend_from_slice(name.as_bytes());
    let digest = md5::compute(&input);
    let num = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    let base = (num % BASE_COUNT) * BASE_STEP as u32 + PORT_MIN as u32;
    base as u16
}

/// `n` consecutive free ports at or after `base`, walking the base grid.
///
/// `is_free` must combine the OS probe with the ports already recorded in
/// state for other worktrees of the project: a stopped worktree still owns
/// its ports, and the OS probe alone would hand them to someone else.
pub fn reserve(base: u16, n: usize, is_free: impl Fn(u16) -> bool) -> Option<Vec<u16>> {
    reserve_window(base, n, |window| window.iter().all(|&p| is_free(p)))
}

/// [`reserve`], judging a whole candidate window at once rather than port
/// by port, for a caller whose answer depends on which role a port would
/// be given.
fn reserve_window(base: u16, n: usize, fits: impl Fn(&[u16]) -> bool) -> Option<Vec<u16>> {
    if n == 0 {
        return Some(Vec::new());
    }
    let mut candidate = align_to_grid(base);
    for _ in 0..MAX_PROBE_ATTEMPTS {
        if let Some(window) = window_at(candidate, n)
            && fits(&window)
        {
            return Some(window);
        }
        candidate = next_base(candidate, n);
    }
    None
}

/// The `n` ports starting at `base`, or `None` when they would run past
/// `PORT_MAX`.
fn window_at(base: u16, n: usize) -> Option<Vec<u16>> {
    let last = base as u32 + n as u32 - 1;
    if last > PORT_MAX as u32 {
        return None;
    }
    Some((0..n as u16).map(|i| base + i).collect())
}

fn align_to_grid(port: u16) -> u16 {
    let clamped = port.clamp(PORT_MIN, PORT_MAX);
    let offset = (clamped - PORT_MIN) % BASE_STEP;
    clamped - offset
}

/// The next base to try, wrapping to `PORT_MIN` once a window of `n` no
/// longer fits below `PORT_MAX`.
fn next_base(base: u16, n: usize) -> u16 {
    let next = base as u32 + BASE_STEP as u32;
    if next + n as u32 - 1 > PORT_MAX as u32 {
        PORT_MIN
    } else {
        next as u16
    }
}

/// Ports for one worktree's roles, and whether they had to move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    pub ports: std::collections::BTreeMap<String, u16>,
    /// True when ports this worktree owned were taken and a new window had
    /// to be found. The caller says so, because a URL the developer had
    /// bookmarked has just changed.
    pub reassigned: bool,
}

/// Assigns a port per role for `name`, recording them in state.
///
/// Ports are stable per worktree: once assigned they are reused on every
/// later start, so a bookmarked URL keeps working across a stop. They move
/// only when something else has taken them in the meantime.
///
/// The freeness test is the OS probe *and* "not recorded for another
/// worktree of this project" — a stopped worktree still owns its ports, and
/// the OS probe alone would hand them to the next caller.
///
/// The worktree's record must already exist: only the caller knows its path
/// and whether pando created it.
pub fn assign(
    paths: &crate::paths::PandoPaths,
    store: &mut crate::state::State,
    name: &str,
    roles: &[String],
) -> anyhow::Result<Assignment> {
    assign_keeping(paths, store, name, roles, &[])
}

/// [`assign`] told which of this worktree's ports one of its *own*
/// processes is already holding.
///
/// A `--only` start leaves the worktree's other processes running, and
/// their listeners make their own ports fail the freeness probe. Without
/// this, starting one process of a pair would decide the pair's ports had
/// been taken and move every one of them — changing the URL of a server
/// that never stopped serving.
pub fn assign_keeping(
    paths: &crate::paths::PandoPaths,
    store: &mut crate::state::State,
    name: &str,
    roles: &[String],
    keep: &[u16],
) -> anyhow::Result<Assignment> {
    assign_with(paths, store, name, roles, keep, is_port_free)
}

/// [`assign`] with the host probe injected.
///
/// Tests drive this one: [`is_port_free`] answers by binding the port for
/// an instant, so two tests probing at the same moment can each see the
/// other's probe and conclude the port is taken. That is a real property of
/// the probe, not a bug — but it makes "the same ports come back" flaky to
/// assert, so the assertions use a probe that does not move.
pub fn assign_with(
    paths: &crate::paths::PandoPaths,
    store: &mut crate::state::State,
    name: &str,
    roles: &[String],
    keep: &[u16],
    is_free: impl Fn(u16) -> bool,
) -> anyhow::Result<Assignment> {
    use anyhow::Context as _;
    use std::collections::{BTreeMap, HashSet};

    if roles.is_empty() {
        if let Some(record) = store.worktrees.get_mut(name) {
            record.ports.clear();
        }
        return Ok(Assignment {
            ports: BTreeMap::new(),
            reassigned: false,
        });
    }

    let taken_by_others: HashSet<u16> = store
        .worktrees
        .iter()
        .filter(|(other, _)| other.as_str() != name)
        .flat_map(|(_, record)| {
            record
                .ports
                .values()
                .copied()
                // A share proxy's port is owned as firmly as any role's:
                // it is not in `ports` only because it must not make this
                // worktree's window look as though its roles had changed.
                .chain(record.share_port)
        })
        .collect();

    let record = store
        .worktrees
        .get_mut(name)
        .with_context(|| format!("no state record for {name}"))?;

    // Reuse only when the recorded set is exactly the roles being asked for:
    // a worktree that grew a second role gets one consecutive window rather
    // than a port here and a port there.
    let recorded: Vec<u16> = roles
        .iter()
        .filter_map(|role| record.ports.get(role).copied())
        .collect();
    if recorded.len() == roles.len() && record.ports.len() == roles.len() {
        // A port one of this worktree's own live processes is holding is
        // not a port somebody took: it is this worktree's, still in use.
        let usable = recorded
            .iter()
            .all(|p| (is_free(*p) || keep.contains(p)) && !taken_by_others.contains(p));
        if usable {
            return Ok(Assignment {
                ports: record.ports.clone(),
                reassigned: false,
            });
        }
    }

    let previous = record.ports.clone();
    let own_share_port = record.share_port;
    let base = derive_base(paths.project_id(), name);
    // A port this worktree's own live process is holding is free *for the
    // role it already has*, and for no other. A shared worktree switching
    // to isolated grows service roles, so the fast path above is skipped —
    // but roles are processes first, so the same window gives the
    // processes the same numbers, and the server kept serving through the
    // switch keeps its URL. Only for the same role: under any other, a
    // kept port still has to pass the probe, so a service is never handed
    // a port a live process is on. One that was kept but is not held any
    // more (a stopped container's) is simply free.
    let fits = |window: &[u16]| {
        roles.iter().zip(window).all(|(role, &port)| {
            let own = keep.contains(&port) && previous.get(role) == Some(&port);
            (own || is_free(port))
                && !taken_by_others.contains(&port)
                && own_share_port != Some(port)
        })
    };
    // The window this worktree already has comes first, and the hash's
    // base only after it. A worktree whose base was taken when it was
    // first given ports sits a base or more along, and re-deriving from
    // the base moved it back the moment the base came free: a switch
    // between isolated and shared changed the URL of an application whose
    // port nobody had taken.
    let own_window = previous
        .values()
        .min()
        .and_then(|&start| window_at(align_to_grid(start), roles.len()))
        .filter(|window| fits(window));
    let window = match own_window {
        Some(window) => window,
        None => reserve_window(base, roles.len(), fits).with_context(|| {
            format!(
                "no free run of {} ports for {name} in {PORT_MIN}..={PORT_MAX}",
                roles.len()
            )
        })?,
    };

    let ports: BTreeMap<String, u16> = roles.iter().cloned().zip(window).collect();
    // Only a role that still exists and really changed number. The reuse
    // fast path above needs the recorded set to be exactly the roles being
    // asked for, so adding or removing a process always re-derives the
    // window — and the new one usually lands on the same numbers. Calling
    // that a reassignment tells the developer their ports were taken, which
    // is a claim about other processes on the machine and is simply untrue.
    let reassigned = ports
        .iter()
        .any(|(role, port)| previous.get(role).is_some_and(|had| had != port));
    record.ports = ports.clone();
    Ok(Assignment { ports, reassigned })
}

/// Reserves the port a worktree's share proxy listens on, leaving every
/// port its processes already hold exactly where it is.
///
/// Additive on purpose. Folded into the role set, `share` would take a slot
/// in the worktree's window that the next process role needs, and that
/// role would have to share the slot or move the window, the port a running
/// application is reached on with it. A share must never do that.
///
/// The port is remembered on the record, so share → unshare → share gives
/// the same number. It is looked for inside the worktree's own eight-port
/// window first, past the roles it already uses, and only walks the base
/// grid when that window is full.
pub fn assign_share_port(
    paths: &crate::paths::PandoPaths,
    store: &mut crate::state::State,
    name: &str,
) -> anyhow::Result<u16> {
    assign_share_port_with(paths, store, name, is_port_free)
}

/// [`assign_share_port`] with the host probe injected, for tests.
pub fn assign_share_port_with(
    paths: &crate::paths::PandoPaths,
    store: &mut crate::state::State,
    name: &str,
    is_free: impl Fn(u16) -> bool,
) -> anyhow::Result<u16> {
    use anyhow::Context as _;
    use std::collections::HashSet;

    let mut taken: HashSet<u16> = store
        .worktrees
        .iter()
        .filter(|(other, _)| other.as_str() != name)
        .flat_map(|(_, record)| record.ports.values().copied().chain(record.share_port))
        .collect();
    let record = store
        .worktrees
        .get_mut(name)
        .with_context(|| format!("no state record for {name}"))?;
    // This worktree's own roles are taken too: the proxy sits beside the
    // application, not on top of it.
    taken.extend(record.ports.values().copied());

    if let Some(port) = record.share_port
        && is_free(port)
        && !taken.contains(&port)
    {
        return Ok(port);
    }

    let base = derive_base(paths.project_id(), name);
    // From the far end of the worktree's own window, walking *down*. The
    // first free port after the roles in use is exactly the port the next
    // role would be given, and `assign_with` may not hand a role the share
    // port — so taking it moved the whole window to another base the first
    // time the project grew a process, under a notice claiming something
    // had taken the worktree's ports. The top of the window is the one
    // place role growth reaches last.
    let found = (0..BASE_STEP)
        .map(|offset| base.saturating_add(BASE_STEP - 1 - offset))
        .find(|port| *port <= PORT_MAX && is_free(*port) && !taken.contains(port))
        .or_else(|| {
            // The worktree's own window is full, so take a whole base of
            // somebody else's grid rather than fail the share.
            reserve(base, 1, |port| is_free(port) && !taken.contains(&port))
                .and_then(|window| window.first().copied())
        })
        .with_context(|| {
            format!("no free port for {name}'s share proxy in {PORT_MIN}..={PORT_MAX}")
        })?;
    record.share_port = Some(found);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const PROJECT: &str = "acme-shop-3f9a2c1d";

    /// A listener that never accepts: the kernel completes the handshake
    /// and the connection waits in the backlog, which is what a server
    /// that has nothing to say looks like from outside.
    fn quiet_server() -> (std::net::TcpListener, u16) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    #[test]
    fn a_server_that_holds_the_socket_open_is_serving() {
        let (_listener, port) = quiet_server();
        assert!(something_is_serving(port));
        assert!(something_is_listening(port));
    }

    /// Docker's published port with nothing listening inside the container:
    /// the connection is accepted and closed at once. `something_is_listening`
    /// cannot tell that from a database, which is the whole point.
    #[test]
    fn a_port_that_answers_and_hangs_up_at_once_is_not_serving() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(2) {
                drop(stream);
            }
        });
        assert!(
            something_is_listening(port),
            "a connect alone cannot tell the difference"
        );
        assert!(!something_is_serving(port), "but one recv can");
        drop(handle);
    }

    #[test]
    fn a_port_nothing_is_on_is_neither() {
        let port = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            listener.local_addr().unwrap().port()
        };
        assert!(!something_is_serving(port));
    }

    #[test]
    fn derive_base_is_deterministic() {
        assert_eq!(
            derive_base(PROJECT, "feat+checkout"),
            derive_base(PROJECT, "feat+checkout")
        );
        assert_ne!(
            derive_base(PROJECT, "feat+checkout"),
            derive_base(PROJECT, "feat+cart")
        );
    }

    #[test]
    fn bases_sit_on_the_grid_and_inside_the_range() {
        for name in [
            "a",
            "b",
            "feat+foo",
            "v4.1",
            "",
            "a-very-long-worktree-name",
        ] {
            let base = derive_base(PROJECT, name);
            assert_eq!(
                (base - PORT_MIN) % BASE_STEP,
                0,
                "{name}: base {base} is off the {BASE_STEP}-port grid"
            );
            assert!(
                (PORT_MIN..=PORT_MAX).contains(&base),
                "{name}: base {base} out of range"
            );
            let last = base + BASE_STEP - 1;
            assert!(
                last <= PORT_MAX,
                "{name}: the base's full window ends at {last}, past PORT_MAX"
            );
        }
    }

    // Without the project id in the hash, two repositories with a branch of
    // the same name would fight over one port.
    #[test]
    fn the_same_name_in_two_projects_gets_different_bases() {
        assert_ne!(
            derive_base("acme-shop-3f9a2c1d", "feat+checkout"),
            derive_base("acme-shop-11111111", "feat+checkout")
        );
    }

    #[test]
    fn the_hash_separator_keeps_split_points_distinct() {
        assert_ne!(derive_base("ab", "c"), derive_base("a", "bc"));
    }

    // A listener on `[::1]` leaves both IPv4 addresses bindable, so a probe
    // that only tries those hands out a port that is already taken.
    #[test]
    fn a_port_held_by_an_ipv6_only_listener_is_not_free() {
        let Ok(listener) = std::net::TcpListener::bind(("::1", 0)) else {
            eprintln!("skipping: no IPv6 loopback on this machine");
            return;
        };
        let port = listener.local_addr().unwrap().port();
        assert!(
            !is_port_free(port),
            "{port} is held by an IPv6 listener and must not be handed out"
        );
        drop(listener);
        // Bounded, not instant: the probe binds three addresses, and on a
        // busy run another thread's probe can hold this one for a moment.
        // What is being pinned is that a released port reads as free.
        assert!(
            crate::testutil::wait_until(std::time::Duration::from_secs(10), || is_port_free(port)),
            "and it is free again once that goes"
        );
    }

    // A listener on `[::]` with `IPV6_V6ONLY` leaves `0.0.0.0`,
    // `127.0.0.1` and, on macOS, `[::1]` bindable: only a probe of `[::]`
    // itself sees it.
    #[test]
    fn a_port_held_by_a_v6_only_wildcard_listener_is_not_free() {
        if !crate::testutil::python3_available() {
            eprintln!("skipping: no python3");
            return;
        }
        let mut child = std::process::Command::new("python3")
            .args([
                "-u",
                "-c",
                "import socket,sys\n\
                 s=socket.socket(socket.AF_INET6)\n\
                 s.setsockopt(socket.IPPROTO_IPV6,socket.IPV6_V6ONLY,1)\n\
                 s.bind(('::',0))\n\
                 s.listen()\n\
                 print(s.getsockname()[1])\n\
                 sys.stdin.read()",
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        let Ok(port) = line.trim().parse::<u16>() else {
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("skipping: no IPv6 on this machine");
            return;
        };
        let free = is_port_free(port);
        let _ = child.kill();
        let _ = child.wait();
        assert!(!free, "{port} is held on [::] and must not be handed out");
    }

    // A machine with no IPv6 at all must not have every port read as taken.
    #[test]
    fn a_free_port_is_free_whatever_this_machine_thinks_of_ipv6() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // Bounded, not instant, for the reason the test above gives: this
        // is an ephemeral port, every other test in the binary is taking
        // and releasing those, and another thread's probe binds three
        // addresses at a time. What is being pinned is that a released
        // port reads as free, not how many microseconds that takes.
        assert!(
            crate::testutil::wait_until(std::time::Duration::from_secs(10), || is_port_free(port)),
            "a released port on a machine without IPv6 must still read as free"
        );
    }

    #[test]
    fn reserve_returns_the_base_window_when_free() {
        assert_eq!(reserve(17_000, 2, |_| true), Some(vec![17_000, 17_001]));
        assert_eq!(reserve(17_000, 1, |_| true), Some(vec![17_000]));
        assert_eq!(reserve(17_000, 0, |_| true), Some(vec![]));
    }

    #[test]
    fn reserve_steps_a_whole_base_past_an_occupied_port() {
        let busy: HashSet<u16> = [17_001].into_iter().collect();
        assert_eq!(
            reserve(17_000, 2, |p| !busy.contains(&p)),
            Some(vec![17_008, 17_009]),
            "a collision moves to the next base, not the next port"
        );
    }

    #[test]
    fn reserve_walks_past_several_occupied_bases() {
        let busy: HashSet<u16> = [17_000, 17_008, 17_016].into_iter().collect();
        assert_eq!(
            reserve(17_000, 4, |p| !busy.contains(&p)),
            Some(vec![17_024, 17_025, 17_026, 17_027])
        );
    }

    #[test]
    fn reserve_aligns_an_off_grid_start_down_to_the_grid() {
        assert_eq!(reserve(17_005, 2, |_| true), Some(vec![17_000, 17_001]));
    }

    #[test]
    fn reserve_wraps_at_the_upper_bound() {
        let last_base = PORT_MIN + (BASE_COUNT as u16 - 1) * BASE_STEP;
        let busy: HashSet<u16> = (last_base..=PORT_MAX).collect();
        assert_eq!(
            reserve(last_base, 2, |p| !busy.contains(&p)),
            Some(vec![PORT_MIN, PORT_MIN + 1])
        );
    }

    #[test]
    fn reserve_gives_up_after_the_attempt_cap() {
        assert_eq!(reserve(17_000, 2, |_| false), None);
    }

    #[test]
    fn reserved_ports_never_leave_the_range() {
        // Sweep every base a name can hash to, including the last one, and
        // ask for more ports than a base's own window holds.
        for k in [0u32, 1, BASE_COUNT / 2, BASE_COUNT - 2, BASE_COUNT - 1] {
            let base = PORT_MIN + (k as u16) * BASE_STEP;
            for n in [1usize, 2, 4, 8, 12] {
                let ports = reserve(base, n, |_| true)
                    .unwrap_or_else(|| panic!("base {base} n {n} found nothing"));
                assert_eq!(ports.len(), n);
                for p in ports {
                    assert!(
                        (PORT_MIN..=PORT_MAX).contains(&p),
                        "base {base} n {n} produced {p}, outside the range"
                    );
                }
            }
        }
    }

    #[test]
    fn reserved_ports_are_consecutive() {
        let ports = reserve(derive_base(PROJECT, "feat+x"), 4, |_| true).unwrap();
        for pair in ports.windows(2) {
            assert_eq!(pair[1], pair[0] + 1);
        }
    }

    // ---- assign ----------------------------------------------------------

    fn assign_fixture() -> (tempfile::TempDir, crate::paths::PandoPaths) {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().join("acme-shop");
        std::fs::create_dir_all(&root).unwrap();
        let project = crate::project::ProjectRef::from_root(&root).unwrap();
        let paths = crate::paths::PandoPaths::new(dir.path().join("pando-home"), project);
        (dir, paths)
    }

    fn with_record(store: &mut crate::state::State, name: &str) {
        store.worktrees.insert(
            name.to_string(),
            crate::state::WorktreeRecord::new(format!("/tmp/{name}"), true),
        );
    }

    fn roles(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// A host with nothing bound on it, so an assertion about which ports
    /// come back is about pando's own rules and not about the machine.
    fn all_free(_: u16) -> bool {
        true
    }

    #[test]
    fn a_share_port_is_reserved_beside_the_roles_and_reused() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let assigned = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "api"]),
            &[],
            all_free,
        )
        .unwrap();

        let share = assign_share_port_with(&paths, &mut store, "feat+one", all_free).unwrap();
        assert!(
            !assigned.ports.values().any(|p| *p == share),
            "the proxy must not sit on a role's port: {share} in {assigned:?}"
        );
        assert_eq!(
            store.worktrees["feat+one"].share_port,
            Some(share),
            "recorded, so the number survives an unshare"
        );
        assert_eq!(
            assign_share_port_with(&paths, &mut store, "feat+one", all_free).unwrap(),
            share,
            "a second share gets the same port"
        );
    }

    // The Phase 3 critical, inverted. Adding a role to the set re-derives
    // the whole window; the share port is kept out of `ports` precisely so
    // that sharing a running worktree cannot move the ports its application
    // is being reached on.
    #[test]
    fn taking_a_share_port_leaves_every_other_port_exactly_where_it_was() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let before = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "api"]),
            &[],
            all_free,
        )
        .unwrap();

        assign_share_port_with(&paths, &mut store, "feat+one", all_free).unwrap();

        assert_eq!(store.worktrees["feat+one"].ports, before.ports);
        let after = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "api"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_eq!(
            after.ports, before.ports,
            "a start after a share must not move a port"
        );
        assert!(!after.reassigned, "and must not claim it did");
    }

    // Finding 6. The share port used to sit immediately after the roles in
    // use — exactly the port the next role would take — and `assign_with`
    // is forbidden to give a role the share port. So the first time the
    // project grew a process, the whole window failed to fit and moved to
    // another base: a worktree that had once been shared lost eight ports
    // and its URL changed, for a reason the developer never caused and
    // under a notice claiming something had taken its ports.
    #[test]
    fn growing_a_role_after_a_share_leaves_the_ports_where_they_would_have_been() {
        let (_dir, paths) = assign_fixture();

        // The control: the same worktree and the same growth, never shared.
        let mut control = crate::state::State::new();
        with_record(&mut control, "feat+one");
        assign_with(
            &paths,
            &mut control,
            "feat+one",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        let grown = assign_with(
            &paths,
            &mut control,
            "feat+one",
            &roles(&["api", "web"]),
            &[],
            all_free,
        )
        .unwrap();

        let mut shared = crate::state::State::new();
        with_record(&mut shared, "feat+one");
        assign_with(
            &paths,
            &mut shared,
            "feat+one",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        let share = assign_share_port_with(&paths, &mut shared, "feat+one", all_free).unwrap();
        let after = assign_with(
            &paths,
            &mut shared,
            "feat+one",
            &roles(&["api", "web"]),
            &[],
            all_free,
        )
        .unwrap();

        assert_eq!(
            after.ports, grown.ports,
            "having once been shared cost the worktree its whole window"
        );
        assert_eq!(
            after.reassigned, grown.reassigned,
            "a share must not change what growing a role does, in either direction"
        );
        assert!(
            !after.ports.values().any(|p| *p == share),
            "the proxy's own port {share} went to a role: {after:?}"
        );
    }

    #[test]
    fn a_share_port_is_taken_from_the_top_of_the_worktrees_own_window() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();

        let base = derive_base(paths.project_id(), "feat+one");
        let share = assign_share_port_with(&paths, &mut store, "feat+one", all_free).unwrap();
        assert_eq!(
            share,
            base + BASE_STEP - 1,
            "the far end of the window, so role growth never reaches it"
        );

        // …and a foreign listener on it still makes the proxy walk, down
        // rather than up, into the same window.
        let mut squatted = crate::state::State::new();
        with_record(&mut squatted, "feat+two");
        let base_two = derive_base(paths.project_id(), "feat+two");
        let moved = assign_share_port_with(&paths, &mut squatted, "feat+two", |port| {
            port != base_two + BASE_STEP - 1
        })
        .unwrap();
        assert_eq!(moved, base_two + BASE_STEP - 2);
    }

    #[test]
    fn a_share_port_is_never_a_port_another_worktree_owns() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        with_record(&mut store, "feat+two");
        let one = assign_share_port_with(&paths, &mut store, "feat+one", all_free).unwrap();

        // Neighbour's roles must avoid it…
        let two = assign_with(
            &paths,
            &mut store,
            "feat+two",
            &roles(&["web", "api", "db", "cache"]),
            &[],
            // Everything free on the host, so only pando's own bookkeeping
            // can keep the two apart.
            all_free,
        )
        .unwrap();
        assert!(
            !two.ports.values().any(|p| *p == one),
            "another worktree took the share port {one}: {two:?}"
        );

        // …and so must its own share port.
        let two_share = assign_share_port_with(&paths, &mut store, "feat+two", all_free).unwrap();
        assert_ne!(two_share, one);
    }

    #[test]
    fn a_share_port_that_something_else_has_taken_moves() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let first = assign_share_port_with(&paths, &mut store, "feat+one", all_free).unwrap();

        let moved =
            assign_share_port_with(&paths, &mut store, "feat+one", |port| port != first).unwrap();
        assert_ne!(moved, first);
        assert_eq!(store.worktrees["feat+one"].share_port, Some(moved));
    }

    #[test]
    fn a_share_port_stays_inside_the_range() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let port = assign_share_port_with(&paths, &mut store, "feat+one", all_free).unwrap();
        assert!((PORT_MIN..=PORT_MAX).contains(&port), "{port}");
    }

    #[test]
    fn a_worktree_with_no_record_cannot_take_a_share_port() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        assert!(assign_share_port_with(&paths, &mut store, "nope", all_free).is_err());
    }

    #[test]
    fn assign_records_a_port_per_role_and_reuses_it() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");

        let first = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "api"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_eq!(first.ports.len(), 2);
        assert!(!first.reassigned);
        assert_eq!(first.ports["api"], first.ports["web"] + 1, "consecutive");
        assert_eq!(
            store.worktrees["feat+one"].ports, first.ports,
            "the assignment is recorded, so a stopped worktree keeps its ports"
        );

        let second = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "api"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_eq!(second.ports, first.ports, "ports are stable per worktree");
        assert!(!second.reassigned);
    }

    // The OS probe alone would hand a stopped worktree's ports to the next
    // caller, and the URL the developer had open would start serving someone
    // else's branch.
    #[test]
    fn a_second_worktree_never_takes_the_ports_another_one_owns() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        with_record(&mut store, "feat+two");

        let one = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        store.worktrees.get_mut("feat+two").unwrap().ports.clear();
        let two = assign_with(
            &paths,
            &mut store,
            "feat+two",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_ne!(one.ports["web"], two.ports["web"]);

        // And explicitly: a worktree asking for a port already recorded
        // elsewhere is moved off it.
        let stolen = one.ports["web"];
        store
            .worktrees
            .get_mut("feat+two")
            .unwrap()
            .ports
            .insert("web".to_string(), stolen);
        let again = assign_with(
            &paths,
            &mut store,
            "feat+two",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_ne!(
            again.ports["web"], stolen,
            "a port recorded for another worktree is not free"
        );
        assert!(again.reassigned);
    }

    #[test]
    fn a_recorded_port_that_something_else_bound_moves_and_says_so() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");

        // Bind first and record that port as this worktree's, rather than
        // assigning and then racing to bind what was just probed free — the
        // range overlaps the OS ephemeral range, so that race is real.
        let squatter = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let held = squatter.local_addr().unwrap().port();
        store
            .worktrees
            .get_mut("feat+one")
            .unwrap()
            .ports
            .insert("web".to_string(), held);

        let moved = assign(&paths, &mut store, "feat+one", &roles(&["web"])).unwrap();
        assert_ne!(moved.ports["web"], held, "a bound port is not reusable");
        assert!(
            (PORT_MIN..=PORT_MAX).contains(&moved.ports["web"]),
            "and the real host probe is what `assign` uses"
        );
        assert!(
            moved.reassigned,
            "a moved port is worth telling the user about"
        );
        drop(squatter);
    }

    // A worktree that grows a role gets one consecutive window rather than
    // its old port plus whatever happened to be next to it.
    #[test]
    fn adding_a_role_reassigns_the_whole_window() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        let grown = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "api"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_eq!(grown.ports.len(), 2);
        assert_eq!(grown.ports["api"], grown.ports["web"] + 1);
        assert_eq!(store.worktrees["feat+one"].ports.len(), 2);
    }

    // Phase 2b review, finding 8. Adding a process re-derives the window,
    // and the new one usually lands on exactly the numbers the old one had.
    // "the ports it had were taken; it moved to new ones" then states
    // something false about other processes on the machine — and adding a
    // process to a workspace config is a normal edit.
    #[test]
    fn a_window_that_lands_on_the_same_numbers_is_not_a_reassignment() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+det");
        let two = assign_with(
            &paths,
            &mut store,
            "feat+det",
            &roles(&["api", "web"]),
            &[],
            all_free,
        )
        .unwrap();
        assert!(!two.reassigned, "nothing was there to move");

        let three = assign_with(
            &paths,
            &mut store,
            "feat+det",
            &roles(&["api", "web", "worker"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_eq!(three.ports["api"], two.ports["api"]);
        assert_eq!(three.ports["web"], two.ports["web"]);
        assert!(
            !three.reassigned,
            "a third role is not two ports moving: {:?} → {:?}",
            two.ports, three.ports
        );

        // A role that goes away is not a move either: what is left kept
        // every number it had.
        let mut shrunk = store.clone();
        let fewer = assign_with(
            &paths,
            &mut shrunk,
            "feat+det",
            &roles(&["api", "web"]),
            &[],
            all_free,
        )
        .unwrap();
        assert!(!fewer.reassigned, "{:?} → {:?}", three.ports, fewer.ports);

        // And a port that really did move still says so.
        let web = three.ports["web"];
        let moved = assign_with(
            &paths,
            &mut store,
            "feat+det",
            &roles(&["api", "web", "worker"]),
            &[],
            move |port| port != web,
        )
        .unwrap();
        assert_ne!(moved.ports["web"], web);
        assert!(moved.reassigned);
    }

    // A `--only` start leaves the worktree's other processes running, and a
    // running listener makes its own port fail the freeness probe. Read as
    // "somebody took it", that moves every port the worktree owns — while
    // the server holding one of them is still serving on the old number.
    #[test]
    fn a_port_this_worktrees_own_process_holds_is_not_one_somebody_took() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let first = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["api", "web"]),
            &[],
            all_free,
        )
        .unwrap();
        let web = first.ports["web"];
        // The worktree's own web server is up, so its port no longer binds.
        let web_is_busy = move |port: u16| port != web;

        let mut untold = store.clone();
        let moved = assign_with(
            &paths,
            &mut untold,
            "feat+one",
            &roles(&["api", "web"]),
            &[],
            web_is_busy,
        )
        .unwrap();
        assert!(
            moved.reassigned && moved.ports["web"] != web,
            "without being told, the probe cannot tell our own listener from a squatter"
        );

        let told = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["api", "web"]),
            &[web],
            web_is_busy,
        )
        .unwrap();
        assert_eq!(
            told.ports, first.ports,
            "the ports stay exactly where they were"
        );
        assert!(!told.reassigned);
    }

    // A live shared → isolated switch adds service roles, so the reuse
    // path is skipped and the window is re-derived while the dev server
    // kept serving through the switch still holds its port. Its own port
    // is free for its own role, so the same window comes back and the URL
    // does not move; a service role is never handed a port a live process
    // is on.
    #[test]
    fn growing_service_roles_keeps_the_port_a_live_process_holds() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let shared = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        let web = shared.ports["web"];
        let web_is_busy = move |port: u16| port != web;

        let isolated = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "postgres"]),
            &[web],
            web_is_busy,
        )
        .unwrap();
        assert_eq!(isolated.ports["web"], web, "the URL stays where it was");
        assert_eq!(isolated.ports["postgres"], web + 1);
        assert!(!isolated.reassigned, "nothing was taken");

        // The same kept port under another role is not free: the window
        // moves rather than giving the database the dev server's port.
        let mut other = crate::state::State::new();
        with_record(&mut other, "feat+one");
        let moved = assign_with(
            &paths,
            &mut other,
            "feat+one",
            &roles(&["web", "postgres"]),
            &[web],
            web_is_busy,
        )
        .unwrap();
        assert!(!moved.ports.values().any(|&p| p == web));
    }

    // A worktree whose base was taken when it was first given ports sits
    // a base along. A change of roles — isolated to shared, and back —
    // re-derived its window from the base, so once the base was free again
    // the web port moved back to it: `start --shared` changed the URL of
    // an application whose port nobody had taken.
    #[test]
    fn a_change_of_roles_keeps_the_window_the_worktree_already_has() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let base = derive_base(paths.project_id(), "feat+one");
        let isolated = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "postgres", "redis"]),
            &[],
            move |port| port != base,
        )
        .unwrap();
        let web = isolated.ports["web"];
        assert_ne!(web, base, "the base was taken, so the worktree sits along");

        let shared = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_eq!(shared.ports["web"], web, "the URL stays where it was");
        assert!(!shared.reassigned);

        let again = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web", "postgres", "redis"]),
            &[],
            all_free,
        )
        .unwrap();
        assert_eq!(again.ports, isolated.ports, "and back again");
        assert!(!again.reassigned);

        // Its own window taken, the base's is next, as it always was.
        let moved = assign_with(
            &paths,
            &mut store,
            "feat+one",
            &roles(&["web"]),
            &[],
            move |port| port != web,
        )
        .unwrap();
        assert_eq!(moved.ports["web"], base);
        assert!(moved.reassigned);
    }

    #[test]
    fn a_process_with_no_roles_gets_no_ports() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        with_record(&mut store, "feat+one");
        let assigned = assign(&paths, &mut store, "feat+one", &[]).unwrap();
        assert!(assigned.ports.is_empty());
        assert!(store.worktrees["feat+one"].ports.is_empty());
    }

    #[test]
    fn assigning_for_a_worktree_with_no_record_is_an_error() {
        let (_dir, paths) = assign_fixture();
        let mut store = crate::state::State::new();
        let err = assign(&paths, &mut store, "ghost", &roles(&["web"])).unwrap_err();
        assert!(format!("{err:#}").contains("ghost"));
    }

    #[test]
    fn is_port_free_reports_a_bound_port_as_taken() {
        let listener = std::net::TcpListener::bind(("0.0.0.0", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!is_port_free(port), "a bound port must not read as free");
        drop(listener);
    }

    // A dev server that binds loopback only leaves `0.0.0.0:<port>`
    // bindable, so probing one address would report it free and readiness
    // would never arrive.
    #[test]
    fn a_loopback_only_listener_is_not_free_either() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        // The premise holds on macOS, where this was found. Linux refuses
        // the wildcard while loopback holds the port, so there a probe of
        // either address already says it is taken.
        #[cfg(target_os = "macos")]
        assert!(
            std::net::TcpListener::bind(("0.0.0.0", port)).is_ok(),
            "the premise: the wildcard address is still bindable"
        );
        assert!(!is_port_free(port), "but the port is in use");
        drop(listener);
    }

    // The page probe of `pando check`.

    /// A server on a free loopback port that takes one connection, hands
    /// back what it was sent, and replies `reply` — or, with `None`, holds
    /// the connection and says nothing.
    fn one_answer(reply: Option<&'static [u8]>) -> (u16, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut buf = [0u8; 1024];
            let n = stream.read(&mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
            match reply {
                Some(bytes) => {
                    let _ = stream.write_all(bytes);
                }
                None => std::thread::sleep(std::time::Duration::from_secs(3)),
            }
        });
        (port, rx)
    }

    fn ask(port: u16, wait_ms: u64) -> PageAnswer {
        ask_for_page(port, std::time::Duration::from_millis(wait_ms), &mut |_| {
            true
        })
    }

    #[test]
    fn a_page_answer_is_its_status_whatever_the_status_is() {
        let (port, sent) = one_answer(Some(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"));
        assert_eq!(ask(port, 5_000), PageAnswer::Status(404));
        let request = sent.recv().unwrap();
        assert!(request.starts_with("GET / HTTP/1.1\r\n"), "{request}");
        assert!(
            request.contains(&format!("\r\nHost: localhost:{port}\r\n")),
            "asked as the browser at localhost asks: {request}"
        );

        let (port, _) = one_answer(Some(b"HTTP/1.0 500 Internal Server Error\r\n\r\n"));
        assert_eq!(ask(port, 5_000), PageAnswer::Status(500));
    }

    #[test]
    fn a_port_nothing_listens_on_is_refused_once_the_wait_is_over() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let began = std::time::Instant::now();
        assert_eq!(ask(port, 300), PageAnswer::Refused);
        assert!(began.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn a_tls_alert_answers_but_is_not_http() {
        // What a server that wants TLS sends back to a plain request.
        let (port, _) = one_answer(Some(b"\x15\x03\x01\x00\x02\x02\x46"));
        assert_eq!(ask(port, 5_000), PageAnswer::NotHttp);
    }

    #[test]
    fn a_server_that_says_nothing_is_silent_and_the_caller_can_end_the_wait() {
        let (port, _) = one_answer(None);
        assert_eq!(ask(port, 400), PageAnswer::Silent);

        let (port, _) = one_answer(None);
        let began = std::time::Instant::now();
        let mut asked = 0;
        let answer = ask_for_page(port, std::time::Duration::from_secs(60), &mut |_| {
            asked += 1;
            asked < 2
        });
        assert_eq!(answer, PageAnswer::Silent);
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "a caller that says stop is not made to sit out the wait"
        );
    }

    #[test]
    fn the_first_bytes_decide_a_page_answer_only_once_they_can() {
        assert_eq!(page_answer(b"", false), None);
        assert_eq!(page_answer(b"HTT", false), None);
        assert_eq!(page_answer(b"HTTP/1.1 2", false), None);
        assert_eq!(
            page_answer(b"HTTP/1.1 204", false),
            Some(PageAnswer::Status(204))
        );
        assert_eq!(
            page_answer(b"HTTP/1.1 301 Moved\r\n", false),
            Some(PageAnswer::Status(301))
        );
        assert_eq!(page_answer(b"HTTP/1.1 2", true), Some(PageAnswer::NotHttp));
        assert_eq!(
            page_answer(b"SSH-2.0-OpenSSH\r\n", false),
            Some(PageAnswer::NotHttp)
        );
        assert_eq!(
            page_answer(b"HTTP/1.1 abc\r\n", false),
            Some(PageAnswer::NotHttp)
        );
    }

    #[test]
    fn only_this_machine_is_a_loopback_host() {
        for host in [
            "localhost",
            "LOCALHOST",
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "[::1]",
        ] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in ["db", "db.example.com", "10.0.0.5", "0.0.0.0", ""] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn a_page_wait_is_shortened_on_this_thread_alone() {
        assert_eq!(page_wait(), PAGE_WAIT);
        let short = std::time::Duration::from_millis(5);
        let inside = with_page_wait(short, || {
            let other = std::thread::spawn(page_wait).join().unwrap();
            (page_wait(), other)
        });
        assert_eq!(inside, (short, PAGE_WAIT));
        assert_eq!(page_wait(), PAGE_WAIT);
    }
}
