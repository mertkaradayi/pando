//! Making, finding and dropping a namespace on a server, through the
//! commands its recipe gives — the one place pando talks to a developer's
//! own database server.

use anyhow::{Result, bail};
use std::path::PathBuf;
use std::time::Duration;

use crate::process::{self as proc, shell_quote};
use crate::recipes::NamespaceRecipe;
use crate::state::NamespaceKind;
use crate::template;

use super::login::Login;
use super::name::{MARKER, is_plain};

/// How long one command gets. Each is one statement against a server that
/// is already up, so a slow answer is a server that is not answering.
const TIMEOUT: Duration = Duration::from_secs(30);

/// One server the main checkout runs, as a namespaced start reaches it:
/// where it is, who to log in as, and the recipe's commands for it.
pub struct Server<'a> {
    pub service: &'a str,
    pub recipe: &'a NamespaceRecipe,
    pub host: String,
    pub port: u16,
    pub login: Login,
    /// pando's own `bin`, ahead of everything on PATH for every command,
    /// as for a service recipe: where a developer puts a client the login
    /// shell does not find, and where a test puts a fake one.
    pub bin_dir: PathBuf,
}

/// What [`Server::create`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    /// The server made it for pando just now.
    Made,
    /// It was there already, so pando did not make it.
    AlreadyThere,
}

impl Server<'_> {
    /// `host:port`, for a sentence.
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// The clients the recipe needs that this machine does not have.
    pub fn missing_binaries(&self) -> Vec<String> {
        if self.recipe.binaries.is_empty() {
            return Vec::new();
        }
        let checks: Vec<String> = self
            .recipe
            .binaries
            .iter()
            .map(|binary| {
                let quoted = shell_quote(binary);
                format!("command -v {quoted} >/dev/null 2>&1 || echo {quoted}")
            })
            .collect();
        let Ok(out) = proc::run_captured(
            &self.with_path(&checks.join("\n")),
            &std::env::temp_dir(),
            &[],
            TIMEOUT,
        ) else {
            return Vec::new();
        };
        out.stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// Whether the server answers and takes the login. The first thing a
    /// namespaced start asks, before anything is made or stopped.
    pub fn ping(&self) -> Result<()> {
        let missing = self.missing_binaries();
        if !missing.is_empty() {
            let install = match &self.recipe.install {
                Some(how) => format!("install it ({how})"),
                None => "install it".to_string(),
            };
            bail!(
                "namespaced mode reaches {} through {}, and {} not on PATH — {install}, or put \
                 it in {}",
                self.service,
                missing.join(", "),
                if missing.len() == 1 {
                    "it is"
                } else {
                    "they are"
                },
                self.bin_dir.display()
            );
        }
        let out = self.run(&self.recipe.ping, None)?;
        if out.success() {
            return Ok(());
        }
        bail!(
            "{} on {} did not take the login from {}: {}",
            self.service,
            self.address(),
            self.login.from,
            out.last_stderr_line().unwrap_or("no output")
        )
    }

    /// Whether the server has a database of this name.
    pub fn exists(&self, name: &str) -> Result<bool> {
        let command = self.command(self.recipe.exists.as_deref(), "exists")?;
        let out = self.run(command, Some(name))?;
        if !out.success() {
            bail!(
                "could not ask {} on {} whether {name} exists: {}",
                self.service,
                self.address(),
                out.last_stderr_line().unwrap_or("no output")
            );
        }
        // Without case: MariaDB on macOS compares names that way, and says
        // a name back the way it was first written.
        Ok(out
            .stdout
            .lines()
            .any(|line| line.trim().eq_ignore_ascii_case(name)))
    }

    /// Makes a database for a worktree whose main one is `main`.
    ///
    /// A server that already has it did not make it for pando, which is
    /// [`Created::AlreadyThere`] and not an error. A login that may not is
    /// an error carrying the grant that lets it — and nothing was made.
    ///
    /// `{main}` in the recipe's command is the main database's name, so
    /// the new one can be made in its shape: a schema written for main's
    /// character set is run into this one, and under a wider default its
    /// keys can outgrow the server's limit. A main that is not a plain
    /// name is not put in a command; the recipe then falls back on the
    /// server's default, as it does when it cannot see main.
    pub fn create(&self, name: &str, main: &str) -> Result<Created> {
        let script = self.create_script(name, main)?;
        let env = self.login.env(self.recipe.password_env.as_deref());
        let out = proc::run_captured(&script, &std::env::temp_dir(), &env, TIMEOUT)?;
        if out.success() {
            return Ok(Created::Made);
        }
        if self.is_denied(&out) {
            bail!("{}", self.denied(name, main, "made"));
        }
        if self.exists(name)? {
            return Ok(Created::AlreadyThere);
        }
        bail!(
            "{} on {} could not make {name}: {}",
            self.service,
            self.address(),
            out.last_stderr_line().unwrap_or("no output")
        )
    }

    /// Drops a database, or empties a slot.
    ///
    /// Only ever called past [`super::may_drop`]; the name is checked again
    /// here anyway, because this is the line that runs the statement.
    pub fn drop(&self, name: &str, main: &str) -> Result<()> {
        if !self.could_have_made(name) {
            bail!("pando will not drop {name:?}: it is not a namespace it could have made");
        }
        let out = self.run(&self.recipe.drop, Some(name))?;
        if out.success() {
            return Ok(());
        }
        if self.is_denied(&out) {
            bail!("{}", self.denied(name, main, "dropped"));
        }
        bail!(
            "{} on {} could not drop {name}: {}",
            self.service,
            self.address(),
            out.last_stderr_line().unwrap_or("no output")
        )
    }

    /// Every database on the server named for a worktree of `main` —
    /// `<main>__…` — as far as this login can see. Empty when the recipe
    /// has no way to ask. A `main` that is not a plain name is refused
    /// before anything runs, as [`Server::script`] refuses a namespace.
    pub fn list(&self, main: &str) -> Result<Vec<String>> {
        let Some(command) = self.recipe.list.as_deref() else {
            return Ok(Vec::new());
        };
        if !is_plain(main) {
            bail!("{main:?} is not a plain name, so pando will not put it in a command");
        }
        let vars = Vars {
            prefix_like: Some(prefix_like(main)),
            ..self.vars(None)
        };
        let script = self.with_path(&template::render_with(command, &vars)?);
        let env = self.login.env(self.recipe.password_env.as_deref());
        let out = proc::run_captured(&script, &std::env::temp_dir(), &env, TIMEOUT)?;
        if !out.success() {
            bail!(
                "could not list {main}{MARKER}… on {}: {}",
                self.address(),
                out.last_stderr_line().unwrap_or("no output")
            );
        }
        Ok(out
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The command that drops `name` — the recipe's own, as pando would
    /// run it — for a person to run when pando could not: printed, never
    /// run, and never with the password in it. `None` for a name
    /// [`Server::drop`] would refuse: a printed command is run as it is
    /// printed.
    pub fn by_hand(&self, name: &str) -> Option<String> {
        if !self.could_have_made(name) {
            return None;
        }
        let rendered = template::render_with(&self.recipe.drop, &self.vars(Some(name))).ok()?;
        Some(match &self.recipe.password_env {
            Some(var) if self.login.has_password() => {
                format!("{rendered}   (with the password in {var})")
            }
            _ => rendered,
        })
    }

    /// How many keys a slot holds.
    pub fn size(&self, slot: u32) -> Result<u64> {
        let command = self.command(self.recipe.size.as_deref(), "size")?;
        let out = self.run(command, Some(&slot.to_string()))?;
        let count = out
            .stdout
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .and_then(|line| line.trim_start_matches("(integer)").trim().parse().ok());
        match (out.success(), count) {
            (true, Some(count)) => Ok(count),
            _ => bail!(
                "could not ask {} on {} how full slot {slot} is: {}",
                self.service,
                self.address(),
                out.last_stderr_line()
                    .or_else(|| out.stdout.lines().last())
                    .unwrap_or("no output")
            ),
        }
    }

    /// What an administrator runs, once, so this login may make and drop
    /// `<main>__…` on this server and nothing else — when the recipe
    /// knows how to say it, and `main` is a plain name.
    pub fn grant(&self, main: &str) -> Option<String> {
        let grant = self.recipe.grant.as_deref()?;
        if !is_plain(main) {
            return None;
        }
        let account = self
            .recipe
            .account
            .as_deref()
            .and_then(|command| self.run(command, None).ok())
            .filter(proc::Captured::success)
            .and_then(|out| {
                let line = out.stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
                let (user, host) = line.rsplit_once('@')?;
                Some((user.to_string(), host.to_string()))
            });
        let (user, host) = account.unwrap_or_else(|| {
            (
                self.login.user.clone().unwrap_or_default(),
                "localhost".to_string(),
            )
        });
        let vars = Vars {
            prefix_like: Some(prefix_like(main)),
            account_user: Some(user),
            account_host: Some(host),
            ..self.vars(None)
        };
        template::render_with(grant, &vars).ok()
    }

    /// The refusal a login that may not make or drop namespaces earns: who
    /// it is, what it may not do, the one statement that fixes it when the
    /// recipe knows it, and that nothing was `done`.
    ///
    /// A database's refusal says the right it needs is to `<main>__…`; a
    /// slot is a number, and its refusal names the slot alone.
    fn denied(&self, name: &str, main: &str, done: &str) -> String {
        let who = match (&self.login.user, self.login.has_password()) {
            (None, false) => "a connection with no login".to_string(),
            _ => format!("the login from {}", self.login.from),
        };
        let (what, scope) = match self.recipe.kind {
            NamespaceKind::Database => (
                format!("make or drop {name}"),
                format!("make and drop databases named {main}{MARKER}…"),
            ),
            NamespaceKind::Slot => (format!("empty slot {name}"), format!("empty slot {name}")),
        };
        let head = format!(
            "{} on {} does not let {who} {what}",
            self.service,
            self.address()
        );
        match (self.grant(main), self.recipe.kind) {
            (Some(grant), NamespaceKind::Database) => {
                let covers = match &self.recipe.grant_covers {
                    Some(covers) => covers.clone(),
                    None => format!("{scope} and nothing else"),
                };
                format!(
                    "{head} — run this once as an administrator of that server:\n\n    \
                     {grant}\n\nIt lets that login {covers}. Nothing was {done}."
                )
            }
            (Some(grant), NamespaceKind::Slot) => format!(
                "{head} — run this once as an administrator of that server:\n\n    {grant}\n\n\
                 Nothing was {done}."
            ),
            (None, _) => {
                format!("{head} — give it the right to {scope} on that server. Nothing was {done}.")
            }
        }
    }

    /// Whether `name` is a namespace pando could have made on this server:
    /// a plain database name with the marker in it, or a slot other than
    /// 0. Nothing else is dropped, or printed as a drop.
    fn could_have_made(&self, name: &str) -> bool {
        match self.recipe.kind {
            NamespaceKind::Database => is_plain(name) && name.contains(MARKER),
            NamespaceKind::Slot => name.parse::<u32>().is_ok_and(|slot| slot > 0),
        }
    }

    fn is_denied(&self, out: &proc::Captured) -> bool {
        self.recipe.denied.iter().any(|needle| {
            out.stderr.contains(needle.as_str()) || out.stdout.contains(needle.as_str())
        })
    }

    fn command<'r>(&self, command: Option<&'r str>, what: &str) -> Result<&'r str> {
        match command {
            Some(command) => Ok(command),
            None => bail!(
                "the recipe for {} has no `{what}` in its [namespace], which a {} needs",
                self.service,
                match self.recipe.kind {
                    NamespaceKind::Database => "database",
                    NamespaceKind::Slot => "slot",
                }
            ),
        }
    }

    fn vars(&self, namespace: Option<&str>) -> Vars {
        Vars {
            host: Some(shell_quote(&self.host)),
            port: Some(self.port.to_string()),
            user: self.login.user.as_deref().map(shell_quote),
            namespace: namespace.map(str::to_string),
            main: None,
            prefix_like: None,
            account_user: None,
            account_host: None,
        }
    }

    fn with_path(&self, script: &str) -> String {
        format!(
            "export PATH={}:\"$PATH\"\n{script}",
            shell_quote(&self.bin_dir.display().to_string())
        )
    }

    /// The script `bash -lc` is handed for one recipe command: what
    /// anyone on the machine can read in `ps` while it runs, so it carries
    /// everything but the password.
    pub fn script(&self, command: &str, namespace: Option<&str>) -> Result<String> {
        if let Some(name) = namespace
            && !is_plain(name)
        {
            bail!("{name:?} is not a plain name, so pando will not put it in a command");
        }
        let rendered = template::render_with(command, &self.vars(namespace))?;
        Ok(self.with_path(&rendered))
    }

    /// One recipe command, run with the password in the environment the
    /// client reads it from, and nowhere else.
    fn run(&self, command: &str, namespace: Option<&str>) -> Result<proc::Captured> {
        let script = self.script(command, namespace)?;
        let env = self.login.env(self.recipe.password_env.as_deref());
        proc::run_captured(&script, &std::env::temp_dir(), &env, TIMEOUT)
    }

    /// [`Self::script`] for the recipe's `create` of `name`, whose `{main}`
    /// is the main database's name — or nothing, for one that is not a
    /// plain name.
    pub fn create_script(&self, name: &str, main: &str) -> Result<String> {
        let command = self.command(self.recipe.create.as_deref(), "create")?;
        if !is_plain(name) {
            bail!("{name:?} is not a plain name, so pando will not put it in a command");
        }
        let vars = Vars {
            main: Some(if is_plain(main) { main } else { "" }.to_string()),
            ..self.vars(Some(name))
        };
        Ok(self.with_path(&template::render_with(command, &vars)?))
    }
}

/// The prefix every namespace of `main` starts with, as an SQL `LIKE`
/// pattern: `_` and `%` match anything there, so they are escaped.
pub fn prefix_like(main: &str) -> String {
    let mut out = String::new();
    for c in format!("{main}{MARKER}").chars() {
        if matches!(c, '_' | '%' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// The placeholders a namespace command may use.
struct Vars {
    host: Option<String>,
    port: Option<String>,
    user: Option<String>,
    namespace: Option<String>,
    /// The main database's name, in `create` alone.
    main: Option<String>,
    prefix_like: Option<String>,
    account_user: Option<String>,
    account_host: Option<String>,
}

const KNOWN: [&str; 8] = [
    "host",
    "port",
    "user",
    "namespace",
    "main",
    "prefix_like",
    "account_user",
    "account_host",
];

impl template::Resolver for Vars {
    fn resolve(&self, key: &str, arg: Option<&str>) -> Result<String> {
        if let Some(arg) = arg {
            bail!("{{{key}:{arg}}} — {key} takes no argument in a [namespace] command");
        }
        let value = match key {
            "host" => &self.host,
            "port" => &self.port,
            "user" => &self.user,
            "namespace" => &self.namespace,
            "main" => &self.main,
            "prefix_like" => &self.prefix_like,
            "account_user" => &self.account_user,
            "account_host" => &self.account_host,
            _ => bail!(
                "unknown placeholder {{{key}}} in a [namespace] command — it understands {}",
                KNOWN.join(", ")
            ),
        };
        match value {
            Some(value) => Ok(value.clone()),
            None => bail!(
                "{{{key}}} has no value here — a [namespace] command used it where it means nothing"
            ),
        }
    }
}
