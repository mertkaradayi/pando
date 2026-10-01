//! The tools section: every program the project runs, probed in the shell a
//! start uses.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::actions::{self, Machine};
use crate::catalog::{package_managers, tools};
use crate::config::{Config, HookScope, ServiceConfig};
use crate::paths::PandoPaths;
use crate::process as proc;
use crate::runtime;
use crate::{detect, services, tunnel};

use super::report::{Finding, Section, Severity, ToolReport};

pub(super) const TOOL_PATH_MARK: &str = "pando-tool-path:";
pub(super) const TOOL_VERSION_MARK: &str = "pando-tool-version:";
pub(super) const TOOL_DETAIL_MARK: &str = "pando-tool-detail:";
pub(super) const TOOL_DONE_MARK: &str = "pando-tool-ok";

/// One executable to look for, and what to ask it.
pub(super) struct ToolProbe {
    pub(super) name: String,
    pub(super) program: String,
    /// What to pass it for a version. Static, and pando's own: nothing a
    /// project wrote reaches a command line here.
    pub(super) args: &'static str,
    /// A second question worth one line, such as which docker context is
    /// active.
    pub(super) detail_args: Option<&'static str>,
    /// How the detail is introduced when there is one.
    pub(super) detail_label: &'static str,
    pub(super) needed_for: String,
    /// What to say when it is not there. `None` means a line and nothing
    /// more — a tool this project has not asked pando to run.
    pub(super) missing: Option<(Severity, String)>,
    /// Whether pando runs it itself rather than through the login shell.
    /// Such a tool is looked for on the PATH pando was started with and
    /// with no prelude in front, because that is where `Command::new`
    /// looks: a profile or a prelude that finds it helps nothing that
    /// runs it.
    pub(super) direct: bool,
}

#[derive(Default)]
struct ToolFound {
    path: Option<String>,
    version: Option<String>,
    detail: Option<String>,
}

pub(super) fn tools_report(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
    findings: &mut Vec<Finding>,
) -> Vec<ToolReport> {
    let probes = tool_probes(paths, config);
    let wsl = crate::wsl::Wsl::at(&machine.system);
    // Run behind the prelude, because every command pando spawns through
    // the shell is run behind it: a package manager that only exists after
    // `nvm use` is there for a real spawn and would read as missing here.
    // A tool pando runs itself is not, and is asked without it.
    let prelude = config.runtime.prelude.clone().unwrap_or_default();
    let (found, failure) = probe_tools(machine.shell, &probes, prelude.trim());
    match &failure {
        Some(ProbeFailure::Unanswered(why)) => findings.push(Finding::note(
            Section::Tools,
            format!("pando could not ask this shell what it has: {why}"),
        )),
        // A problem, not a note: every process, hook and share runs behind
        // the same prelude, so every one of them fails the same way — and
        // the runtime section says so only for a language the repository
        // pins.
        Some(ProbeFailure::Prelude(last)) => findings.push(Finding::problem(
            Section::Tools,
            format!(
                "the prelude {:?}{} fails before any command runs — {last}; every process, \
                 hook and share runs behind it",
                prelude.trim(),
                match crate::config::prelude_origin(paths) {
                    Some(from) => format!(" in {}", from.display()),
                    None => String::new(),
                }
            ),
            "fix that line, or set `[runtime].prelude = \"\"` if this machine needs nothing in \
             front of a command",
        )),
        None => {}
    }
    let mut out = Vec::new();
    let mut windows_programs: Vec<String> = Vec::new();
    for (index, probe) in probes.iter().enumerate() {
        let entry = found.get(&index);
        let present = entry.is_some_and(|f| f.path.is_some());
        // Only for a tool the shell was asked about and did not have.
        let install = (!present && failure.is_none())
            .then(|| tools::how_to_get(&probe.name))
            .flatten();
        // Only when the probe itself ran: "not found" is a claim about
        // this machine, and a shell that never answered has not made it.
        if !present
            && failure.is_none()
            && let Some((severity, reason)) = &probe.missing
        {
            let install_it = match install {
                Some(get) => format!("install it with {get}"),
                None => "install it".to_string(),
            };
            let (on, fix) = match probe.direct {
                true => (
                    "the PATH pando was started with",
                    format!(
                        "{install_it} so it is on the PATH pando is started with, or put a \
                         shim at {}",
                        paths.home.join("bin").join(&probe.name).display()
                    ),
                ),
                false => (
                    "the PATH `bash -lc` has",
                    format!(
                        "{install_it}, or set [runtime].prelude so a login bash shell finds it"
                    ),
                ),
            };
            findings.push(Finding {
                section: Section::Tools,
                severity: *severity,
                message: format!("{} is not on {on} — {reason}", probe.name),
                fix: Some(fix),
            });
        }
        // Once per program: `docker compose` is docker found again.
        if let Some(wsl) = &wsl
            && let Some(path) = entry.and_then(|f| f.path.as_deref())
            && let Some(drive) = wsl.drive_of(Path::new(path))
            && !windows_programs.contains(&probe.program)
        {
            windows_programs.push(probe.program.clone());
            findings.push(windows_side(probe, path, drive));
        }
        out.push(ToolReport {
            name: probe.name.clone(),
            path: entry.and_then(|f| f.path.clone()),
            version: entry.and_then(|f| f.version.clone()),
            detail: entry
                .and_then(|f| f.detail.clone())
                .map(|d| format!("{}{d}", probe.detail_label)),
            needed_for: probe.needed_for.clone(),
            install: install.map(str::to_string),
            found: present,
            asked: failure.is_none(),
        });
    }
    if failure.is_none() {
        daemon_check(paths, config, machine, &mut out, findings);
    }
    out
}

/// A tool found on a Windows drive under WSL, which appends Windows' PATH
/// to its own: with no Linux copy installed, the shell finds Windows'.
/// That is a shell script Windows' installers leave for Git Bash — Node's
/// `npm`, nvm-windows' `pnpm` — which runs Windows' files from Linux and,
/// with no Linux node, ends in `node: not found`; or Docker Desktop's
/// `docker`, which only says to turn on its WSL integration. It is as good
/// as missing, so it is as serious as missing would be.
fn windows_side(probe: &ToolProbe, path: &str, drive: &Path) -> Finding {
    let name = &probe.name;
    let install = match (name.as_str(), tools::how_to_get(name)) {
        ("docker", _) => "turn on Docker Desktop's WSL integration for this distro (Settings → \
                          Resources → WSL integration), which puts its Linux docker first"
            .to_string(),
        (_, Some(get)) => {
            format!("install {name} inside WSL ({get}), so the Linux one comes first")
        }
        (_, None) => format!("install {name} inside WSL, so the Linux one comes first"),
    };
    Finding {
        section: Section::Tools,
        severity: probe
            .missing
            .as_ref()
            .map_or(Severity::Note, |(severity, _)| *severity),
        message: format!(
            "{name} is Windows' copy, on the drive at {} ({path}): WSL appends Windows' PATH to \
             its own, and no Linux {name} comes before it",
            drive.display()
        ),
        fix: Some(format!(
            "{install}; or keep Windows' PATH out of WSL with `appendWindowsPath = false` under \
             `[interop]` in /etc/wsl.conf, then `wsl --shutdown` from Windows"
        )),
    }
}

/// The assignment that gives a script the PATH pando itself was started
/// with, which is where a program pando runs directly is looked for.
fn own_path() -> String {
    let path = std::env::var_os("PATH").unwrap_or_default();
    format!("PATH={}", proc::shell_quote(&path.to_string_lossy()))
}

/// Marks the daemon probe's own output, so a chatty login profile cannot
/// be read as the answer.
pub(super) const DAEMON_MARK: &str = "pando-docker-daemon:";

/// How long the daemon gets to answer. `docker info` against a daemon
/// that is down fails at once; one that hangs is as good as down, and is
/// not worth a login shell's whole deadline.
pub(super) const DAEMON_WAIT_SECS: u64 = 5;

/// What the Docker daemon said when asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Daemon {
    Up,
    /// Not answering, and what `docker info` said about it.
    Down(String),
    /// The probe itself did not report: nothing to claim.
    Unknown,
}

/// Whether an isolated start of this project runs anything in Docker: a
/// compose entry that includes a service. One that includes none is the
/// written-down answer "none of them", and a start brings nothing up for
/// it, so it needs neither the client nor the daemon.
fn runs_containers(config: &Config) -> bool {
    config
        .services
        .iter()
        .any(|s| matches!(s, ServiceConfig::Compose { .. }) && s.brings_anything_up())
}

/// Asks the Docker daemon whether it is up, when this project declares
/// compose services and the client is there to ask.
///
/// `docker --version` answers without a daemon, so a tools line that only
/// showed the client said nothing about whether `start --isolated` could
/// work. Same shell as every other probe, but on the PATH pando runs
/// docker with and with no prelude, as the docker probe itself is, and
/// bounded by its own short watchdog.
fn daemon_check(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
    tools: &mut [ToolReport],
    findings: &mut Vec<Finding>,
) {
    let Some(docker) = tools.iter_mut().find(|t| t.name == "docker" && t.found) else {
        return;
    };
    if !runs_containers(config) {
        return;
    }
    let program = services::docker_program(paths).display().to_string();
    let answer = (machine.shell)(&daemon_script(&program, &own_path()))
        .map(|text| parse_daemon(&text))
        .unwrap_or(Daemon::Unknown);
    let state = match &answer {
        Daemon::Up => "daemon: running",
        Daemon::Down(_) => "daemon: not running",
        Daemon::Unknown => return,
    };
    docker.detail = Some(match docker.detail.take() {
        Some(detail) => format!("{detail}, {state}"),
        None => state.to_string(),
    });
    if let Daemon::Down(reason) = answer {
        // Under WSL, Docker Desktop's daemon runs on Windows and answers
        // in a distro only once its WSL integration is on for it: until
        // then the distro's own `docker` reaches for a socket nobody
        // serves, with Docker Desktop up the whole time.
        let start = match crate::wsl::Wsl::at(&machine.system) {
            Some(_) => {
                "start Docker Desktop with its WSL integration on for this distro (Settings → \
                 Resources → WSL integration), since a daemon on Windows answers in WSL only \
                 through it"
            }
            None => "start Docker",
        };
        findings.push(Finding::problem(
            Section::Tools,
            format!(
                "the Docker daemon is not running (`docker info`: {reason}) — this project \
                 declares compose services, so `start --isolated` cannot bring them up"
            ),
            // `prefer` alone changes nothing here: it only settles the
            // services question, and the compose entries have answered it.
            format!(
                "{start}; or run the services without it: remove the `[[services]] kind = \
                 \"compose\"` entries from {} and set `[isolation] prefer = \"native\"` in {}, \
                 so the next `start --isolated` settles them again on pando's recipes where it \
                 has them; or {}",
                crate::config::services_origin(paths).display(),
                super::config::user_config_shown(paths),
                crate::remedy::SHARED.cli
            ),
        ));
    }
}

/// The daemon probe: `docker info` with a watchdog that kills it after
/// [`DAEMON_WAIT_SECS`], then its exit status behind [`DAEMON_MARK`].
///
/// The watchdog's own output goes to /dev/null so it cannot hold the
/// command substitution's pipe open after docker has answered. `before`
/// runs first, and the probe only when it succeeds.
pub(super) fn daemon_script(program: &str, before: &str) -> String {
    let program = proc::shell_quote(program);
    let body = format!(
        "__pando_d=$( {program} info --format '{{{{.ServerVersion}}}}' 2>&1 & __pando_p=$!; \
         ( sleep {DAEMON_WAIT_SECS}; kill $__pando_p ) >/dev/null 2>&1 & __pando_w=$!; \
         wait $__pando_p; __pando_s=$?; kill $__pando_w 2>/dev/null; \
         echo \"{DAEMON_MARK}$__pando_s\" )\nprintf '%s\\n' \"$__pando_d\"\n"
    );
    match before {
        "" => body,
        before => format!("{before} && {{\n{body}}}"),
    }
}

pub(super) fn parse_daemon(text: &str) -> Daemon {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let Some(at) = lines.iter().position(|l| l.starts_with(DAEMON_MARK)) else {
        return Daemon::Unknown;
    };
    let status = lines[at][DAEMON_MARK.len()..].trim();
    if status == "0" {
        return Daemon::Up;
    }
    // 143 is SIGTERM: the watchdog's.
    if status == "143" {
        return Daemon::Down(format!("no answer within {DAEMON_WAIT_SECS}s"));
    }
    let reason = lines[..at]
        .iter()
        .rev()
        .find(|l| !l.is_empty())
        .copied()
        .unwrap_or("it exited without saying why");
    Daemon::Down(reason.to_string())
}

/// Every tool worth asking about: the ones pando itself runs, and the ones
/// this project's own config and lockfiles say it will run.
fn tool_probes(paths: &PandoPaths, config: &Config) -> Vec<ToolProbe> {
    let mut probes = vec![ToolProbe {
        name: "git".to_string(),
        program: "git".to_string(),
        args: "--version",
        detail_args: None,
        detail_label: "",
        needed_for: "everything: worktrees are git's".to_string(),
        missing: Some((
            Severity::Problem,
            "pando has nothing to manage without it".to_string(),
        )),
        direct: false,
    }];

    // The shim hook `services::docker_program` already knows about, so a
    // developer whose docker is not on PATH is reported through the same
    // binary an isolated start would use — and looked for where that start
    // looks, since pando runs docker itself rather than through the shell.
    let docker = services::docker_program(paths).display().to_string();
    let needed = "`start --isolated`, which runs private copies of the project's services";
    let missing_docker = runs_containers(config).then(|| {
        (
            Severity::Note,
            "this project declares compose services, so `--isolated` cannot run; a plain \
             `start` still can"
                .to_string(),
        )
    });
    probes.push(ToolProbe {
        name: "docker".to_string(),
        program: docker.clone(),
        args: "--version",
        detail_args: Some("context show"),
        detail_label: "context: ",
        needed_for: needed.to_string(),
        missing: missing_docker.clone(),
        direct: true,
    });
    probes.push(ToolProbe {
        name: "docker compose".to_string(),
        program: docker,
        args: "compose version --short",
        detail_args: None,
        detail_label: "",
        needed_for: needed.to_string(),
        // Never its own finding: when docker is missing this is the same
        // news twice, and when docker is there a compose plugin that is
        // not is reported by the line.
        missing: None,
        direct: true,
    });
    probes.push(ToolProbe {
        name: "cloudflared".to_string(),
        program: tunnel::cloudflared_program(paths).display().to_string(),
        args: "--version",
        detail_args: None,
        detail_label: "",
        needed_for: "`pando share`, which publishes a worktree at a public URL".to_string(),
        // `share` is opt-in and says this itself. A line is enough. Direct,
        // because `share` checks for it on pando's own PATH before it runs.
        missing: None,
        direct: true,
    });
    probes.push(ToolProbe {
        name: "gh".to_string(),
        program: "gh".to_string(),
        args: "--version",
        detail_args: None,
        detail_label: "",
        needed_for: "the TUI's pull request picker".to_string(),
        // Opt-in like `share`, and the picker says so itself. Direct,
        // because the TUI runs it without a shell.
        missing: None,
        direct: true,
    });

    for (program, needed_for, missing) in project_programs(paths, config) {
        if probes.iter().any(|p| p.program == program) {
            continue;
        }
        probes.push(ToolProbe {
            name: program.clone(),
            program,
            args: "--version",
            detail_args: None,
            detail_label: "",
            needed_for,
            missing,
            direct: false,
        });
    }
    probes
}

/// Programs this project will have pando run, each with what it is for
/// and what to say when it is not there.
///
/// Config naming one is the difference between a line and a problem: a
/// lockfile is a signal that a manager is *probably* wanted, an
/// `install =` is pando being told to run it. A hook is a problem only
/// when every start runs it: one only a start with data of its own runs is
/// a note, as missing docker for compose services is, and one switched
/// off with `on = "never"` needs nothing.
fn project_programs(paths: &PandoPaths, config: &Config) -> Vec<ProjectProgram> {
    let mut out: Vec<ProjectProgram> = Vec::new();
    if let Some(install) = config.project.install.as_deref()
        && let Some(program) = command_program(install)
    {
        let needed_for = "the install step every new worktree runs".to_string();
        out.push((
            program,
            needed_for.clone(),
            Some((Severity::Problem, needed_for)),
        ));
    }
    for hook in &config.hooks {
        let scope = actions::hook_scope(config, hook);
        if scope == HookScope::Never {
            continue;
        }
        let Some(program) = command_program(&hook.cmd) else {
            continue;
        };
        let needed_for = format!("the hook {:?}", hook.name);
        let missing = match scope {
            HookScope::Isolated => (
                Severity::Note,
                format!(
                    "{needed_for}, which only a start with data of its own (`--isolated` or \
                     `--namespaced`) runs; a plain `start` still can"
                ),
            ),
            _ => (Severity::Problem, needed_for.clone()),
        };
        match out.iter_mut().find(|(p, _, _)| *p == program) {
            // One line per program, named after the hook that needs it on
            // every start when there is one.
            Some(entry)
                if missing.0 == Severity::Problem
                    && matches!(entry.2, Some((Severity::Note, _))) =>
            {
                *entry = (program, needed_for, Some(missing));
            }
            Some(_) => {}
            None => out.push((program, needed_for, Some(missing))),
        }
    }
    let signals = detect::signals(paths.root());
    // The root's lockfiles, then each app directory's under its path.
    let app_lockfiles = signals.app_dirs.iter().flat_map(|app| {
        app.lockfiles
            .iter()
            .map(move |lockfile| (lockfile, format!("{}/{lockfile}", app.dir)))
    });
    let lockfiles = signals
        .lockfiles
        .iter()
        .map(|lockfile| (lockfile, lockfile.clone()))
        .chain(app_lockfiles);
    for (lockfile, path) in lockfiles {
        let Some(manager) = package_managers::for_lockfile(lockfile) else {
            continue;
        };
        let program = manager.program;
        if out.iter().any(|(p, _, _)| p == program) {
            continue;
        }
        out.push((
            program.to_string(),
            format!("{path} is in this repository"),
            None,
        ));
    }
    out
}

/// A program the project runs, what it is for, and what to say when it is
/// not there: [`ToolProbe::missing`].
type ProjectProgram = (String, String, Option<(Severity, String)>);

/// The program a shell command really runs: the last `&&`-joined step's
/// first word that is not a `KEY=value` prefix.
///
/// The last step, because `corepack enable && pnpm install` is about pnpm.
pub(super) fn command_program(cmd: &str) -> Option<String> {
    let steps: Vec<&str> = cmd.split("&&").filter(|s| !s.trim().is_empty()).collect();
    let step = steps.last()?;
    let word = step.split_whitespace().find(|w| !w.contains('='))?;
    // A path, a template or a quoted word is not a program name worth
    // asking `command -v` about, and it is exactly the shape that would
    // put something surprising on a command line.
    let plain = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    plain.then(|| word.to_string())
}

/// Asks one shell where every tool is and what it says it is.
///
/// One shell for all of them, and it is the *same* shell the runtime probe
/// and every spawn use — `bash -lc` in the main checkout, output captured,
/// with a deadline on it. A login shell reads a profile, so a dozen of
/// them is a dozen times the wait for one answer; and going through
/// `Machine` is what lets a test report on a laptop it does not have.
///
/// Returns what it learned plus, when the shell never got to the end, why:
/// "not found" is a claim about this machine, and a probe that did not run
/// has not made it.
fn probe_tools(
    shell: runtime::Shell<'_>,
    probes: &[ToolProbe],
    prelude: &str,
) -> (BTreeMap<usize, ToolFound>, Option<ProbeFailure>) {
    if probes.is_empty() {
        return (BTreeMap::new(), None);
    }
    let script = tool_script(probes, prelude);
    let Some(text) = shell(&script) else {
        return (
            BTreeMap::new(),
            Some(ProbeFailure::Unanswered(
                "the shell did not answer inside its deadline".to_string(),
            )),
        );
    };
    let mut found: BTreeMap<usize, ToolFound> = BTreeMap::new();
    let mut done = false;
    for line in text.lines() {
        let line = line.trim();
        if line == TOOL_DONE_MARK {
            done = true;
            continue;
        }
        for (mark, field) in [
            (TOOL_PATH_MARK, 0u8),
            (TOOL_VERSION_MARK, 1),
            (TOOL_DETAIL_MARK, 2),
        ] {
            let Some(rest) = line.strip_prefix(mark) else {
                continue;
            };
            let Some((index, value)) = rest.split_once(' ') else {
                continue;
            };
            let Ok(index) = index.parse::<usize>() else {
                continue;
            };
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            let entry = found.entry(index).or_default();
            match field {
                0 => entry.path = Some(value.to_string()),
                1 => entry.version = Some(value.to_string()),
                _ => entry.detail = Some(value.to_string()),
            }
        }
    }
    if done {
        return (found, None);
    }
    // The body never ran. With a prelude set that is the prelude's
    // failure, and it is the same one every spawn would hit. A body that
    // printed a mark did run, so the prelude in front of it got through;
    // a tool pando runs itself is asked before the prelude, so its marks
    // say nothing about it.
    let last = text
        .lines()
        .map(str::trim)
        .rev()
        .find(|line| !line.is_empty())
        .unwrap_or("no output")
        .to_string();
    let behind_prelude = found
        .keys()
        .any(|index| probes.get(*index).is_some_and(|probe| !probe.direct));
    let failure = match prelude.is_empty() || behind_prelude {
        true => ProbeFailure::Unanswered(format!("the probe did not finish — {last}")),
        false => ProbeFailure::Prelude(last),
    };
    (found, Some(failure))
}

/// Why the tools probe claims nothing about this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProbeFailure {
    /// The shell did not answer, or stopped before the end of the probe
    /// with nothing in front of it that could have stopped it.
    Unanswered(String),
    /// The prelude in front of the probe failed, and this is the last
    /// thing it said.
    Prelude(String),
}

/// One shell script for every probe, composed the way a real spawn is.
///
/// A tool pando runs itself is asked first, in a subshell on the PATH
/// pando was started with and outside the prelude; every other one behind
/// the prelude, as a spawn is.
pub(super) fn tool_script(probes: &[ToolProbe], prelude: &str) -> String {
    let mut direct = String::new();
    let mut body = String::new();
    for (index, probe) in probes.iter().enumerate() {
        let out = match probe.direct {
            true => &mut direct,
            false => &mut body,
        };
        let program = proc::shell_quote(&probe.program);
        let _ = write!(
            out,
            "if __pando_p=$(command -v {program} 2>/dev/null); then \
             printf '{TOOL_PATH_MARK}{index} %s\\n' \"$__pando_p\"; \
             printf '{TOOL_VERSION_MARK}{index} %s\\n' \
             \"$({program} {} 2>&1 | head -n 1)\"; ",
            probe.args
        );
        if let Some(detail) = probe.detail_args {
            let _ = write!(
                out,
                "printf '{TOOL_DETAIL_MARK}{index} %s\\n' \
                 \"$({program} {detail} 2>&1 | head -n 1)\"; "
            );
        }
        let _ = writeln!(out, "fi");
    }
    let _ = writeln!(body, "echo {TOOL_DONE_MARK}");
    let body = match prelude {
        "" => body,
        prelude => format!("{prelude} && {{\n{body}}}"),
    };
    match direct.is_empty() {
        true => body,
        false => format!("( {}\n{direct})\n{body}", own_path()),
    }
}
