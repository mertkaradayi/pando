//! Commands that run at a point in a worktree's life, at most as often as
//! they need to.
//!
//! This phase has exactly one, the built-in install step, but it is built as
//! the general mechanism rather than a special case: a hook is a name, a
//! command, and a set of globs whose content decides whether it has to run
//! again. `[[hooks]]` in Phase 3 is the same thing with the name and
//! command coming from config.
//!
//! A hook's output goes to its own log file under pando's home, never to
//! the terminal: the TUI paints an alternate screen, and a child that
//! writes to stdout would paint over it.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::platform::signals::{self, HangUpOnExit};

/// Whether hooks and probes start with no controlling terminal; see
/// [`detach_from_terminal`].
static DETACHED: AtomicBool = AtomicBool::new(false);

/// Directories never walked when matching a fingerprint glob. Dependency
/// trees and build output are enormous and are never what a hook is keyed
/// on.
const PRUNED: [&str; 10] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".venv",
    "vendor",
    "__pycache__",
];

/// How deep a wildcard glob is walked. Deep enough for `*/migrations/*.sql`
/// in a monorepo, shallow enough that a pathological tree cannot stall a
/// start.
const MAX_DEPTH: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookOutcome {
    Ran,
    /// Its fingerprint has not changed since the last run.
    Skipped,
}

/// A content hash over the files the globs match *and* the command itself,
/// or `None` when the globs match nothing at all.
///
/// `None` is "no fingerprint", which means the hook runs every time — the
/// same thing an empty `fingerprint` list means. A project with no lockfile
/// has no way to tell pando its dependencies are unchanged.
///
/// The command is part of the hash because editing a hook is a reason to
/// run it again: keying on the files alone meant a migration command
/// corrected in `pando.toml` never ran, since the migrations it watches
/// had not changed.
pub fn fingerprint(worktree: &Path, globs: &[String], cmd: &str) -> Option<String> {
    fingerprint_with(worktree, globs, &[], cmd)
}

/// [`fingerprint`], with more files in the hash that cannot make one on
/// their own: `None` still means the globs matched nothing, whatever
/// `also` matches.
///
/// The install step's runtime pins are these. A changed `.nvmrc` is a
/// reason to install again, but a project with no lockfile still has
/// nothing that says its dependencies are unchanged, and an `.nvmrc`
/// alone must not start saying so.
pub fn fingerprint_with(
    worktree: &Path,
    globs: &[String],
    also: &[String],
    cmd: &str,
) -> Option<String> {
    let keyed = matched(worktree, globs);
    if keyed.is_empty() {
        return None;
    }
    let pinned = matched(worktree, also);
    let mut context = md5::Context::new();
    for relative in keyed.iter().chain(&pinned) {
        // The path is hashed too: a file renamed is a change, even when the
        // bytes are the same.
        context.consume(relative.to_string_lossy().as_bytes());
        context.consume([0]);
        match std::fs::read(worktree.join(relative)) {
            Ok(bytes) => context.consume(&bytes),
            // A file that vanished between the walk and the read is a
            // change like any other.
            Err(_) => context.consume(b"<unreadable>"),
        }
        context.consume([0]);
    }
    context.consume(cmd.as_bytes());
    context.consume([0]);
    Some(format!("md5:{:x}", context.finalize()))
}

/// The files a hook's globs match, relative to the worktree, sorted and
/// deduplicated.
///
/// Split out of [`fingerprint`] because `doctor` reports *what* a hook is
/// keyed on as well as whether the hash would change, and a hook keyed on
/// nothing runs on every start.
pub fn matched(worktree: &Path, globs: &[String]) -> Vec<PathBuf> {
    if globs.is_empty() {
        return Vec::new();
    }
    let mut matched: Vec<PathBuf> = Vec::new();
    for glob in globs {
        if is_literal(glob) {
            // The common case by far, and it needs no walk at all.
            let path = worktree.join(glob);
            if path.is_file() {
                matched.push(PathBuf::from(glob));
            }
        } else {
            collect(worktree, Path::new(""), glob, 0, &mut matched);
        }
    }
    matched.sort();
    matched.dedup();
    matched
}

fn is_literal(glob: &str) -> bool {
    !glob.contains(['*', '?'])
}

fn collect(worktree: &Path, relative: &Path, glob: &str, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(worktree.join(relative)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if PRUNED.contains(&name.as_ref()) {
            continue;
        }
        let child = relative.join(name.as_ref());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect(worktree, &child, glob, depth + 1, out);
        } else if matches_glob(glob, &child.to_string_lossy()) {
            out.push(child);
        }
    }
}

/// Path glob matching: `*` and `?` inside one segment, `**` across any
/// number of them.
pub fn matches_glob(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    match_segments(&pattern, &path)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(&"**") => {
            // Zero or more segments: try every split point.
            (0..=path.len()).any(|skip| match_segments(&pattern[1..], &path[skip..]))
        }
        Some(segment) => match path.first() {
            Some(name) if match_one(segment, name) => match_segments(&pattern[1..], &path[1..]),
            _ => false,
        },
    }
}

/// `*` matches any run of characters inside a segment, `?` exactly one.
fn match_one(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    let (mut pi, mut vi) = (0usize, 0usize);
    let (mut star, mut star_vi) = (None, 0usize);
    while vi < v.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_vi = vi;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            star_vi += 1;
            vi = star_vi;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Starts every hook and probe from now on in a session of its own, with
/// no controlling terminal.
///
/// For the TUI, once it owns the screen. A tool in a hook that prompts on
/// `/dev/tty` — git asking for a username, ssh for a passphrase, sudo —
/// drew over the frame and waited for ever on keys the TUI read first;
/// with no terminal the prompt fails at once, and what it printed is the
/// hook's reason. From the CLI the terminal is the developer's, and a hook
/// that asks is answered there.
///
/// A hook in pando's own session gets the terminal's SIGHUP when the
/// window closes or the pane is killed; one in a session of its own hears
/// nothing, and would go on installing into a worktree nothing waits for.
/// So from here on pando hangs up on them itself before a SIGHUP or a
/// SIGTERM ends it: [`hang_up_detached`].
pub fn detach_from_terminal() {
    DETACHED.store(true, Ordering::Relaxed);
    signals::hang_up_on_exit();
}

/// Hangs up on the process group of every detached hook and probe still
/// running, and on any that starts after this: what each would have had
/// from the terminal had it shared the TUI's.
///
/// For the TUI when it goes. Nothing waits for a hook once it has, and an
/// install left running would go on writing into the worktree beside the
/// one the next start runs.
pub fn hang_up_detached() {
    signals::hang_up_now();
}

/// Spawns `command` and hands the child to `wait`: when `detached`, as a
/// group of its own with no terminal, held for a hang-up until `wait`
/// returns.
fn waited<T>(
    command: &mut Command,
    detached: bool,
    wait: impl FnOnce(Child) -> std::io::Result<T>,
) -> std::io::Result<T> {
    if !detached {
        return wait(command.spawn()?);
    }
    let (child, group) = crate::platform::process::spawn_group(command)?;
    let _held = HangUpOnExit::hold(group);
    wait(child)
}

/// What a hook is not started for once pando has been told to stop.
pub const NOT_STARTED: &str = "not started, since pando was told to stop";

/// What a hook pando stopped, on being told to stop itself, ends with.
pub const STOPPED: &str = "stopped, since pando was told to stop";

/// How long a hook pando stops has between SIGTERM and SIGKILL: an install
/// told to stop gets to finish the file it is writing.
const STOP_GRACE: Duration = Duration::from_secs(5);

/// How often a hook pando may have to stop is looked at, for its end or
/// for pando's.
const POLL: Duration = Duration::from_millis(50);

/// [`waited`] for a hook, which is stopped when pando is.
///
/// Never started once pando has been told to stop. And while pando
/// catches interrupts — `check`, a `new` from the CLI — a hook that a
/// terminal's Ctrl-C would not reach leads a group of its own, which
/// pando stops the moment it is told to: an agent's pando, CI's, one in
/// the background, or a `kill` of pando alone. Before, it ran in pando's
/// group and was waited for to its end, so an interrupt that landed
/// between the hook's log being opened and its spawn reached pando and
/// not the install, and the check sat until the install ended by itself.
///
/// In front of a terminal it keeps pando's group, as before: the
/// terminal's Ctrl-C reaches it as it reaches pando, and a tool in it can
/// still prompt there, which a background group cannot.
fn run_to_end(command: &mut Command, detached: bool) -> std::io::Result<ExitStatus> {
    if signals::interrupted() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            NOT_STARTED,
        ));
    }
    let catching = signals::catching();
    let (mut child, group, _held) = if detached {
        let (child, group) = crate::platform::process::spawn_group(command)?;
        (child, group, Some(HangUpOnExit::hold(group)))
    } else if catching && !crate::platform::terminal::in_foreground() {
        let (child, group) = crate::platform::process::spawn_in_group(command)?;
        (child, group, None)
    } else {
        return command.spawn()?.wait();
    };
    if !catching {
        return child.wait();
    }
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if signals::interrupted() {
            // The stop reaps the group's leader, which is the child: there
            // is no status left to wait for.
            let _ = crate::platform::process::stop(group, STOP_GRACE);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                STOPPED,
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// `bash -lc` running `shell_cmd` in `cwd` with `env` added and nothing on
/// stdin. [`waited`] decides whether it gets a terminal.
fn shell(shell_cmd: &str, cwd: &Path, env: &[(String, String)]) -> std::io::Result<Command> {
    let mut command = crate::platform::shell::login(shell_cmd)?;
    command.current_dir(cwd).stdin(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    Ok(command)
}

/// Runs a hook to completion, appending everything it printed to its own
/// log. Fails with the log path, because that is where the reason is.
///
/// The log file is the hook's stdout and its stderr both, rather than an
/// inherited stdio: nothing pando runs may write to the terminal, and a
/// hook started from the TUI would otherwise paint over the frame. The
/// child is handed the file itself, not a pipe pando drains once it exits,
/// so the log fills while a three-minute install runs, keeps its lines in
/// the order they were printed, and keeps them when pando is interrupted
/// before the hook finishes. A hook the TUI starts has no terminal at all
/// to open: [`detach_from_terminal`].
pub fn run(log_file: &Path, shell_cmd: &str, cwd: &Path, env: &[(String, String)]) -> Result<()> {
    if let Some(parent) = log_file.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create log dir {}", parent.display()))?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file)
        .with_context(|| format!("open log file {}", log_file.display()))?;
    // Where this run's output begins, so the reason a failure gives is
    // this run's closing line and never an earlier run's.
    let start = out.metadata().map(|m| m.len()).unwrap_or(0);
    let err = out
        .try_clone()
        .context("clone log file handle for stderr")?;
    let detached = DETACHED.load(Ordering::Relaxed);
    let mut command = shell(shell_cmd, cwd, env).with_context(|| format!("run {shell_cmd:?}"))?;
    command.stdout(out).stderr(err);
    let status =
        run_to_end(&mut command, detached).with_context(|| format!("run {shell_cmd:?}"))?;

    // A hook's last line ends in a newline, so whatever the log gets next
    // starts a line of its own.
    let text = printed_since(log_file, start);
    if !text.is_empty() && !text.ends_with('\n') {
        append(log_file, "\n");
    }

    if status.success() {
        return Ok(());
    }
    let reason = last_line(&text);
    anyhow::bail!(
        "exited {}{reason} — the whole log is at {}",
        status.code().unwrap_or(-1),
        log_file.display()
    )
}

/// How much of a run's output is read back for its reason. The closing
/// line is all that is wanted, and an install's log can be large.
const TAIL_BYTES: u64 = 16 * 1024;

/// The end of what a log gained after `start`, at most [`TAIL_BYTES`] of
/// it. Empty when the log cannot be read back.
fn printed_since(log_file: &Path, start: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(log_file) else {
        return String::new();
    };
    let end = file.metadata().map(|m| m.len()).unwrap_or(0);
    let from = start.max(end.saturating_sub(TAIL_BYTES));
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(from)).is_err() || file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Runs a pre-start check whose *answer* is its output rather than its
/// effect: `Ok(None)` when it succeeded, `Ok(Some(stderr))` when it did
/// not.
///
/// No log file. A probe is a question pando asks before it starts
/// anything, and the only thing it can produce is the hint the developer
/// sees; a `logs --source native-abi` tab for it would be empty on every
/// run that mattered.
pub fn probe(shell_cmd: &str, cwd: &Path, env: &[(String, String)]) -> Result<Option<String>> {
    let detached = DETACHED.load(Ordering::Relaxed);
    let mut command =
        shell(shell_cmd, cwd, env).with_context(|| format!("run the probe {shell_cmd:?}"))?;
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = waited(&mut command, detached, Child::wait_with_output)
        .with_context(|| format!("run the probe {shell_cmd:?}"))?;
    if out.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// [`run`], with a second command tried when the first one fails.
///
/// The error reported is the *fallback's*. A hook with a fallback has two
/// ways of doing one job; when both are gone, the reason the second one
/// failed is the one that says what is actually wrong with the machine —
/// the first one's reason is why the fallback exists at all. It is still
/// named, so nobody has to guess which command the message came from.
pub fn run_with_fallback(
    log_file: &Path,
    shell_cmd: &str,
    fallback: Option<&str>,
    cwd: &Path,
    env: &[(String, String)],
) -> Result<()> {
    let Err(first) = run(log_file, shell_cmd, cwd, env) else {
        return Ok(());
    };
    let Some(fallback) = fallback else {
        return Err(first);
    };
    // The first one failed because pando was told to stop, most likely,
    // and nothing starts after that.
    if signals::interrupted() {
        return Err(first);
    }
    append(
        log_file,
        &format!("\npando: {shell_cmd} failed; trying the fallback\n"),
    );
    run(log_file, fallback, cwd, env)
        .with_context(|| format!("the fallback ran because {shell_cmd:?} failed ({first:#})"))
}

fn append(path: &Path, text: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(text.as_bytes());
        if !text.ends_with('\n') {
            let _ = file.write_all(b"\n");
        }
    }
}

/// The closing line of the output, which is where a build tool puts the
/// reason it stopped.
/// The line of a failed run's output that says why: the last one that
/// reads as an error, or else the last one that says anything at all.
///
/// The closing line alone was often nothing — a `}` ending a printed
/// error object, a stack frame — while the sentence that said what went
/// wrong sat a few lines up. Stack frames (`at …`) and lines of bare
/// punctuation are passed over; a line naming an error, a failure, a
/// refusal or a missing thing is preferred to one that does not.
fn last_line(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| says_something(l))
        .collect();
    let reason = lines
        .iter()
        .rev()
        .find(|l| reads_as_an_error(l))
        .or(lines.last());
    match reason {
        Some(line) => format!(": {}", clip(line, REASON_CHARS)),
        None => String::new(),
    }
}

/// How much of a reason line is kept: it goes into a sentence, and the
/// whole log is a path away.
const REASON_CHARS: usize = 240;

/// Whether a line of output has words in it: not blank, not bare
/// punctuation, not a frame of a stack trace.
fn says_something(line: &str) -> bool {
    line.chars().any(char::is_alphanumeric) && !line.starts_with("at ")
}

fn reads_as_an_error(line: &str) -> bool {
    const SIGNS: [&str; 13] = [
        "error",
        "err!",
        "failed",
        "failure",
        "exception",
        "denied",
        "refused",
        "not found",
        "no such",
        "cannot",
        "can't",
        "panicked",
        "fatal error",
    ];
    let lower = line.to_lowercase();
    SIGNS.iter().any(|sign| lower.contains(sign))
}

fn clip(line: &str, chars: usize) -> String {
    match line.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &line[..at]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// The command a fingerprint test that is about the *files* uses.
    /// Shadowing the real function keeps those tests reading the way they
    /// did before the command joined the hash.
    fn fingerprint(worktree: &Path, globs: &[String]) -> Option<String> {
        super::fingerprint(worktree, globs, "pnpm install --frozen-lockfile")
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    // ---- globs -----------------------------------------------------------

    #[test]
    fn a_glob_matches_within_one_segment() {
        assert!(matches_glob("pnpm-lock.yaml", "pnpm-lock.yaml"));
        assert!(matches_glob("*.lock", "uv.lock"));
        assert!(matches_glob("a/b.txt", "a/b.txt"));
        assert!(!matches_glob("*.lock", "a/uv.lock"));
        assert!(!matches_glob("a/b.txt", "a/c.txt"));
        assert!(matches_glob("?.txt", "a.txt"));
        assert!(!matches_glob("?.txt", "ab.txt"));
    }

    #[test]
    fn a_double_star_matches_any_number_of_directories() {
        assert!(matches_glob(
            "prisma/migrations/**",
            "prisma/migrations/1/up.sql"
        ));
        assert!(matches_glob("prisma/migrations/**", "prisma/migrations/x"));
        assert!(matches_glob("**/*.py", "app/migrations/0001.py"));
        assert!(matches_glob("**/*.py", "a.py"));
        assert!(!matches_glob("**/*.py", "a.rs"));
        assert!(matches_glob("*/migrations/*.py", "app/migrations/0001.py"));
        assert!(!matches_glob("*/migrations/*.py", "a/b/migrations/0001.py"));
    }

    // ---- fingerprints ----------------------------------------------------

    #[test]
    fn a_fingerprint_changes_only_when_the_content_does() {
        let dir = tempdir().unwrap();
        write(dir.path(), "pnpm-lock.yaml", "one\n");
        let globs = vec!["pnpm-lock.yaml".to_string()];
        let first = fingerprint(dir.path(), &globs).unwrap();
        assert_eq!(fingerprint(dir.path(), &globs).unwrap(), first);

        write(dir.path(), "pnpm-lock.yaml", "two\n");
        assert_ne!(fingerprint(dir.path(), &globs).unwrap(), first);

        // Something else changing is not this hook's business.
        write(dir.path(), "README.md", "hello");
        let second = fingerprint(dir.path(), &globs).unwrap();
        write(dir.path(), "README.md", "goodbye");
        assert_eq!(fingerprint(dir.path(), &globs).unwrap(), second);
    }

    #[test]
    fn nothing_to_fingerprint_means_run_every_time() {
        let dir = tempdir().unwrap();
        assert_eq!(fingerprint(dir.path(), &[]), None);
        assert_eq!(fingerprint(dir.path(), &["absent.lock".to_string()]), None);
    }

    #[test]
    fn a_wildcard_fingerprint_covers_every_file_it_matches() {
        let dir = tempdir().unwrap();
        write(dir.path(), "prisma/migrations/1_init/up.sql", "create;");
        let globs = vec!["prisma/migrations/**".to_string()];
        let first = fingerprint(dir.path(), &globs).unwrap();

        write(dir.path(), "prisma/migrations/2_more/up.sql", "alter;");
        assert_ne!(
            fingerprint(dir.path(), &globs).unwrap(),
            first,
            "a new migration is a reason to run again"
        );
    }

    // A fingerprint walk must never descend into a dependency tree: it would
    // be slower than the install it is trying to skip.
    #[test]
    fn a_walk_never_enters_a_dependency_tree() {
        let dir = tempdir().unwrap();
        write(dir.path(), "src/a.js", "x");
        write(dir.path(), "node_modules/pkg/index.js", "y");
        let globs = vec!["**/*.js".to_string()];
        let first = fingerprint(dir.path(), &globs).unwrap();
        write(dir.path(), "node_modules/pkg/index.js", "changed");
        assert_eq!(fingerprint(dir.path(), &globs).unwrap(), first);
    }

    // The Phase 2 gap: editing what a hook runs changed nothing, so the
    // corrected command never ran until one of its inputs happened to.
    #[test]
    fn editing_the_command_is_a_reason_to_run_again() {
        let dir = tempdir().unwrap();
        write(dir.path(), "prisma/migrations/1/up.sql", "create;");
        let globs = vec!["prisma/migrations/**".to_string()];
        let first = super::fingerprint(dir.path(), &globs, "prisma migrate deploy").unwrap();
        assert_eq!(
            super::fingerprint(dir.path(), &globs, "prisma migrate deploy").unwrap(),
            first,
            "the same command over the same files is the same fingerprint"
        );
        assert_ne!(
            super::fingerprint(dir.path(), &globs, "pnpm prisma migrate deploy").unwrap(),
            first
        );
    }

    // ...but a hook with nothing to watch has no fingerprint at all, so it
    // runs every time whatever its command is.
    #[test]
    fn a_command_alone_is_not_a_fingerprint() {
        let dir = tempdir().unwrap();
        assert_eq!(super::fingerprint(dir.path(), &[], "anything"), None);
        assert_eq!(
            super::fingerprint(dir.path(), &["absent.lock".to_string()], "anything"),
            None
        );
    }

    // ---- fallbacks -------------------------------------------------------

    #[test]
    fn a_fallback_runs_only_when_the_first_command_failed() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("migrate.log");
        run_with_fallback(&log, "echo first", Some("echo second"), dir.path(), &[]).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("first"), "{text}");
        assert!(
            !text.contains("second"),
            "the fallback is a fallback: {text}"
        );
    }

    #[test]
    fn a_fallback_that_succeeds_makes_the_hook_succeed() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("migrate.log");
        run_with_fallback(
            &log,
            "echo no-migrations >&2 && exit 1",
            Some("echo pushed-the-schema"),
            dir.path(),
            &[],
        )
        .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("no-migrations"), "{text}");
        assert!(text.contains("trying the fallback"), "{text}");
        assert!(text.contains("pushed-the-schema"), "{text}");
    }

    #[test]
    fn when_both_fail_the_fallbacks_reason_is_the_reported_one() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("migrate.log");
        let err = run_with_fallback(
            &log,
            "echo FIRST_REASON >&2 && exit 1",
            Some("echo SECOND_REASON >&2 && exit 2"),
            dir.path(),
            &[],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exited 2"), "{msg}");
        assert!(msg.contains("SECOND_REASON"), "{msg}");
        assert!(
            msg.contains("FIRST_REASON"),
            "and it still says which command sent it there: {msg}"
        );
    }

    #[test]
    fn a_renamed_file_is_a_change() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.lock", "same");
        let globs = vec!["*.lock".to_string()];
        let first = fingerprint(dir.path(), &globs).unwrap();
        std::fs::remove_file(dir.path().join("a.lock")).unwrap();
        write(dir.path(), "b.lock", "same");
        assert_ne!(fingerprint(dir.path(), &globs).unwrap(), first);
    }

    // ---- running ---------------------------------------------------------

    #[test]
    fn a_hook_writes_what_it_printed_to_its_log() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("logs/feat+one/install.log");
        run(
            &log,
            "echo installing && echo a-warning >&2",
            dir.path(),
            &[],
        )
        .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("installing"), "{text}");
        assert!(text.contains("a-warning"), "stderr too: {text}");
    }

    #[test]
    fn a_hook_s_output_is_in_its_log_while_it_is_still_running() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("logs/feat+one/install.log");
        // The hook looks for its own first line in the log before it
        // exits: held back until the end, it is not there to find.
        run(
            &log,
            "echo resolving-packages && grep -q resolving-packages \"$HOOK_LOG\"",
            dir.path(),
            &[("HOOK_LOG".to_string(), log.display().to_string())],
        )
        .unwrap();
    }

    #[test]
    fn a_hook_s_log_keeps_its_lines_in_the_order_they_were_printed() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("install.log");
        run(
            &log,
            "echo out-one && echo err-one >&2 && echo out-two",
            dir.path(),
            &[],
        )
        .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text, "out-one\nerr-one\nout-two\n");
    }

    #[test]
    fn a_failing_hook_s_reason_is_its_own_and_never_an_earlier_run_s() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("install.log");
        run(&log, "echo EARLIER_REASON >&2 && exit 1", dir.path(), &[]).unwrap_err();
        let err = run(&log, "exit 4", dir.path(), &[]).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exited 4"), "{msg}");
        assert!(!msg.contains("EARLIER_REASON"), "{msg}");
    }

    #[test]
    fn a_hook_s_last_line_is_ended_in_its_log() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("install.log");
        run(&log, "printf no-newline", dir.path(), &[]).unwrap();
        run(&log, "echo next-run", dir.path(), &[]).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert_eq!(text, "no-newline\nnext-run\n");
    }

    #[test]
    fn a_failing_hook_names_its_log_and_its_last_line() {
        let dir = tempdir().unwrap();
        let log = dir.path().join("install.log");
        let err = run(
            &log,
            "echo ERR_PNPM_OUTDATED_LOCKFILE >&2 && exit 3",
            dir.path(),
            &[],
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("exited 3"), "{msg}");
        assert!(msg.contains("ERR_PNPM_OUTDATED_LOCKFILE"), "{msg}");
        assert!(msg.contains("install.log"), "{msg}");
        assert!(std::fs::read_to_string(&log).unwrap().contains("ERR_PNPM"));
    }

    // A schema step died on a database error and printed the error object
    // after it: the closing line was `}`, and "exited 1: }" was all a
    // failed start said. The line that says what went wrong is the reason.
    #[test]
    fn a_failing_hook_is_explained_by_the_line_that_says_why() {
        let log = "Connected to the database\n\
                   Executing migration: schema.sql...\n\
                   Database initialization failed:\n\
                   SqlError: (conn:7, no: 1071, SQLState: 42000) Specified key was too long\n\
                   \x20   at Module.createError (file:///x/errors.js:66:10)\n\
                   \x20   at Query.readResponsePacket (file:///x/parser.js:70:21)\n\
                   {\n  errno: 1071,\n  fatal: false,\n  code: 'ER_TOO_LONG_KEY'\n}\n";
        assert_eq!(
            last_line(log),
            ": SqlError: (conn:7, no: 1071, SQLState: 42000) Specified key was too long"
        );
        // With nothing that reads as an error, the last line with words.
        assert_eq!(last_line("building\ndone in 3s\n)\n"), ": done in 3s");
        assert_eq!(last_line("\n  \n"), "");
        // A long line is clipped: the log has the rest.
        let long = format!("error: {}\n", "x".repeat(400));
        assert_eq!(last_line(&long).chars().count(), 2 + REASON_CHARS + 1);
    }

    #[test]
    fn a_hook_runs_in_its_directory_with_its_environment() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("apps/web")).unwrap();
        let log = dir.path().join("install.log");
        run(
            &log,
            "pwd && echo \"name=$PANDO_NAME\"",
            &dir.path().join("apps/web"),
            &[("PANDO_NAME".to_string(), "feat+one".to_string())],
        )
        .unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(text.contains("apps/web"), "{text}");
        assert!(text.contains("name=feat+one"), "{text}");
    }

    // ---- the terminal ----------------------------------------------------

    /// What a hook says about where it runs: its pid and its process
    /// group, and whether it could open a terminal to prompt on.
    const WHERE_AM_I: &str = "echo \"ids $$ $(ps -o pgid= -p $$)\"; \
        if (exec 3</dev/tty) 2>/dev/null; then echo 'tty open'; else echo 'no tty'; fi";

    fn where_it_ran(detached: bool) -> (String, String, bool) {
        let dir = tempdir().unwrap();
        let mut command = shell(WHERE_AM_I, dir.path(), &[]).unwrap();
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        // Spawned as `waited` would, but not held: the hang-up test leaves
        // every group held after it hung up, and this one must answer.
        let child = match detached {
            true => {
                crate::platform::process::spawn_group(&mut command)
                    .unwrap()
                    .0
            }
            false => command.spawn().unwrap(),
        };
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let ids = text
            .lines()
            .find_map(|line| line.strip_prefix("ids "))
            .unwrap_or_else(|| panic!("{text}"));
        let (pid, pgid) = ids.split_once(' ').unwrap();
        let tty = text.lines().any(|line| line == "tty open");
        (pid.trim().to_string(), pgid.trim().to_string(), tty)
    }

    // From the TUI a hook that prompted on /dev/tty drew over the frame
    // and waited for ever on keys the TUI read. Detached, it leads a
    // session of its own with no terminal in it, so the prompt fails.
    #[test]
    fn a_detached_hook_has_no_terminal_to_prompt_on() {
        let (pid, pgid, tty) = where_it_ran(true);
        assert_eq!(pid, pgid, "it leads a process group of its own");
        assert!(!tty, "and there is no terminal for it to open");
    }

    // From the CLI the terminal is the developer's, and a hook that asks
    // is answered there.
    #[test]
    fn a_hook_is_not_detached_unless_the_tui_asked() {
        assert!(!DETACHED.load(Ordering::Relaxed));
        let (_, pgid, _) = where_it_ran(false);
        assert_eq!(pgid, nix::unistd::getpgrp().to_string());
    }

    // Detached, a hook left the TUI's process group, and with it the
    // SIGHUP the terminal sends when its window closes: an install went
    // on writing into a worktree nothing waited for any more. The TUI
    // hangs up on it itself, and on a fallback that starts after, as the
    // first command failing would start one.
    //
    // The only test that records a detached hook: the hang-up reaches
    // every recorded one, and the flag it leaves set is the process's.
    #[test]
    fn a_detached_hook_is_hung_up_on_when_the_tui_goes() {
        use std::os::unix::process::ExitStatusExt;
        use std::time::{Duration, Instant};
        let dir = tempdir().unwrap();
        let started = Instant::now();
        let cwd = dir.path().to_path_buf();
        let hook = std::thread::spawn(move || {
            let mut command = shell("sleep 30", &cwd, &[]).unwrap();
            waited(&mut command, true, |mut child| child.wait()).unwrap()
        });
        let recorded = || signals::held().first().copied();
        let pgid = loop {
            if let Some(pgid) = recorded() {
                break pgid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "never recorded"
            );
            std::thread::sleep(Duration::from_millis(10));
        };

        hang_up_detached();
        let status = hook.join().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGHUP), "{status:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(
            !signals::held().contains(&pgid),
            "a hook that ended is off the list"
        );

        let mut fallback = shell("sleep 30", dir.path(), &[]).unwrap();
        let status = waited(&mut fallback, true, |mut child| child.wait()).unwrap();
        assert_eq!(status.signal(), Some(libc::SIGHUP), "{status:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
