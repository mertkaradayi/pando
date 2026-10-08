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
//! one body table, `[service]` or `[language]`; a service recipe may carry
//! only a `[namespace]` or `[prefix]` instead, for an engine pando never
//! starts but namespaced mode knows. The language body is the
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
    /// A `slot`'s: prints how many keys it holds as a bare number on its
    /// last line, failing the same way. A client that decorates its
    /// numbers is told not to here, as `redis-cli --raw` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<String>,
    /// A `slot`'s: prints the mark pando wrote into it, `{owner}` as
    /// `claim` wrote it, or nothing when it has none. With `claim`, what
    /// tells a slot pando gave out from the same number on another server
    /// that answers on the same port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// A `slot`'s: writes `{owner}` into it as its mark.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim: Option<String>,
    /// Prints which server process answers, alone on its last line: a
    /// Redis's `run_id`. The same one means the same server, whose slot an
    /// app may have emptied of pando's mark along with its own keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
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
    /// namespaces under this worktree's prefix and nothing else. Printed
    /// for a person, or run by pando itself through `run_sql` as the
    /// administrator a container's environment keeps. Sees `{prefix_like}` — the prefix as an SQL `LIKE`
    /// pattern — and `{account_user}`, `{account_host}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<String>,
    /// What `grant` lets the login do, when it is more than make and drop
    /// databases under the worktree's prefix: an engine that cannot grant
    /// by prefix says so, rather than a refusal promising a wall the
    /// server does not have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_covers: Option<String>,
    /// Where an app on this engine names its database or slot, so pando
    /// finds the main checkout's and points the worktree's at its own.
    /// [`NamespaceRecipe::address`] gives the kind's own when a recipe
    /// says nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<AddressRecipe>,
    /// Runs the SQL it is handed on stdin, as `{user}`: how pando runs the
    /// `grant` itself, as the server's own administrator, when it can log
    /// in as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_sql: Option<String>,
    /// Where a container running this engine keeps its administrator's
    /// login, in its own environment, as its image documents it:
    /// `POSTGRES_USER`, `MARIADB_ROOT_PASSWORD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_admin: Option<ContainerAdmin>,
}

/// The administrator of a server a container runs, read from that
/// container's own environment: `[namespace.container_admin]`.
///
/// A database in Docker is the developer's own, made from their compose
/// file, whose environment already names its administrator. With it, a
/// namespaced start needs no question and no grant from them: pando gives
/// the app's login the right the `grant` names itself, or logs in as the
/// administrator where the app's env files carry no login at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerAdmin {
    /// The container's env keys that name the administrator, the first one
    /// set winning: `POSTGRES_USER`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_from: Vec<String>,
    /// The administrator when none of those is set: `postgres`, `root`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// The container's env keys that hold the administrator's password, the
    /// first one set winning, empty counting as none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub password_from: Vec<String>,
}

/// Where an app names its database or slot: `[namespace.address]`.
///
/// Engine data rather than code: a Postgres URL's path is its database, a
/// Redis client may read `?db=` before the path, and an app keeping its
/// database apart from its address calls it `DATABASE_NAME` beside
/// `DATABASE_PORT`. Every value pando finds here is the main checkout's,
/// and every one is pointed at the worktree's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddressRecipe {
    /// Whether the path of a URL the app reads names it:
    /// `mysql://…/shop`, `redis://…/0`.
    #[serde(default = "yes")]
    pub url_path: bool,
    /// The query parameters of that URL that name it besides:
    /// `redis://…?db=0`, which redis-py reads before the path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub url_query: Vec<String>,
    /// A key beside the address that names it, by suffix, the first one
    /// set winning: `DATABASE_NAME` for `DATABASE_PORT` with `_NAME`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

/// How an app on this engine is told a prefix to put on every name it
/// makes there: `[prefix]`. For an engine whose namespaces are the app's
/// own convention — an index prefix, a topic prefix, a key prefix — where
/// the server has nothing pando could make or drop.
///
/// A prefix is told, never made: pando writes nothing to the server, so
/// nothing is recorded and nothing is dropped. What the app wrote under
/// it stays where it is when the worktree goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefixRecipe {
    /// A key beside the address that holds the prefix, by suffix, the
    /// first one the main checkout's env files set winning:
    /// `ELASTICSEARCH_INDEX_PREFIX` for `ELASTICSEARCH_URL` with
    /// `_INDEX_PREFIX`. Set to an empty value counts: the app reads it,
    /// and the main checkout puts nothing in front.
    pub keys: Vec<String>,
}

fn yes() -> bool {
    true
}

/// `POSTGRES_USER`: a whole env key.
fn is_env_key(key: &str) -> bool {
    key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `_NAME`: what follows an env key's stem, as `[namespace.address]` and
/// `[prefix]` name the keys an app reads.
fn is_key_suffix(suffix: &str) -> bool {
    suffix.len() > 1
        && suffix.starts_with('_')
        && suffix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl AddressRecipe {
    /// What a recipe written before `[namespace.address]` existed means:
    /// the conventions pando held for each kind then. The built-ins say
    /// the same in their own tables, and a test holds them together.
    pub fn of_kind(kind: crate::state::NamespaceKind) -> AddressRecipe {
        use crate::state::NamespaceKind;
        let strings = |list: &[&str]| list.iter().map(|s| s.to_string()).collect();
        match kind {
            NamespaceKind::Database => AddressRecipe {
                url_path: true,
                url_query: Vec::new(),
                keys: strings(&["_NAME", "_DATABASE", "_DB"]),
            },
            NamespaceKind::Slot => AddressRecipe {
                url_path: true,
                url_query: strings(&["db"]),
                keys: strings(&["_DB"]),
            },
        }
    }
}

impl NamespaceRecipe {
    /// The longest database name this engine keeps as given.
    pub fn max_name(&self) -> usize {
        self.max_name.unwrap_or(crate::namespace::MAX_NAME)
    }

    /// Where an app names its database or slot: the recipe's own
    /// `[namespace.address]`, or its kind's.
    pub fn address(&self) -> AddressRecipe {
        self.address
            .clone()
            .unwrap_or_else(|| AddressRecipe::of_kind(self.kind))
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
        if self.owner.is_some() != self.claim.is_some() {
            bail!("a [namespace] marks its slots with both `owner` and `claim`, or neither");
        }
        if self.owner.is_some() && self.kind != NamespaceKind::Slot {
            bail!("`owner` and `claim` mark a `kind = \"slot\"` [namespace]'s slots");
        }
        if let Some(address) = &self.address {
            if !address.url_path && address.url_query.is_empty() && address.keys.is_empty() {
                bail!("a [namespace.address] that names nothing leaves the app nowhere to be told");
            }
            if let Some(bad) = address.keys.iter().find(|suffix| !is_key_suffix(suffix)) {
                bail!(
                    "[namespace.address] keys are suffixes of an env key, as `_NAME` — not {bad:?}"
                );
            }
            if let Some(bad) = address.url_query.iter().find(|name| {
                name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            }) {
                bail!("[namespace.address] url_query names a query parameter, not {bad:?}");
            }
        }
        if let Some(admin) = &self.container_admin {
            if admin.user.is_none() && admin.user_from.is_empty() {
                bail!(
                    "a [namespace.container_admin] names its administrator: `user` or `user_from`"
                );
            }
            if let Some(bad) = admin
                .user_from
                .iter()
                .chain(&admin.password_from)
                .find(|key| !is_env_key(key))
            {
                bail!("[namespace.container_admin] reads env keys, not {bad:?}");
            }
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
    /// A service recipe with no `[service]`: it knows how a worktree gets
    /// data of its own in a server somebody else runs — a container the
    /// main checkout's compose file starts — and nothing about starting
    /// one. A `[[services]]` entry cannot run it natively.
    NamespaceOnly,
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
    /// How the app is told a prefix of the worktree's own, when the
    /// server has no namespace pando can make. A service recipe's only.
    pub prefix: Option<PrefixRecipe>,
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
            Body::Language(_) | Body::NamespaceOnly => None,
        }
    }

    /// The service body, to be overridden by what a `[[services]]` entry
    /// says inline.
    pub fn service_mut(&mut self) -> Option<&mut ServiceRecipe> {
        match &mut self.body {
            Body::Service(s) => Some(s),
            Body::Language(_) | Body::NamespaceOnly => None,
        }
    }

    pub fn language(&self) -> Option<&LanguageRecipe> {
        match &self.body {
            Body::Language(l) => Some(l),
            Body::Service(_) | Body::NamespaceOnly => None,
        }
    }

    /// Why a `[[services]]` entry naming this recipe cannot start a server
    /// from it, when it cannot: a language recipe, or one that only knows
    /// how a server somebody else runs is namespaced.
    pub fn starts_no_server(&self) -> Option<String> {
        match &self.body {
            Body::Service(_) => None,
            Body::Language(_) => Some(format!("{:?} is a language recipe", self.name)),
            Body::NamespaceOnly => Some(format!(
                "{:?} only says how a worktree gets data of its own in a server somebody else \
                 runs, not how to start one — run it from the compose file",
                self.name
            )),
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
            .filter(|(_, l)| l.recipe.service().is_some())
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
    if let Some(prefix) = &raw.prefix {
        if raw.kind != Kind::Service {
            bail!("a [prefix] belongs to a `kind = \"service\"` recipe");
        }
        if prefix.keys.is_empty() {
            bail!("a [prefix] names the keys an app reads its prefix from, and it names none");
        }
        if let Some(bad) = prefix.keys.iter().find(|suffix| !is_key_suffix(suffix)) {
            bail!("[prefix] keys are suffixes of an env key, as `_INDEX_PREFIX` — not {bad:?}");
        }
    }
    let body = match raw.kind {
        Kind::Service => {
            if raw.language.is_some() {
                bail!("a `kind = \"service\"` recipe may not carry a `[language]` table");
            }
            match raw.service {
                Some(service) => {
                    if service.cmd.trim().is_empty() {
                        bail!(
                            "`[service] cmd` is what starts the server, and it must not be empty"
                        );
                    }
                    Body::Service(service)
                }
                // An engine pando never starts, only finds in a container:
                // what it knows is how a worktree gets data of its own there.
                None if raw.namespace.is_some() || raw.prefix.is_some() => Body::NamespaceOnly,
                None => bail!(
                    "a `kind = \"service\"` recipe needs a `[service]` table that starts the \
                     server, or a `[namespace]` or `[prefix]` that says how a worktree gets data \
                     of its own in one somebody else runs"
                ),
            }
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
        prefix: raw.prefix,
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
    #[serde(default)]
    prefix: Option<PrefixRecipe>,
}

/// The recipes this build ships, as `(file name, TOML text)`.
///
/// Each one is a file in `src/recipes/builtin/`, written in exactly the
/// format a developer drops into `~/.pando/recipes/`, and the comment at
/// the top of each says what in it is deliberate. Adding a built-in is a
/// file there and a row here.
///
/// Of the four that start a server, three were run against a real one
/// while they were written; MongoDB was not, because no machine here has
/// `mongod`, and it says so in its own `untested` field rather than in a
/// comment. The other four start nothing and run nothing: each says only
/// which key an app reads a prefix from on its engine.
pub const BUILT_IN: [(&str, &str); 8] = [
    ("elasticsearch", include_str!("builtin/elasticsearch.toml")),
    ("kafka", include_str!("builtin/kafka.toml")),
    ("mariadb", include_str!("builtin/mariadb.toml")),
    ("meilisearch", include_str!("builtin/meilisearch.toml")),
    ("memcached", include_str!("builtin/memcached.toml")),
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
        // Every one of them is about a service: one that starts a server,
        // or one that only knows how a worktree gets data of its own in a
        // server somebody else runs. A language recipe that got into this
        // list would be a build that ships something no `[[services]]`
        // entry could ever use.
        for (name, loaded) in recipes.entries() {
            let recipe = &loaded.recipe;
            assert_eq!(recipe.kind, Kind::Service, "{name}");
            assert!(
                recipe.service().is_some() || recipe.namespace.is_some() || recipe.prefix.is_some(),
                "{name} says nothing a service could use"
            );
        }
        assert_eq!(
            recipes.service_names(),
            ["mariadb", "mongodb", "postgres", "redis"],
            "which built-ins start a server changed"
        );
    }

    /// Rules every shipped service recipe is held to, whichever engine it
    /// is for. Written as one test over the table rather than four
    /// near-copies: the next recipe anyone adds is held to them for free,
    /// which is the whole reason recipes are data.
    #[test]
    fn every_built_in_service_recipe_is_private_to_this_machine() {
        for (name, text) in BUILT_IN {
            let recipe = parse(text).unwrap();
            // One that starts nothing binds nothing.
            let Some(service) = recipe.service() else {
                assert_eq!(recipe.body, Body::NamespaceOnly, "{name}");
                continue;
            };
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
                ns.owner.as_deref(),
                ns.claim.as_deref(),
                ns.server_id.as_deref(),
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
                    let marks = [ns.owner.as_deref(), ns.claim.as_deref()];
                    for command in [Some(ns.drop.as_str()), ns.size.as_deref()]
                        .into_iter()
                        .chain(marks)
                        .flatten()
                    {
                        assert!(!command.contains(" -n "), "{name}: {command}");
                        assert!(command.contains("{namespace}"), "{name}: {command}");
                    }
                    // Another Redis on the same port is told apart.
                    assert!(
                        ns.owner.is_some() && ns.server_id.is_some(),
                        "{name} marks its slots"
                    );
                    assert!(ns.slots.is_some_and(|n| n > 1), "{name}");
                }
            }
        }
        assert_eq!(
            namespaced,
            vec!["mariadb", "postgres", "redis"],
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

    /// Where an app names its database or slot is the recipe's to say, and
    /// a table that would leave the app nowhere to be told, or name a key
    /// that is no suffix, is refused when the recipe is read.
    #[test]
    fn a_namespace_address_is_data_and_held_to_what_it_names() {
        let head = "kind = \"service\"\nname = \"x\"\n\n[service]\ncmd = \"c\"\n\n\
                    [namespace]\nkind = \"database\"\nping = \"p\"\ndrop = \"d\"\n\
                    create = \"c\"\nexists = \"e\"\n\n[namespace.address]\n";
        let parsed = parse(&format!("{head}url_path = false\nkeys = [\"_SCHEMA\"]\n")).unwrap();
        let address = parsed.namespace.unwrap().address();
        assert!(!address.url_path);
        assert_eq!(address.keys, ["_SCHEMA"]);
        for (table, says) in [
            ("url_path = false\n", "names nothing"),
            ("keys = [\"NAME\"]\n", "suffixes of an env key"),
            ("keys = [\"_\"]\n", "suffixes of an env key"),
            ("url_query = [\"a&b\"]\n", "names a query parameter"),
            ("url_path = true\nport = 1\n", "unknown field"),
        ] {
            let e = format!("{:#}", parse(&format!("{head}{table}")).unwrap_err());
            assert!(e.contains(says), "{table}: {e}");
        }
        // A recipe written before the table existed means its kind's.
        let recipe = parse(
            "kind = \"service\"\nname = \"x\"\n\n[service]\ncmd = \"c\"\n\n[namespace]\n\
             kind = \"slot\"\nping = \"p\"\ndrop = \"d\"\nsize = \"s\"\nslots = 4\n",
        )
        .unwrap();
        assert_eq!(
            recipe.namespace.unwrap().address(),
            AddressRecipe::of_kind(crate::state::NamespaceKind::Slot)
        );
    }

    /// A service recipe may know only how a worktree gets data of its own
    /// in a server somebody else runs; one that knows neither that nor how
    /// to start one is refused, and none of it can be run natively.
    #[test]
    fn a_recipe_may_know_only_a_namespace_and_never_starts_a_server() {
        let recipe = parse(
            "kind = \"service\"\nname = \"ch\"\n\n[namespace]\nkind = \"database\"\n\
             ping = \"p\"\ndrop = \"d\"\ncreate = \"c\"\nexists = \"e\"\n",
        )
        .unwrap();
        assert_eq!(recipe.body, Body::NamespaceOnly);
        assert!(recipe.service().is_none());
        let why = recipe.starts_no_server().unwrap();
        assert!(why.contains("not how to start one"), "{why}");
        let e = format!(
            "{:#}",
            parse("kind = \"service\"\nname = \"ch\"\n").unwrap_err()
        );
        assert!(e.contains("or a `[namespace]`"), "{e}");

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("ch.toml"),
            "kind = \"service\"\nname = \"ch\"\n\n[namespace]\nkind = \"database\"\n\
             ping = \"p\"\ndrop = \"d\"\ncreate = \"c\"\nexists = \"e\"\n",
        )
        .unwrap();
        let recipes = Recipes::load(dir.path());
        assert!(recipes.get("ch").is_ok());
        assert!(!recipes.service_names().contains(&"ch"));
    }

    /// The built-ins say their address in their own files, and what they
    /// say is what a recipe of their kind with no table means: two lists
    /// of one fact, held to one.
    #[test]
    fn every_built_in_namespace_states_its_kinds_address() {
        for (name, loaded) in Recipes::built_in().entries() {
            let Some(namespace) = &loaded.recipe.namespace else {
                continue;
            };
            assert_eq!(
                namespace.address.as_ref(),
                Some(&AddressRecipe::of_kind(namespace.kind)),
                "{name}"
            );
        }
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
            prefix: None,
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
