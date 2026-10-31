//! Tests for process groups: what the backend promises beneath the
//! contracts `process.rs`'s own tests prove through `spawn_detached`.

use super::*;
use std::path::Path;
use std::time::Instant;

// The TUI's first FSEvents watcher sets libnotify up on a thread of its
// own, and a fork made while it did was killed before `exec`: a tunnel
// whose log stayed empty, one full run in fifteen. Settled before the
// command can be spawned, the set-up is over before any fork starts.
#[cfg(target_os = "macos")]
#[test]
fn a_command_given_its_own_group_forks_only_once_libnotify_is_settled() {
    let (mut child, _) = spawn_group(&mut Command::new("true")).unwrap();
    assert!(unix::LIBNOTIFY_SETTLED.is_completed());
    let _ = child.wait();
}

// `pre_exec` is the fork trigger pando has reason to use: std also forks
// rather than spawns for a changed PATH with a bare program, a uid or
// gid, or a relative program with a cwd, none of which pando does, and
// `main` settles libnotify before any of them could. Within the tests,
// which never run `main`, a fork that skipped the backend's own session
// start would skip the settling, so the one `pre_exec` in pando is there.
#[test]
fn every_group_pando_starts_is_started_by_the_backend() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    // Built rather than written out, so this file's own needle is not one
    // of the uses it finds.
    let needle = [".pre", "_exec("].concat();
    let mut uses = Vec::new();
    let mut dirs = vec![root.join("src")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                for (n, line) in text.lines().enumerate() {
                    if line.contains(&needle) {
                        uses.push(format!("{rel}:{}", n + 1));
                    }
                }
            }
        }
    }
    assert_eq!(uses.len(), 1, "{uses:?}");
    assert!(
        uses[0].starts_with("src/platform/process/unix.rs:"),
        "{uses:?}"
    );
}

#[test]
fn a_spawned_group_is_led_by_the_child_and_stops_as_one() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 & sleep 30"]);
    let (mut child, group) = spawn_group(&mut command).unwrap();
    assert!(group_alive(group));
    stop(group, Duration::from_secs(5)).unwrap();
    let _ = child.wait();
    assert!(!group_alive(group), "the backgrounded sleep went with it");
}

// git is the one external call every command makes first, so a git
// that never answers must not be a pando that never answers.
#[test]
fn a_command_that_does_not_answer_is_killed_at_the_deadline() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 & sleep 30"]);
    let started = Instant::now();
    let err = output_within(command, Duration::from_millis(300)).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "bounded, not waited out: {:?}",
        started.elapsed()
    );

    // And one that answers is read exactly as `output` would read it.
    let mut command = Command::new("sh");
    command.args(["-c", "echo out; echo err >&2; exit 3"]);
    let out = output_within(command, Duration::from_secs(10)).unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "out\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
}

// Exited, with a child it backgrounded still holding stdout: the call
// returns rather than waiting on the child.
#[test]
fn a_background_child_holding_the_pipe_does_not_hold_the_call() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 & echo done"]);
    let started = Instant::now();
    let out = output_within(command, Duration::from_secs(1)).unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("done"));
    assert!(started.elapsed() < Duration::from_secs(10));
}

// The group is only signalled while its leader is unreaped: a reaped
// leader of an empty group frees the number for a stranger. So the
// exit is seen without being collected, and the status still comes
// back whole once the child is reaped afterwards.
#[cfg(unix)]
#[test]
fn an_exit_is_seen_without_reaping_the_child() {
    let mut child = Command::new("sh").args(["-c", "exit 7"]).spawn().unwrap();
    let pid = child.id() as i32;
    let started = Instant::now();
    while unix::exited_unreaped(pid) != Some(true) {
        assert!(started.elapsed() < Duration::from_secs(10), "never seen");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Still ours to reap, with its status intact.
    assert_eq!(child.wait().unwrap().code(), Some(7));

    let mut running = Command::new("sleep").arg("30").spawn().unwrap();
    assert_eq!(unix::exited_unreaped(running.id() as i32), Some(false));
    let _ = running.kill();
    let _ = running.wait();
}

// A stop's mark names the pando that wrote it by pid and by this: a pid
// handed to another process once that pando is gone must not read as it.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_process_start_is_its_own_and_not_the_next_holder_of_its_pid() {
    let own = started(std::process::id());
    assert!(own.is_some());
    assert_eq!(own, started(std::process::id()), "asked twice, one answer");
    let mut child = Command::new("sleep").arg("5").spawn().unwrap();
    let theirs = started(child.id());
    let _ = child.kill();
    let _ = child.wait();
    assert!(theirs.is_some());
    assert_ne!(theirs, own);
    assert_eq!(started(child.id()), None, "a pid nobody holds has no start");
}
