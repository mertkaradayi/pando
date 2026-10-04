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
    // Read before any thread starts: reading the umask means setting it,
    // process-wide, for a moment.
    let _ = pando::cow::umask();
    // Before any thread exists: a fork made while another thread sets up
    // libnotify kills the child on macOS (see the function).
    pando::process::settle_before_fork();
    // Parsed before anything else so `--help` and `--version` work outside a
    // repository, and a usage error exits 2 through clap.
    let cli = Cli::parse();
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
    let cwd = std::env::current_dir().context(
        "cannot read the current directory — it may have been deleted; cd somewhere that exists",
    )?;
    let project = project::discover(&cwd)?;
    // A relative `PANDO_HOME` is resolved here rather than compared as-is:
    // `.pando` looks like it is outside the repository until the moment it
    // is created inside it.
    let paths = PandoPaths::new(cwd.join(default_home()), project);
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
    // Once, before dispatch: every command shares the same home, so a home
    // in the working tree is worth refusing even on a read-only command.
    actions::guard_write_locations(&paths, &loaded.config)?;
    match cli.command {
        Some(command) => dispatch(command, &paths, &loaded.config),
        // The TUI draws on stdout. Into a pipe it drew for nobody, and
        // waited for keys nobody would press.
        None if !std::io::stdout().is_terminal() => Err(pando::cli::UsageError(
            "`pando` with no command opens the TUI, which needs a terminal — `pando --help` \
             lists the commands, and `pando ls` the worktrees"
                .to_string(),
        )
        .into()),
        None => tui::run(paths, loaded.config),
    }
}
