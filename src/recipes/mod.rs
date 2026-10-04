//! Recipes: what pando knows about a thing it did not write.
//!
//! A recipe is **data**. For a service it says what initialises a data
//! directory, what starts the server on a port pando allocated, and how to
//! tell when it is ready; for a language it says which files pin it and
//! which version managers can satisfy it. Built-ins are compiled into the
//! binary as TOML text and go through exactly the same parser as a file in
//! `~/.pando/recipes/`, so a built-in has no privilege a developer's own
//! file does not: dropping in `postgres.toml` replaces the Postgres recipe
//! outright.
//!
//! **One loader, two kinds of recipe.** Every recipe shares an envelope —
//! `kind`, `name`, `aliases`, the binaries it needs on PATH, how to ask one
//! for its version, how to install it, a note worth printing — and carries
//! one body table, `[service]` or `[language]`. The language body is the
//! shape of [`crate::runtime::LANGUAGES`], which is a `const` table today
//! and was deliberately built to be loadable from disk later. Nothing here
//! migrates it; a round-trip test in this module is the proof that the
//! format could, and that "one loader" is not a slogan.
//!
//! Nothing in this module runs anything. It reads TOML and hands back
//! structs; `native.rs` is what spawns, and `doctor.rs` is what reports.

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Which body a recipe carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Service,
    Language,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Service => "service",
            Kind::Language => "language",
        }
    }
}

/// How to start, wait for, and address one server.
///
/// Every field is a shell command run through `bash -lc`, with the
/// placeholders [`crate::native`] documents. `cmd` is the only one that is
/// required: a server pando cannot start is not a service recipe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ServiceRecipe {
    /// Starts the server in the foreground, on `{port}`. Detached by
    /// pando, so it must not daemonise itself: a recipe that forks leaves
    /// pando holding the pid of something that has already exited.
    pub cmd: String,
    /// Initialises `{datadir}`. Runs once, and never against a data
    /// directory that already holds data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub init: Option<String>,
    /// Runs after every readiness, and has to be idempotent: it is where a
    /// recipe creates the role and database the app's own URL names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<String>,
    /// Answers "is it up?" with its exit status. Omitted means a TCP
    /// connect, which only proves something is behind the port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_timeout_s: Option<u64>,
    /// The environment key an app usually reads to find this service, used
    /// when the `[[services]]` block names no `env` map of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port_env: Option<String>,
    /// What `{db_user}` means when the app's own URL names no user, and
    /// what `{db_name}` means when it names no database. Recipe data
    /// rather than adapter policy: `postgres`/`postgres` is true of
    /// Postgres and of nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_name: Option<String>,
}

/// How a worktree gets a namespace of its own inside a server the main
/// checkout already runs: `start --namespaced`.
///
/// Every command runs through `bash -lc` with pando's own `bin` ahead on
/// PATH, as a service recipe's do, and sees `{host}`, `{port}` and
/// `{user}` — the main checkout's server and login, shell-quoted — and
/// `{namespace}`, the database's name or the slot's number, which pando
/// has already held to a plain identifier. The password is never a
/// placeholder: it reaches the client in the variable `password_env`
/// names, and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespaceRecipe {
    /// A `database` is made and dropped by name; a `slot` is a number the
    /// server always has, taken only while empty and emptied to free it.
    pub kind: crate::state::NamespaceKind,
    /// What has to be on PATH for any of this: the engine's client, not
    /// its server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binaries: Vec<String>,
    /// How to get the client alone, said when it is missing: a server in
    /// Docker leaves the host with no client at all, and the package that
    /// has the server is more than the client needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<String>,
    /// Whether the engine logs in as somebody. MariaDB does; a
    /// development Redis asks for a password, if that.
    #[serde(default)]
    pub user: bool,
    /// The variable the client reads a password from: `MYSQL_PWD`,
    /// `REDISCLI_AUTH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_env: Option<String>,
    /// Answers "does the server take this login?" with its exit status.
    pub ping: String,
    /// A `database`'s: prints its name when the server has it, nothing
    /// when it does not, and fails when it cannot be asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<String>,
    /// A `database`'s: prints the name of every database on the server
    /// whose name matches `{prefix_like}`, one a line — what `doctor`
    /// reads to find a worktree's database no record holds any more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list: Option<String>,
    /// A `database`'s: makes it, and **fails when it is there already**,
    /// so one pando did not make is never taken for one it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<String>,
    /// Drops a database, or empties a slot. A slot's must fail rather than
    /// fall back to another slot when this one cannot be selected —
    /// `redis-cli -n` does fall back, onto slot 0.
    pub drop: String,
    /// A `slot`'s: prints how many keys it holds, failing the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// A `slot`'s: how many the server has. 0 is the main checkout's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slots: Option<u32>,
    /// A line of stderr that means the login may not make or drop this
    /// namespace — what turns a failure into the grant below.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied: Vec<String>,
    /// Prints the account the server knows the login as, `user@host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    /// A `database`'s: the longest name the engine keeps as given, when it
    /// is shorter than [`crate::namespace::MAX_NAME`] — Postgres's 63.
    /// Longer than that is refused when the recipe is read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_name: Option<usize>,
    /// What an administrator runs, once, so the login may make and drop
    /// namespaces under this worktree's prefix and nothing else. Printed,
    /// never run. Sees `{prefix_like}` — the prefix as an SQL `LIKE`
    /// pattern — and `{account_user}`, `{account_host}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
}

impl NamespaceRecipe {
    /// The longest database name this engine keeps as given.
    pub fn max_name(&self) -> usize {
        self.max_name.unwrap_or(crate::namespace::MAX_NAME)
    }

    /// The shape a kind needs, or what is missing from it.
    fn check(&self) -> Result<()> {
        use crate::state::NamespaceKind;
        let (needs, kind): (&[(&str, bool)], &str) = match self.kind {
            NamespaceKind::Database => (
                &[
                    ("exists", self.exists.is_some()),
                    ("create", self.create.is_some()),
                ],
                "database",
            ),
            NamespaceKind::Slot => (
                &[
                    ("size", self.size.is_some()),
                    ("slots", self.slots.is_some_and(|n| n > 1)),
                ],
                "slot",
            ),
        };
        for (key, present) in needs {
            if !present {
                bail!("a `kind = \"{kind}\"` [namespace] needs `{key}`");
            }
        }
        if self.ping.trim().is_empty() || self.drop.trim().is_empty() {
            bail!("a [namespace] needs `ping` and `drop`");
        }
        // Room for `<main>__`, a hash, and one character of the worktree's
        // name, and no more than the guard on every drop allows.
        if let Some(max) = self.max_name
            && !(16..=crate::namespace::MAX_NAME).contains(&max)
        {
            bail!(
                "a [namespace]'s `max_name` is from 16 to {}, not {max}",
                crate::namespace::MAX_NAME
            );
        }
        Ok(())
    }
}

/// One file that pins a language, in the shape [`crate::runtime::Source`]
/// already has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeSource {
    pub file: String,
    /// The path to the value inside a TOML file, as
    /// `rust-toolchain.toml`'s `["toolchain", "channel"]`. Empty means the
    /// whole file is the version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toml_key: Vec<String>,
}

/// What a project says when it pins a language, and who can satisfy it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LanguageRecipe {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<RecipeSource>,
    /// The `package.json` `engines` key that describes it, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engines_key: Option<String>,
    /// Version managers that can satisfy it, by name, in the order a
    /// question should offer them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    Service(ServiceRecipe),
    Language(LanguageRecipe),
}

/// One recipe, envelope and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipe {
    pub kind: Kind,
    pub name: String,
    /// Other names this recipe answers to. `.tool-versions` calls node
    /// `nodejs`; a developer may well call MariaDB `mysql`.
    pub aliases: Vec<String>,
    /// One line for a report.
    pub summary: Option<String>,
    /// What has to be on PATH, in the order to try them. The first one is
    /// what a version probe asks.
    pub binaries: Vec<String>,
    /// What to pass a binary to make it print its version.
    pub version_flag: Option<String>,
    /// How a developer installs it. **Printed, never run** — pando does
    /// not install an engine.
    pub install: Option<String>,
    /// Something true about this recipe that a developer should know
    /// without reading it: Postgres's trust authentication, for one.
    pub notes: Option<String>,
    /// How a worktree gets a namespace of its own in the main checkout's
    /// server of this kind, when it can. A service recipe's only.
    pub namespace: Option<NamespaceRecipe>,
    /// Whether this recipe has ever been run against a real server.
    ///
    /// pando ships recipes for engines nobody on the project had
    /// installed. Shipping one and implying it is proven would be worse
    /// than not shipping it: the whole point of a recipe is that a
    /// developer can fix it, and they can only do that if they know which
    /// one to suspect.
    pub untested: bool,
    pub body: Body,
}

impl Recipe {
    pub fn service(&self) -> Option<&ServiceRecipe> {
        match &self.body {
            Body::Service(s) => Some(s),
            Body::Language(_) => None,
        }
    }

    /// The service body, to be overridden by what a `[[services]]` entry
    /// says inline.
    pub fn service_mut(&mut self) -> Option<&mut ServiceRecipe> {
        match &mut self.body {
            Body::Service(s) => Some(s),
            Body::Language(_) => None,
        }
    }

    pub fn language(&self) -> Option<&LanguageRecipe> {
        match &self.body {
            Body::Language(l) => Some(l),
            Body::Service(_) => None,
        }
    }

    /// The binary a version probe asks, which is the first one listed.
    pub fn version_cmd(&self) -> Option<String> {
        let binary = self.binaries.first()?;
        let flag = self.version_flag.as_deref()?;
        Some(format!("{binary} {flag}"))
    }
}

/// Where a recipe came from, which is the first thing a developer asks
/// when one behaves unexpectedly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Compiled into this build.
    BuiltIn,
    /// A file the developer put in the recipes directory.
    User(PathBuf),
}

impl Origin {
    /// `built-in`, or the path of the file that replaced it.
    pub fn describe(&self) -> String {
        match self {
            Origin::BuiltIn => "built-in".to_string(),
            Origin::User(path) => path.display().to_string(),
        }
    }

    pub fn is_user(&self) -> bool {
        matches!(self, Origin::User(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub recipe: Recipe,
    pub origin: Origin,
    /// Whether a built-in of the same name was replaced by this file.
    pub replaces_built_in: bool,
}

/// A user file that did not parse. Kept rather than dropped: the name it
/// claimed is *unusable*, not quietly served by the built-in it was
/// meant to replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub path: PathBuf,
    pub error: String,
}

/// Every recipe this build and this machine have, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recipes {
    entries: BTreeMap<String, Loaded>,
    broken: BTreeMap<String, Broken>,
}

impl Recipes {
    /// The recipes compiled into this build, with nothing read from disk.
    pub fn built_in() -> Recipes {
        let mut entries = BTreeMap::new();
        for (name, text) in BUILT_IN {
            // A built-in that does not parse is a bug in this build, not a
            // developer's problem, and `every_built_in_recipe_parses`
            // fails the commit that introduces one. Skipped rather than
            // panicked over, so a bad build still runs `pando ls`.
            match parse(text) {
                Ok(recipe) if recipe.name == name => {
                    entries.insert(
                        name.to_string(),
                        Loaded {
                            recipe,
                            origin: Origin::BuiltIn,
                            replaces_built_in: false,
                        },
                    );
                }
                _ => continue,
            }
        }
        Recipes {
            entries,
            broken: BTreeMap::new(),
        }
    }

    /// The built-ins with the developer's own recipes merged over them.
    ///
    /// A file's *stem* is the name it claims, so "drop in a file of the
    /// same name and it replaces the built-in" is literally true. A file
    /// whose `name` disagrees with its stem is refused naming both, rather
    /// than silently registering under one of them.
    pub fn load(dir: &Path) -> Recipes {
        let mut recipes = Recipes::built_in();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return recipes;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
            .filter(|path| {
                !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('.'))
            })
            .collect();
        // Sorted, so two files cannot swap which of them was read last
        // between runs on filesystems that do not order a directory.
        files.sort();
        for path in files {
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            match read(&path, &stem) {
                Ok(recipe) => {
                    let replaces_built_in = BUILT_IN.iter().any(|(name, _)| *name == stem);
                    recipes.broken.remove(&stem);
                    recipes.entries.insert(
                        stem,
                        Loaded {
                            recipe,
                            origin: Origin::User(path),
                            replaces_built_in,
                        },
                    );
                }
                Err(e) => {
                    // The built-in is *not* used in its place. A developer
                    // who edits `postgres.toml` and mistypes one line must
                    // not have pando quietly run the built-in against the
                    // data directory their own recipe made.
                    recipes.entries.remove(&stem);
                    recipes.broken.insert(
                        stem,
                        Broken {
                            path,
                            error: format!("{e:#}"),
                        },
                    );
                }
            }
        }
        recipes
    }

    /// The recipe `name` asks for, or an error naming the ones there are.
    pub fn get(&self, name: &str) -> Result<&Loaded> {
        if let Some(loaded) = self.entries.get(name) {
            return Ok(loaded);
        }
        if let Some(broken) = self.broken.get(name) {
            bail!(
                "the recipe {name:?} is in {} and does not load: {} — fix that file, or delete \
                 it to go back to the built-in",
                broken.path.display(),
                broken.error
            );
        }
        // An alias is a second chance before the refusal, not a first
        // lookup: a file named `mysql.toml` beats a built-in that merely
        // answers to `mysql`.
        if let Some(loaded) = self
            .entries
            .values()
            .find(|l| l.recipe.aliases.iter().any(|a| a == name))
        {
            return Ok(loaded);
        }
        bail!(
            "there is no recipe named {name:?} — this build knows {}",
            self.listed()
        )
    }

    /// The recipe registered under exactly this name, with no alias
    /// lookup and no error for a name nothing has.
    ///
    /// What "is there a recipe called this?" means when the answer decides
    /// whether a `[[services]]` entry is running a recipe at all.
    pub fn exact(&self, name: &str) -> Option<&Loaded> {
        self.entries.get(name)
    }

    /// Whether a file claiming this name is there and does not load. A
    /// name in this state is *unusable*, which is not the same as absent.
    pub fn is_broken(&self, name: &str) -> bool {
        self.broken.contains_key(name)
    }

    /// Every name, built-in and user, alphabetically.
    pub fn names(&self) -> Vec<&str> {
        self.entries.keys().map(String::as_str).collect()
    }

    /// The names of every recipe that starts a server.
    pub fn service_names(&self) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(_, l)| l.recipe.kind == Kind::Service)
            .map(|(name, _)| name.as_str())
            .collect()
    }

    pub fn entries(&self) -> impl Iterator<Item = (&str, &Loaded)> {
        self.entries.iter().map(|(name, l)| (name.as_str(), l))
    }

    pub fn broken(&self) -> impl Iterator<Item = (&str, &Broken)> {
        self.broken.iter().map(|(name, b)| (name.as_str(), b))
    }

    fn listed(&self) -> String {
        if self.entries.is_empty() {
            return "none".to_string();
        }
        self.names().join(", ")
    }
}

/// Reads one recipe file, checking that it claims the name its file does.
fn read(path: &Path, stem: &str) -> Result<Recipe> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let recipe = parse(&text)?;
    if recipe.name != stem {
        bail!(
            "it is called {:?} inside and {stem:?} by its file name — a recipe is found by its \
             file name, so rename one of them",
            recipe.name
        );
    }
    Ok(recipe)
}

/// The one parser, for a built-in string and a developer's file alike.
pub fn parse(text: &str) -> Result<Recipe> {
    let raw: RawRecipe = toml::from_str(text).context("parse the recipe")?;
    if raw.name.trim().is_empty() {
        bail!("a recipe needs a `name`");
    }
    if raw.name.contains(['/', '\\']) || raw.name == "." || raw.name == ".." {
        bail!(
            "the recipe name {:?} is not a name — it is what a file is called and what a \
             `[[services]] preset` says",
            raw.name
        );
    }
    if let Some(namespace) = &raw.namespace {
        if raw.kind != Kind::Service {
            bail!("a [namespace] belongs to a `kind = \"service\"` recipe");
        }
        namespace.check()?;
    }
    let body = match raw.kind {
        Kind::Service => {
            if raw.language.is_some() {
                bail!("a `kind = \"service\"` recipe may not carry a `[language]` table");
            }
            let service = raw
                .service
                .context("a `kind = \"service\"` recipe needs a `[service]` table")?;
            if service.cmd.trim().is_empty() {
                bail!("`[service] cmd` is what starts the server, and it must not be empty");
            }
            Body::Service(service)
        }
        Kind::Language => {
            if raw.service.is_some() {
                bail!("a `kind = \"language\"` recipe may not carry a `[service]` table");
            }
            Body::Language(
                raw.language
                    .context("a `kind = \"language\"` recipe needs a `[language]` table")?,
            )
        }
    };
    Ok(Recipe {
        kind: raw.kind,
        name: raw.name,
        aliases: raw.aliases,
        summary: raw.summary,
        binaries: raw.binaries,
        version_flag: raw.version_flag,
        install: raw.install,
        notes: raw.notes,
        namespace: raw.namespace,
        untested: raw.untested,
        body,
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRecipe {
    kind: Kind,
    name: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    binaries: Vec<String>,
    #[serde(default)]
    version_flag: Option<String>,
    #[serde(default)]
    install: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    untested: bool,
    #[serde(default)]
    service: Option<ServiceRecipe>,
    #[serde(default)]
    language: Option<LanguageRecipe>,
    #[serde(default)]
    namespace: Option<NamespaceRecipe>,
}

/// The recipes this build ships, as `(file name, TOML text)`.
///
/// Each one is a file in `src/recipes/builtin/`, written in exactly the
/// format a developer drops into `~/.pando/recipes/`, and the comment at
/// the top of each says what in it is deliberate. Adding a built-in is a
/// file there and a row here.
///
/// Three of the four were run against a real server while they were
/// written; MongoDB was not, because no machine here has `mongod`, and
/// it says so in its own `untested` field rather than in a comment.
pub const BUILT_IN: [(&str, &str); 4] = [
    ("mariadb", include_str!("builtin/mariadb.toml")),
    ("mongodb", include_str!("builtin/mongodb.toml")),
    ("postgres", include_str!("builtin/postgres.toml")),
    ("redis", include_str!("builtin/redis.toml")),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), text).unwrap();
    }

    const MINIMAL: &str = "kind = \"service\"\nname = \"tiny\"\n\n[service]\ncmd = \"sleep 1\"\n";

    #[test]
    fn every_built_in_recipe_parses_and_claims_its_own_file_name() {
        for (name, text) in BUILT_IN {
            let recipe = parse(text).unwrap_or_else(|e| panic!("built-in {name}: {e:#}"));
            assert_eq!(
                recipe.name, name,
                "built-in {name} calls itself something else"
            );
        }
        let recipes = Recipes::built_in();
        let names = recipes.names();
        for expected in ["mariadb", "mongodb", "postgres", "redis"] {
            assert!(names.contains(&expected), "{names:?} has no {expected}");
        }
        // Every one of them starts a server; a language recipe that got
        // into this list would be a build that ships something no
        // `[[services]]` entry could ever run.
        assert_eq!(recipes.service_names(), names);
    }

    /// Rules every shipped service recipe is held to, whichever engine it
    /// is for. Written as one test over the table rather than four
    /// near-copies: the next recipe anyone adds is held to them for free,
    /// which is the whole reason recipes are data.
    #[test]
    fn every_built_in_service_recipe_is_private_to_this_machine() {
        for (name, text) in BUILT_IN {
            let recipe = parse(text).unwrap();
            let service = recipe
                .service()
                .unwrap_or_else(|| panic!("{name} is not a service"));
            let cmd = &service.cmd;
            // Loopback, spelled out. A default is not good enough: most
            // engines default to every interface, and one that does not
            // may still resolve `localhost` to `::1` as well.
            assert!(
                cmd.contains("127.0.0.1"),
                "{name} does not bind loopback explicitly: {cmd}"
            );
            assert!(
                !cmd.contains("0.0.0.0"),
                "{name} binds every interface: {cmd}"
            );
            // The server pando records the pid of has to be the server.
            assert!(cmd.starts_with("exec "), "{name} does not exec: {cmd}");
            // The socket trap, for every engine rather than only the one
            // it was found on: a Unix socket inside the data directory is
            // a path that will not fit in `sun_path`.
            assert!(
                !cmd.contains("{datadir}/mysql.sock") && !cmd.contains("-k {datadir}"),
                "{name} puts its socket in the data directory: {cmd}"
            );
            // Every recipe says what it needs and how to get it, because
            // a missing engine is a sentence naming what to install.
            assert!(!recipe.binaries.is_empty(), "{name} names no binaries");
            assert!(recipe.install.is_some(), "{name} has no install hint");
            assert!(recipe.notes.is_some(), "{name} says nothing about its auth");
            assert!(recipe.version_cmd().is_some(), "{name} cannot be versioned");
        }
    }

    /// Rules every shipped `[namespace]` is held to, whichever engine it is
    /// for: these commands run against a developer's own server.
    #[test]
    fn every_built_in_namespace_keeps_its_password_off_the_command_line_and_its_slot_exact() {
        use crate::state::NamespaceKind;
        let mut namespaced: Vec<&str> = Vec::new();
        for (name, text) in BUILT_IN {
            let recipe = parse(text).unwrap();
            let Some(ns) = &recipe.namespace else {
                continue;
            };
            namespaced.push(name);
            let commands: Vec<&str> = [
                Some(ns.ping.as_str()),
                ns.exists.as_deref(),
                ns.create.as_deref(),
                Some(ns.drop.as_str()),
                ns.size.as_deref(),
                ns.account.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect();
            for command in &commands {
                // The client reads it from the environment or not at all.
                assert!(!command.contains("{password}"), "{name}: {command}");
                assert!(!command.contains(" -a "), "{name}: {command}");
                assert!(!command.contains(" -p{"), "{name}: {command}");
            }
            assert!(
                ns.password_env.is_some(),
                "{name} names no password variable"
            );
            assert!(!ns.binaries.is_empty(), "{name} names no client");
            assert!(
                ns.install.is_some(),
                "{name} says not how to get its client"
            );
            match ns.kind {
                // A database already there is not one pando made.
                NamespaceKind::Database => {
                    let create = ns.create.as_deref().unwrap();
                    assert!(
                        !create.to_uppercase().contains("IF NOT EXISTS"),
                        "{name}: {create}"
                    );
                    assert!(ns.grant.is_some() && !ns.denied.is_empty(), "{name}");
                }
                // `-n` falls back to slot 0 when the slot cannot be
                // selected; every command naming a slot must fail instead.
                NamespaceKind::Slot => {
                    for command in [ns.drop.as_str(), ns.size.as_deref().unwrap()] {
                        assert!(!command.contains(" -n "), "{name}: {command}");
                        assert!(command.contains("{namespace}"), "{name}: {command}");
                    }
                    assert!(ns.slots.is_some_and(|n| n > 1), "{name}");
                }
            }
        }
        assert_eq!(
            namespaced,
            vec!["mariadb", "redis"],
            "which engines can namespace changed"
        );
    }

    #[test]
    fn a_namespace_table_is_held_to_what_its_kind_needs() {
        let service = "kind = \"service\"\nname = \"x\"\n\n[service]\ncmd = \"c\"\n\n";
        for (table, says) in [
            (
                "[namespace]\nkind = \"database\"\nping = \"p\"\ndrop = \"d\"\ncreate = \"c\"\n",
                "needs `exists`",
            ),
            (
                "[namespace]\nkind = \"slot\"\nping = \"p\"\ndrop = \"d\"\nslots = 16\n",
                "needs `size`",
            ),
            (
                "[namespace]\nkind = \"slot\"\nping = \"p\"\ndrop = \"d\"\nsize = \"s\"\nslots = 1\n",
                "needs `slots`",
            ),
            (
                "[namespace]\nkind = \"slot\"\nping = \"\"\ndrop = \"d\"\nsize = \"s\"\nslots = 4\n",
                "needs `ping` and `drop`",
            ),
            (
                "[namespace]\nkind = \"database\"\nping = \"p\"\ndrop = \"d\"\ncreate = \"c\"\n\
                 exists = \"e\"\nmax_name = 128\n",
                "`max_name` is from 16 to 64, not 128",
            ),
        ] {
            let e = format!("{:#}", parse(&format!("{service}{table}")).unwrap_err());
            assert!(e.contains(says), "{says}: {e}");
        }
        let e = format!(
            "{:#}",
            parse(
                "kind = \"language\"\nname = \"x\"\n\n[language]\nmanagers = [\"mise\"]\n\n\
                 [namespace]\nkind = \"slot\"\nping = \"p\"\ndrop = \"d\"\nsize = \"s\"\nslots = 4\n"
            )
            .unwrap_err()
        );
        assert!(e.contains("belongs to a `kind = \"service\"`"), "{e}");
    }

    /// An engine nobody here could run says so in the one place that is
    /// data rather than prose.
    #[test]
    fn a_recipe_no_one_has_run_against_a_real_server_says_so() {
        let recipes = Recipes::built_in();
        let untested: Vec<&str> = recipes
            .entries()
            .filter(|(_, loaded)| loaded.recipe.untested)
            .map(|(name, _)| name)
            .collect();
        assert_eq!(
            untested,
            vec!["mongodb"],
            "which recipes are unproven changed"
        );
        let mongo = recipes.get("mongodb").unwrap();
        assert!(
            mongo
                .recipe
                .notes
                .as_deref()
                .unwrap()
                .contains("never been run"),
            "and the note has to say it too: {:?}",
            mongo.recipe.notes
        );
    }

    #[test]
    fn the_postgres_recipe_says_how_to_init_start_wait_and_address_it() {
        let loaded = Recipes::built_in().get("postgres").unwrap().clone();
        assert_eq!(loaded.origin, Origin::BuiltIn);
        let service = loaded.recipe.service().expect("a service recipe");
        assert!(service.init.as_deref().unwrap().contains("initdb"));
        assert!(service.cmd.contains("{port}"), "{}", service.cmd);
        // The whole point of the socket directory: never inside the data
        // directory, which is long enough to overflow `sun_path`.
        assert!(service.cmd.contains("-k {socket_dir}"), "{}", service.cmd);
        assert!(!service.cmd.contains("-k {datadir}"), "{}", service.cmd);
        assert!(service.ready.as_deref().unwrap().contains("pg_isready"));
        assert_eq!(service.port_env.as_deref(), Some("DATABASE_URL"));
        assert_eq!(
            loaded.recipe.version_cmd().as_deref(),
            Some("postgres --version")
        );
        // Printed, never run — but it has to exist, because a missing
        // engine is a sentence naming what to install.
        assert!(
            loaded
                .recipe
                .install
                .as_deref()
                .unwrap()
                .contains("postgresql")
        );
        assert!(loaded.recipe.notes.as_deref().unwrap().contains("trust"));
    }

    #[test]
    fn a_user_file_replaces_the_built_in_of_the_same_name() {
        let dir = tempfile::tempdir().unwrap();
        let recipes = Recipes::load(dir.path());
        assert!(!recipes.get("postgres").unwrap().origin.is_user());

        write(
            dir.path(),
            "postgres.toml",
            "kind = \"service\"\nname = \"postgres\"\n\n[service]\ncmd = \"mine -p {port}\"\n",
        );
        let recipes = Recipes::load(dir.path());
        let loaded = recipes.get("postgres").unwrap();
        assert_eq!(
            loaded.origin,
            Origin::User(dir.path().join("postgres.toml"))
        );
        assert!(loaded.replaces_built_in);
        assert_eq!(loaded.recipe.service().unwrap().cmd, "mine -p {port}");
        // Replaced outright, not merged: the built-in's initdb line is
        // gone, because a recipe is one file and not a patch on another.
        assert_eq!(loaded.recipe.service().unwrap().init, None);
    }

    #[test]
    fn a_user_recipe_of_its_own_name_is_added_beside_the_built_ins() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "tiny.toml", MINIMAL);
        let recipes = Recipes::load(dir.path());
        assert!(recipes.names().contains(&"tiny"));
        assert!(recipes.names().contains(&"postgres"));
        assert!(!recipes.get("tiny").unwrap().replaces_built_in);
    }

    #[test]
    fn a_broken_user_recipe_shadows_the_built_in_rather_than_falling_back_to_it() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "postgres.toml", "kind = \"service\"\nname =");
        let recipes = Recipes::load(dir.path());
        let e = format!("{:#}", recipes.get("postgres").unwrap_err());
        assert!(e.contains("does not load"), "{e}");
        assert!(e.contains("postgres.toml"), "{e}");
        assert!(e.contains("delete it to go back to the built-in"), "{e}");
        assert_eq!(recipes.broken().count(), 1);
        // And it is not silently answered by the built-in.
        assert!(!recipes.names().contains(&"postgres"));
    }

    #[test]
    fn a_recipe_whose_name_disagrees_with_its_file_name_is_refused_naming_both() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "mine.toml", MINIMAL);
        let recipes = Recipes::load(dir.path());
        let e = format!("{:#}", recipes.get("mine").unwrap_err());
        assert!(e.contains("\"tiny\""), "{e}");
        assert!(e.contains("\"mine\""), "{e}");
    }

    #[test]
    fn an_unknown_recipe_names_the_ones_there_are() {
        let recipes = Recipes::built_in();
        let e = format!("{:#}", recipes.get("cassandra").unwrap_err());
        assert!(e.contains("no recipe named \"cassandra\""), "{e}");
        assert!(e.contains("postgres"), "{e}");
        // `mysql` is not unknown: it is what MariaDB is usually called.
        assert_eq!(recipes.get("mysql").unwrap().recipe.name, "mariadb");
    }

    #[test]
    fn an_alias_finds_a_recipe_but_never_beats_a_file_of_that_name() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Recipes::built_in().get("pg").unwrap().recipe.name,
            "postgres"
        );
        write(
            dir.path(),
            "pg.toml",
            "kind = \"service\"\nname = \"pg\"\n\n[service]\ncmd = \"x\"\n",
        );
        let recipes = Recipes::load(dir.path());
        assert_eq!(
            recipes.get("pg").unwrap().recipe.service().unwrap().cmd,
            "x"
        );
    }

    #[test]
    fn a_service_recipe_with_no_command_is_refused() {
        let e = format!(
            "{:#}",
            parse("kind = \"service\"\nname = \"x\"\n\n[service]\ncmd = \"\"\n").unwrap_err()
        );
        assert!(e.contains("cmd"), "{e}");
        let e = format!(
            "{:#}",
            parse("kind = \"service\"\nname = \"x\"\n").unwrap_err()
        );
        assert!(e.contains("[service]"), "{e}");
    }

    #[test]
    fn a_recipe_may_not_carry_the_other_kinds_body() {
        let e = format!(
            "{:#}",
            parse(
                "kind = \"service\"\nname = \"x\"\n\n[service]\ncmd = \"c\"\n\n[language]\n\
                 managers = [\"mise\"]\n"
            )
            .unwrap_err()
        );
        assert!(e.contains("[language]"), "{e}");
    }

    #[test]
    fn an_unknown_key_is_a_typo_and_is_refused() {
        let e = format!(
            "{:#}",
            parse("kind = \"service\"\nname = \"x\"\n\n[service]\ncmd = \"c\"\nreadyy = \"r\"\n")
                .unwrap_err()
        );
        assert!(e.contains("readyy"), "{e}");
    }

    #[test]
    fn a_recipe_name_that_is_a_path_is_refused() {
        let e = format!(
            "{:#}",
            parse("kind = \"service\"\nname = \"../x\"\n\n[service]\ncmd = \"c\"\n").unwrap_err()
        );
        assert!(e.contains("not a name"), "{e}");
    }

    // ---- the runtime table, in this format ------------------------------
    //
    // The claim Phase 6 has to make good on is "one loader, two kinds of
    // recipe": the per-language `const` table in `runtime.rs` could be read
    // off disk later without a second format being invented for it. Nothing
    // migrates here; these two tests are the proof that it could.

    /// What `runtime::LANGUAGES`' entry for `name` looks like in this
    /// format. Written by hand, deliberately: a generated fixture would
    /// prove only that the generator agrees with itself.
    fn language_toml(name: &str) -> &'static str {
        match name {
            "node" => {
                r#"
kind = "language"
name = "node"
aliases = ["nodejs"]
binaries = ["node"]
version_flag = "-v"

[language]
engines_key = "node"
managers = ["volta", "mise", "asdf", "nvm", "fnm"]
files = [{ file = ".nvmrc" }, { file = ".node-version" }]
"#
            }
            "rust" => {
                r#"
kind = "language"
name = "rust"
binaries = ["rustc"]
version_flag = "-V"

[language]
managers = ["rustup", "mise", "asdf"]
files = [{ file = "rust-toolchain.toml", toml_key = ["toolchain", "channel"] }]
"#
            }
            other => panic!("no fixture for {other}"),
        }
    }

    /// The same entry, read out of the `const` table.
    fn from_table(name: &str) -> Recipe {
        let entry = crate::runtime::LANGUAGES
            .iter()
            .find(|l| l.name == name)
            .expect("a language in the table");
        Recipe {
            kind: Kind::Language,
            name: entry.name.to_string(),
            aliases: entry.aliases.iter().map(|a| a.to_string()).collect(),
            summary: None,
            binaries: entry.binaries.iter().map(|b| b.to_string()).collect(),
            version_flag: Some(entry.version_flag.to_string()),
            install: None,
            notes: None,
            namespace: None,
            untested: false,
            body: Body::Language(LanguageRecipe {
                files: entry
                    .files
                    .iter()
                    .map(|source| RecipeSource {
                        file: source.file.to_string(),
                        toml_key: match source.kind {
                            crate::runtime::SourceKind::Plain => Vec::new(),
                            crate::runtime::SourceKind::TomlKey(table, key) => {
                                vec![table.to_string(), key.to_string()]
                            }
                        },
                    })
                    .collect(),
                engines_key: entry.engines_key.map(str::to_string),
                managers: entry.managers.iter().map(|m| m.name.to_string()).collect(),
            }),
        }
    }

    #[test]
    fn a_language_from_the_runtime_table_round_trips_through_this_format() {
        // node: aliases, an engines key, five managers, two plain files.
        assert_eq!(parse(language_toml("node")).unwrap(), from_table("node"));
        // rust: the other source shape, a key inside a TOML file, which is
        // the one thing a flat `files = [...]` list could not carry.
        assert_eq!(parse(language_toml("rust")).unwrap(), from_table("rust"));
    }

    #[test]
    fn a_language_recipe_loads_beside_a_service_recipe_from_one_directory() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "node.toml", language_toml("node"));
        write(dir.path(), "tiny.toml", MINIMAL);
        let recipes = Recipes::load(dir.path());
        assert!(recipes.names().contains(&"node"));
        // One loader, two kinds: the service list is not polluted by the
        // language, and the language keeps its own body.
        assert!(!recipes.service_names().contains(&"node"));
        assert!(recipes.service_names().contains(&"tiny"));
        let node = recipes.get("node").unwrap();
        assert_eq!(node.recipe.kind, Kind::Language);
        assert_eq!(
            node.recipe.language().unwrap().managers,
            vec!["volta", "mise", "asdf", "nvm", "fnm"]
        );
        assert!(node.recipe.service().is_none());
    }
}
