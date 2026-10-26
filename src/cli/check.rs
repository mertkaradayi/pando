//! `pando check`: its narration, its one line of result, and `--json`.

use super::JSON_VERSION;
use super::ls::ProjectOut;
use crate::actions::{self, Checked, Narration};
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::setup::{CheckMode, CheckOutcome, CheckRecord, FailureKind, RanBy};
use anyhow::Result;
use serde::Serialize;
use std::io::{IsTerminal, Write};

/// Runs the check and says how it went: each step on stderr as it
/// happens, the result on stdout — one line, or with `--json` the
/// published shape, printed whatever the result, a question included.
///
/// A check that did not pass is exit 1 with its reason on stderr, after
/// the failed process's closing lines; one a question stopped is exit 3,
/// with the question, as every command that has one exits.
pub(super) fn check<W: Write>(
    paths: &PandoPaths,
    config: &Config,
    json: bool,
    base: Option<&str>,
    out: &mut W,
) -> Result<()> {
    actions::catch_check_interrupts();
    let terminal = std::io::stderr().is_terminal();
    let ran_by = actions::ran_by(
        std::env::var(actions::CHECK_RAN_BY_ENV).ok().as_deref(),
        terminal,
    );
    // The step last said: the start under the check narrates its own
    // "starting dev", and said twice in a row it reads as a stutter.
    let last_step = std::cell::RefCell::new(String::new());
    let step = |line: &str| {
        super::notice(line);
        *last_step.borrow_mut() = line.to_string();
    };
    // What the commands under the check say is for a person watching it
    // happen: a program reads the steps, and the result.
    let detail = |line: &str| {
        if terminal && *last_step.borrow() != line {
            let style = crate::term::Style::for_stderr();
            let line = style.paint(&format!("pando: {line}"), crate::term::Paint::Faint);
            super::to_stderr(&format!("{line}\n"));
        }
    };
    let say = Narration {
        step: &step,
        detail: &detail,
    };
    let Checked {
        record,
        unanswered,
        probe,
    } = actions::check_at(paths, config, base, ran_by, &say)?;
    if json {
        serde_json::to_writer_pretty(&mut *out, &check_json(paths, &record))?;
        writeln!(out)?;
    }
    if record.changed_while_running() {
        super::notice(
            "the settings changed while the check ran, so this result speaks for neither — \
             `pando check` tests them as they are now",
        );
    }
    if let Some(needs) = unanswered {
        // The check never answers a question itself: a test that took
        // its own guess would test the guess. So the question is worded
        // for `pando init`, and `pando check` again after it.
        super::notice("the check starts nothing while a question is open");
        return Err(anyhow::Error::new(super::CheckNeedsAnswer(needs)));
    }
    match &record.outcome {
        CheckOutcome::Passed => {
            if !json {
                // A person watching sees the setup screen's pictures,
                // the grove's roots lit, and the line in colour. A
                // program reads the same line, plain.
                if terminal {
                    for line in super::art::banner_lines(
                        super::art::stderr_columns(),
                        crate::art::seed_of(&paths.project.id),
                        true,
                        &crate::term::Style::for_stderr(),
                    ) {
                        super::draw(&line);
                    }
                }
                let style = crate::term::Style::for_stdout();
                // A pass at a base the settings do not choose proves that
                // base, not the setup `new` forks worktrees from.
                let said = match probe.as_deref() {
                    Some(given) => format!(
                        "{} passes at {given}: answer `base` with it to make that the setup's",
                        paths.project.display_name
                    ),
                    _ => format!("{} is ready: `pando` opens it", paths.project.display_name),
                };
                writeln!(
                    out,
                    "{} {}",
                    style.paint("✓", crate::term::Paint::Good),
                    style.paint(&said, crate::term::Paint::Heading)
                )?;
            }
            Ok(())
        }
        CheckOutcome::Failed { kind, reason } => {
            if let Some(process) = &record.failed_process
                && !record.failed_tail.is_empty()
            {
                super::notice(&format!("the last lines of the {process} log:"));
                for line in &record.failed_tail {
                    super::to_stderr(&format!("  {line}\n"));
                }
            }
            anyhow::bail!("{}", failure_sentence(*kind, reason))
        }
        CheckOutcome::Interrupted => anyhow::bail!(
            "the check was stopped before it finished, and took its test worktree down — \
             `pando check` runs it again"
        ),
        CheckOutcome::NotSetUp { slot } => {
            anyhow::bail!("the {slot} question is still open — `pando init` answers it")
        }
        CheckOutcome::Running => unreachable!("a check returns once it has ended"),
    }
}

/// What a failed check says last, by whose the failure is to fix.
fn failure_sentence(kind: FailureKind, reason: &str) -> String {
    match kind {
        FailureKind::Settings => format!(
            "the check failed: {reason} — change the settings, then run `pando check` again"
        ),
        FailureKind::Machine => format!(
            "the check could not run: {reason}. This is the machine's to fix; no setting \
             changes it"
        ),
        FailureKind::Base => format!("the check failed on the commit it tested: {reason}"),
    }
}

/// `pando check --json`, documented in `agent/json.md`.
#[derive(Serialize)]
pub(super) struct CheckOut<'a> {
    version: u32,
    project: ProjectOut,
    result: &'static str,
    mode: CheckMode,
    kind: Option<FailureKind>,
    reason: Option<&'a str>,
    slot: Option<&'a str>,
    commit: Option<&'a str>,
    base_ref: Option<&'a str>,
    processes: Vec<CheckProcessOut<'a>>,
    failed_process: Option<&'a str>,
    failed_tail: &'a [String],
    notes: &'a [String],
    ran_by: RanBy,
    pando_version: &'a str,
    started_at: String,
    finished_at: Option<String>,
    settings_changed: bool,
}

#[derive(Serialize)]
struct CheckProcessOut<'a> {
    name: &'a str,
    ready: bool,
    port: Option<u16>,
    http_status: Option<u16>,
    secs: f64,
}

/// The published shape of a check's record. `check.json` itself is
/// pando's own file and free to change; this is the contract.
pub(super) fn check_json<'a>(paths: &PandoPaths, record: &'a CheckRecord) -> CheckOut<'a> {
    let (result, kind, reason, slot) = match &record.outcome {
        CheckOutcome::Passed => ("passed", None, None, None),
        CheckOutcome::Failed { kind, reason } => {
            ("failed", Some(*kind), Some(reason.as_str()), None)
        }
        CheckOutcome::NotSetUp { slot } => ("not_set_up", None, None, Some(slot.as_str())),
        // Never printed running: the command prints once the check has
        // ended, and a record that says so ended without saying how.
        CheckOutcome::Interrupted | CheckOutcome::Running => ("interrupted", None, None, None),
    };
    CheckOut {
        version: JSON_VERSION,
        project: ProjectOut::of(&paths.project),
        result,
        mode: record.mode,
        kind,
        reason,
        slot,
        commit: record.commit.as_deref(),
        base_ref: record.base_ref.as_deref(),
        processes: record
            .processes
            .iter()
            .map(|p| CheckProcessOut {
                name: &p.name,
                ready: p.ready,
                port: p.port,
                http_status: p.http_status,
                secs: (p.secs * 10.0).round() / 10.0,
            })
            .collect(),
        failed_process: record.failed_process.as_deref(),
        failed_tail: &record.failed_tail,
        notes: &record.notes,
        ran_by: record.ran_by,
        pando_version: &record.pando_version,
        started_at: record
            .started_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        finished_at: record
            .finished_at
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        settings_changed: record.changed_while_running(),
    }
}

/// Every result `--json` can print, for the test that holds the document
/// to it.
#[cfg(test)]
pub(super) const RESULTS: [&str; 4] = ["passed", "failed", "not_set_up", "interrupted"];
