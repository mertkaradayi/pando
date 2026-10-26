//! `logs`: printing and following a worktree's logs.

use super::JSON_VERSION;
use crate::actions;
use crate::log_tail::{self, LogLevel};
use crate::paths::PandoPaths;
use crate::state::Phase;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::io::Write;
use std::time::Duration;

// ---- logs -----------------------------------------------------------------

/// How often `-f` looks for new lines. Fast enough to feel live, slow
/// enough not to spin a core.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(250);

/// Lines a follower keeps room for between two polls. Big enough that a
/// dev server's startup burst is printed in full rather than clipped to
/// whatever `--tail` happened to be.
const FOLLOW_CAPACITY: usize = 4096;

/// The most a follower reads of a log at once. A backlog — a pager that
/// stopped reading, a Ctrl-Z — is read in reads of this, each printed
/// before the next, so it is printed whole and never held in memory whole.
const FOLLOW_READ_BYTES: u64 = 1024 * 1024;

/// One line of one log, as the machine-readable shape.
///
/// The one published shape that is a *stream* rather than a document: one
/// object per line, so `pando logs -f --json | …` stays useful while it
/// runs. It carries `version` on every line for exactly that reason — a
/// consumer reading a pipe has no other object to learn the shape from,
/// and running a second command to ask is not something a follower can do.
#[derive(Serialize)]
struct LogLineOut<'a> {
    version: u32,
    /// The line's own timestamp when it has one pando can read, else null.
    ts: Option<String>,
    level: &'static str,
    line: &'a str,
    /// Which log the line came from — only when several are merged into
    /// one stream. A read of one named log leaves it out, as absent and
    /// empty are the same thing in the published shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<&'a str>,
}

pub(super) fn level_word(level: LogLevel) -> &'static str {
    match level {
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

#[allow(clippy::too_many_arguments)]
/// Prints the tail of one log, and — when there is nothing to print —
/// says so.
///
/// `notice` is where "nothing to print" goes, and it is not `out`:
/// `pando logs > file` and `pando logs --json | …` both take stdout
/// literally, so a sentence about the log cannot be on it. Printing
/// nothing and exiting 0 was the third dead end of the same first run —
/// `doctor` said the process failed and pointed here, here said nothing,
/// and there was no way to tell an empty log from a broken command.
pub fn logs<W: Write>(
    paths: &PandoPaths,
    name: &str,
    source: &str,
    tail: usize,
    follow: bool,
    json: bool,
    out: &mut W,
    notice: &dyn Fn(&str),
) -> Result<()> {
    // `source` is a path component of the file about to be read, so a
    // traversal here reads outside the worktree's log directory entirely —
    // a file `available_sources` would never list.
    crate::paths::validate_log_source("log source", source)?;
    let path = paths.log_file(name, source);
    if !path.exists() {
        let shown = super::names::shown(paths, name);
        let available = available_sources(paths, name);
        if available.is_empty() {
            anyhow::bail!(
                "no logs for {shown} yet — `pando start {shown}` writes them to {}",
                paths.logs_dir(name).display()
            );
        }
        anyhow::bail!(
            "no {source} log for {shown} — this worktree has: {}; `--source` picks one",
            available.join(", ")
        );
    }
    // A one-shot read keeps only what it prints. A follower needs room for
    // whatever arrives between two polls, which has nothing to do with how
    // many lines the reader asked to see first.
    let capacity = if follow {
        tail.max(FOLLOW_CAPACITY)
    } else {
        tail.max(1)
    };
    let mut tailer = log_tail::LogTail::new(path.clone(), capacity);
    tailer
        .poll()
        .with_context(|| format!("read {}", path.display()))?;
    // A one-shot read takes the file as it stands, unterminated last line
    // and all. A follower may not: the rest of that line is still coming.
    if !follow {
        tailer.flush_pending();
    }
    let first = tailer.lines().len().saturating_sub(tail);
    for line in tailer.lines().iter().skip(first) {
        write_log_line(out, &line.plain, line.level, json)?;
    }
    // Whether the log has a line, not whether one was printed: `-n 0`
    // prints none by request, and used to be told the log was empty.
    if tailer.lines().is_empty() {
        for line in silence_notes(paths, name, source, &path) {
            notice(&line);
        }
    }
    if !follow {
        return Ok(());
    }
    // How many lines have gone past, not how many are in the buffer: the
    // buffer stops growing at `capacity`, and comparing against its length
    // is how a follower goes silent forever a few seconds into a run.
    let mut printed = tailer.lines_seen();
    // Ends on Ctrl-C, which is what `-f` means everywhere else.
    loop {
        std::thread::sleep(FOLLOW_INTERVAL);
        let mut any = false;
        // Read on until caught up, printing each read before the next: a
        // read holds no more lines than the buffer does, so none is
        // evicted unprinted.
        loop {
            let grew = tailer
                .poll_bounded(FOLLOW_READ_BYTES)
                .with_context(|| format!("read {}", path.display()))?;
            if grew {
                let seen = tailer.lines_seen();
                // Truncation resets the file's offsets but not this count,
                // so a restarted process picks up where the follower left
                // off.
                let fresh = seen.saturating_sub(printed) as usize;
                printed = seen;
                let skip = tailer.lines().len().saturating_sub(fresh);
                for line in tailer.lines().iter().skip(skip) {
                    write_log_line(out, &line.plain, line.level, json)?;
                }
                any = true;
            }
            if !tailer.has_unread() {
                break;
            }
        }
        if any {
            out.flush()?;
        }
    }
}

/// Why `logs` printed nothing: what the file is, and — when the record
/// knows — what the process did.
///
/// Three states that used to be one silence. An empty file is a fact about
/// the run: the process wrote nothing, which is itself the strongest thing
/// anyone can say about a dev command that exited at once. A file with
/// bytes but no complete line is a different fact and is not pando's doing
/// either. Neither is "the command is broken", which is the only thing a
/// silent exit 0 leaves a developer free to conclude.
///
/// The second line comes from the state record, read without writing
/// anything: the process's own phase, which for a failure is the reason
/// `explain_failure` already composed. That is the sentence the developer
/// came here looking for.
pub(super) fn silence_notes(
    paths: &PandoPaths,
    name: &str,
    source: &str,
    path: &std::path::Path,
) -> Vec<String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let shown = super::names::shown(paths, name);
    let mut out = vec![match size {
        0 => format!("the {source} log for {shown} is empty — nothing was ever written to it"),
        n => format!("the {source} log for {shown} holds {n} bytes and not one complete line yet"),
    }];
    // `inspect`, not `refresh`: reading a log may not signal anything or
    // write state.
    let state = actions::inspect(paths).state;
    let Some(process) = state
        .worktrees
        .get(name)
        .and_then(|record| record.processes.get(source))
    else {
        return out;
    };
    out.push(match &process.phase {
        Phase::Failed { reason, .. } => format!("{source} failed: {reason}"),
        Phase::Running { .. } => {
            format!("{source} is running and has not printed anything yet")
        }
        Phase::Starting { .. } => format!("{source} is still starting"),
    });
    out
}

fn write_log_line<W: Write>(out: &mut W, plain: &str, level: LogLevel, json: bool) -> Result<()> {
    write_line_from(out, None, plain, level, json)
}

/// One line on stdout: the text, or its JSON object. `from` is the source
/// and the column it is padded to, when several logs are merged.
fn write_line_from<W: Write>(
    out: &mut W,
    from: Option<(&str, usize)>,
    plain: &str,
    level: LogLevel,
    json: bool,
) -> Result<()> {
    if json {
        let entry = LogLineOut {
            version: JSON_VERSION,
            ts: leading_timestamp(plain),
            level: level_word(level),
            line: plain,
            source: from.map(|(source, _)| source),
        };
        writeln!(out, "{}", serde_json::to_string(&entry)?)?;
    } else {
        match from {
            Some((source, width)) => writeln!(out, "{source:<width$} | {plain}")?,
            None => writeln!(out, "{plain}")?,
        }
    }
    Ok(())
}

/// Which logs `logs` reads when no `--source` is given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Sources {
    One(String),
    /// Every process's log, merged the way `docker compose logs` merges
    /// services: a worktree with several processes and no `dev` log has
    /// no one right guess, and all of them is what a developer wants to
    /// see.
    Merged(Vec<String>),
}

/// The logs to read when none was asked for: `preferred` (`dev`) when the
/// worktree has one, else its only process — a project whose one process
/// is called `web` should not need `--source web` every time — and, for a
/// worktree with several processes and no `preferred` log, all of them
/// rather than an error. Only the processes: a hook's, a service's and the
/// tunnel's logs are read by naming them.
pub(super) fn default_sources(paths: &PandoPaths, name: &str, preferred: &str) -> Sources {
    if paths.log_file(name, preferred).exists() {
        return Sources::One(preferred.to_string());
    }
    let processes = process_names(paths, name);
    let with_logs: Vec<String> = processes
        .into_iter()
        .filter(|process| paths.log_file(name, process).exists())
        .collect();
    match with_logs.len() {
        0 => Sources::One(preferred.to_string()),
        1 => Sources::One(with_logs[0].clone()),
        _ => Sources::Merged(with_logs),
    }
}

/// Every process the record names for this worktree, once, sorted.
fn process_names(paths: &PandoPaths, name: &str) -> Vec<String> {
    // The state as saved, not `inspect`'s: choosing a log may not write
    // state, and it reads only which processes and roles the record names,
    // which no phase advance changes. `inspect` scans every live group's
    // sockets first, a `ps` and an `lsof` before a line is printed.
    let state = crate::state::load(&paths.state_file()).unwrap_or_default();
    let mut out: Vec<String> = state
        .worktrees
        .get(name)
        .map(|record| {
            record
                .processes
                .keys()
                .chain(record.roles.keys())
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out.dedup();
    out
}

/// The processes that run in `repo`, a repository of a multi-repo project,
/// by name: those whose `cwd` is in it. None when `repo` is no repository
/// of the project, which `--source` then reads as a log's name.
pub fn sources_in_repo(
    paths: &PandoPaths,
    config: &crate::config::Config,
    repo: &str,
) -> Vec<String> {
    config
        .runnable_processes()
        .filter(|(_, process)| {
            process
                .cwd
                .as_deref()
                .and_then(|cwd| paths.project.repo_of(cwd))
                == Some(repo)
        })
        .map(|(name, _)| name.clone())
        .collect()
}

/// Prints several logs as one, each line prefixed with its source padded
/// to a common width — `api | …`, `web | …` — or, in JSON, carrying a
/// `source` key.
///
/// In timestamp order when every line printed has a timestamp pando can
/// read; otherwise one source after the other, each in its own order,
/// because interleaving lines with no time on them would invent an order
/// that never happened. `tail` counts per source, as `docker compose logs
/// --tail` does. `-f` polls every file and prints what each has gained.
#[allow(clippy::too_many_arguments)]
pub fn logs_merged<W: Write>(
    paths: &PandoPaths,
    name: &str,
    sources: &[String],
    tail: usize,
    follow: bool,
    json: bool,
    out: &mut W,
    notice: &dyn Fn(&str),
) -> Result<()> {
    let width = sources.iter().map(|s| s.chars().count()).max().unwrap_or(0);
    let capacity = if follow {
        tail.max(FOLLOW_CAPACITY)
    } else {
        tail.max(1)
    };
    let mut tailers: Vec<(String, log_tail::LogTail)> = Vec::new();
    for source in sources {
        crate::paths::validate_log_source("log source", source)?;
        let path = paths.log_file(name, source);
        let mut tailer = log_tail::LogTail::new(path.clone(), capacity);
        tailer
            .poll()
            .with_context(|| format!("read {}", path.display()))?;
        if !follow {
            tailer.flush_pending();
        }
        tailers.push((source.clone(), tailer));
    }
    notice(&format!(
        "{} together — `-s {}` shows one alone",
        sources.join(", "),
        sources.first().map_or("<process>", String::as_str)
    ));

    // (timestamp, source, text, level), in the order they are printed.
    let mut lines: Vec<(Option<DateTime<Utc>>, String, String, LogLevel)> = Vec::new();
    for (source, tailer) in &tailers {
        let first = tailer.lines().len().saturating_sub(tail);
        for line in tailer.lines().iter().skip(first) {
            lines.push((
                leading_time(&line.plain),
                source.clone(),
                line.plain.clone(),
                line.level,
            ));
        }
    }
    if !lines.is_empty() && lines.iter().all(|(ts, ..)| ts.is_some()) {
        // Stable, so two lines of one instant keep their sources' order.
        lines.sort_by_key(|(ts, ..)| *ts);
    }
    for (_, source, plain, level) in &lines {
        write_line_from(out, Some((source, width)), plain, *level, json)?;
    }
    if !follow {
        return Ok(());
    }
    let mut printed: Vec<u64> = tailers.iter().map(|(_, t)| t.lines_seen()).collect();
    loop {
        std::thread::sleep(FOLLOW_INTERVAL);
        let mut any = false;
        for (i, (source, tailer)) in tailers.iter_mut().enumerate() {
            let path = tailer.path().to_path_buf();
            // Read on until caught up, as a single follower does.
            loop {
                let grew = tailer
                    .poll_bounded(FOLLOW_READ_BYTES)
                    .with_context(|| format!("read {}", path.display()))?;
                if grew {
                    let seen = tailer.lines_seen();
                    let fresh = seen.saturating_sub(printed[i]) as usize;
                    printed[i] = seen;
                    let skip = tailer.lines().len().saturating_sub(fresh);
                    for line in tailer.lines().iter().skip(skip) {
                        write_line_from(out, Some((source, width)), &line.plain, line.level, json)?;
                        any = true;
                    }
                }
                if !tailer.has_unread() {
                    break;
                }
            }
        }
        if any {
            out.flush()?;
        }
    }
}

/// [`leading_timestamp`], as a time to sort by.
fn leading_time(line: &str) -> Option<DateTime<Utc>> {
    let first = line.split_whitespace().next()?;
    let trimmed = first.trim_start_matches('[').trim_end_matches(']');
    DateTime::parse_from_rfc3339(trimmed)
        .ok()
        .map(|ts| ts.with_timezone(&Utc))
}

/// Every log source this worktree has, from the files that exist. Nothing
/// enumerates the set: a hook adds one by writing one.
fn available_sources(paths: &PandoPaths, name: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(paths.logs_dir(name)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (path.extension()?.to_str()? == "log")
                .then(|| path.file_stem()?.to_str().map(str::to_string))?
        })
        .collect();
    out.sort();
    out
}

/// The timestamp a line starts with, when it has one pando can read.
///
/// Dev servers disagree about log formats, so this is deliberately narrow:
/// an RFC 3339 stamp, optionally in brackets, at the very start. Anything
/// else is `null` rather than a guess.
pub(super) fn leading_timestamp(line: &str) -> Option<String> {
    leading_time(line).map(|ts| ts.to_rfc3339())
}
