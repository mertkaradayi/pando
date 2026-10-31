//! The native adapter: a private database without a container.
//!
//! The compose adapter in `services.rs` isolates a worktree by giving it
//! its own compose project; this one gives it its own *process*. A recipe
//! says what initialises a data directory, what starts the server on a
//! port pando allocated, and how to tell when it is ready; everything else
//! — the role, the env rewriting, the log tab, the teardown — is the same
//! machinery either kind of service goes through.
//!
//! Four things here are decisions rather than mechanics:
//!
//! - **The data directory is initialised once, and never re-initialised.**
//!   A marker inside it records that pando did so. A directory that
//!   already holds data and has no marker is *adopted*, because running
//!   `initdb` over a real cluster would destroy it. An init that fails
//!   takes the directory it was making with it, so the next start is a
//!   clean retry rather than an adoption of a half-built wreck.
//! - **The socket directory is a fixed-length path in the temporary
//!   directory.** `sun_path` is 104 bytes; pando's own data directory path
//!   is most of that before the branch name is interesting. See
//!   [`crate::paths::PandoPaths::service_socket_dir`].
//! - **`<pando home>/bin` is prepended to PATH for every recipe command.**
//!   The same hook the docker shim uses, and for the same two reasons: a
//!   developer whose Postgres is not on the PATH a login shell resolves
//!   has somewhere to put a shim, and the tests drive a fake engine per
//!   test home without mutating the process environment. It is prepended
//!   *inside* the command, after the login profile has run, so nothing the
//!   profile does can undo it.
//! - **Readiness is gated on a connect.** The recipe's own check is a
//!   process spawn through a login shell; asking four times a second for a
//!   minute is expensive for an answer a TCP connect can rule out for
//!   free. Nothing listening means not ready, without asking.
//!
//! Stopping is not here. `stop` and `rm` never load config, so they have
//! no recipe to read — they signal the process group, which is the one
//! thing that works whatever the recipe said, and is why the pid and the
//! pgid go into `ServiceRecord` before anything is waited on.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::paths::{MAX_SOCKET_PATH, PandoPaths, SOCKET_NAME_BUDGET};
use crate::process::{self as proc, shell_quote};
use crate::recipes::Recipe;
use crate::template;

/// How long an init command gets. `initdb` on a cold page cache is
/// seconds, not minutes, but a first run that also compiles something is
/// not pando's business to cut short.
const INIT_TIMEOUT: Duration = Duration::from_secs(300);

/// How long one readiness probe gets before it is treated as "not yet".
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the `create` step gets. It talks to a server that is already
/// answering, so this is generous rather than considered.
const CREATE_TIMEOUT: Duration = Duration::from_secs(60);

/// How often readiness is re-checked, matching the compose adapter.
const POLL: Duration = Duration::from_millis(250);

/// What a data directory may be called in a recipe's `{db_name}` or
/// `{db_user}`: it is spliced into a SQL statement by the `create`
/// command, so anything that could end a quoted identifier is refused.
fn is_plain_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// One `[[services]] kind = "native"` entry, flattened.
///
/// Borrowed from the config rather than copied, and built here rather than
/// in `actions`, so the one place that knows what a native block *means*
/// is the module that runs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry<'a> {
    pub name: &'a str,
    pub preset: Option<&'a str>,
    pub port_env: Option<&'a str>,
    pub init: Option<&'a str>,
    pub cmd: Option<&'a str>,
    pub ready: Option<&'a str>,
    pub ready_timeout_s: Option<u64>,
    pub env: &'a std::collections::BTreeMap<String, String>,
}

impl<'a> Entry<'a> {
    /// The entry a `[[services]]` block is, when it is a native one.
    pub fn of(service: &'a crate::config::ServiceConfig) -> Option<Entry<'a>> {
        match service {
            crate::config::ServiceConfig::Compose { .. } => None,
            crate::config::ServiceConfig::Native {
                name,
                preset,
                port_env,
                init,
                cmd,
                ready,
                ready_timeout_s,
                env,
            } => Some(Entry {
                name,
                preset: preset.as_deref(),
                port_env: port_env.as_deref(),
                init: init.as_deref(),
                cmd: cmd.as_deref(),
                ready: ready.as_deref(),
                ready_timeout_s: *ready_timeout_s,
                env,
            }),
        }
    }

    /// Every native entry of a config, in file order.
    pub fn all(config: &'a crate::config::Config) -> Vec<Entry<'a>> {
        config.services.iter().filter_map(Entry::of).collect()
    }

    /// Which recipe this entry names. `preset` when it says, otherwise its
    /// own name — `name = "postgres"` needs no second line saying so.
    pub fn preset(&self) -> &'a str {
        self.preset.unwrap_or(self.name)
    }

    /// What the app reads to find this service, and whether the entry said
    /// so itself.
    ///
    /// A key the *recipe* supplied is a default rather than an
    /// instruction: a project with no `DATABASE_URL` anywhere has not
    /// asked for one to be rewritten, and failing its start over a key it
    /// never wrote would be pando inventing a requirement.
    pub fn env_map(
        &self,
        recipe: Option<&Recipe>,
    ) -> (std::collections::BTreeMap<String, String>, bool) {
        if !self.env.is_empty() {
            return (self.env.clone(), true);
        }
        if let Some(key) = self.port_env {
            return (
                std::collections::BTreeMap::from([(key.to_string(), self.name.to_string())]),
                true,
            );
        }
        let defaulted = recipe
            .and_then(|r| r.service())
            .and_then(|s| s.port_env.clone());
        match defaulted {
            Some(key) => (
                std::collections::BTreeMap::from([(key, self.name.to_string())]),
                false,
            ),
            None => (std::collections::BTreeMap::new(), true),
        }
    }
}

/// Where the recipe a native service runs actually came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A recipe, built-in or the developer's own.
    Recipe(crate::recipes::Origin),
    /// No recipe at all: the `[[services]]` entry carries its own `cmd`.
    Inline,
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Recipe(origin) => origin.describe(),
            Source::Inline => "the [[services]] entry itself".to_string(),
        }
    }
}

/// A recipe with the entry's own fields laid over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub recipe: Recipe,
    pub source: Source,
    /// Which fields the entry overrode, for a report that has to explain
    /// why a built-in is not behaving like the built-in.
    pub overrides: Vec<&'static str>,
}

/// Which recipe a native entry runs, with its inline fields applied.
///
/// A named `preset` that does not resolve is always a refusal, even when
/// the entry carries its own `cmd`: it is a typo far more often than it is
/// a leftover. A preset that came from the entry's *name* is a guess, so
/// an entry called `db` with its own `cmd` and no recipe called `db` is
/// simply an inline service.
///
/// The guess is checked against recipe *names* only, never their aliases.
/// An entry called `pg` with its own `cmd` means what it says; resolving
/// it through Postgres's `pg` alias would run `initdb` against it and
/// demand an engine on PATH for a service the developer wrote out in
/// full. An alias still resolves a `preset` that names one, and still
/// resolves a name that has no command of its own — those are both asking
/// for a recipe.
pub fn resolve(recipes: &crate::recipes::Recipes, entry: &Entry<'_>) -> Result<Resolved> {
    let preset = entry.preset();
    // A *broken* file claiming this name is not an absent recipe: falling
    // through to the entry's own `cmd` would silently ignore the file the
    // developer is trying to fix.
    let known = recipes.exact(preset).is_some() || recipes.is_broken(preset);
    let (mut recipe, source) = if !known && entry.preset.is_none() && entry.cmd.is_some() {
        (
            Recipe {
                kind: crate::recipes::Kind::Service,
                name: entry.name.to_string(),
                aliases: Vec::new(),
                summary: None,
                binaries: Vec::new(),
                version_flag: None,
                install: None,
                notes: None,
                namespace: None,
                prefix: None,
                untested: false,
                body: crate::recipes::Body::Service(crate::recipes::ServiceRecipe::default()),
            },
            Source::Inline,
        )
    } else {
        let loaded = recipes
            .get(preset)
            .with_context(|| format!("the service {:?} names the recipe {preset:?}", entry.name))?;
        (loaded.recipe.clone(), Source::Recipe(loaded.origin.clone()))
    };
    let why = recipe.starts_no_server();
    let service = recipe.service_mut().with_context(|| {
        format!(
            "the service {:?} names the recipe {}: a [[services]] entry needs one that starts a \
             server",
            entry.name,
            why.unwrap_or_default()
        )
    })?;
    let mut overrides = Vec::new();
    if let Some(cmd) = entry.cmd {
        service.cmd = cmd.to_string();
        overrides.push("cmd");
    }
    if let Some(init) = entry.init {
        service.init = Some(init.to_string());
        overrides.push("init");
    }
    if let Some(ready) = entry.ready {
        service.ready = Some(ready.to_string());
        overrides.push("ready");
    }
    if let Some(timeout) = entry.ready_timeout_s {
        service.ready_timeout_s = Some(timeout);
        overrides.push("ready_timeout_s");
    }
    if let Some(port_env) = entry.port_env {
        service.port_env = Some(port_env.to_string());
        overrides.push("port_env");
    }
    if service.cmd.trim().is_empty() {
        bail!(
            "the service {:?} has no command to start: name a `preset`, or give the entry its \
             own `cmd`",
            entry.name
        );
    }
    Ok(Resolved {
        recipe,
        source,
        overrides,
    })
}

/// One native service of one worktree, fully resolved: which recipe, which
/// port, which directories, and what the app's own URL calls its database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Native {
    /// The worktree, for the sentence a failure prints.
    pub worktree: String,
    /// The service name, which is also its role and its log tab.
    pub service: String,
    pub recipe: Recipe,
    pub port: u16,
    pub datadir: PathBuf,
    pub socket_dir: PathBuf,
    pub log_file: PathBuf,
    /// Prepended to PATH for every command this recipe runs.
    pub bin_dir: PathBuf,
    /// What `{db_user}` and `{db_name}` resolve to: what the app's own URL
    /// names, else what the recipe says they mean when it names none.
    db_user: Option<String>,
    db_name: Option<String>,
}

/// What [`Native::ensure_init`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Init {
    /// The init command ran and the marker was written.
    Ran,
    /// The directory already held data and no marker, so it was taken as
    /// it is. Re-running an init over it would have destroyed it.
    Adopted,
    /// The marker was already there.
    Already,
}

/// What the marker inside a data directory records. Read back by `doctor`;
/// its *presence* is the whole of the decision the start path makes, so a
/// recipe whose init line is edited later does not re-initialise over real
/// data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub recipe: String,
    pub at: chrono::DateTime<chrono::Utc>,
    /// Whether the directory was adopted rather than initialised.
    #[serde(default)]
    pub adopted: bool,
    /// A hash of the init command that made it, for a report. Not a
    /// trigger: see above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init: Option<String>,
}

/// The file inside a data directory that says pando initialised it.
///
/// Inside, not beside: deleting the data has to delete the marker with it,
/// or a developer who wiped the directory by hand would get a start that
/// skips the init and a server that finds nothing there.
pub fn marker_file(datadir: &Path) -> PathBuf {
    datadir.join(".pando-init.toml")
}

/// The marker a data directory carries, if it carries one.
pub fn marker(datadir: &Path) -> Option<Marker> {
    toml::from_str(&std::fs::read_to_string(marker_file(datadir)).ok()?).ok()
}

impl Native {
    /// Everything a start needs to know about one native service, before
    /// anything is created or spawned.
    ///
    /// `url` is what the app will read to find this service, after
    /// [`crate::services::app_env`] has rewritten its port — which is
    /// where the database and role a recipe has to create come from.
    pub fn plan(
        paths: &PandoPaths,
        worktree: &str,
        service: &str,
        recipe: Recipe,
        port: u16,
        url: Option<&str>,
    ) -> Result<Native> {
        Native::plan_with(paths, worktree, service, recipe, port, url, (None, None))
    }

    /// [`Native::plan`], with what the app's env says about the service in
    /// keys of their own: `DATABASE_NAME` beside a bare `DATABASE_PORT`.
    ///
    /// Between the URL and the recipe's default in precedence: a URL that
    /// names a database has said it in the one value the app connects
    /// with, and a sibling key is still the app's word where the recipe's
    /// default is only the engine's.
    pub fn plan_with(
        paths: &PandoPaths,
        worktree: &str,
        service: &str,
        recipe: Recipe,
        port: u16,
        url: Option<&str>,
        sibling: (Option<String>, Option<String>),
    ) -> Result<Native> {
        let (url_user, url_db) = url.map(crate::services::url_identity).unwrap_or_default();
        let (sibling_user, sibling_db) = sibling;
        let url_user = url_user.or(sibling_user);
        let url_db = url_db.or(sibling_db);
        let fallback = recipe
            .service()
            .map(|s| (s.db_user.clone(), s.db_name.clone()));
        let (default_user, default_db) = fallback.unwrap_or_default();
        Ok(Native {
            worktree: worktree.to_string(),
            service: service.to_string(),
            port,
            datadir: paths.service_data_dir(worktree, service),
            socket_dir: paths.service_socket_dir(worktree, service),
            log_file: paths.log_file(worktree, service),
            bin_dir: paths.home.join("bin"),
            db_user: checked(url_user, "user name", service)?.or(default_user),
            db_name: checked(url_db, "database name", service)?.or(default_db),
            recipe,
        })
    }

    fn service_recipe(&self) -> Result<&crate::recipes::ServiceRecipe> {
        self.recipe.service().with_context(|| {
            format!(
                "the recipe {}, and a [[services]] entry needs one that starts a server",
                self.recipe.starts_no_server().unwrap_or_default()
            )
        })
    }

    /// How long this service gets to become ready.
    pub fn ready_timeout(&self) -> Duration {
        Duration::from_secs(
            self.recipe
                .service()
                .and_then(|s| s.ready_timeout_s)
                .unwrap_or(crate::services::DEFAULT_READY_TIMEOUT_S),
        )
    }

    /// One recipe command, with its placeholders filled in and pando's own
    /// `bin` directory ahead of everything on PATH.
    pub fn shell_cmd(&self, cmd: &str) -> Result<String> {
        let rendered = template::render_with(cmd, self)?;
        Ok(format!(
            "export PATH={}:\"$PATH\"\n{rendered}",
            shell_quote(&self.bin_dir.display().to_string())
        ))
    }

    /// The binaries this recipe needs that this machine does not have.
    ///
    /// Asked once, before anything is created or spawned: a start that
    /// initialises a data directory and *then* discovers there is no
    /// server to run against it has left something behind for nothing.
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
        let Ok(script) = self.shell_cmd(&checks.join("\n")) else {
            return Vec::new();
        };
        let Ok(out) = proc::run_captured(&script, &std::env::temp_dir(), &[], PROBE_TIMEOUT) else {
            // The probe itself could not run. That is not evidence about
            // the engine, and refusing a start over it would be a guess.
            return Vec::new();
        };
        out.stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The refusal a missing engine earns: what is missing, which recipe
    /// wanted it, and how a developer installs it — printed, never run.
    pub fn missing_binaries_error(&self, missing: &[String]) -> anyhow::Error {
        let install = match self.recipe.install.as_deref() {
            Some(hint) => format!(" — install it with: {hint}"),
            None => String::new(),
        };
        anyhow::anyhow!(
            "the service {:?} runs the {:?} recipe, and {} {} not on PATH{install}. pando \
             never installs an engine: install it yourself, put a shim in {}, or give {:?} a \
             recipe of your own in {}",
            self.service,
            self.recipe.name,
            missing.join(", "),
            if missing.len() == 1 { "is" } else { "are" },
            self.bin_dir.display(),
            self.service,
            self.recipes_dir().display(),
        )
    }

    /// Where a developer's own recipes live, for a message that has to
    /// name it.
    fn recipes_dir(&self) -> PathBuf {
        self.bin_dir
            .parent()
            .unwrap_or(&self.bin_dir)
            .join("recipes")
    }

    /// Initialises the data directory, once.
    pub fn ensure_init(&self, progress: &dyn Fn(&str)) -> Result<Init> {
        let service = self.service_recipe()?;
        if marker(&self.datadir).is_some() {
            return Ok(Init::Already);
        }
        // A directory with something in it and no marker is somebody
        // else's data — a developer's own cluster pointed at by hand, or
        // one pando made before the marker existed. Adopted, never
        // re-initialised: `initdb` over a real cluster destroys it.
        if has_entries(&self.datadir) {
            progress(&format!(
                "{}: adopting the data directory that is already there",
                self.service
            ));
            self.write_marker(true, None)?;
            return Ok(Init::Adopted);
        }
        let parent = self
            .datadir
            .parent()
            .context("a data directory with no parent")?;
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        let Some(init) = service.init.clone() else {
            std::fs::create_dir_all(&self.datadir)
                .with_context(|| format!("create {}", self.datadir.display()))?;
            self.write_marker(false, None)?;
            return Ok(Init::Ran);
        };
        // From here pando owns the directory: it did not exist, or it was
        // empty. Anything the init leaves behind on the way to failing is
        // pando's to remove, because the alternative is that the next
        // start *adopts* the wreck.
        progress(&format!(
            "{}: initialising its data directory",
            self.service
        ));
        let script = self.shell_cmd(&init)?;
        let ran = proc::run_captured(&script, parent, &[], INIT_TIMEOUT);
        let captured = match ran {
            Ok(captured) if captured.success() => captured,
            other => {
                let _ = std::fs::remove_dir_all(&self.datadir);
                let reason = match &other {
                    Ok(captured) => captured
                        .last_stderr_line()
                        .or_else(|| captured.stdout.lines().rev().find(|l| !l.trim().is_empty()))
                        .unwrap_or("no output")
                        .to_string(),
                    Err(e) => format!("{e:#}"),
                };
                bail!(
                    "initialising the data directory for {:?} failed: {reason} — nothing was \
                     left behind, so fixing it and starting again is a clean retry",
                    self.service
                );
            }
        };
        let _ = captured;
        self.write_marker(false, Some(&init))?;
        Ok(Init::Ran)
    }

    fn write_marker(&self, adopted: bool, init: Option<&str>) -> Result<()> {
        let marker = Marker {
            recipe: self.recipe.name.clone(),
            at: chrono::Utc::now(),
            adopted,
            init: init.map(|text| format!("md5:{:x}", md5::compute(text))),
        };
        let path = marker_file(&self.datadir);
        std::fs::write(
            &path,
            format!(
                "# pando initialised this data directory. Delete the directory to start over;\n\
                 # deleting this file alone would make pando adopt what is left.\n{}",
                toml::to_string(&marker).context("serialise the init marker")?
            ),
        )
        .with_context(|| format!("write {}", path.display()))
    }

    /// Creates the socket directory, 0700 and ours, and refuses a path a
    /// socket could not be bound under.
    pub fn ensure_socket_dir(&self) -> Result<()> {
        let longest = self.socket_dir.as_os_str().len() + 1 + SOCKET_NAME_BUDGET;
        if longest > MAX_SOCKET_PATH {
            bail!(
                "the socket directory for {:?} would be {} bytes and a Unix socket path may be \
                 {MAX_SOCKET_PATH} — set TMPDIR to something shorter",
                self.service,
                self.socket_dir.as_os_str().len()
            );
        }
        std::fs::create_dir_all(&self.socket_dir)
            .with_context(|| format!("create {}", self.socket_dir.display()))?;
        let meta = std::fs::metadata(&self.socket_dir)
            .with_context(|| format!("stat {}", self.socket_dir.display()))?;
        // The temporary directory is world-writable, so a directory that
        // is already there and is not ours is not one to put a database
        // socket in.
        if !crate::platform::files::owned_by_current_user(&meta) {
            bail!(
                "{} already exists and belongs to someone else — pando will not put {:?}'s \
                 socket there",
                self.socket_dir.display(),
                self.service
            );
        }
        crate::platform::files::set_permission_bits(&self.socket_dir, 0o700)
            .with_context(|| format!("make {} private", self.socket_dir.display()))
    }

    /// Starts the server, detached, as pando's own child.
    pub fn spawn(&self) -> Result<proc::SpawnResult> {
        let service = self.service_recipe()?;
        self.ensure_socket_dir()?;
        let shell_cmd = self.shell_cmd(&service.cmd)?;
        proc::spawn_detached(proc::SpawnOptions {
            shell_cmd: &shell_cmd,
            cwd: &self.datadir,
            log_file: &self.log_file,
            env: &[],
            status_file: None,
        })
        .with_context(|| format!("start the service {:?}", self.service))
    }

    /// Waits until the server answers, or fails saying what was waited for.
    ///
    /// Two things end the wait early. A server whose process is gone is
    /// never going to be ready, and the whole minute spent finding that
    /// out hides the one line in its log that explains it. And nothing
    /// listening on the port means the recipe's own check cannot possibly
    /// pass, so it is not run — a login shell four times a second for a
    /// minute is an expensive way to be told "not yet".
    pub fn wait_ready(&self, pid: u32, timeout: Duration, progress: &dyn Fn(&str)) -> Result<()> {
        let service = self.service_recipe()?;
        let deadline = Instant::now() + timeout;
        progress(&format!("waiting for {}", self.service));
        let mut last: Option<String> = None;
        loop {
            if !proc::is_alive(pid) {
                bail!(
                    "the service {:?} exited before it was ready — `pando logs {} --source {}` \
                     says why",
                    self.service,
                    self.worktree,
                    self.service
                );
            }
            if crate::ports::something_is_listening(self.port) {
                match &service.ready {
                    None => return Ok(()),
                    Some(ready) => {
                        let script = self.shell_cmd(ready)?;
                        match proc::run_captured(&script, &self.datadir, &[], PROBE_TIMEOUT) {
                            Ok(captured) if captured.success() => return Ok(()),
                            Ok(captured) => {
                                last = captured.last_stderr_line().map(str::to_string);
                            }
                            Err(e) => last = Some(format!("{e:#}")),
                        }
                    }
                }
            }
            // A start cancelled while it waits stops here, within a round,
            // rather than at the deadline: the bring-up stops the server.
            crate::cancel::check()?;
            if Instant::now() >= deadline {
                let why = match &last {
                    Some(line) => format!(" — its readiness check last said: {line}"),
                    None => format!(
                        " — nothing is listening on port {}; `pando logs {} --source {}` says why",
                        self.port, self.worktree, self.service
                    ),
                };
                bail!(
                    "the service {:?} did not become ready in {}s{why}",
                    self.service,
                    timeout.as_millis().div_ceil(1000)
                );
            }
            std::thread::sleep(POLL);
        }
    }

    /// The recipe's `create` step, run after every readiness.
    ///
    /// Idempotent by contract, because what it creates is what the *app's*
    /// URL names and that can change without the data directory changing:
    /// a data directory initialised last week has the database the URL
    /// named last week.
    pub fn create(&self, progress: &dyn Fn(&str)) -> Result<()> {
        let service = self.service_recipe()?;
        let Some(create) = &service.create else {
            return Ok(());
        };
        // A `create` that asks for the app's database is about the app's
        // database. A service nothing addresses — no `env` map, no
        // `port_env`, no such key in the worktree — has none, and there
        // is nothing to make. Checked rather than discovered by letting
        // the render fail: a skip that happens because an error looked
        // right is a skip nobody can reason about.
        let wants_identity = create.contains("{db_name}") || create.contains("{db_user}");
        if wants_identity && self.db_name.is_none() && self.db_user.is_none() {
            progress(&format!(
                "{}: nothing in this project addresses it, so there is no database to create",
                self.service
            ));
            return Ok(());
        }
        let script = self.shell_cmd(create)?;
        let captured = proc::run_captured(&script, &self.datadir, &[], CREATE_TIMEOUT)
            .with_context(|| format!("prepare the service {:?}", self.service))?;
        if captured.success() {
            return Ok(());
        }
        bail!(
            "preparing the service {:?} failed: {}",
            self.service,
            captured.last_stderr_line().unwrap_or("no output")
        )
    }
}

/// A value the app's URL supplied that is safe to splice into a recipe's
/// `create` command, or a refusal naming it.
fn checked(value: Option<String>, what: &str, service: &str) -> Result<Option<String>> {
    match value {
        Some(value) if !is_plain_identifier(&value) => bail!(
            "the {what} the env of the service {service:?} names is {value:?}, which is not a \
             plain identifier — a recipe puts it in a SQL statement, so it has to be letters, \
             digits, `_`, `-` or `.`"
        ),
        other => Ok(other),
    }
}

fn has_entries(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// The placeholders a recipe command may use. Deliberately small: a recipe
/// is about one server in one directory on one port. `{datadir}`,
/// `{socket_dir}` and `{log}` are shell-quoted already, so a recipe writes
/// them bare: `--datadir={datadir}`, `{datadir}/pando.pid`.
const KNOWN: [&str; 7] = [
    "port",
    "datadir",
    "socket_dir",
    "service",
    "log",
    "db_user",
    "db_name",
];

impl template::Resolver for Native {
    fn resolve(&self, key: &str, arg: Option<&str>) -> Result<String> {
        let named = |value: &Option<String>, key: &str, label: &str| match value {
            Some(value) => Ok(value.clone()),
            None => bail!(
                "{{{key}}} — nothing says what the {label} of the service {:?} is: give the \
                 [[services]] entry an `env` key whose URL names one, or give the recipe a \
                 `{key}` default",
                self.service
            ),
        };
        // A path comes back shell-quoted, the way a namespace recipe's
        // `{host}` and `{user}` do: it is built from the repository's
        // directory name and the branch's, and either may hold a space, a
        // paren or a quote that would split or break the command.
        let path = |path: &Path| shell_quote(&path.display().to_string());
        match (key, arg) {
            ("port", None) => Ok(self.port.to_string()),
            ("datadir", None) => Ok(path(&self.datadir)),
            ("socket_dir", None) => Ok(path(&self.socket_dir)),
            ("service", None) => Ok(self.service.clone()),
            ("log", None) => Ok(path(&self.log_file)),
            ("db_user", None) => named(&self.db_user, "db_user", "user"),
            ("db_name", None) => named(&self.db_name, "db_name", "database name"),
            (key, Some(arg)) if KNOWN.contains(&key) => {
                bail!("{{{key}:{arg}}} — {key} takes no argument in a recipe")
            }
            (key, _) => bail!(
                "unknown placeholder {{{key}}} in a recipe — a recipe understands {}",
                KNOWN.join(", ")
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recipes::Recipes;
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;

    /// A pando home with a project in it, and no engine anywhere near it.
    struct Fixture {
        _dir: tempfile::TempDir,
        paths: PandoPaths,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        crate::testutil::init_repo(&root);
        let paths = PandoPaths {
            home: dir.path().join("home"),
            project: crate::project::ProjectRef {
                id: "fixture-00000000".to_string(),
                root: root.clone(),
                display_name: "fixture".to_string(),
                ..Default::default()
            },
        };
        paths.ensure_home().unwrap();
        Fixture { _dir: dir, paths }
    }

    /// A script on pando's own `bin` hook, which is what every recipe
    /// command sees first on PATH. No engine is installed for these tests
    /// and none is needed: what is under test is the adapter.
    fn shim(paths: &PandoPaths, name: &str, body: &str) {
        let bin = paths.home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let path = bin.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn recipe(text: &str) -> Recipe {
        crate::recipes::parse(text).unwrap()
    }

    fn plan(fx: &Fixture, text: &str, port: u16) -> Native {
        Native::plan(
            &fx.paths,
            "feat+one",
            "db",
            recipe(text),
            port,
            Some("postgres://app:secret@localhost:5432/acme_dev"),
        )
        .unwrap()
    }

    const QUIET: &dyn Fn(&str) = &|_: &str| {};

    // ---- placeholders ---------------------------------------------------

    #[test]
    fn a_recipe_command_sees_the_port_the_directories_and_the_apps_own_database() {
        let fx = fixture();
        let native = plan(
            &fx,
            "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n",
            17_400,
        );
        let rendered = native
            .shell_cmd("s {service} {port} {datadir} {socket_dir} {db_user} {db_name} {log}")
            .unwrap();
        assert!(rendered.contains("s db 17400 "), "{rendered}");
        assert!(rendered.contains(&native.datadir.display().to_string()));
        assert!(rendered.contains(&native.socket_dir.display().to_string()));
        assert!(rendered.contains(" app acme_dev "), "{rendered}");
        assert!(rendered.contains(&native.log_file.display().to_string()));
        // And pando's own bin first, inside the command rather than in the
        // environment, so the login profile cannot undo it.
        assert!(
            rendered.starts_with(&format!(
                "export PATH='{}':\"$PATH\"\n",
                fx.paths.home.join("bin").display()
            )),
            "{rendered}"
        );
    }

    // The data directory is named after the repository's directory and the
    // branch, and git takes `fix(db)` and `it's` as branch names. Spliced
    // in bare, a space split `initdb`'s argument and a paren was a syntax
    // error, so every built-in recipe failed for that repository.
    #[test]
    fn a_path_placeholder_reaches_the_command_as_one_word_whatever_it_holds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("My App");
        crate::testutil::init_repo(&root);
        let paths = PandoPaths {
            home: dir.path().join("home"),
            project: crate::project::ProjectRef {
                id: "My App-00000000".to_string(),
                root,
                display_name: "My App".to_string(),
                ..Default::default()
            },
        };
        paths.ensure_home().unwrap();
        let native = Native::plan(
            &paths,
            "fix(db)+it's;a&b",
            "db",
            recipe("kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n"),
            17_400,
            None,
        )
        .unwrap();
        for (placeholder, want) in [
            ("{datadir}", &native.datadir),
            ("{socket_dir}", &native.socket_dir),
            ("{log}", &native.log_file),
        ] {
            let script = native
                .shell_cmd(&format!(
                    "printf %s {placeholder}; printf '|%s' {placeholder}/x"
                ))
                .unwrap();
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .output()
                .unwrap();
            assert!(out.status.success(), "{placeholder}: {script}");
            let want = want.display().to_string();
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("{want}|{want}/x"),
                "{placeholder}"
            );
        }
    }

    #[test]
    fn a_recipe_falls_back_to_its_own_defaults_when_the_app_names_no_database() {
        let fx = fixture();
        let native = Native::plan(
            &fx.paths,
            "feat+one",
            "db",
            recipe(
                "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n\
                 db_user = \"postgres\"\ndb_name = \"postgres\"\n",
            ),
            17_400,
            None,
        )
        .unwrap();
        assert_eq!(
            native
                .shell_cmd("c {db_user} {db_name}")
                .unwrap()
                .lines()
                .nth(1),
            Some("c postgres postgres")
        );
    }

    // A database addressed in parts — a bare port beside a name — names
    // its database in a key of its own. Without it the recipe had nothing
    // to create, and the app met a server with no database in it.
    #[test]
    fn a_sibling_key_names_the_database_between_the_url_and_the_recipe() {
        let fx = fixture();
        let defaults = "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n\
                        db_user = \"root\"\ndb_name = \"mysql\"\n";
        let sibling = (Some("app_user".to_string()), Some("app_db".to_string()));
        let from_sibling = Native::plan_with(
            &fx.paths,
            "feat+one",
            "db",
            recipe(defaults),
            17_400,
            Some("17400"),
            sibling.clone(),
        )
        .unwrap();
        assert_eq!(
            from_sibling
                .shell_cmd("c {db_user} {db_name}")
                .unwrap()
                .lines()
                .nth(1),
            Some("c app_user app_db"),
            "the app's own word beats the engine's default"
        );
        let from_url = Native::plan_with(
            &fx.paths,
            "feat+one",
            "db",
            recipe(defaults),
            17_400,
            Some("mysql://url_user@localhost:17400/url_db"),
            sibling,
        )
        .unwrap();
        assert_eq!(
            from_url
                .shell_cmd("c {db_user} {db_name}")
                .unwrap()
                .lines()
                .nth(1),
            Some("c url_user url_db"),
            "and the URL the app connects with beats both"
        );
        // Spliced into SQL all the same, so held to the same rule.
        let e = format!(
            "{:#}",
            Native::plan_with(
                &fx.paths,
                "feat+one",
                "db",
                recipe(defaults),
                17_400,
                None,
                (None, Some("x';DROP".to_string())),
            )
            .unwrap_err()
        );
        assert!(e.contains("plain identifier"), "{e}");
    }

    #[test]
    fn a_placeholder_nothing_can_answer_names_what_it_wanted() {
        let fx = fixture();
        let native = Native::plan(
            &fx.paths,
            "feat+one",
            "db",
            recipe("kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n"),
            17_400,
            None,
        )
        .unwrap();
        let e = format!("{:#}", native.shell_cmd("c {db_name}").unwrap_err());
        assert!(e.contains("database name"), "{e}");
        let e = format!("{:#}", native.shell_cmd("c {datadirr}").unwrap_err());
        assert!(e.contains("unknown placeholder"), "{e}");
        assert!(e.contains("socket_dir"), "{e}");
    }

    #[test]
    fn a_database_name_that_is_not_an_identifier_is_refused_before_anything_runs() {
        let fx = fixture();
        let e = format!(
            "{:#}",
            Native::plan(
                &fx.paths,
                "feat+one",
                "db",
                recipe("kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n"),
                17_400,
                Some("postgres://app@localhost:5432/acme';DROP"),
            )
            .unwrap_err()
        );
        assert!(e.contains("plain identifier"), "{e}");
        assert!(e.contains("acme';DROP"), "{e}");
    }

    #[test]
    fn a_recipes_alias_never_beats_an_entrys_own_command() {
        let fx = fixture();
        let recipes = Recipes::built_in();
        // `pg` is an alias of the built-in Postgres recipe. An entry with
        // its own `cmd` and no `preset` is not asking for that recipe,
        // and resolving it through the alias would run `initdb` against
        // the service and demand an engine on PATH.
        let env = std::collections::BTreeMap::new();
        let inline = Entry {
            name: "pg",
            preset: None,
            port_env: None,
            init: None,
            cmd: Some("exec sleep 300"),
            ready: None,
            ready_timeout_s: None,
            env: &env,
        };
        let resolved = resolve(&recipes, &inline).unwrap();
        assert_eq!(resolved.source, Source::Inline);
        assert!(resolved.recipe.binaries.is_empty(), "{:?}", resolved.recipe);
        assert_eq!(resolved.recipe.service().unwrap().init, None);

        // The same name with no command of its own *is* asking for a
        // recipe, so the alias still answers.
        let by_alias = Entry {
            cmd: None,
            ..inline
        };
        let resolved = resolve(&recipes, &by_alias).unwrap();
        assert_eq!(resolved.recipe.name, "postgres");

        // And so does an alias written down as a preset.
        let spelled = Entry {
            preset: Some("pg"),
            ..inline
        };
        assert_eq!(resolve(&recipes, &spelled).unwrap().recipe.name, "postgres");
        let _ = fx;
    }

    #[test]
    fn a_broken_recipe_file_is_not_answered_by_the_entrys_own_command() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("db.toml"), "kind = \"service\"\nname =").unwrap();
        let recipes = Recipes::load(dir.path());
        let env = std::collections::BTreeMap::new();
        let entry = Entry {
            name: "db",
            preset: None,
            port_env: None,
            init: None,
            cmd: Some("exec sleep 300"),
            ready: None,
            ready_timeout_s: None,
            env: &env,
        };
        // Falling through to the inline command would silently ignore the
        // file the developer is trying to fix.
        let e = format!("{:#}", resolve(&recipes, &entry).unwrap_err());
        assert!(e.contains("does not load"), "{e}");
    }

    // Both refusals are one sentence broken over two source lines. The
    // continuation had lost its backslash, so each message carried the
    // next line's indentation — a run of spaces mid-sentence.
    #[test]
    fn a_recipe_that_cannot_start_a_server_is_refused_in_one_clean_sentence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("node.toml"),
            "kind = \"language\"\nname = \"node\"\n\n[language]\nmanagers = [\"nvm\"]\n",
        )
        .unwrap();
        let recipes = Recipes::load(dir.path());
        let env = std::collections::BTreeMap::new();
        let language = Entry {
            name: "node",
            preset: None,
            port_env: None,
            init: None,
            cmd: None,
            ready: None,
            ready_timeout_s: None,
            env: &env,
        };
        // A recipe's own empty `cmd` is refused when the file loads, so
        // the one way to an empty command is an entry blanking it.
        let blanked = Entry {
            name: "db",
            preset: Some("postgres"),
            cmd: Some("  "),
            ..language
        };
        for (entry, says) in [
            (language, "language recipe"),
            (blanked, "no command to start"),
        ] {
            let e = format!("{:#}", resolve(&recipes, &entry).unwrap_err());
            assert!(e.contains(says), "{e}");
            assert!(!e.contains("  "), "{e:?}");
        }
    }

    // ---- the socket path ------------------------------------------------

    #[test]
    fn the_socket_path_is_the_same_length_however_long_the_names_are() {
        let fx = fixture();
        let short = fx.paths.service_socket_dir("a", "b");
        let long = fx
            .paths
            .service_socket_dir(&"x".repeat(300), &"y".repeat(60));
        assert_eq!(short.as_os_str().len(), long.as_os_str().len());
        assert_ne!(short, long, "different services get different directories");
        // The whole point, asserted arithmetically rather than by starting
        // a server: the longest socket an engine puts in there still fits
        // in `sun_path`.
        assert!(
            long.as_os_str().len() + 1 + SOCKET_NAME_BUDGET <= MAX_SOCKET_PATH,
            "{} + a socket name is past {MAX_SOCKET_PATH}",
            long.display()
        );
        // And it is nowhere near the data directory, which is exactly what
        // would overflow.
        let datadir = fx.paths.service_data_dir(&"x".repeat(300), &"y".repeat(60));
        assert!(datadir.as_os_str().len() > MAX_SOCKET_PATH);
        assert!(!long.starts_with(&fx.paths.home));
    }

    #[test]
    fn the_socket_directory_is_created_private_and_is_ours() {
        let fx = fixture();
        let native = plan(
            &fx,
            "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n",
            17_400,
        );
        native.ensure_socket_dir().unwrap();
        let mode = std::fs::metadata(&native.socket_dir)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700, "{:o}", mode);
        // Idempotent: a second start of the same worktree must not fail on
        // the directory its first one made.
        native.ensure_socket_dir().unwrap();
        std::fs::remove_dir_all(&native.socket_dir).unwrap();
    }

    // ---- init -----------------------------------------------------------

    const INIT_RECIPE: &str = "kind = \"service\"\nname = \"r\"\nbinaries = [\"fake-init\"]\n\
                               install = \"brew install nothing\"\n\n[service]\n\
                               init = \"fake-init {datadir}\"\ncmd = \"exec sleep 300\"\n";

    #[test]
    fn init_runs_once_and_writes_a_marker_the_next_start_reads() {
        let fx = fixture();
        shim(
            &fx.paths,
            "fake-init",
            "mkdir -p \"$1\" && echo v1 > \"$1/VERSION\" && echo \"ran\" >> \"$1/../ran\"\n",
        );
        let native = plan(&fx, INIT_RECIPE, 17_400);
        assert_eq!(native.ensure_init(QUIET).unwrap(), Init::Ran);
        assert!(native.datadir.join("VERSION").is_file());
        let marker = marker(&native.datadir).expect("a marker");
        assert_eq!(marker.recipe, "r");
        assert!(!marker.adopted);
        assert!(marker.init.unwrap().starts_with("md5:"));

        assert_eq!(native.ensure_init(QUIET).unwrap(), Init::Already);
        let ran = std::fs::read_to_string(native.datadir.parent().unwrap().join("ran")).unwrap();
        assert_eq!(ran.lines().count(), 1, "the init ran twice");
    }

    #[test]
    fn a_failed_init_leaves_nothing_behind_so_the_next_start_is_a_clean_retry() {
        let fx = fixture();
        // Half-writes the directory and then fails, which is the shape
        // that matters: an empty failure would be indistinguishable from
        // no failure at all.
        shim(
            &fx.paths,
            "fake-init",
            "mkdir -p \"$1\" && echo half > \"$1/PARTIAL\"\n\
             echo 'initdb: error: directory is not what I expected' >&2\nexit 1\n",
        );
        let native = plan(&fx, INIT_RECIPE, 17_400);
        let e = format!("{:#}", native.ensure_init(QUIET).unwrap_err());
        assert!(e.contains("directory is not what I expected"), "{e}");
        assert!(e.contains("clean retry"), "{e}");
        assert!(!native.datadir.exists(), "the wreck was left behind");

        // And the retry really is clean: the same recipe, a working
        // binary, and the result is an initialisation rather than an
        // adoption of what the failure left.
        shim(
            &fx.paths,
            "fake-init",
            "mkdir -p \"$1\" && echo v1 > \"$1/VERSION\"\n",
        );
        assert_eq!(native.ensure_init(QUIET).unwrap(), Init::Ran);
        assert!(!native.datadir.join("PARTIAL").exists());
    }

    #[test]
    fn a_data_directory_that_is_already_there_is_adopted_and_never_reinitialised() {
        let fx = fixture();
        shim(
            &fx.paths,
            "fake-init",
            "echo 'initdb: I destroyed it' >&2\nexit 9\n",
        );
        let native = plan(&fx, INIT_RECIPE, 17_400);
        std::fs::create_dir_all(&native.datadir).unwrap();
        std::fs::write(native.datadir.join("PG_VERSION"), "16\n").unwrap();

        assert_eq!(native.ensure_init(QUIET).unwrap(), Init::Adopted);
        assert_eq!(
            std::fs::read_to_string(native.datadir.join("PG_VERSION")).unwrap(),
            "16\n"
        );
        assert!(marker(&native.datadir).unwrap().adopted);
        // And it is a marker like any other, so the start after it does
        // nothing at all.
        assert_eq!(native.ensure_init(QUIET).unwrap(), Init::Already);
    }

    #[test]
    fn a_recipe_with_no_init_step_still_gets_a_directory_and_a_marker() {
        let fx = fixture();
        let native = plan(
            &fx,
            "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"exec sleep 300\"\n",
            17_400,
        );
        assert_eq!(native.ensure_init(QUIET).unwrap(), Init::Ran);
        assert!(native.datadir.is_dir());
        assert_eq!(marker(&native.datadir).unwrap().init, None);
    }

    #[test]
    fn deleting_the_data_directory_deletes_the_marker_with_it() {
        let fx = fixture();
        shim(
            &fx.paths,
            "fake-init",
            "mkdir -p \"$1\" && echo v1 > \"$1/VERSION\"\n",
        );
        let native = plan(&fx, INIT_RECIPE, 17_400);
        native.ensure_init(QUIET).unwrap();
        std::fs::remove_dir_all(&native.datadir).unwrap();
        // The marker lived inside, so a developer who wiped the data by
        // hand gets an initialisation rather than a server pointed at a
        // directory that is not there.
        assert_eq!(native.ensure_init(QUIET).unwrap(), Init::Ran);
    }

    // ---- the engine this machine does not have ---------------------------

    #[test]
    fn a_missing_engine_is_named_with_what_wanted_it_and_how_to_install_it() {
        let fx = fixture();
        let native = plan(&fx, INIT_RECIPE, 17_400);
        let missing = native.missing_binaries();
        assert_eq!(missing, vec!["fake-init".to_string()]);
        let e = format!("{:#}", native.missing_binaries_error(&missing));
        assert!(e.contains("fake-init"), "{e}");
        assert!(e.contains("\"r\" recipe"), "{e}");
        assert!(e.contains("brew install nothing"), "{e}");
        assert!(e.contains("pando never installs an engine"), "{e}");

        // And once the shim is there, nothing is missing — which is also
        // the proof that the probe looks at pando's own bin directory.
        shim(&fx.paths, "fake-init", "exit 0\n");
        assert!(native.missing_binaries().is_empty());
    }

    // ---- readiness -------------------------------------------------------

    /// A port with a listener on it, held for as long as the guard lives.
    /// Enough for the readiness gate, which only asks whether *something*
    /// is behind the port before it runs the recipe's own check.
    fn listening() -> (TcpListener, u16) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    const READY_RECIPE: &str = "kind = \"service\"\nname = \"r\"\n\n[service]\n\
                                cmd = \"exec sleep 300\"\nready = \"fake-ready\"\n\
                                ready_timeout_s = 3\n";

    #[test]
    fn a_service_is_ready_when_its_own_check_says_so() {
        let fx = fixture();
        let (_held, port) = listening();
        shim(&fx.paths, "fake-ready", "exit 0\n");
        let native = plan(&fx, READY_RECIPE, port);
        native.ensure_init(QUIET).unwrap();
        let spawned = native.spawn().unwrap();
        let _guard = crate::testutil::Detached {
            pid: spawned.pid,
            pgid: spawned.pgid,
        };
        native
            .wait_ready(spawned.pid, native.ready_timeout(), QUIET)
            .unwrap();
        std::fs::remove_dir_all(&native.socket_dir).ok();
    }

    #[test]
    fn a_port_that_answers_is_not_ready_while_the_recipes_own_check_refuses() {
        let fx = fixture();
        let (_held, port) = listening();
        shim(
            &fx.paths,
            "fake-ready",
            "echo 'FATAL: the database system is starting up' >&2\nexit 2\n",
        );
        let native = plan(&fx, READY_RECIPE, port);
        native.ensure_init(QUIET).unwrap();
        let spawned = native.spawn().unwrap();
        let _guard = crate::testutil::Detached {
            pid: spawned.pid,
            pgid: spawned.pgid,
        };
        let e = format!(
            "{:#}",
            native
                .wait_ready(spawned.pid, Duration::from_millis(600), QUIET)
                .unwrap_err()
        );
        assert!(e.contains("did not become ready"), "{e}");
        // The reason the check gave, not just the fact that it failed.
        assert!(e.contains("the database system is starting up"), "{e}");
        std::fs::remove_dir_all(&native.socket_dir).ok();
    }

    #[test]
    fn a_server_that_exits_fails_the_wait_at_once_rather_than_at_the_deadline() {
        let fx = fixture();
        let native = plan(
            &fx,
            "kind = \"service\"\nname = \"r\"\n\n[service]\n\
             cmd = \"echo 'could not bind IPv4 address' >&2; exit 1\"\nready = \"true\"\n",
            17_499,
        );
        native.ensure_init(QUIET).unwrap();
        let spawned = native.spawn().unwrap();
        let _guard = crate::testutil::Detached {
            pid: spawned.pid,
            pgid: spawned.pgid,
        };
        let began = Instant::now();
        let e = format!(
            "{:#}",
            native
                .wait_ready(spawned.pid, Duration::from_secs(30), QUIET)
                .unwrap_err()
        );
        assert!(e.contains("exited before it was ready"), "{e}");
        assert!(e.contains("pando logs feat+one --source db"), "{e}");
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "it waited it out"
        );
        // And what it printed is in the log, which is what the sentence
        // just told the developer to read.
        let log = std::fs::read_to_string(&native.log_file).unwrap();
        assert!(log.contains("could not bind IPv4 address"), "{log}");
        std::fs::remove_dir_all(&native.socket_dir).ok();
    }

    #[test]
    fn nothing_listening_fails_the_wait_without_ever_running_the_check() {
        let fx = fixture();
        // A check that would pass, behind a port nothing is on: the gate
        // is what decides, and it decides without spawning a login shell
        // four times a second.
        shim(
            &fx.paths,
            "fake-ready",
            "echo asked >> \"$0.asked\"\nexit 0\n",
        );
        let native = plan(&fx, READY_RECIPE, 17_498);
        native.ensure_init(QUIET).unwrap();
        let spawned = native.spawn().unwrap();
        let _guard = crate::testutil::Detached {
            pid: spawned.pid,
            pgid: spawned.pgid,
        };
        let e = format!(
            "{:#}",
            native
                .wait_ready(spawned.pid, Duration::from_millis(600), QUIET)
                .unwrap_err()
        );
        assert!(e.contains("nothing is listening on port 17498"), "{e}");
        assert!(!fx.paths.home.join("bin/fake-ready.asked").exists());
        std::fs::remove_dir_all(&native.socket_dir).ok();
    }

    #[test]
    fn a_service_with_no_readiness_check_is_ready_once_something_answers() {
        let fx = fixture();
        let (_held, port) = listening();
        let native = plan(
            &fx,
            "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"exec sleep 300\"\n",
            port,
        );
        native.ensure_init(QUIET).unwrap();
        let spawned = native.spawn().unwrap();
        let _guard = crate::testutil::Detached {
            pid: spawned.pid,
            pgid: spawned.pgid,
        };
        native
            .wait_ready(spawned.pid, Duration::from_secs(5), QUIET)
            .unwrap();
        std::fs::remove_dir_all(&native.socket_dir).ok();
    }

    // ---- create ----------------------------------------------------------

    #[test]
    fn the_create_step_runs_with_the_apps_own_database_and_reports_its_failure() {
        let fx = fixture();
        let asked = fx.paths.home.join("asked");
        shim(
            &fx.paths,
            "fake-create",
            &format!(
                "echo \"$@\" >> {}\n",
                shell_quote(&asked.display().to_string())
            ),
        );
        let native = plan(
            &fx,
            "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n\
             create = \"fake-create {db_name} {db_user}\"\n",
            17_400,
        );
        native.ensure_init(QUIET).unwrap();
        native.create(QUIET).unwrap();
        // What the app's own URL named, not what the data directory was
        // initialised with.
        assert_eq!(
            std::fs::read_to_string(&asked).unwrap().trim(),
            "acme_dev app"
        );

        // And a failing create is reported with the line the engine gave,
        // rather than an exit code.
        shim(
            &fx.paths,
            "fake-create",
            "echo 'createdb: error: no such role' >&2\nexit 1\n",
        );
        let e = format!("{:#}", native.create(QUIET).unwrap_err());
        assert!(e.contains("no such role"), "{e}");
        assert!(e.contains("\"db\""), "{e}");
    }

    #[test]
    fn a_recipe_with_no_create_step_does_nothing_at_all() {
        let fx = fixture();
        let native = plan(
            &fx,
            "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n",
            17_400,
        );
        native.create(QUIET).unwrap();
    }

    // ---- a language recipe is not a service ------------------------------

    #[test]
    fn a_language_recipe_in_a_services_entry_is_refused_by_name() {
        let fx = fixture();
        let native = Native::plan(
            &fx.paths,
            "feat+one",
            "db",
            recipe("kind = \"language\"\nname = \"node\"\n\n[language]\nmanagers = [\"nvm\"]\n"),
            17_400,
            None,
        )
        .unwrap();
        let e = format!("{:#}", native.ensure_init(QUIET).unwrap_err());
        assert!(e.contains("language recipe"), "{e}");
        assert!(e.contains("starts a server"), "{e}");
    }

    #[test]
    fn a_recipe_that_only_knows_a_namespace_in_a_services_entry_is_refused_by_name() {
        let fx = fixture();
        let native = Native::plan(
            &fx.paths,
            "feat+one",
            "warehouse",
            recipe(
                "kind = \"service\"\nname = \"ch\"\n\n[namespace]\nkind = \"database\"\n\
                 ping = \"p\"\ndrop = \"d\"\ncreate = \"c\"\nexists = \"e\"\n",
            ),
            17_400,
            None,
        )
        .unwrap();
        let e = format!("{:#}", native.ensure_init(QUIET).unwrap_err());
        assert!(e.contains("run it from the compose file"), "{e}");
        assert!(e.contains("starts a server"), "{e}");
    }

    #[test]
    fn every_built_in_recipe_renders_into_commands_with_no_placeholders_left() {
        let fx = fixture();
        // The trap this catches: `{ping: 1}` in a mongosh `--eval` is a
        // *placeholder* to pando's own lexer, so a recipe that writes it
        // unescaped fails at the readiness check of an engine nobody
        // here can run — which is to say, in front of a user.
        for (name, loaded) in Recipes::built_in().entries() {
            // One that starts no server has no command of its own to render.
            if loaded.recipe.service().is_none() {
                continue;
            }
            let recipe = Recipes::built_in().get(name).unwrap().recipe.clone();
            let native = Native::plan(
                &fx.paths,
                "feat+one",
                name,
                recipe,
                17_402,
                Some("scheme://app:secret@localhost:17402/acme_dev"),
            )
            .unwrap();
            let service = native.recipe.service().unwrap().clone();
            for command in [
                Some(service.cmd.clone()),
                service.init.clone(),
                service.ready.clone(),
                service.create.clone(),
            ]
            .into_iter()
            .flatten()
            {
                // Rendering at all is most of the claim: the lexer
                // refuses a placeholder it does not know, so a recipe
                // that writes `{ping:1}` in a mongosh `--eval` fails
                // here rather than in front of whoever has mongod.
                let rendered = native
                    .shell_cmd(&command)
                    .unwrap_or_else(|e| panic!("{name}: {command}: {e:#}"));
                for known in KNOWN {
                    assert!(
                        !rendered.contains(&format!("{{{known}}}")),
                        "{name} left {{{known}}} unresolved: {rendered}"
                    );
                }
                // Braces that survive are the escaped kind — `{{` and
                // `}}` render to one brace, which is how a recipe writes
                // a JSON literal — so they are not asserted away.
            }
            // The one placeholder every engine's own command must carry:
            // a server started on a port pando did not allocate is a
            // worktree sharing a database with its neighbour.
            let cmd = native.shell_cmd(&service.cmd).unwrap();
            assert!(cmd.contains("17402"), "{name} ignores its port: {cmd}");
        }
    }

    #[test]
    fn the_built_in_postgres_recipe_renders_the_port_the_socket_and_the_app_database() {
        let fx = fixture();
        let postgres = Recipes::built_in().get("postgres").unwrap().recipe.clone();
        let native = Native::plan(
            &fx.paths,
            "feat+one",
            "postgres",
            postgres,
            17_402,
            Some("postgres://app:secret@localhost:17402/acme_dev"),
        )
        .unwrap();
        let service = native.recipe.service().unwrap().clone();
        let cmd = native.shell_cmd(&service.cmd).unwrap();
        assert!(cmd.contains("-p 17402"), "{cmd}");
        assert!(cmd.contains("listen_addresses=127.0.0.1"), "{cmd}");
        assert!(
            cmd.contains(&format!(
                "-k {}",
                shell_quote(&native.socket_dir.display().to_string())
            )),
            "{cmd}"
        );
        let create = native.shell_cmd(service.create.as_ref().unwrap()).unwrap();
        assert!(create.contains("acme_dev"), "{create}");
        assert!(create.contains("app"), "{create}");
    }

    #[test]
    fn a_create_step_is_skipped_for_a_service_nothing_in_the_project_addresses() {
        let fx = fixture();
        let asked = fx.paths.home.join("asked");
        shim(
            &fx.paths,
            "fake-create",
            &format!(
                "echo ran >> {}\n",
                shell_quote(&asked.display().to_string())
            ),
        );
        // MariaDB's shape: a `create` that needs the app's database name,
        // and a recipe with no default for it.
        let native = Native::plan(
            &fx.paths,
            "feat+one",
            "db",
            recipe(
                "kind = \"service\"\nname = \"r\"\n\n[service]\ncmd = \"x\"\n\
                 create = \"fake-create {db_name}\"\n",
            ),
            17_400,
            None,
        )
        .unwrap();
        native.ensure_init(QUIET).unwrap();
        let said = std::cell::RefCell::new(Vec::<String>::new());
        native
            .create(&|line: &str| said.borrow_mut().push(line.to_string()))
            .unwrap();
        assert!(!asked.exists(), "it made a database for nobody");
        let said = said.into_inner();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(
            said[0].contains("nothing in this project addresses it"),
            "{said:?}"
        );
    }
}
