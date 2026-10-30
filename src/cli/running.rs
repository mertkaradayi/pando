//! `start`, `stop` and `restart`: the worktree you are in, one named, or
//! several side by side.
//!
//! None and one are the commands they always were, word for word. Several
//! are resolved first, each once; a start's or a restart's questions are
//! settled one name after another, since they are a person's to answer or
//! an exit 3; then the work runs side by side, each outcome said under its
//! name, and the command fails naming any that did not happen.

use anyhow::Result;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::actions;
use crate::config::Config;
use crate::paths::PandoPaths;

use super::each::Outcomes;
use super::names::{self, Named};
use super::{
    UsageError, draw, everyday_asker, hint, notice, stderr_is_terminal, tip, url_suffix, wait,
    with_a_way_past,
};

/// How a start or a restart was asked to run, the same for every worktree
/// it is given.
pub(super) struct Run<'a> {
    pub(super) yes: bool,
    pub(super) only: Option<&'a str>,
    pub(super) mode: actions::Mode,
    /// Whether it waits for readiness, already decided.
    pub(super) wait: bool,
}

/// Which of the two a [`Run`] is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verb {
    Start,
    Restart,
}

impl Verb {
    fn word(self) -> &'static str {
        match self {
            Verb::Start => "start",
            Verb::Restart => "restart",
        }
    }

    fn done(self) -> &'static str {
        match self {
            Verb::Start => "started",
            Verb::Restart => "restarted",
        }
    }
}

/// `pando start` of every name typed, or of the worktree you are in.
pub(super) fn start<W: Write + Send>(
    paths: &PandoPaths,
    config: &Config,
    typed: &[String],
    run: &Run<'_>,
    out: &mut W,
) -> Result<()> {
    run_each(paths, config, typed, run, Verb::Start, out)
}

/// `pando restart` of every name typed, or of the worktree you are in.
pub(super) fn restart<W: Write + Send>(
    paths: &PandoPaths,
    config: &Config,
    typed: &[String],
    run: &Run<'_>,
    out: &mut W,
) -> Result<()> {
    run_each(paths, config, typed, run, Verb::Restart, out)
}

fn run_each<W: Write + Send>(
    paths: &PandoPaths,
    config: &Config,
    typed: &[String],
    run: &Run<'_>,
    verb: Verb,
    out: &mut W,
) -> Result<()> {
    let typed = match typed {
        [] => None,
        [one] => Some(one.as_str()),
        many => return run_several(paths, config, many, run, verb, out),
    };
    let named = names::target_named(paths, typed, verb.word())?;
    let config = settle(paths, config, &named, run, verb, &notice, true)?;
    let started = run_one(paths, &config, &named, run, verb, &notice, out)?;
    if verb == Verb::Start && started && !run.wait {
        hint(&format!(
            "`pando status {}` shows when it is ready",
            named.typed
        ));
    }
    Ok(())
}

/// Every question a start or restart of `named` has, settled before
/// anything runs: the refusal of an `--only` across a mode change, then
/// what config is missing, asked or exit 3. The config it runs on.
fn settle(
    paths: &PandoPaths,
    config: &Config,
    named: &Named,
    run: &Run<'_>,
    verb: Verb,
    progress: &dyn Fn(&str),
    tip_first: bool,
) -> Result<Config> {
    let name = &named.dir;
    // Before any question: a login asked for, or a slot freed, for a start
    // that is refused is a cost with nothing for it.
    actions::refuse_only_on_a_mode_change(paths, config, name, run.only, run.mode)
        .map_err(|e| with_a_way_past(paths, named.reword(e)))?;
    // After the refusals, which say nothing unless the start is not going
    // to happen, and before the first question.
    if verb == Verb::Start && tip_first {
        tip::first_time_tip(paths, config, stderr_is_terminal(), &notice, &draw);
    }
    // Resolved exactly as `start` resolves it, for a restart too: a project
    // whose process question has never been answered gets the question,
    // not a refusal.
    actions::resolve_for_start(
        paths,
        config,
        name,
        run.mode,
        &everyday_asker(run.yes),
        progress,
    )
}

/// One settled start or restart: run, waited for when asked, and said on
/// `out`. Whether a start started anything.
fn run_one(
    paths: &PandoPaths,
    config: &Config,
    named: &Named,
    run: &Run<'_>,
    verb: Verb,
    progress: &dyn Fn(&str),
    out: &mut dyn Write,
) -> Result<bool> {
    let name = &named.dir;
    let report = match verb {
        Verb::Start => actions::start(paths, config, name, run.only, run.mode, progress),
        Verb::Restart => actions::restart(paths, config, name, run.only, run.mode, progress),
    }
    .map_err(|e| with_a_way_past(paths, named.reword(e)))?;
    if verb == Verb::Start && report.reassigned {
        progress(&format!(
            "the ports {} had were taken; it moved to new ones",
            named.shown
        ));
    }
    if run.wait {
        wait::wait_ready(paths, named, run.only, &report.spawned(), progress)?;
    }
    let url = url_suffix(report.url.as_deref());
    match verb {
        Verb::Restart => writeln!(out, "restarted {name}{url}")?,
        Verb::Start if report.started_nothing() => {
            writeln!(out, "{name} is already running{url}")?;
            return Ok(false);
        }
        Verb::Start => writeln!(out, "started {name}{url}")?,
    }
    Ok(true)
}

/// Several names: each question settled in turn, then every start or
/// restart side by side.
fn run_several<W: Write + Send>(
    paths: &PandoPaths,
    config: &Config,
    typed: &[String],
    run: &Run<'_>,
    verb: Verb,
    out: &mut W,
) -> Result<()> {
    let mut outcomes = Outcomes::new(verb.done());
    let named = outcomes.resolve(
        typed,
        |t| names::target_named(paths, Some(t), verb.word()),
        |n| n.dir.as_str(),
    );
    // One name after another: a question is put to a person, or is an
    // exit 3 that nothing should have started before. Each answer is
    // written to config and taken into the next name's, so it is asked
    // once.
    let mut settled: Vec<(Named, Config)> = Vec::new();
    let mut base = config.clone();
    for named in named {
        let progress = |line: &str| notice(&format!("{}: {line}", named.dir));
        match settle(
            paths,
            &base,
            &named,
            run,
            verb,
            &progress,
            settled.is_empty(),
        ) {
            Ok(config) => {
                base = config.clone();
                settled.push((named, config));
            }
            Err(e) if is_a_question(&e) => return Err(e),
            Err(e) => outcomes.fail(&named.dir, &named.reword(e)),
        }
    }
    let started = AtomicBool::new(false);
    let unwritten = outcomes.side_by_side(
        &settled,
        |(named, _)| named.dir.as_str(),
        |(named, config), progress, out| {
            if run_one(paths, config, named, run, verb, progress, out)? {
                started.store(true, Ordering::Relaxed);
            }
            Ok(())
        },
        out,
    );
    if verb == Verb::Start && started.into_inner() && !run.wait {
        hint("`pando status` shows when they are ready");
    }
    outcomes.conclude(unwritten)
}

/// An error that is a question, or a wrong answer to one: the whole
/// command's, exit 3 or 2, never one name's failure among others.
fn is_a_question(e: &anyhow::Error) -> bool {
    e.downcast_ref::<actions::NeedsAnswer>().is_some()
        || e.downcast_ref::<UsageError>().is_some()
        || e.downcast_ref::<actions::RefusedAnswer>().is_some()
}

/// `pando stop` of every name typed; of the worktree you are in; or of
/// every one, with `--all` or from outside any.
pub(super) fn stop<W: Write + Send>(
    paths: &PandoPaths,
    typed: &[String],
    only: Option<&str>,
    all: bool,
    out: &mut W,
) -> Result<()> {
    let name = match (typed, all) {
        (_, true) => None,
        ([one], false) => Some(names::resolve(paths, one)?),
        // No name: the worktree the shell is in, said out loud because
        // `stop` alone used to mean every one.
        ([], false) => {
            let cwd = std::env::current_dir()?;
            let here = names::containing(paths, &cwd)?;
            if let Some(here) = &here {
                notice(&format!(
                    "stopping {}, the worktree you are in — `pando stop --all` stops every one",
                    names::shown(paths, here)
                ));
            }
            here
        }
        (many, false) => return stop_several(paths, many, only, out),
    };
    match name {
        Some(name) => stop_one(paths, &name, only, &notice, out),
        None if only.is_some() => Err(UsageError(
            "--only needs a worktree: `pando stop <name> --only <process>`, or run it from \
             inside one"
                .to_string(),
        )
        .into()),
        None => {
            let stopped = actions::stop_all(paths, &notice)?;
            if stopped.is_empty() {
                writeln!(out, "nothing was running")?;
            } else {
                // A running `pando check` is stopped with the rest, and
                // named for what it is, not its directory.
                let stopped: Vec<&str> = stopped
                    .iter()
                    .map(|name| crate::worktree::label(name))
                    .collect();
                writeln!(out, "stopped {}", stopped.join(", "))?;
            }
            Ok(())
        }
    }
}

fn stop_several<W: Write + Send>(
    paths: &PandoPaths,
    typed: &[String],
    only: Option<&str>,
    out: &mut W,
) -> Result<()> {
    let mut outcomes = Outcomes::new("stopped");
    let resolved = outcomes.resolve(typed, |t| names::resolve(paths, t), String::as_str);
    let unwritten = outcomes.side_by_side(
        &resolved,
        String::as_str,
        |name, progress, out| stop_one(paths, name, only, progress, out),
        out,
    );
    outcomes.conclude(unwritten)
}

fn stop_one(
    paths: &PandoPaths,
    name: &str,
    only: Option<&str>,
    progress: &dyn Fn(&str),
    out: &mut dyn Write,
) -> Result<()> {
    match actions::stop(paths, name, only, progress)? {
        actions::StopOutcome::Stopped(processes) => {
            // Empty when the worktree had only its services left up, which
            // is a real thing to stop and a silly thing to narrate as
            // "stopped ".
            if !processes.is_empty() {
                progress(&format!("stopped {}", processes.join(", ")));
            }
            writeln!(out, "stopped {name}")?;
        }
        actions::StopOutcome::NotRunning => writeln!(out, "{name} was not running")?,
    }
    Ok(())
}
