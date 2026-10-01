//! `open`: a worktree's URL in the browser, or its app on a simulator or
//! a device — the TUI's `o` and `O` keys, from a shell.

use super::names::Named;
use crate::actions::{self, NotOpened, worktree_url};
use crate::catalog::frameworks::AppLinks;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::state::{self, Aggregate, Phase};
use anyhow::{Result, bail};
use std::io::Write;

/// What `open` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Want {
    /// The worktree's page, or its app where it serves none.
    Page,
    /// The public URL `share` published: `--public`.
    Public,
    /// Its app a device runs, even beside a page: `--app`.
    App,
}

/// What `open` does with a worktree that is up.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Opening {
    /// Hand this URL to the browser.
    Url(String),
    /// Open these apps, by process, on a simulator or a device, after
    /// saying `said`: what else of the worktree serves no page, and why.
    /// Then refuse `refused`: apps whose development build on the booted
    /// simulator was made for another SDK, with the command that replaces
    /// it.
    Apps {
        apps: Vec<(String, AppLinks)>,
        said: Vec<String>,
        refused: Vec<String>,
    },
}

/// What `open` would hand the browser: the local URL while something is
/// up to answer it, or the public one — or, for a worktree whose
/// processes serve no page, or with [`Want::App`], the apps a device runs
/// that are running now.
///
/// Messages name the worktree as a person knows it, and the commands they
/// suggest spell it the way it was typed.
pub(super) fn url_to_open(
    paths: &PandoPaths,
    config: &Config,
    named: &Named,
    want: Want,
) -> Result<Opening> {
    let Named { dir, shown, typed } = named;
    let refreshed = actions::refresh(paths);
    let record = refreshed.state.worktrees.get(dir.as_str());
    // A state file pando could not read says nothing about this worktree,
    // so its reason is the answer, not "not running". One it read and
    // could not save still does, and the warning is said with the rest.
    if refreshed.unreadable
        && let Some(warning) = &refreshed.warning
    {
        bail!("{warning}");
    }
    // Said before the answer: a tunnel that died is why there is no public
    // URL to open, and the refresh has already saved it as gone.
    super::report_refresh(&refreshed);
    if want == Want::Public {
        return match record.and_then(|r| r.share.as_ref()) {
            Some(share) => Ok(Opening::Url(share.public_url.clone())),
            None => bail!("{shown} is not shared — `pando share {typed}` publishes it"),
        };
    }
    let not_running = || -> anyhow::Error {
        // "`pando start` starts it" is no advice for a project with nothing
        // to start.
        if config.processes.is_empty() {
            return anyhow::anyhow!(
                "{shown} is not running, and this project has nothing to run — {}",
                crate::doctor::nothing_to_run_fix(&paths.config_file())
            );
        }
        anyhow::anyhow!("{shown} is not running — `pando start {typed}` starts it")
    };
    let Some(record) = record else {
        return Err(not_running());
    };
    match state::aggregate_phase(record) {
        None => Err(not_running()),
        Some(Aggregate::Failed { .. }) => bail!(
            "{shown} has failed — `pando status {typed}` says which process, and \
             `pando restart {typed}` tries again"
        ),
        Some(_) if want == Want::App => {
            let DeviceApps { apps, starting, .. } = device_apps(config, record, |_| true);
            if apps.is_empty() {
                match starting.first() {
                    Some(process) => bail!(
                        "{shown}'s {process} is still starting — `pando start {typed} --wait` \
                         waits for it"
                    ),
                    None => bail!(
                        "{shown} runs no app a simulator or a device opens — `pando open \
                         {typed}` opens its page"
                    ),
                }
            }
            let (apps, refused) = actions::openable_apps(paths, config, record, apps);
            Ok(Opening::Apps {
                apps,
                said: Vec::new(),
                refused,
            })
        }
        Some(_) => {
            // The URL is one process's port: a sibling that is up serves
            // none of it, as `share` says of the same state.
            if let Some(owner) = actions::url_owner_not_running(record) {
                bail!(
                    "{shown} is not running {owner}, the process its URL points at — \
                     `pando start {typed} --only {owner}` starts it"
                );
            }
            if let Some(url) = worktree_url(record) {
                return Ok(Opening::Url(url));
            }
            if record.pageless.is_empty() {
                bail!("{shown} is running and holds no port, so it has no URL to open");
            }
            // Its every port is a process's that serves no page: each app
            // a device runs is opened instead, and the rest said.
            let DeviceApps {
                apps,
                starting,
                stopped,
            } = device_apps(config, record, |p| record.pageless.contains(p));
            let mut said = Vec::new();
            if apps.is_empty() {
                said.push(format!("{shown} serves no page to open in a browser"));
            }
            for process in &record.pageless {
                if starting.contains(process) {
                    said.push(format!(
                        "{process}: its app opens once it runs — `pando start {typed} --wait` \
                         waits for it"
                    ));
                } else if stopped.contains(process) {
                    said.push(format!(
                        "{process}: its app is not running — `pando start {typed} --only \
                         {process}` starts it"
                    ));
                } else if !apps.iter().any(|(name, _)| name == process) {
                    said.push(format!("{process}: its settings say `page = false`"));
                }
            }
            let (apps, refused) = actions::openable_apps(paths, config, record, apps);
            Ok(Opening::Apps {
                apps,
                said,
                refused,
            })
        }
    }
}

/// The processes whose app a device runs, by where they stand.
#[derive(Default)]
struct DeviceApps {
    /// Running: each one's links, to open.
    apps: Vec<(String, AppLinks)>,
    starting: Vec<String>,
    /// Not running, or failed.
    stopped: Vec<String>,
}

/// [`DeviceApps`] of the processes `which` takes.
fn device_apps(
    config: &Config,
    record: &state::WorktreeRecord,
    which: impl Fn(&str) -> bool,
) -> DeviceApps {
    let mut found = DeviceApps::default();
    for (process, links) in actions::app_links(config, record) {
        if !which(&process) {
            continue;
        }
        match record.processes.get(&process).map(|p| &p.phase) {
            Some(Phase::Running { .. }) => found.apps.push((process, links)),
            Some(Phase::Starting { .. }) => found.starting.push(process),
            _ => found.stopped.push(process),
        }
    }
    found
}

/// Opens each app on this machine's simulator or device, saying each
/// wait through `say` and where each opened on `out`. Where there is
/// nothing to open one on, says why and the commands that open it once
/// there is; an open that ran and failed is the error.
pub(super) fn open_apps(
    paths: &PandoPaths,
    apps: &[(String, AppLinks)],
    out: &mut dyn Write,
    say: &dyn Fn(&str),
) -> Result<()> {
    let run = |command: &str| actions::run_command(paths, command);
    let opener = actions::Opener::new(&run);
    for (process, links) in apps {
        say(&format!("opening {process}'s app in {}", links.client));
        match actions::open_app(links, &opener, say) {
            Ok(on) => writeln!(out, "{process}: opened its app in {} on {on}", links.client)?,
            Err(NotOpened::Nowhere(why)) => {
                writeln!(
                    out,
                    "{process}: {why} — once one is, this opens its app in {}:",
                    links.client
                )?;
                for (on, command) in actions::open_commands(links) {
                    writeln!(out, "  on {on}: {command}")?;
                }
            }
            Err(NotOpened::Unknown(why)) => {
                writeln!(
                    out,
                    "{process}: {why} — filled in, this opens its app in {}:",
                    links.client
                )?;
                for (on, command) in actions::open_commands(links) {
                    writeln!(out, "  on {on}: {command}")?;
                }
            }
            Err(failed) => bail!("{process}: {failed}"),
        }
    }
    Ok(())
}

/// Hands `url` to the browser: `$BROWSER` when it is set, the desktop's
/// own opener otherwise — `open` on macOS, Windows' browser under WSL,
/// `xdg-open` elsewhere ([`crate::env_command::browser_commands`]).
pub(super) fn launch(url: &str) -> Result<()> {
    let browser = std::env::var("BROWSER").ok();
    let mut failure = String::new();
    for command in crate::env_command::browser_commands(browser.as_deref(), url) {
        let Some((program, args)) = command.split_first() else {
            continue;
        };
        let ran = std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status();
        failure = match ran {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => format!("{program} could not open {url} ({status}) — open it by hand"),
            Err(e) => format!("could not run {program:?} to open {url} — open it by hand: {e}"),
        };
    }
    bail!("{failure}")
}
