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
    /// Where the recipe's commands run: here, or inside the container that
    /// publishes the server's port. [`Server::reach`] decides.
    pub runner: Runner,
}

/// Where a server's recipe commands run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Runner {
    /// On this machine, with its own client.
    Host,
    /// Inside the container that publishes the server's port, with the
    /// client its image ships: a database in Docker usually leaves the
    /// host with none. `{host}` is the container's own loopback there and
    /// `{port}` the port the server listens on inside it.
    Container { id: String, port: u16 },
}

/// A refusal from the server: the login may not make or drop this
/// namespace. Its message carries the grant that would let it, and what
/// was not done.
#[derive(Debug)]
pub struct Denied(pub String);

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Denied {}

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

    /// This server, with its commands run where its client is: here when
    /// this machine has every one the recipe needs, else inside the
    /// container that publishes its port when that one has them all. With
    /// neither, it stays here, and [`Server::ping`] says what to install.
    pub fn reach(self) -> Self {
        if self.runner != Runner::Host || self.missing_binaries().is_empty() {
            return self;
        }
        let Some((id, port)) = self.container() else {
            return self;
        };
        let inside = Server {
            runner: Runner::Container { id, port },
            ..self
        };
        match inside.missing_binaries().is_empty() {
            true => inside,
            false => Server {
                runner: Runner::Host,
                ..inside
            },
        }
    }

    /// The keys of a container's environment the recipe names, each one
    /// asked for alone with `printenv`: nothing else the container keeps is
    /// read. Held in memory for the one login they give, never printed.
    fn container_env(
        &self,
        id: &str,
        keys: &[&String],
    ) -> std::collections::BTreeMap<String, String> {
        let mut out = std::collections::BTreeMap::new();
        for key in keys {
            let script = self.with_path(&format!(
                "docker exec {} printenv {}",
                shell_quote(id),
                shell_quote(key)
            ));
            let Ok(read) = proc::run_captured(&script, &std::env::temp_dir(), &[], TIMEOUT) else {
                continue;
            };
            if read.success() {
                let value = read.stdout.strip_suffix('\n').unwrap_or(&read.stdout);
                out.insert(key.to_string(), value.to_string());
            }
        }
        out
    }

    /// Gives this server's login the right to make and drop this
    /// worktree's namespaces, by running the recipe's `grant` as `admin`:
    /// what the developer would otherwise run by hand, once. Returns the
    /// statement it ran, for a line that says so.
    pub fn grant_as(&self, admin: &Server<'_>, main: &str) -> Result<String> {
        // Run, not printed, so only for a login whose names cannot end a
        // quoted name in the statement: anything else is a person's to read.
        let (user, host) = self.account();
        let plain = |name: &str| {
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-.%@:".contains(c))
        };
        if user.is_empty() || !plain(&user) || !plain(&host) {
            bail!(
                "the login's name {user:?} is not a plain one, so pando will not run a grant for \
                 it"
            );
        }
        let Some(grant) = self.grant(main) else {
            bail!(
                "the recipe for {} says no grant pando could run for {main}",
                self.service
            );
        };
        admin.run_sql(&grant)?;
        Ok(grant)
    }

    /// Runs SQL as this server's login, through the recipe's `run_sql`,
    /// the statement on stdin and never on a command line.
    fn run_sql(&self, sql: &str) -> Result<()> {
        let command = self.command(self.recipe.run_sql.as_deref(), "run_sql")?;
        if sql.lines().any(|line| line.trim() == "PANDO_SQL") {
            bail!("a statement with a line `PANDO_SQL` would end its own here-document");
        }
        let rendered = template::render_with(command, &self.vars(None))?;
        let script = self.wrap(&format!("{rendered} <<'PANDO_SQL'\n{sql}\nPANDO_SQL"));
        let env = self.login.env(self.recipe.password_env.as_deref());
        let out = proc::run_captured(&script, &std::env::temp_dir(), &env, TIMEOUT)?;
        if out.success() {
            return Ok(());
        }
        bail!(
            "{} on {} did not run the grant as {}: {}",
            self.service,
            self.address(),
            self.login.from,
            out.last_stderr_line().unwrap_or("no output")
        )
    }

    /// The running container that publishes this server's port on this
    /// machine, and the port it listens on inside: every running
    /// container's `docker port`, matched on the host's side. Not `docker
    /// ps --filter publish=`, which matches the port inside the container:
    /// a `5433:5432` mapping is never found by 5433. `None`
    /// with no Docker, no such container, or an answer it cannot read.
    ///
    /// Only for a server the app reaches on this machine's own loopback,
    /// only when exactly one container publishes the port there, and only
    /// when nothing but a container runtime's port forwarder listens on it
    /// here: a native server beside a container on the same port, or a
    /// server elsewhere, is never mistaken for the container.
    fn container(&self) -> Option<(String, u16)> {
        // Nothing under test reaches a developer's own containers: only a
        // stand-in `docker` in the test's own `bin` is ever asked.
        if cfg!(test) && !self.bin_dir.join("docker").is_file() {
            return None;
        }
        if !is_loopback(&self.host) {
            return None;
        }
        let script = self.with_path(&format!(
            "ids=$(docker ps --format '{{{{.ID}}}}') || exit 1\n\
             echo --\n\
             for id in $ids; do echo \"$id\"; docker port \"$id\"; done\n\
             echo ==\n\
             if command -v lsof >/dev/null 2>&1; then \
             lsof -nP -iTCP:{port} -sTCP:LISTEN -Fc 2>/dev/null; fi\n\
             exit 0",
            port = self.port
        ));
        let out = proc::run_captured(&script, &std::env::temp_dir(), &[], TIMEOUT).ok()?;
        if !out.success() {
            return None;
        }
        let (published, listeners) = out.stdout.split_once("==\n").unwrap_or((&out.stdout, ""));
        let native = listeners
            .lines()
            .filter_map(|line| line.strip_prefix('c'))
            .any(|command| !is_forwarder(command));
        if native {
            return None;
        }
        container_publishing(published, self.port)
    }

    /// The clients the recipe needs that are not where its commands run.
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
            &self.wrap(&checks.join("\n")),
            &std::env::temp_dir(),
            &[],
            TIMEOUT,
        ) else {
            return Vec::new();
        };
        // A container that went away answers with an error, not a list.
        if !out.success() {
            return self.recipe.binaries.clone();
        }
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
            return Err(anyhow::Error::new(Denied(self.denied(name, main, "made"))));
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
            return Err(anyhow::Error::new(Denied(
                self.denied(name, main, "dropped"),
            )));
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
        let script = self.wrap(&template::render_with(command, &vars)?);
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
        let rendered = match &self.runner {
            Runner::Host => rendered,
            Runner::Container { id, .. } => self.in_container(id, &rendered),
        };
        Some(match &self.recipe.password_env {
            Some(var) if self.login.has_password() => {
                format!("{rendered}   (with the password in {var})")
            }
            _ => rendered,
        })
    }

    /// How many keys a slot holds: the recipe's `size` prints it as a bare
    /// number on its last line. What an engine's client wraps a number in
    /// is the recipe's to strip, never this code's.
    pub fn size(&self, slot: u32) -> Result<u64> {
        let command = self.command(self.recipe.size.as_deref(), "size")?;
        let out = self.run(command, Some(&slot.to_string()))?;
        let count = out
            .stdout
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .and_then(|line| line.parse().ok());
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
        let (user, host) = self.account();
        let vars = Vars {
            prefix_like: Some(prefix_like(main)),
            account_user: Some(user),
            account_host: Some(host),
            ..self.vars(None)
        };
        template::render_with(grant, &vars).ok()
    }

    /// Who the server knows this login as, `user` and `host`: the recipe's
    /// `account` answer, else the login's user at `localhost`.
    fn account(&self) -> (String, String) {
        self.recipe
            .account
            .as_deref()
            .and_then(|command| self.run(command, None).ok())
            .filter(proc::Captured::success)
            .and_then(|out| {
                let line = out.stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
                let (user, host) = line.rsplit_once('@')?;
                Some((user.to_string(), host.to_string()))
            })
            .unwrap_or_else(|| {
                (
                    self.login.user.clone().unwrap_or_default(),
                    "localhost".to_string(),
                )
            })
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
        let (host, port) = match &self.runner {
            Runner::Host => (self.host.as_str(), self.port),
            Runner::Container { port, .. } => ("127.0.0.1", *port),
        };
        Vars {
            host: Some(shell_quote(host)),
            port: Some(port.to_string()),
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

    /// The script `bash -lc` runs for one rendered command: the command
    /// itself here, or `docker exec` of it in the container.
    fn wrap(&self, rendered: &str) -> String {
        match &self.runner {
            Runner::Host => self.with_path(rendered),
            Runner::Container { id, .. } => self.with_path(&self.in_container(id, rendered)),
        }
    }

    /// `docker exec` of one command in a container, its password variable
    /// passed by name alone: `-e PGPASSWORD` hands the container the value
    /// `docker` itself was started with, so it is never on a command line.
    fn in_container(&self, id: &str, rendered: &str) -> String {
        let pass = match &self.recipe.password_env {
            Some(var) if self.login.has_password() => format!("-e {} ", shell_quote(var)),
            _ => String::new(),
        };
        format!(
            "docker exec {pass}{} sh -c {}",
            shell_quote(id),
            shell_quote(rendered)
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
        Ok(self.wrap(&rendered))
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
        Ok(self.wrap(&template::render_with(command, &vars)?))
    }
}

impl<'a> Server<'a> {
    /// This server reached as its own administrator, inside the container
    /// that runs it, with the login its environment keeps: the recipe's
    /// `[namespace.container_admin]`. `None` when the recipe names none,
    /// no container publishes the port, or the container's environment
    /// names nobody.
    pub fn admin(&self) -> Option<Server<'a>> {
        let admin = self.recipe.container_admin.as_ref()?;
        let (id, port) = match &self.runner {
            Runner::Container { id, port } => (id.clone(), *port),
            Runner::Host => self.container()?,
        };
        let wanted: Vec<&String> = admin.user_from.iter().chain(&admin.password_from).collect();
        let env = self.container_env(&id, &wanted);
        let first = |keys: &[String]| {
            keys.iter()
                .find_map(|key| env.get(key).map(|value| (key.clone(), value.clone())))
        };
        let user = first(&admin.user_from);
        let password = first(&admin.password_from);
        let from = match (&user, &password) {
            (Some((u, _)), Some((p, _))) => format!("{u} and {p} in the container's environment"),
            (Some((u, _)), None) => format!("{u} in the container's environment"),
            (None, Some((p, _))) => format!("{p} in the container's environment"),
            (None, None) => "the image's own administrator".to_string(),
        };
        let user = user
            .map(|(_, value)| value)
            .or_else(|| admin.user.clone())?;
        let server = Server {
            service: self.service,
            recipe: self.recipe,
            host: self.host.clone(),
            port: self.port,
            login: Login::new(Some(user), password.map(|(_, value)| value), from),
            bin_dir: self.bin_dir.clone(),
            runner: Runner::Container { id, port },
        };
        // An image made with a random password, or one kept in a file,
        // names an administrator pando cannot log in as: none, then, and
        // whatever would have been asked is asked.
        server.ping().ok()?;
        Some(server)
    }
}

/// Whether a host is this machine's own loopback, as an app's env names it.
fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback() || ip.is_unspecified())
}

/// Whether a process listening on a published port is a container
/// runtime forwarding it, by the command name `lsof` gives: Docker
/// Desktop, OrbStack, Colima and Lima, Podman, rootless Docker.
fn is_forwarder(command: &str) -> bool {
    const FORWARDERS: [&str; 10] = [
        "com.docke",
        "docker",
        "vpnkit",
        "orbstack",
        "rootlessk",
        "gvproxy",
        "podman",
        "limactl",
        "colima",
        "ssh",
    ];
    let command = command.to_ascii_lowercase();
    FORWARDERS.iter().any(|name| command.starts_with(name))
}

/// The container in `docker ps` and `docker port` output that publishes
/// `published` on this machine, and the port it listens on inside: the
/// lines after `--` are each container's id followed by its mappings,
/// `5432/tcp -> 0.0.0.0:15432`. The first that maps it wins.
pub(super) fn container_publishing(stdout: &str, published: u16) -> Option<(String, u16)> {
    let (_, listing) = stdout.split_once("--\n")?;
    let mut current: Option<&str> = None;
    let mut found: Vec<(String, u16)> = Vec::new();
    for line in listing.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some((inside, outside)) = line.split_once(" -> ") else {
            current = Some(line);
            continue;
        };
        let Some((ip, port)) = outside.rsplit_once(':') else {
            continue;
        };
        // Published where this machine's loopback reaches it: every
        // address, or loopback itself.
        if port.parse::<u16>().ok() != Some(published) || !is_loopback(ip) {
            continue;
        }
        let Some(id) = current else {
            continue;
        };
        let Some(inside) = inside.split('/').next().and_then(|p| p.parse::<u16>().ok()) else {
            continue;
        };
        if !found.iter().any(|(known, _)| known == id) {
            found.push((id.to_string(), inside));
        }
    }
    // Two containers on one port are a choice pando does not make.
    match found.as_slice() {
        [one] => Some(one.clone()),
        _ => None,
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
