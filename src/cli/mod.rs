//! The clap front end. A thin wrapper: every behaviour lives in `actions`.

use crate::actions;
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::share_proxy;
use crate::worktree::Worktree;
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::io::Write;

mod agent;
mod answers;
mod art;
mod check;
mod completion;
mod doctor;
mod logs;
mod ls;
mod names;
mod open;
mod prompt;
mod signals;
mod status;
mod tip;
mod wait;

use self::answers::init_asker;
use self::answers::read_answers;
use self::answers::refuse_answered;
use self::answers::report_unused;
use self::answers::volunteered_from;
use self::answers::{InitFiles, render_init};
use self::prompt::everyday_asker;
pub use agent::Reference;
pub use answers::{
    Answers, CheckNeedsAnswer, Rerun, UsageError, render_needs_answer, render_needs_answer_for,
    slot_name,
};
pub use doctor::{adopt_project, doctor};
pub use logs::logs;
pub use ls::{Col, LsView, keep_columns, ls_json, ls_text, ls_text_at, ls_text_with};
pub use signals::signals_json;
pub use status::{status_json, status_text, status_text_at};

/// Shape version for machine-readable output, bumped independently of the
/// crate version so agents can pin what they parse.
pub const JSON_VERSION: u32 = 2;

/// How long the sha `ls --json` publishes is, as the JSON documents it.
/// Not what `%h` gives: git lengthens that in a large repository, and
/// `core.abbrev` sets it to anything.
const SHORT_SHA_LEN: usize = 7;

/// Documented on `--help` because an agent driving pando needs to know that
/// 3 is "ask the human", not "it broke". The examples come first: they are
/// what a person opening `--help` for the first time is looking for.
const MAIN_AFTER_HELP: &str = "\
Examples:
  pando                            open the TUI for the repository you are in
  pando new feat/login             a worktree and branch, forked from the default base
  pando start feat/login           start it; on a terminal, wait until it answers
  pando open feat/login            its URL in the browser
  pando logs -f                    follow the log of the worktree you are in
  pando ls                         every worktree: status, URL, ports, git
  pando check                      test the setup in a throwaway worktree

A worktree is named by its branch (feat/login) or its directory (feat+login).
Inside a worktree, start, stop, restart, logs, open, share and unshare need
no name: they act on the worktree you are in.

The main checkout runs too, named by its branch or its directory, or with
no name from inside it — but for stop, which there stops every one. pando
runs only its processes: no install, no hooks, on the project's own
services.

Exit codes:
  0  ok
  1  error
  2  usage
  3  needs an answer — the question is printed on stderr; --yes accepts \
pando's own recommendation";

#[derive(Parser, Debug)]
#[command(
    name = "pando",
    version = crate::version::version(),
    about = "One repo. Every branch alive.",
    after_help = MAIN_AFTER_HELP
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Create a worktree and branch from the default base.
    #[command(after_help = "\
Examples:
  pando new feat/login               a new branch, from origin's default branch
  pando new feat/login --base dev    forked from dev instead
  pando new fix/123                  an existing branch, local or on origin")]
    #[command(display_order = 1)]
    New {
        /// Branch name. Slashes become plus signs in the directory name.
        branch: String,
        /// Accept pando's own recommendation for anything it would ask.
        #[arg(long)]
        yes: bool,
        /// Base to fork a new branch from.
        ///
        /// A bare name prefers the remote-tracking ref, so a stale local
        /// branch is never the fork point.
        #[arg(long)]
        base: Option<String>,
    },
    /// List worktrees: what each one runs, its URL and ports, and its git
    /// state. The main checkout is the first row.
    ///
    /// Fitted to the terminal: on a narrow one the least useful columns go
    /// first, and the name, status and URL stay. MODE appears once a
    /// worktree runs isolated services, PUBLIC once one is shared, and
    /// BRANCH only for a worktree not named for its branch.
    #[command(after_help = "\
Examples:
  pando ls           the table
  pando ls -l        with each worktree's commit and path
  pando ls --json    the documented machine-readable shape")]
    #[command(display_order = 8)]
    Ls {
        /// The documented machine-readable shape, for scripts and agents.
        #[arg(long)]
        json: bool,
        /// Add each worktree's commit and its path.
        #[arg(short, long, conflicts_with = "json")]
        long: bool,
        /// Worktree names, one per line, for shell completion.
        ///
        /// Not a documented shape.
        #[arg(long, hide = true, conflicts_with_all = ["json", "long"])]
        names: bool,
    },
    /// Remove a worktree and wipe its pando data. The branch is kept.
    #[command(display_order = 12)]
    Rm {
        /// The worktree, by branch or directory name. Never the main
        /// checkout.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: String,
        /// Confirm removing a worktree pando did not create.
        #[arg(long)]
        yes: bool,
        /// Remove it whatever is in the way.
        ///
        /// Lets git discard modified or untracked files, and goes ahead
        /// with Docker not running — its services' data volumes then stay
        /// behind, and the command that removes them is printed.
        #[arg(long)]
        force: bool,
    },
    /// Print a worktree's absolute path.
    #[command(after_help = "\
Example:
  cd \"$(pando path feat/login)\"")]
    #[command(display_order = 11)]
    Path {
        /// The worktree or the main checkout, by branch or directory name.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: String,
    },
    /// Start a worktree's processes, or the main checkout's.
    ///
    /// On a terminal it waits until every process is ready, and says why
    /// when one is not. Anywhere else — a script, an agent, a pipe — it
    /// returns as soon as everything is spawned unless `--wait` is given.
    ///
    /// The main checkout is yours, set up by you: pando runs its processes
    /// on ports it allocates and nothing else — no install, no hooks — on
    /// the project's own services. `--isolated` and `--namespaced` are for
    /// worktrees.
    #[command(after_help = "\
Examples:
  pando start feat/login               start everything; on a terminal, wait until it is ready
  pando start feat/login --no-wait     return as soon as everything is spawned
  pando start feat/login --wait        wait, even from a script
  pando start feat/login --isolated    with private copies of the services
  pando start feat/login --namespaced  experimental: a database of its own in your own server
  pando start main                     the main checkout's processes, beside the worktrees
  pando start --only api               one process of the worktree you are in")]
    #[command(display_order = 2)]
    Start {
        /// The worktree or the main checkout, by branch or directory name.
        ///
        /// The one you are in when left out.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: Option<String>,
        /// Accept pando's own recommendation for anything it would ask.
        ///
        /// Without it, an unanswerable question exits 3.
        #[arg(long)]
        yes: bool,
        /// One process by name, instead of every one config declares.
        ///
        /// The rest are left exactly as they are, on the ports they have.
        #[arg(long)]
        only: Option<String>,
        /// Run private copies of the project's services for this worktree.
        ///
        /// On ports of its own. Remembered: a later plain `start` keeps
        /// them.
        #[arg(long)]
        isolated: bool,
        /// Experimental: a namespace of its own in each of the project's
        /// own servers.
        ///
        /// The main checkout's MariaDB and Redis, with a database and a slot
        /// of this worktree's own in them, built by its own schema step: no
        /// server to start, and main's data untouched. Remembered like
        /// `--isolated`; `rm` drops what it made.
        #[arg(long, conflicts_with = "isolated")]
        namespaced: bool,
        /// Stop this worktree's private services and use the shared ones.
        ///
        /// The way back from `--isolated` and `--namespaced`. Its processes
        /// restart, so they see them. A namespace is kept until `rm`.
        #[arg(long, conflicts_with_all = ["isolated", "namespaced"])]
        shared: bool,
        /// Wait until every process is ready; exit 1 if one fails.
        ///
        /// Narrates each process as it gets there, and exits 1 with the
        /// reason if one fails. The default when stderr is a terminal.
        /// A process with a port is ready once the port answers. One with
        /// no port has nothing to check, so it is watched instead: ready if
        /// it is still up 5s after it started, or after its own
        /// `ready.timeout_s` when it declares one, capped at 10s.
        #[arg(long)]
        wait: bool,
        /// Return as soon as everything is spawned.
        ///
        /// The default when stderr is not a terminal, so scripts and agents
        /// see no change.
        #[arg(long, conflicts_with = "wait")]
        no_wait: bool,
    },
    /// Answer every question pando has about this project, in one pass.
    ///
    /// The batch form of the questions `new` and `start` ask just in time,
    /// through the same paths: nothing is started, nothing is written into
    /// the repository, and a second run asks nothing.
    #[command(after_help = "\
Examples:
  pando init                          ask every open question, on a terminal
  pando init --answers - --dry-run    a program's answers, from stdin, previewed
  pando init --agent                  the setup job, for your coding agent to follow")]
    #[command(display_order = 13)]
    Init {
        /// Accept pando's own recommendation for anything it would ask.
        ///
        /// Without it, an unanswerable question exits 3.
        #[arg(long)]
        yes: bool,
        /// A JSON file of answers — `-` reads stdin.
        ///
        /// One key per question, named as `pando signals` names it, whose
        /// value is the option's own text, a command of your own, a list
        /// for a question whose answer is a set, null for "none of them",
        /// or, at `processes`, an object of process tables. Every answer
        /// goes through the same checks a person's does and is written
        /// down as a program's.
        #[arg(long, value_name = "PATH", value_hint = clap::ValueHint::FilePath)]
        answers: Option<String>,
        /// Apply the answers to questions already answered too.
        ///
        /// Without it, an answer for a slot config already has is reported
        /// and left alone. With it, the answer replaces what config says —
        /// through the same checks, written down as a program's — and wins
        /// over what a rule would decide. The runtime prelude is the one
        /// exception: it is about this machine, only a person changes it,
        /// and an answer for it once it has one is refused.
        #[arg(long, requires = "answers")]
        replace: bool,
        /// Print the config this would write, and write nothing.
        #[arg(long)]
        dry_run: bool,
        /// Print the setup job for a coding agent, and write nothing.
        ///
        /// What pando sees in this project now, the questions still open
        /// with pando's options, what each command the agent will run
        /// writes, and the steps: answer, save, `pando check`, remember
        /// how to run the project, and say when it is done. It ends with
        /// the block to remember. After a failed check, the failure comes
        /// first.
        /// Asks nothing and exits 0, even when pando cannot read the
        /// project's settings: that is reported in the job.
        #[arg(long, conflicts_with_all = ["yes", "answers", "dry_run", "replace"])]
        agent: bool,
        /// With `--agent`: print a whole reference instead of the job.
        ///
        /// `brief` is the procedure the job's steps come from, `json` the
        /// contract for every JSON shape pando publishes: both the text
        /// this pando was built with. `memory` is not a document but the
        /// block the job ends with, made for this project: how to run its
        /// worktrees with pando, for an agent to save in its own memory
        /// once the developer says yes.
        #[arg(long, requires = "agent", value_name = "DOC")]
        reference: Option<Reference>,
    },
    /// Test the setup: start it all in a throwaway worktree, then remove it.
    ///
    /// Makes a worktree of the commit `new` would fork from, with no
    /// branch, and runs it as a first shared start does: the install,
    /// every process until it is ready, and the page the browser's app
    /// serves. Then stops and removes all of it, keeps its logs, and
    /// records the result. Hooks after `services`
    /// are skipped: on the shared services they would run against your
    /// own data. Everything it runs has `PANDO_CHECK=1` in its
    /// environment. Exits 0 when it passed, 1 when it failed — the
    /// settings' or the machine's, `--json` says which — and 3 when a
    /// question is still open.
    #[command(after_help = "\
Examples:
  pando check                 test it, and say what went wrong
  pando check --json          the documented machine-readable result
  pando check --base dev      test the commit dev is at, this once")]
    #[command(display_order = 14)]
    Check {
        /// The result as one JSON object, whatever it is.
        #[arg(long)]
        json: bool,
        /// Test the commit this branch is at, for this run only.
        ///
        /// Looked up as `new --base` looks one up. The settings' own base,
        /// or origin/HEAD, is what `new` forks from, so a result at another
        /// one is the setup's only once the project's `base` names it.
        #[arg(long)]
        base: Option<String>,
    },
    /// What the repository says about how to run itself, as JSON.
    ///
    /// Read-only, and identical on two runs: the input an agent reads
    /// before deciding anything.
    #[command(display_order = 16)]
    Signals,
    /// What pando found, from where, and what is wrong.
    ///
    /// Read-only. Problems and notes come first, each with what to do
    /// about it, then the facts section by section. Exits 0 when nothing
    /// found will break a command and 1 when something will.
    #[command(display_order = 15)]
    Doctor {
        /// Move a moved repository's project folder under its current id.
        ///
        /// Keeps its config, its state and its worktrees. The one thing
        /// doctor does rather than reports, and it asks first.
        #[arg(long, value_name = "OLD-ID")]
        adopt: Option<String>,
        /// Do not ask before adopting.
        #[arg(long, requires = "adopt")]
        yes: bool,
        /// The whole report as one JSON object.
        ///
        /// Versioned like the other machine-readable shapes. The exit code
        /// is the same either way.
        #[arg(long, conflicts_with = "adopt")]
        json: bool,
    },
    /// Stop a worktree's processes, or the main checkout's.
    ///
    /// With no name: the worktree you are in, or every worktree and the
    /// main checkout when you are in none of them — in the main checkout
    /// itself too. `--all` stops every one from anywhere.
    #[command(after_help = "\
Examples:
  pando stop feat/login               one worktree
  pando stop feat/login --only api    one process; the others keep running
  pando stop                          the worktree you are in
  pando stop --all                    every worktree of this repository")]
    #[command(display_order = 3)]
    Stop {
        /// The worktree or the main checkout, by branch or directory name.
        #[arg(
            conflicts_with = "all",
            value_name = WORKTREE,
            value_hint = clap::ValueHint::Other
        )]
        name: Option<String>,
        /// One process by name. The others keep running.
        #[arg(long, conflicts_with = "all")]
        only: Option<String>,
        /// Every worktree, even from inside one.
        #[arg(long)]
        all: bool,
    },
    /// Stop and start again, keeping the ports: a worktree, or the main
    /// checkout.
    ///
    /// Waits for readiness on a terminal, like `start`, and not elsewhere
    /// unless `--wait` is given.
    #[command(display_order = 4)]
    Restart {
        /// The worktree or the main checkout, by branch or directory name.
        ///
        /// The one you are in when left out.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: Option<String>,
        /// Accept pando's own recommendation for anything it would ask.
        ///
        /// Without it, an unanswerable question exits 3.
        #[arg(long)]
        yes: bool,
        /// One process by name. The others are not restarted.
        #[arg(long)]
        only: Option<String>,
        /// Run private copies of the project's services for this worktree.
        #[arg(long)]
        isolated: bool,
        /// Experimental: a namespace of its own in each of the project's
        /// own servers.
        ///
        /// As `start --namespaced`.
        #[arg(long, conflicts_with = "isolated")]
        namespaced: bool,
        /// Wait until every process is ready again; exit 1 if one fails.
        ///
        /// The default when stderr is a terminal. Readiness is judged as
        /// `start --wait` judges it: a port that answers, or, for a process
        /// with no port, still being up 5s later (its own `ready.timeout_s`,
        /// capped at 10s, when it declares one).
        #[arg(long)]
        wait: bool,
        /// Return as soon as everything is spawned. The default when
        /// stderr is not a terminal.
        #[arg(long, conflicts_with = "wait")]
        no_wait: bool,
    },
    /// What is running, and on which ports.
    #[command(after_help = "\
Examples:
  pando status                       every worktree, one line each and its processes
  pando status feat/login --json     the documented machine-readable shape
  eval \"$(pando status --env feat/login)\"   its environment, in this shell")]
    #[command(display_order = 7)]
    Status {
        /// The worktree or the main checkout, by branch or directory name.
        ///
        /// Every one when left out.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: Option<String>,
        #[arg(long)]
        json: bool,
        /// `export KEY=value` lines for this worktree's environment.
        ///
        /// Its resolved environment, so a shell can
        /// `eval "$(pando status --env <name>)"` and run the project's own
        /// commands by hand.
        #[arg(long, requires = "name", conflicts_with = "json")]
        env: bool,
    },
    /// Publish a running worktree at a public URL.
    #[command(display_order = 9)]
    Share {
        /// The worktree or the main checkout, by branch or directory name.
        ///
        /// The one you are in when left out.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: Option<String>,
    },
    /// Take a worktree's public URL down.
    #[command(display_order = 10)]
    Unshare {
        /// The worktree or the main checkout, by branch or directory name.
        ///
        /// The one you are in when left out.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: Option<String>,
    },
    /// Open a running worktree's URL in the browser, or its app.
    ///
    /// The TUI's `o` key, from a shell. The URL is printed too, so it
    /// works over SSH where there is no browser to open. A worktree that
    /// serves no page, only an app a phone or a simulator runs (Expo's),
    /// has that app opened instead: on the booted iOS simulator, else on
    /// a connected Android device or emulator, else, on a Mac with Xcode,
    /// on a simulator it starts. With none of those, it prints the
    /// commands that open it.
    #[command(after_help = "\
Examples:
  pando open feat/login           http://localhost:<port>
  pando open feat/login --public  the URL `pando share` published
  pando open feat/login --app     its app on the simulator, beside a page")]
    #[command(display_order = 5)]
    Open {
        /// The worktree or the main checkout, by branch or directory name.
        ///
        /// The one you are in when left out.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: Option<String>,
        /// The public URL `share` published, instead of the local one.
        #[arg(long, conflicts_with = "app")]
        public: bool,
        /// Its app on a simulator or a device, even when it serves a page.
        #[arg(long)]
        app: bool,
    },
    /// Print a worktree's log.
    #[command(after_help = "\
Examples:
  pando logs feat/login                 the last 50 lines of its dev log
  pando logs feat/login --source api    one process's, a service's or a hook's log
  pando logs -f                         follow the worktree you are in")]
    #[command(display_order = 6)]
    Logs {
        /// The worktree or the main checkout, by branch or directory name.
        ///
        /// The one you are in when left out.
        #[arg(value_name = WORKTREE, value_hint = clap::ValueHint::Other)]
        name: Option<String>,
        /// Which log: a process, a service or a hook, by name.
        ///
        /// `dev` by default; with no `dev`, the only process, or every
        /// process's log merged, each line prefixed with its source.
        #[arg(short, long)]
        source: Option<String>,
        /// How many lines from the end.
        ///
        /// Of each log, when several are merged.
        #[arg(short = 'n', long, default_value_t = DEFAULT_TAIL)]
        tail: usize,
        /// Keep printing as the log grows. Ends on Ctrl-C.
        #[arg(short = 'f', long)]
        follow: bool,
        /// One JSON object per line: timestamp, level, text.
        #[arg(long)]
        json: bool,
    },
    /// Print a shell completion script.
    #[command(after_help = "\
Examples:
  pando completions zsh > ~/.zfunc/_pando      then `fpath+=~/.zfunc` in .zshrc
  pando completions bash > ~/.local/share/bash-completion/completions/pando
  pando completions fish > ~/.config/fish/completions/pando.fish")]
    #[command(display_order = 17)]
    Completions {
        /// The shell to complete for.
        shell: clap_complete::Shell,
    },
    /// The share proxy, spawned by `share`.
    ///
    /// Never run by hand: it reads its cookie from the environment and
    /// would have nothing to inject.
    #[command(name = share_proxy::SUBCOMMAND, hide = true)]
    ShareProxy {
        #[arg(long)]
        listen: u16,
        #[arg(long)]
        upstream: u16,
    },
}

impl Command {
    /// Whether this command acts on `pando.toml`.
    ///
    /// The ones that do not run on whatever layers are left when pando's
    /// own is unusable: a broken config is exactly when `stop`, `ls` and
    /// `logs` are worth having.
    pub fn needs_config(&self) -> bool {
        matches!(
            self,
            Command::New { .. }
                | Command::Start { .. }
                | Command::Restart { .. }
                // `[share]` says which provider to use and whether there is
                // an auth command. `unshare` needs none of that: the record
                // holds both pgids, and taking a URL down is something you
                // want most when the config is broken.
                | Command::Share { .. }
                // `init`'s whole job is to fill this file in. A layer
                // pando cannot read is exactly the thing to fix first,
                // and patching on top of one it could not parse would
                // lose whatever is in it. `--agent` writes nothing, and a
                // config it cannot read is a fact the job reports: the
                // agent is who tells the developer.
                | Command::Init { agent: false, .. }
                // It starts the project, on the settings a start would use.
                | Command::Check { .. }
                // `--env` renders templates, which only config holds. Plain
                // `status` still runs on whatever is left.
                | Command::Status { env: true, .. }
        )
    }
}

/// Lines `logs` prints when nothing else is asked for.
const DEFAULT_TAIL: usize = 50;

/// The value name every worktree argument carries: what completion looks
/// for to offer worktree names there.
const WORKTREE: &str = completion::WORKTREE;

/// The log `logs` reads when no `--source` is given.
const DEFAULT_SOURCE: &str = "dev";

/// The reader of pando's stdout went away: `pando logs | head` once `head`
/// has its lines, `logs -f | grep -m1` once grep has its match. Not a
/// failure — what was read was what was wanted — so `main` ends quietly
/// with success on it.
#[derive(Debug)]
pub struct StdoutClosed;

impl std::fmt::Display for StdoutClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("nothing is reading stdout any more")
    }
}

impl std::error::Error for StdoutClosed {}

/// stdout, with a write to a reader that went away told apart from every
/// other broken pipe: one to a process pando runs is a real failure.
#[derive(Debug, Default)]
pub struct Stdout;

impl Write for Stdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::stdout().write(buf).map_err(stdout_error)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stdout().flush().map_err(stdout_error)
    }
}

fn stdout_error(e: std::io::Error) -> std::io::Error {
    match e.kind() {
        std::io::ErrorKind::BrokenPipe => {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, StdoutClosed)
        }
        _ => e,
    }
}

/// Whether `e` is [`StdoutClosed`], however much context it gathered on
/// the way up.
pub fn stdout_closed(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            .is_some_and(|inner| inner.is::<StdoutClosed>())
    })
}

pub fn dispatch(command: Command, paths: &PandoPaths, config: &Config) -> Result<()> {
    let mut out = Stdout;
    match command {
        Command::New { branch, base, yes } => {
            tip::first_time_tip(paths, config, stderr_is_terminal(), &notice, &draw);
            let config = &actions::resolve_for_new(paths, config, &everyday_asker(yes), &notice)?;
            // After the questions, which read a terminal Ctrl-C must still
            // end: from here a Ctrl-C unwinds a half-made worktree.
            actions::catch_check_interrupts();
            let name = actions::new(paths, config, &branch, base.as_deref(), &notice)
                .map_err(|e| with_a_way_past(paths, e))?;
            // The canonical path, the one the state record and `pando path`
            // carry: the raw one differs on macOS (/var against /private/var)
            // and reads as a second, different location.
            let created = config.worktrees_dir(paths).join(&name);
            let created = std::fs::canonicalize(&created).unwrap_or(created);
            writeln!(out, "created {name} at {}", created.display())?;
            // The next step, for a person; a script has what it needs above.
            hint(&format!("`pando start {branch}` starts it"));
            Ok(())
        }
        Command::Ls { json, long, names } => {
            if names {
                return completion::names(paths, &mut out);
            }
            if json {
                ls_json(paths, &mut out)
            } else {
                ls_text(paths, &mut out, long)
            }
        }
        Command::Rm { name, yes, force } => {
            let name = names::resolve(paths, &name)?;
            actions::rm(paths, &name, yes, force, &notice)?;
            writeln!(out, "removed {name}")?;
            Ok(())
        }
        Command::Path { name } => {
            writeln!(out, "{}", names::path(paths, &name)?.display())?;
            Ok(())
        }
        Command::Start {
            name,
            yes,
            only,
            isolated,
            namespaced,
            shared,
            wait,
            no_wait,
        } => {
            // Before any question: a misspelt name is worth knowing about
            // before being asked which dev command to run.
            let typed = name;
            let named = names::target_named(paths, typed.as_deref(), "start")?;
            let name = named.dir.clone();
            let mode = actions::Mode::of(isolated, namespaced, shared);
            // Before any question too: a login asked for, or a slot freed,
            // for a start that is refused is a cost with nothing for it.
            actions::refuse_only_on_a_mode_change(paths, config, &name, only.as_deref(), mode)
                .map_err(|e| with_a_way_past(paths, named.reword(e)))?;
            // After the refusals, which say nothing unless the start is
            // not going to happen, and before the first question.
            tip::first_time_tip(paths, config, stderr_is_terminal(), &notice, &draw);
            let config = &actions::resolve_for_start(
                paths,
                config,
                &name,
                mode,
                &everyday_asker(yes),
                &notice,
            )?;
            let report = actions::start(paths, config, &name, only.as_deref(), mode, &notice)
                .map_err(|e| with_a_way_past(paths, named.reword(e)))?;
            if report.reassigned {
                notice(&format!(
                    "the ports {} had were taken; it moved to new ones",
                    named.shown
                ));
            }
            let wait = waits(wait, no_wait);
            if wait {
                wait::wait_ready(paths, &named, only.as_deref(), &report.spawned(), &notice)?;
            }
            let url = url_suffix(report.url.as_deref());
            if report.started_nothing() {
                writeln!(out, "{name} is already running{url}")?;
            } else {
                writeln!(out, "started {name}{url}")?;
                if !wait {
                    hint(&format!(
                        "`pando status {}` shows when it is ready",
                        named.typed
                    ));
                }
            }
            Ok(())
        }
        Command::Init {
            agent: true,
            reference,
            ..
        } => agent::agent(paths, reference, &mut out),
        Command::Init {
            yes,
            answers,
            replace,
            dry_run,
            ..
        } => {
            let answers = answers.as_deref().map(read_answers).transpose()?;
            // Before the dry run too: a preview that exits 0 where the
            // real run would refuse is a preview of something else.
            if let Some(answers) = &answers {
                refuse_answered(answers, config, replace)?;
            }
            // Every slot the file names: `actions` refuses the ones
            // `--replace` may not change, before anything is written.
            let replacing: Vec<crate::detect::Slot> = match (&answers, replace) {
                (Some(answers), true) => answers.slots(),
                _ => Vec::new(),
            };
            let ask = init_asker(answers.as_ref(), yes);
            // The second channel: what the file has to say about a slot no
            // rule proposed anything for, where there is no question to
            // put to anybody and a program is the only one who could know.
            let volunteered = volunteered_from(answers.as_ref());
            // And every slot it names is put to it, where a rule decided the
            // slot too: an answer given beats a guess.
            let answered: Vec<crate::detect::Slot> =
                answers.as_ref().map(|a| a.slots()).unwrap_or_default();
            let answering = match &volunteered {
                Some(program) => actions::Answering::by_program(&ask, program),
                None => actions::Answering::asking(&ask),
            }
            .replacing(&replacing)
            .answered(&answered);
            let (report, preview) = match dry_run {
                true => actions::init_dry_run(paths, config, &answering, &notice)?,
                false => {
                    let before = InitFiles::read(paths);
                    match actions::init(paths, config, &answering, &notice) {
                        Ok(report) => (report, Vec::new()),
                        // A question stops the pass after the answers
                        // before it were written: said as a finished run
                        // says it, so none of them reads as refused.
                        Err(e) => {
                            if e.is::<actions::NeedsAnswer>() {
                                write!(out, "{}", before.render_written())?;
                            }
                            return Err(e);
                        }
                    }
                }
            };
            // Before the summary, because it is about what the file the
            // summary describes does *not* say.
            if let Some(answers) = &answers {
                report_unused(answers);
            }
            for warning in &report.warnings {
                notice(warning);
            }
            if !dry_run {
                write!(out, "{}", render_init(&report, "wrote"))?;
                return Ok(());
            }
            for (path, body) in &preview {
                writeln!(out, "# {}", path.display())?;
                write!(out, "{body}")?;
            }
            // The summary goes to stderr on this path: stdout is the
            // config, so `pando init --dry-run > preview.toml` is a file
            // and nothing else.
            to_stderr(&render_init(&report, "would write"));
            Ok(())
        }
        Command::Check { json, base } => {
            check::check(paths, config, json, base.as_deref(), &mut out)
        }
        Command::Signals => signals_json(paths, config, &mut out),
        // Deliberately not given the config `main` loaded: the one thing
        // worth reporting about a project layer pando cannot read is the
        // error, and `main` keeps that to itself.
        Command::Doctor { adopt, yes, json } => match adopt {
            Some(old_id) => adopt_project(paths, &old_id, yes, &mut out),
            None => doctor(paths, json, &mut out),
        },
        Command::Stop { name, only, all } => {
            let name = match (name, all) {
                (_, true) => None,
                (Some(name), false) => Some(names::resolve(paths, &name)?),
                // No name: the worktree the shell is in, said out loud
                // because `stop` alone used to mean every one.
                (None, false) => {
                    let cwd = std::env::current_dir()?;
                    let here = names::containing(paths, &cwd)?;
                    if let Some(here) = &here {
                        notice(&format!(
                            "stopping {}, the worktree you are in — `pando stop --all` \
                             stops every one",
                            names::shown(paths, here)
                        ));
                    }
                    here
                }
            };
            match name {
                Some(name) => stop_one(paths, &name, only.as_deref(), &mut out),
                None if only.is_some() => Err(UsageError(
                    "--only needs a worktree: `pando stop <name> --only <process>`, or run it \
                     from inside one"
                        .to_string(),
                )
                .into()),
                None => {
                    let stopped = actions::stop_all(paths, &notice)?;
                    if stopped.is_empty() {
                        writeln!(out, "nothing was running")?;
                    } else {
                        // A running `pando check` is stopped with the rest,
                        // and named for what it is, not its directory.
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
        Command::Restart {
            name,
            yes,
            only,
            isolated,
            namespaced,
            wait,
            no_wait,
        } => {
            let typed = name;
            let named = names::target_named(paths, typed.as_deref(), "restart")?;
            let name = named.dir.clone();
            // Resolved exactly as `start` resolves it: a project whose
            // process question has never been answered gets the question,
            // not a refusal.
            // `--shared` is `start`'s: a restart into shared mode is what
            // `start --shared` already is, since changing the mode
            // restarts the processes anyway.
            let mode = actions::Mode::of(isolated, namespaced, false);
            actions::refuse_only_on_a_mode_change(paths, config, &name, only.as_deref(), mode)
                .map_err(|e| with_a_way_past(paths, named.reword(e)))?;
            let config = &actions::resolve_for_start(
                paths,
                config,
                &name,
                mode,
                &everyday_asker(yes),
                &notice,
            )?;
            let report = actions::restart(paths, config, &name, only.as_deref(), mode, &notice)
                .map_err(|e| with_a_way_past(paths, named.reword(e)))?;
            if waits(wait, no_wait) {
                wait::wait_ready(paths, &named, only.as_deref(), &report.spawned(), &notice)?;
            }
            writeln!(out, "restarted {name}{}", url_suffix(report.url.as_deref()))?;
            Ok(())
        }
        Command::Status { name, json, env } => {
            let name = name
                .as_deref()
                .map(|name| names::resolve(paths, name))
                .transpose()?;
            if env {
                let name = name.as_deref().expect("--env requires a name");
                let resolved = actions::resolved_env(paths, config, name)?;
                write!(out, "{}", actions::export_lines(&resolved))?;
                return Ok(());
            }
            if json {
                status_json(paths, name.as_deref(), &mut out)
            } else {
                status_text(paths, name.as_deref(), &mut out)
            }
        }
        Command::Share { name: typed } => {
            let named = names::target_named(paths, typed.as_deref(), "share")?;
            let name = named.dir.clone();
            let outcome =
                actions::share(paths, config, &name, &notice).map_err(|e| named.reword(e))?;
            if outcome.already {
                notice(&format!("{} was already shared", named.shown));
            }
            if outcome.pre_authed {
                notice("a proxy in front of it is injecting the Cookie header from auth_cmd");
            }
            // The URL alone on stdout, so `open "$(pando share x)"` works.
            writeln!(out, "{}", outcome.public_url)?;
            Ok(())
        }
        Command::Unshare { name: typed } => {
            let named = names::target_named(paths, typed.as_deref(), "unshare")?;
            let name = named.dir.clone();
            actions::unshare(paths, &name).map_err(|e| named.reword(e))?;
            writeln!(out, "unshared {name}")?;
            Ok(())
        }
        Command::Open {
            name: typed,
            public,
            app,
        } => {
            let named = names::target_named(paths, typed.as_deref(), "open")?;
            let want = match (public, app) {
                (true, _) => open::Want::Public,
                (_, true) => open::Want::App,
                _ => open::Want::Page,
            };
            let url = match open::url_to_open(paths, config, &named, want)? {
                open::Opening::Url(url) => url,
                // A browser at Metro's root shows nothing anybody wants:
                // the app it serves is opened where it runs.
                open::Opening::Apps {
                    apps,
                    said,
                    refused,
                } => {
                    for line in said {
                        writeln!(out, "{line}")?;
                    }
                    out.flush()?;
                    open::open_apps(paths, &apps, &mut out, &notice)?;
                    // Opened, it would crash on this worktree's JavaScript.
                    if !refused.is_empty() {
                        anyhow::bail!("{}", refused.join("\n"));
                    }
                    return Ok(());
                }
            };
            // Printed first, so the URL is there to copy even when there is
            // no browser to hand it to.
            writeln!(out, "{url}")?;
            out.flush()?;
            open::launch(&url)
        }
        Command::Logs {
            name,
            source,
            tail,
            follow,
            json,
        } => {
            let name = names::target(paths, name.as_deref(), "logs")?;
            let source = match source {
                Some(source) => source,
                None => match logs::default_sources(paths, &name, DEFAULT_SOURCE) {
                    logs::Sources::One(source) => source,
                    logs::Sources::Merged(sources) => {
                        return logs::logs_merged(
                            paths, &name, &sources, tail, follow, json, &mut out, &notice,
                        );
                    }
                },
            };
            logs(paths, &name, &source, tail, follow, json, &mut out, &notice)
        }
        Command::Completions { shell } => completions(shell, &mut out),
        // Never reached: `main` runs the proxy before it goes looking for a
        // repository, because the proxy has none.
        Command::ShareProxy { listen, upstream } => run_share_proxy(listen, upstream),
    }
}

fn stop_one<W: Write>(
    paths: &PandoPaths,
    name: &str,
    only: Option<&str>,
    out: &mut W,
) -> Result<()> {
    match actions::stop(paths, name, only, &notice)? {
        actions::StopOutcome::Stopped(processes) => {
            // Empty when the worktree had only its services left up, which
            // is a real thing to stop and a silly thing to narrate as
            // "stopped ".
            if !processes.is_empty() {
                notice(&format!("stopped {}", processes.join(", ")));
            }
            writeln!(out, "stopped {name}")?;
        }
        actions::StopOutcome::NotRunning => writeln!(out, "{name} was not running")?,
    }
    Ok(())
}

/// The completion script for `shell`, generated from the same clap
/// definition the binary parses, so it can never offer a flag that is not
/// there — and, where the shell allows it, completing a worktree's name
/// from the worktrees there are rather than from file names.
pub fn completions<W: Write>(shell: clap_complete::Shell, out: &mut W) -> Result<()> {
    use clap::CommandFactory;
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), "pando", &mut script);
    let script = String::from_utf8(script).context("a completion script that is not UTF-8")?;
    out.write_all(completion::with_worktree_names(shell, &script).as_bytes())?;
    Ok(())
}

/// Runs the share proxy, reading its cookie from the environment.
///
/// Called from `main` before project discovery: the proxy's working
/// directory is a temp directory, it has no repository and no config, and
/// the one thing it needs is an environment variable.
pub fn run_share_proxy(listen: u16, upstream: u16) -> Result<()> {
    let cookie = std::env::var(share_proxy::ENV_COOKIE).with_context(|| {
        format!(
            "{} is not set — `{}` is spawned by `pando share`, not run by hand",
            share_proxy::ENV_COOKIE,
            share_proxy::SUBCOMMAND
        )
    })?;
    share_proxy::run_in_process(listen, upstream, &cookie)
}

/// What `actions` says when a config names no process to start.
const NO_PROCESSES: &str = "no processes configured";

/// An error from `new`, `start` or `restart`, with the way past it when
/// `actions` could only say what went wrong: the file to edit, by its
/// absolute path, and what to write there.
///
/// Anything else — a question, a usage error — is passed through as it
/// is, so the exit code main reads off its type survives.
fn with_a_way_past(paths: &PandoPaths, e: anyhow::Error) -> anyhow::Error {
    let text = format!("{e:#}");
    if text.starts_with(NO_PROCESSES) {
        return anyhow::anyhow!(
            "nothing to run: this project configures no process — {}",
            crate::doctor::nothing_to_run_fix(&paths.config_file())
        );
    }
    if let Some(fixed) = actions::install_remedy(paths, &text) {
        return anyhow::anyhow!(fixed);
    }
    e
}

/// A line of a picture on stderr, as it is: no `pando:` in front.
fn draw(line: &str) {
    to_stderr(&format!("{line}\n"));
}

/// Everything pando narrates goes to stderr, so a command's stdout stays
/// exactly what a script asked for.
fn notice(message: &str) {
    to_stderr(&format!("pando: {message}\n"));
}

/// Text on stderr, dropped when nothing reads stderr any more. `eprint!`
/// panics there instead, and a panic between a spawn and the save that
/// records it leaves a process nothing can stop — or turns exit 1, 2 or 3
/// into a panic's 101.
pub fn to_stderr(text: &str) {
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}

/// A next step, for a person at a terminal: printed only when stderr is
/// one, so a script or an agent reading the output never sees it and a
/// log of a CI run is not padded with advice.
fn hint(message: &str) {
    use std::io::IsTerminal;
    if std::io::stderr().is_terminal() {
        let style = crate::term::Style::for_stderr();
        let line = style.paint(&format!("pando: {message}"), crate::term::Paint::Faint);
        to_stderr(&format!("{line}\n"));
    }
}

/// Whether `start` or `restart` waits for readiness: when asked to, and by
/// default when a person is watching — stderr a terminal. Anything else
/// keeps the old contract, returning once everything is spawned, because
/// scripts and agents were written against it.
fn waits(wait: bool, no_wait: bool) -> bool {
    waits_on(wait, no_wait, stderr_is_terminal())
}

/// Whether a person is watching what pando narrates.
fn stderr_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stderr().is_terminal()
}

fn waits_on(wait: bool, no_wait: bool, terminal: bool) -> bool {
    wait || (!no_wait && terminal)
}

fn url_suffix(url: Option<&str>) -> String {
    match url {
        Some(url) => format!(" — {url}"),
        None => String::new(),
    }
}

/// What a refresh found and what it had to do about it — a share whose
/// tunnel died, most often. Always stderr: `--json`'s stdout has to stay
/// parseable, and none of this is part of the documented shape.
fn report_refresh(refreshed: &actions::Refreshed) {
    if let Some(warning) = &refreshed.warning {
        notice(warning);
    }
    for line in &refreshed.notices {
        notice(line);
    }
}

/// The sha `ls --json` publishes: seven characters, always. Porcelain's
/// `head` is the full forty, and enrichment's `head_sha` is git's own
/// abbreviation, as long as the repository or `core.abbrev` makes it, so
/// either is cut: one listing never holds two shapes of a field documented
/// as `"abc1234"`.
fn short_head(w: &Worktree) -> Option<String> {
    w.head
        .as_deref()
        .or(w.head_sha.as_deref())
        .map(|sha| sha.chars().take(SHORT_SHA_LEN).collect())
}

#[cfg(test)]
mod tests;
