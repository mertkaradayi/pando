use anyhow::{Context, Result};
use clap::Parser;
use std::io::IsTerminal;
use std::process::ExitCode;

use pando::cli::{Cli, dispatch, to_stderr};
use pando::paths::{PandoPaths, default_home};
use pando::{actions, config, project, tui};

/// 0 ok, 1 error, 2 usage (clap's own), 3 needs-answer.
const EXIT_ERROR: u8 = 1;
/// A mistake in what was asked for: clap's own code for it, used for the
/// argument errors clap cannot catch — an answers file naming a question
/// pando does not ask.
const EXIT_USAGE: u8 = 2;
/// pando has a question it cannot answer on its own. Its own code, so an
/// agent can tell "ask the human" from "it broke" without parsing text.
pub const EXIT_NEEDS_ANSWER: u8 = 3;

fn main() -> ExitCode {
    // Before any thread starts: reading the umask means setting it,
    // process-wide, for a moment, and a fork made while another thread
    // sets up libnotify kills the child on macOS.
    pando::platform::init();
    // Parsed before anything else so `--help` and `--version` work outside a
    // repository, and a usage error exits 2 through clap.
    let cli = Cli::parse();
    // A completion script is only pando's own arguments, printed: it asks
    // the OS for nothing, so it is written wherever pando builds.
    let prints_only = matches!(cli.command, Some(pando::cli::Command::Completions { .. }));
    if !prints_only && let Some(why) = pando::platform::unsupported() {
        eprintln!("pando: {why}");
        return ExitCode::FAILURE;
    }
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        // A question is not a failure. It gets its own exit code and its own
        // shape, so an agent can answer it instead of guessing what broke.
        // The same, from `check`, which takes no `--yes`: the way out goes
        // through `pando init`.
        Err(e) if e.downcast_ref::<pando::cli::CheckNeedsAnswer>().is_some() => {
            let needs = e
                .downcast_ref::<pando::cli::CheckNeedsAnswer>()
                .expect("just checked");
            to_stderr(&pando::cli::render_needs_answer_for(
                &needs.0,
                pando::cli::Rerun::InitThenCheck,
            ));
            ExitCode::from(EXIT_NEEDS_ANSWER)
        }
        Err(e) if e.downcast_ref::<actions::NeedsAnswer>().is_some() => {
            let needs = e
                .downcast_ref::<actions::NeedsAnswer>()
                .expect("just checked");
            to_stderr(&pando::cli::render_needs_answer(needs));
            ExitCode::from(EXIT_NEEDS_ANSWER)
        }
        // And a question that was answered *wrongly* is not a failure
        // either: it is the same class of mistake as a misspelled flag,
        // and it gets the same code.
        Err(e)
            if e.downcast_ref::<pando::cli::UsageError>().is_some()
                || e.downcast_ref::<actions::RefusedAnswer>().is_some() =>
        {
            say(&e);
            ExitCode::from(EXIT_USAGE)
        }
        // `doctor` has already printed every problem it found, with what
        // to do about each one. All that is left is the code, and a line
        // under the report repeating that it failed would be a reason the
        // command did not print.
        Err(e) if e.downcast_ref::<pando::doctor::Unhealthy>().is_some() => {
            ExitCode::from(EXIT_ERROR)
        }
        // Whoever read stdout stopped, having read what they wanted —
        // `pando logs | head`. Nothing failed, so nothing is said.
        Err(e) if pando::cli::stdout_closed(&e) => ExitCode::SUCCESS,
        Err(e) => {
            say(&e);
            ExitCode::from(EXIT_ERROR)
        }
    }
}

/// The error on stderr. `{:#}` flattens the context chain onto one line: a
/// CLI failure is one sentence, not a stack.
fn say(e: &anyhow::Error) {
    to_stderr(&format!(
        "pando: {}\n",
        pando::remedy::for_cli(&format!("{e:#}"))
    ));
}

fn run(cli: Cli) -> Result<()> {
    // Before anything looks for a repository. The share proxy runs detached
    // from a temp directory, with no project, no config and no home to
    // guard — one environment variable and two ports are all it has.
    if let Some(pando::cli::Command::ShareProxy { listen, upstream }) = cli.command {
        return pando::cli::run_share_proxy(listen, upstream);
    }
    // A completion script is about pando, not about any repository, and
    // it is typically generated from a dotfiles setup that is in none.
    if let Some(pando::cli::Command::Completions { shell }) = cli.command {
        return pando::cli::completions(shell, &mut pando::cli::Stdout);
    }
    // Updating pando is about the binary, not about any repository.
    if let Some(pando::cli::Command::Update { check }) = cli.command {
        return pando::cli::update(check, &mut pando::cli::Stdout);
    }
    // The home before the current directory: `ls --all` and the grove
    // need only the home, and a shell left in a directory that was
    // removed — a worktree `rm`'d from another pane — still lists what is
    // left. A relative `PANDO_HOME` is resolved against the current
    // directory rather than compared as-is: `.pando` looks like it is
    // outside the repository until the moment it is created inside it.
    let configured = default_home();
    let cwd = std::env::current_dir().context(
        "cannot read the current directory — it may have been deleted; cd somewhere that exists",
    );
    let home = match &cwd {
        Ok(cwd) => cwd.join(&configured),
        Err(_) => configured,
    };
    // Every project on the machine is about the home, not about any
    // repository: from anywhere, and with nothing to load or guard.
    if let Some(pando::cli::Command::Ls {
        all: true,
        json,
        long,
        ..
    }) = cli.command
        && home.is_absolute()
    {
        return pando::cli::ls_machine(&home, json, long, &mut pando::cli::Stdout);
    }
    let cwd = cwd?;
    // `pando` alone outside any repository opens the grove: every project
    // this machine's pando knows, with none in front. A folder of
    // repositories pando does not know opens it too: the TUI would make
    // the folder a project by looking at it, and only a command —
    // `init` or `new` — says that is what was wanted.
    let sight = match cli.command {
        None => project::FirstSight::Deny,
        Some(_) => project::FirstSight::Allow,
    };
    let project = match project::discover_as(&cwd, &home, sight) {
        Ok(project) => project,
        Err(e) if cli.command.is_none() && std::io::stdout().is_terminal() => {
            let style = pando::term::Style::with(false, pando::platform::dirs::home());
            let tilde = |p: &std::path::Path| style.tilde(&p.display().to_string());
            let never = e.downcast_ref::<project::NeverAFolder>();
            let why = match (e.downcast_ref::<project::UnknownFolder>(), never) {
                // Repositories here that are no project: what the folder
                // is, then the way into one of them.
                (None, Some(never)) => format!(
                    "{} {} · `cd` into one of its {} repositories for its project",
                    tilde(&never.dir),
                    never.reason(tilde),
                    never.repos.len()
                ),
                // The reason first, so a narrow screen cuts the names
                // rather than it; the folder is where pando was run.
                (Some(folder), _) => format!(
                    "{} repositories here, no project of them yet · `pando init` or `pando new \
                     <branch>` here makes them one ({})",
                    folder.repos.len(),
                    folder.names()
                ),
                // Not being in a repository is why the home is open, not
                // a fault: said as where pando is, the reason first so a
                // narrow screen cuts the path rather than it.
                (None, None) if format!("{e:#}").starts_with("not inside a git repository") => {
                    format!("no repository here ({})", tilde(&cwd))
                }
                (None, None) => format!("{e:#}"),
            };
            return tui::run_home(home, why);
        }
        // Into a pipe the TUI would draw for nobody: said as such, rather
        // than as the reason it would have opened the grove.
        Err(_) if cli.command.is_none() => return Err(no_terminal().into()),
        Err(e) if format!("{e:#}").starts_with("not inside a git repository") => {
            return Err(anyhow::anyhow!("{e:#}{}", way_in(&cwd)));
        }
        Err(e) => return Err(e),
    };
    let mut paths = PandoPaths::new(home, project);
    // `new`, `start`, `restart` and the TUI act on `pando.toml`; nothing
    // else needs it. A home layer pando cannot use stops those and only
    // those — you need `stop` most when that file is broken, and `ls` to
    // see what is there at all.
    let needs_config = cli
        .command
        .as_ref()
        .map(pando::cli::Command::needs_config)
        .unwrap_or(true);
    let loaded = match config::load(&paths) {
        Ok(loaded) => loaded,
        Err(e) if needs_config => return Err(e),
        Err(e) => {
            to_stderr(&format!(
                "pando: {e:#} — carrying on without it; `new`, `start` and `restart` need it \
                 fixed\n"
            ));
            config::load_without_home(&paths)
        }
    };
    for warning in &loaded.warnings {
        to_stderr(&format!("pando: {warning}\n"));
    }
    // A multi-repo project's repositories are what is on disk, unless its
    // config names fewer.
    for warning in loaded.config.apply_to(&mut paths.project) {
        to_stderr(&format!("pando: {warning}\n"));
    }
    // Once, before dispatch: every command shares the same home, so a home
    // in the working tree is worth refusing even on a read-only command.
    actions::guard_write_locations(&paths, &loaded.config)?;
    // Where this repository is, for `ls --all` and Grove: a project pando
    // made a directory for before the registry existed is registered by
    // whatever runs in it next, a read included. Never a directory made
    // for it — that is the first write's, `ensure_home`'s, which registers
    // it too.
    let _ = project::remember(&paths.project_dir(), &paths.project);
    match cli.command {
        Some(command) => {
            // A command that waits for another pando's lock says so, once,
            // rather than looking hung. The TUI's stderr is its screen.
            pando::state::announce_lock_waits(|line| to_stderr(&format!("pando: {line}\n")));
            dispatch(command, &paths, &loaded.config)
        }
        // The TUI draws on stdout. Into a pipe it drew for nobody, and
        // waited for keys nobody would press.
        None if !std::io::stdout().is_terminal() => Err(no_terminal().into()),
        None => tui::run(paths, loaded.config),
    }
}

/// What `pando` with no command says into a pipe.
fn no_terminal() -> pando::cli::UsageError {
    pando::cli::UsageError(
        "`pando` with no command opens the TUI, which needs a terminal — `pando --help` lists \
         the commands, `pando ls` the worktrees, and `pando ls --all` every project pando knows"
            .to_string(),
    )
}

/// Where to go from `cwd`, which is in no project: the repository in it
/// when there is one, and `ls --all`, which works from anywhere.
fn way_in(cwd: &std::path::Path) -> String {
    let repos = project::repos_in(cwd);
    let into = match repos.as_slice() {
        [] => String::new(),
        [one] => format!("; `cd {one}` for its project"),
        many => format!(
            "; `cd` into one of the {} repositories here for its project",
            many.len()
        ),
    };
    format!("{into} — `pando ls --all` lists every project pando knows, from anywhere")
}
