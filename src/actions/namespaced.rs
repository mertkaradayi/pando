//! Namespaced starts: which services get a namespace, the login they are
//! made with, making them, and what the app is told.

use anyhow::{Result, bail};

use crate::config::{self, Config};
use crate::detect::Slot;
use crate::namespace::{self, Login};
use crate::paths::PandoPaths;
use crate::recipes::NamespaceRecipe;
use crate::services::EnvFiles;
use crate::state::NamespaceKind;

use super::questions::{Answer, Ask, NeedsAnswer, Question, answered_by};

/// The login a namespaced start makes and drops `service`'s namespaces
/// with, asked for when nothing says.
///
/// The main checkout's own first: namespaced mode is its servers, and the
/// app in a worktree logs in the way the main checkout's does. Then the one
/// pando was given before. Then the question — the same one every front end
/// puts, a terminal prompt with nothing echoed, the TUI's modal with the
/// password as dots, exit 3 for a script — and the answer written to
/// pando's own file for the project, which is 0600 and never committed,
/// under `[namespaced.<service>]`.
///
/// `keys` are the env keys the app finds the service by, which is where
/// the main checkout keeps its login too; `needs_user` is the engine's.
pub fn namespace_login(
    paths: &PandoPaths,
    config: &Config,
    service: &str,
    keys: &[String],
    needs_user: bool,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<Login> {
    let file = paths.config_file();
    if let Some(login) = namespace::find_login(
        &main_env(paths, config),
        config,
        service,
        keys,
        needs_user,
        &file,
    )
    .map_err(|unresolved| unreadable_login(service, &file, unresolved))?
    {
        return Ok(login);
    }
    let (answer, by) = answered_by(ask(&login_question(paths, service, keys))?);
    let Answer::Custom(typed) = answer else {
        bail!("the login for {service} is typed, as user:password — nothing was written");
    };
    let (user, password) = match typed.split_once(':') {
        Some((user, password)) => (user.trim(), Some(password)),
        None => (typed.trim(), None),
    };
    if user.is_empty() {
        bail!(
            "a login needs a user: type it as user:password, or the user alone when there is no \
             password — nothing was written"
        );
    }
    let password = password.filter(|p| !p.is_empty());
    let mut entries = vec![("user".to_string(), toml_edit::Value::from(user))];
    if let Some(password) = password {
        entries.push(("password".to_string(), toml_edit::Value::from(password)));
    }
    config::set_detected_table(
        paths,
        config::Layer::Project,
        &["namespaced", service],
        entries,
        by.note(config::Note::Answered),
    )?;
    let from = format!("[namespaced.{service}] in {}", file.display());
    progress(&format!(
        "{service}: the login for its namespaces is kept in {from}, and only pando reads it there"
    ));
    Ok(Login::new(
        Some(user.to_string()),
        password.map(str::to_string),
        from,
    ))
}

/// The keys an app keeps its server's host in, beside its address:
/// `DATABASE_HOST`, and `POSTGRES_SERVER` as FastAPI's template names it.
const HOST_SUFFIXES: &[&str] = &["_HOST", "_SERVER"];

/// The main checkout's env files, as namespaced mode reads a server's
/// address, its database and its login from them: the root's, then those
/// of the directories the processes run in — `backend/.env` in a project
/// whose root has no manifest.
pub(super) fn main_env(paths: &PandoPaths, config: &Config) -> EnvFiles {
    EnvFiles::read(paths.root(), &super::services::env_dirs(config))
}

/// Why a namespaced start stops when the main checkout's env files hold
/// a login pando cannot read and none is written down: asked instead, the
/// question would say they hold none.
fn unreadable_login(
    service: &str,
    file: &std::path::Path,
    unresolved: crate::services::Unresolved,
) -> anyhow::Error {
    anyhow::anyhow!(
        "{unresolved}, so pando cannot read the login for {service}'s namespaces — set it in \
         the environment pando runs in, or write [namespaced.{service}] with a user and a \
         password in {}",
        file.display()
    )
}

/// The question [`namespace_login`] puts: nothing to choose from, only a
/// login to type, and the table to write it in by hand instead.
pub fn login_question(paths: &PandoPaths, service: &str, keys: &[String]) -> Question {
    Question {
        slot: Slot::Login,
        prompt: format!(
            "Which login may create and drop this worktree's own databases in {service}?"
        ),
        options: Vec::new(),
        preselect: None,
        allow_custom: true,
        allow_none: false,
        multi: false,
        checked: Vec::new(),
        details: vec![
            format!(
                "the main checkout's env files give {service} no login — nothing beside {} \
                 names a user",
                match keys.is_empty() {
                    true => "its address".to_string(),
                    false => keys.join(", "),
                }
            ),
            "it is kept in pando's own config for this project, readable by you alone, and \
             handed to the database client in its environment — never on a command line"
                .to_string(),
        ],
        answer_file: Some(paths.config_file()),
        snippet: format!("[namespaced.{service}]\nuser = \"<user>\"\npassword = \"<password>\"\n"),
    }
}

/// A service a namespaced start gives the worktree a namespace in: where
/// the main checkout's server is, what its own database or slot is there,
/// and the keys that tell the app which one is the worktree's.
#[derive(Debug, Clone)]
pub(super) struct Target {
    pub service: String,
    /// The recipe whose `[namespace]` this is, by name — what state
    /// records, so `rm` can find the commands without config.
    pub recipe: String,
    pub namespace: NamespaceRecipe,
    /// The env keys the app finds the service by: where its login is too.
    pub keys: Vec<String>,
    pub host: String,
    pub port: u16,
    /// The main checkout's own database or slot on that server.
    pub main: String,
    /// Every one the main checkout's env files name on that server, `main`
    /// first: a second URL can name another slot of the same Redis — a
    /// queue's beside a cache's — and that one is main's just as much.
    /// None of them is ever given out or emptied.
    pub mains: Vec<String>,
    /// Every key the app reads its database or slot from.
    pub tells: Vec<Tell>,
}

/// One key the app reads which database, or slot, is its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tell {
    /// A key of its own: `DATABASE_NAME=shop`, `REDIS_DB=0`.
    Key(String),
    /// A URL, at the parts its recipe's `[namespace.address]` says name
    /// it: the path, `mysql://…/shop`, and any query parameter,
    /// `redis://…?db=0`.
    Url(String),
}

/// A service whose app puts a prefix of the worktree's own on every name
/// it makes there — an index, a topic, a key — because the server has no
/// namespace pando can make: the keys it reads one from, each with the
/// main checkout's value, which may be empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefixed {
    pub service: String,
    /// The recipe whose `[prefix]` named a key, or that the project named.
    pub recipe: Option<String>,
    pub keys: Vec<(String, String)>,
}

impl Prefixed {
    /// Each key with the worktree's value: the main checkout's, then the
    /// worktree's slug and the marker.
    pub fn values(&self, project: &str, worktree: &str) -> Vec<(String, String)> {
        self.keys
            .iter()
            .map(|(key, main)| {
                (
                    key.clone(),
                    namespace::worktree_prefix(main, project, worktree),
                )
            })
            .collect()
    }

    /// `prefix feat_x__ in ELASTICSEARCH_INDEX_PREFIX`, as `status` says it.
    pub fn describe(&self, project: &str, worktree: &str) -> String {
        let values = self.values(project, worktree);
        let mut shown: Vec<String> = Vec::new();
        for (_, value) in &values {
            if !shown.contains(value) {
                shown.push(value.clone());
            }
        }
        let keys: Vec<&str> = values.iter().map(|(key, _)| key.as_str()).collect();
        format!("prefix {} in {}", shown.join(", "), keys.join(", "))
    }
}

/// Every service of the project, as a namespaced start sees it.
#[derive(Debug, Clone, Default)]
pub(super) struct Plan {
    /// The ones that get a namespace.
    pub targets: Vec<Target>,
    /// The ones that get a prefix of the worktree's own instead: told,
    /// never made, recorded or dropped.
    pub prefixed: Vec<Prefixed>,
    /// Each one that stays on the main checkout's own data, and why —
    /// said at every namespaced start, because an app half on its own data
    /// and half on main's is only safe when somebody knows.
    pub shared: Vec<(String, String)>,
    /// The shared ones a step after the services could reach the main
    /// checkout's data through: every one but an engine whose namespace is
    /// a slot — a Redis the app names no slot for, which decision 3 leaves
    /// shared — and a helper the app keeps nothing in, like a mail catcher.
    pub shared_data: Vec<String>,
}

impl Plan {
    /// Whether a namespaced start gives the worktree anything of its own:
    /// a database or a slot the server makes, or a prefix its app is told.
    pub fn gives_own(&self) -> bool {
        !self.targets.is_empty() || !self.prefixed.is_empty()
    }

    /// `redis: shared — the app reads no slot setting — …`, one a service
    /// that stays on main's data.
    pub fn shared_lines(&self) -> Vec<String> {
        self.shared
            .iter()
            .map(|(service, why)| format!("{service}: shared — {why}"))
            .collect()
    }

    /// Why the steps after the services — a schema, a migration, a seed —
    /// would not run on data of the worktree's own, when they would not: a
    /// service that stays on the main checkout's data they could reach, or
    /// no database of the worktree's own at all, only a slot. `None` when
    /// they run on the worktree's own, as on an isolated start.
    pub fn not_own_data(&self) -> Option<String> {
        if !self.shared_data.is_empty() {
            return Some(format!(
                "{} {} on the main checkout's data",
                self.shared_data.join(", "),
                match self.shared_data.len() {
                    1 => "stays",
                    _ => "stay",
                }
            ));
        }
        (!self
            .targets
            .iter()
            .any(|target| target.namespace.kind == NamespaceKind::Database)
            && self.prefixed.is_empty())
        .then(|| "no database here is this worktree's own, only a slot".to_string())
    }
}

/// Why the next namespaced start's steps after the services would not run
/// on data of the worktree's own: [`Plan::not_own_data`] of the plan that
/// start makes. `None` when they would.
///
/// Public because `doctor` says at rest whether that start runs a hook
/// scoped to data of the worktree's own. Its own answer went by the
/// namespaces a worktree recorded, and a database of its own beside one
/// that stays on main's read as data of its own.
pub fn namespaced_not_own_data(paths: &PandoPaths, config: &Config) -> Option<String> {
    plan(paths, config).not_own_data()
}

/// Why `pando check` cannot run the steps after the services in
/// namespaces of its own, or `None` when it can: the plan a namespaced
/// start makes gives it a database of its own with nothing left on the
/// main checkout's data, and that start would put no question — every
/// login it needs is written down somewhere, and no slot has to be
/// chosen to free.
///
/// The check never asks and never answers for anybody, so a login that
/// is not there yet is a reason, not a question. Asks a server only when
/// every slot of it is held, which is when a start would ask too.
pub(super) fn check_stays_shared(paths: &PandoPaths, config: &Config) -> Option<String> {
    let plan = plan(paths, config);
    if !plan.gives_own() {
        return Some("this project has no server pando can make a database in".to_string());
    }
    if let Some(why) = plan.not_own_data() {
        return Some(format!(
            "a namespaced start here would not be on data of its own: {why}"
        ));
    }
    let file = paths.config_file();
    let env = main_env(paths, config);
    for target in &plan.targets {
        match namespace::find_login(
            &env,
            config,
            &target.service,
            &target.keys,
            target.namespace.user,
            &file,
        ) {
            Ok(Some(_)) => {}
            Ok(None) if !target.namespace.user => {}
            Ok(None) if container_admin(paths, target).is_some() => {}
            Ok(None) => {
                return Some(format!(
                    "namespaced mode has no login for {} yet — the first `pando start \
                     --namespaced` asks for one",
                    target.service
                ));
            }
            Err(unresolved) => {
                return Some(format!(
                    "{:#}",
                    unreadable_login(&target.service, &file, unresolved)
                ));
            }
        }
    }
    let unasked = |question: &Question| -> Result<Answer> {
        Err(anyhow::Error::new(NeedsAnswer {
            question: question.clone(),
        }))
    };
    let check = crate::paths::CHECK_WORKTREE;
    match free_slots_if_full(paths, config, check, &unasked, &|_| {}) {
        Ok(()) => None,
        Err(e) if e.is::<NeedsAnswer>() => Some(
            "every slot a namespaced start could use is held, and which one to free is a \
             question the check does not answer"
                .to_string(),
        ),
        Err(e) => Some(format!("{e:#}")),
    }
}

/// Which of the project's services a namespaced start gives the worktree a
/// namespace in, read from config, the recipes, and the main checkout's
/// env files.
///
/// A service gets one when its recipe says what a namespace is on its
/// engine, and the main checkout's env files say where its server is and
/// what the main checkout's own is called there. Anything else stays
/// shared, with the reason.
pub(super) fn plan(paths: &PandoPaths, config: &Config) -> Plan {
    let recipes = crate::recipes::Recipes::load(&paths.recipes_dir());
    let env = main_env(paths, config);
    let mut out = Plan::default();
    for mut declared in services_with_recipes(paths, config, &recipes) {
        // A recipe the project names wins over what the image says: the
        // image is a guess, and the project's word is not.
        if let Some(named) = config
            .namespaced
            .get(&declared.service)
            .and_then(|settings| settings.recipe.as_deref())
        {
            match recipes.get(named) {
                Ok(loaded) => declared.recipe = Some(loaded.recipe.clone()),
                Err(e) => {
                    out.shared.push((
                        declared.service.clone(),
                        format!("[namespaced.{}] recipe: {e:#}", declared.service),
                    ));
                    out.shared_data.push(declared.service);
                    continue;
                }
            }
        }
        let data = !declared.helper
            && !declared
                .recipe
                .as_ref()
                .and_then(|recipe| recipe.namespace.as_ref())
                .is_some_and(|namespace| namespace.kind == NamespaceKind::Slot);
        let settings = config.namespaced.get(&declared.service);
        let db_env = settings
            .map(|settings| settings.db_env.as_slice())
            .unwrap_or_default();
        let prefix_env = settings
            .map(|settings| settings.prefix_env.as_slice())
            .unwrap_or_default();
        let prefixes = prefix_keys(
            &env,
            &declared.service,
            &declared.keys,
            declared.recipe.as_ref().and_then(|r| r.prefix.as_ref()),
            prefix_env,
        );
        let why = match target(
            &env,
            db_env,
            &declared.service,
            declared.recipe.as_ref(),
            declared.keys,
        ) {
            Ok(target) => {
                out.targets.push(target);
                continue;
            }
            Err(why) => why,
        };
        // No namespace the server makes: a prefix the app puts on its own
        // names, when it reads one, is the worktree's data all the same.
        let why = match prefixes {
            Ok(keys) if !keys.is_empty() => {
                out.prefixed.push(Prefixed {
                    service: declared.service,
                    recipe: declared.recipe.as_ref().map(|recipe| recipe.name.clone()),
                    keys,
                });
                continue;
            }
            // An engine whose namespaces are the app's own says what the
            // app would have to read, and how to name a key of its own.
            Ok(_) => match declared.recipe.as_ref() {
                Some(recipe) if recipe.namespace.is_none() && recipe.prefix.is_some() => {
                    let suffixes = recipe
                        .prefix
                        .as_ref()
                        .map(|prefix| prefix.keys.join(" or "))
                        .unwrap_or_default();
                    format!(
                        "the app reads no prefix for it ({suffixes} beside its address) — \
                         [namespaced.{}] prefix_env names one it reads under another name",
                        declared.service
                    )
                }
                _ => why,
            },
            Err(e) => e,
        };
        if data {
            out.shared_data.push(declared.service.clone());
        }
        out.shared.push((declared.service, why));
    }
    out
}

/// One service config declares, as a namespaced start first sees it.
struct Declared {
    service: String,
    /// The recipe that knows its engine, when one does.
    recipe: Option<crate::recipes::Recipe>,
    /// The env keys the app finds it by.
    keys: Vec<String>,
    /// A compose image the catalog knows as a helper the app keeps no
    /// data in: a mail catcher.
    helper: bool,
}

/// Every service config declares, the recipe that knows its engine when
/// one does, and the env keys the app finds it by.
fn services_with_recipes(
    paths: &PandoPaths,
    config: &Config,
    recipes: &crate::recipes::Recipes,
) -> Vec<Declared> {
    let mut out = Vec::new();
    for service in &config.services {
        match service {
            config::ServiceConfig::Native { .. } => {
                let Some(entry) = crate::native::Entry::of(service) else {
                    continue;
                };
                let recipe = crate::native::resolve(recipes, &entry)
                    .ok()
                    .map(|resolved| resolved.recipe);
                let keys = entry
                    .env_map(recipe.as_ref())
                    .0
                    .into_iter()
                    .filter(|(_, name)| name == entry.name)
                    .map(|(key, _)| key)
                    .collect();
                out.push(Declared {
                    service: entry.name.to_string(),
                    recipe,
                    keys,
                    helper: false,
                });
            }
            config::ServiceConfig::Compose {
                file, include, env, ..
            } => {
                // The image the main checkout's compose file runs is what
                // says which engine it is: a service called `db` running
                // `mariadb:11` is a MariaDB.
                let parsed = crate::compose::file_in(paths.root(), file)
                    .and_then(|file| crate::compose::read(&file))
                    .ok();
                for name in include {
                    let reference = parsed
                        .as_ref()
                        .and_then(|p| p.services.get(name))
                        .and_then(|s| s.image.as_deref());
                    // A recipe of the image's own name first, then the
                    // engine the catalog knows it as: `pgvector/pgvector`
                    // is a Postgres.
                    let image = reference.map(crate::catalog::images::image_name);
                    let engine = reference
                        .and_then(crate::catalog::images::known)
                        .and_then(|known| known.engine);
                    let recipe = image
                        .into_iter()
                        .chain(engine)
                        .chain(std::iter::once(name.as_str()))
                        .find_map(|candidate| recipes.get(candidate).ok())
                        .map(|loaded| loaded.recipe.clone());
                    let keys = env
                        .iter()
                        .filter(|(_, service)| *service == name)
                        .map(|(key, _)| key.clone())
                        .collect();
                    let helper = reference
                        .and_then(crate::catalog::images::known)
                        .is_some_and(|known| known.role == crate::catalog::images::Role::Utility);
                    out.push(Declared {
                        service: name.clone(),
                        recipe,
                        keys,
                        helper,
                    });
                }
            }
        }
    }
    // A compose service the project runs no private copy of is still one
    // a namespaced start can give data of its own, once the project says
    // how in `[namespaced.<service>]`: isolation and namespaces are two
    // choices, and the first should not gate the second.
    // Read only for a table that names a service config does not: the
    // plan is asked for on every start and every `status`.
    let undeclared = config
        .namespaced
        .keys()
        .any(|service| !out.iter().any(|known| known.service == *service));
    let compose = match undeclared {
        true => compose_services(paths),
        false => Vec::new(),
    };
    for (service, image) in compose {
        if out.iter().any(|known| known.service == service)
            || !config.namespaced.contains_key(&service)
        {
            continue;
        }
        let recipe = image
            .as_deref()
            .map(crate::catalog::images::image_name)
            .into_iter()
            .chain(
                image
                    .as_deref()
                    .and_then(crate::catalog::images::known)
                    .and_then(|known| known.engine),
            )
            .chain(std::iter::once(service.as_str()))
            .find_map(|candidate| recipes.get(candidate).ok())
            .map(|loaded| loaded.recipe.clone());
        out.push(Declared {
            service,
            recipe,
            keys: Vec::new(),
            helper: false,
        });
    }
    out
}

/// Every service the main checkout's compose files at its root run, with
/// its image: the root's own, as `signals` lists them.
fn compose_services(paths: &PandoPaths) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = Vec::new();
    for file in crate::detect::signals(paths.root()).compose_files {
        let Ok(parsed) = crate::compose::read(&paths.root().join(&file)) else {
            continue;
        };
        for (service, entry) in parsed.services {
            if !out.iter().any(|(known, _)| *known == service) {
                out.push((service, entry.image.clone()));
            }
        }
    }
    out
}

/// The keys a service's app reads a prefix from, each with the main
/// checkout's value: every one `[namespaced.<service>] prefix_env` names,
/// set or not — the project says its app reads it — and, beside each
/// address key, the first of its recipe's `[prefix]` keys the main
/// checkout's env files set, empty counting.
fn prefix_keys(
    env: &EnvFiles,
    service: &str,
    keys: &[String],
    recipe: Option<&crate::recipes::PrefixRecipe>,
    prefix_env: &[String],
) -> std::result::Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut add = |key: String, value: Option<String>| {
        if !out.iter().any(|(known, _)| *known == key) {
            out.push((key, value.unwrap_or_default().trim().to_string()));
        }
    };
    for key in prefix_env {
        if !crate::detect::is_env_name(key) {
            return Err(format!(
                "[namespaced.{service}] prefix_env names {key:?}, which is not an env key"
            ));
        }
        add(key.clone(), env.value(key).map_err(|e| e.to_string())?);
    }
    let Some(recipe) = recipe else {
        return Ok(out);
    };
    for key in keys {
        let Some(stem) = ["_PORT", "_HOST", "_URL"]
            .iter()
            .find_map(|suffix| key.strip_suffix(suffix))
        else {
            continue;
        };
        for suffix in &recipe.keys {
            let sibling = format!("{stem}{suffix}");
            if let Some(value) = env.value(&sibling).map_err(|e| e.to_string())? {
                add(sibling, Some(value));
                break;
            }
        }
    }
    Ok(out)
}

/// One service as a namespaced start would reach it, or why it cannot.
fn target(
    env: &EnvFiles,
    db_env: &[String],
    service: &str,
    recipe: Option<&crate::recipes::Recipe>,
    keys: Vec<String>,
) -> std::result::Result<Target, String> {
    // Each reason ends with the setting that would change it, so whoever
    // reads it — a person, or a setup agent — has the way in.
    let recipe = recipe.ok_or_else(|| {
        format!(
            "pando has no recipe that knows its engine — [namespaced.{service}] recipe names one"
        )
    })?;
    let namespace = recipe
        .namespace
        .clone()
        .ok_or_else(|| format!("pando knows no namespace for {}", recipe.name))?;
    if keys.is_empty() {
        return Err("nothing in the project's env tells the app where it is".to_string());
    }
    // A value that holds a reference nothing sets keeps the service shared:
    // pando cannot tell which server, or which database, the app will read
    // it as, and the shared env leaves that key to the app's own loader.
    let mut values: Vec<(String, String)> = Vec::new();
    for key in &keys {
        let value = env.value(key).map_err(|e| e.to_string())?;
        values.extend(value.map(|value| (key.clone(), value.trim().to_string())));
    }
    let urls: Vec<&(String, String)> = values.iter().filter(|(_, v)| v.contains("://")).collect();
    let port = values
        .iter()
        .find_map(|(_, value)| crate::services::port_of_value(value))
        .ok_or("the main checkout's env files give no port for it")?;
    let host = match urls
        .iter()
        .find_map(|(_, url)| crate::services::url_host(url))
    {
        Some(host) => host,
        None => env
            .sibling(keys.iter().map(String::as_str), HOST_SUFFIXES)
            .map_err(|e| e.to_string())?
            .map_or_else(|| "127.0.0.1".to_string(), |(_, host)| host),
    };
    let found = mains_in(env, &keys, &urls, namespace.kind, &namespace.address())?;
    let (mains, tells) =
        with_db_env(env, service, namespace.kind, db_env, found)?.ok_or_else(|| {
            format!(
                "{} — [namespaced.{service}] db_env names the key it reads, if it reads one",
                match namespace.kind {
                    NamespaceKind::Database => {
                        "nothing in the main checkout's env files names its database"
                    }
                    NamespaceKind::Slot => "the app reads no slot setting",
                }
            )
        })?;
    Ok(Target {
        service: service.to_string(),
        recipe: recipe.name.clone(),
        namespace,
        keys,
        host,
        port,
        main: mains[0].clone(),
        mains,
        tells,
    })
}

/// Every name the main checkout's env files give its own database or slot
/// on one server, the one it is known by first, and every key that tells
/// the app which one it uses.
type Mains = (Vec<String>, Vec<Tell>);

/// Whether a value can be a namespace of this kind: a slot is a number,
/// and a database is a name — all digits is a slot, never a database.
fn fits(kind: NamespaceKind, value: &str) -> bool {
    let numbered = value.chars().all(|c| c.is_ascii_digit());
    !value.is_empty()
        && match kind {
            NamespaceKind::Slot => numbered,
            NamespaceKind::Database => !numbered,
        }
}

/// Every database or slot the main checkout's env files name, the one it
/// is known by first, and every key that says which one the app uses, at
/// the places the recipe's `[namespace.address]` says an app names it:
/// each URL's path and query parameters, and a key beside each address
/// key — `DATABASE_NAME` next to `DATABASE_PORT`. An app that names it
/// nowhere has nowhere to be told the worktree's, and stays shared.
///
/// A URL counts only when every place it names one fits the kind: a
/// Redis URL with no path is slot 0, and its `?db=` is main's as much as
/// its path, since one client reads the one and another the other. A key
/// whose value holds a reference nothing sets is the error that names it.
fn mains_in(
    env: &EnvFiles,
    keys: &[String],
    urls: &[&(String, String)],
    kind: NamespaceKind,
    address: &crate::recipes::AddressRecipe,
) -> std::result::Result<Option<Mains>, String> {
    let mut mains: Vec<String> = Vec::new();
    let mut tells: Vec<Tell> = Vec::new();
    for (key, url) in urls {
        let mut named: Vec<String> = address
            .url_query
            .iter()
            .flat_map(|parameter| crate::services::url_query_values(url, parameter))
            .collect();
        if address.url_path {
            match (crate::services::url_identity(url).1, kind) {
                (Some(path), _) => named.push(path),
                // No path is a Redis's slot 0; a database it never is.
                (None, NamespaceKind::Slot) => named.push("0".to_string()),
                (None, NamespaceKind::Database) => {}
            }
        }
        if !named.is_empty() && named.iter().all(|value| fits(kind, value)) {
            for value in named {
                add_main(&mut mains, value);
            }
            tells.push(Tell::Url(key.clone()));
        }
    }
    for key in keys {
        let Some(stem) = ["_PORT", "_HOST", "_URL"]
            .iter()
            .find_map(|suffix| key.strip_suffix(suffix))
        else {
            continue;
        };
        for suffix in &address.keys {
            let sibling = format!("{stem}{suffix}");
            let Some(value) = env.value(&sibling).map_err(|e| e.to_string())? else {
                continue;
            };
            let value = value.trim();
            if !fits(kind, value) {
                continue;
            }
            add_main(&mut mains, value.to_string());
            let tell = Tell::Key(sibling);
            if !tells.contains(&tell) {
                tells.push(tell);
            }
            break;
        }
    }
    Ok((!mains.is_empty()).then_some((mains, tells)))
}

/// What pando found the app's database or slot by, and the keys
/// `[namespaced.<service>] db_env` names besides: one `REDIS_DB` that three
/// Redis roles share sits beside none of their addresses. Each is main's
/// as much as what pando found, and each is pointed at the worktree's own.
///
/// A key it names that the env files do not set, or set to something
/// that cannot be a database or a slot, is the error that says so: the
/// developer wrote it down because pando could not find it.
fn with_db_env(
    env: &EnvFiles,
    service: &str,
    kind: NamespaceKind,
    db_env: &[String],
    found: Option<Mains>,
) -> std::result::Result<Option<Mains>, String> {
    let (mut mains, mut tells) = found.unwrap_or_default();
    for key in db_env {
        let value = env
            .value(key)
            .map_err(|e| e.to_string())?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                format!(
                    "[namespaced.{service}] db_env names {key}, which the main checkout's env \
                     files do not set"
                )
            })?;
        let numbered = value.chars().all(|c| c.is_ascii_digit());
        match (kind, numbered) {
            (NamespaceKind::Slot, false) => {
                return Err(format!(
                    "[namespaced.{service}] db_env names {key}, whose value is not a slot number"
                ));
            }
            (NamespaceKind::Database, true) => {
                return Err(format!(
                    "[namespaced.{service}] db_env names {key}, whose value is a number, not a \
                     database's name"
                ));
            }
            _ => {}
        }
        add_main(&mut mains, value);
        let tell = Tell::Key(key.clone());
        if !tells.contains(&tell) {
            tells.push(tell);
        }
    }
    Ok((!mains.is_empty()).then_some((mains, tells)))
}

/// Adds one name the main checkout's env files give its own, once: `07`
/// and `7` are one slot, `Shop` and `shop` one database to MariaDB on
/// macOS.
fn add_main(mains: &mut Vec<String>, main: String) {
    let same = |a: &str, b: &str| match (a.parse::<u32>(), b.parse::<u32>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.eq_ignore_ascii_case(b),
    };
    if !mains.iter().any(|known| same(known, &main)) {
        mains.push(main);
    }
}

/// What a namespaced start made ready before anything was stopped.
#[derive(Debug, Clone, Default)]
pub(super) struct Ready {
    pub plan: Plan,
    /// One per target, as recorded. Carried to the start rather than read
    /// back from state, so the start writes them into the record it spawns
    /// from — whatever happened to that record in between.
    pub namespaces: Vec<crate::state::NamespaceRecord>,
    /// Whether a namespace was made just now: an empty database the
    /// schema step has to fill, whatever its fingerprint says.
    pub fresh: bool,
}

impl Ready {
    /// [`Plan::not_own_data`] of the plan these were made for: every
    /// target of it has its namespace here.
    pub fn not_own_data(&self) -> Option<String> {
        self.plan.not_own_data()
    }
}

/// Everything a namespaced start does before any process is stopped or
/// spawned: a login for each server, each server answering, and each
/// namespace made — or found again — and recorded.
///
/// First, so that a start which cannot happen costs nothing: a server that
/// does not answer, a login nothing gives, or a login the server will not
/// let make a database stops the start here with nothing made, and the
/// worktree's processes as they were. Nothing it does is undone by a later
/// failure either: a namespace is the worktree's until `rm`.
pub(super) fn prepare(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    progress: &dyn Fn(&str),
) -> Result<Ready> {
    let plan = plan(paths, config);
    let mut servers: Vec<(&Target, namespace::Server<'_>)> = Vec::new();
    for target in &plan.targets {
        let server = server_for(paths, config, target)?;
        server.ping()?;
        servers.push((target, server));
    }
    let mut fresh = false;
    let mut namespaces = Vec::new();
    for (target, server) in &servers {
        let (namespace, made) = match target.namespace.kind {
            NamespaceKind::Database => ensure_database(paths, name, target, server, progress)?,
            NamespaceKind::Slot => ensure_slot(paths, name, target, server, progress)?,
        };
        // A slot given out is empty and needs no schema; only a database
        // made just now does.
        fresh |= made && namespace.kind == NamespaceKind::Database;
        progress(&format!(
            "{}: {}{}",
            target.service,
            own(&namespace),
            if made { ", made just now" } else { "" }
        ));
        namespaces.push(namespace);
    }
    for line in plan.shared_lines() {
        progress(&line);
    }
    Ok(Ready {
        plan,
        namespaces,
        fresh,
    })
}

/// `own database northwind_traders__feat_x`, `slot 3` — what a worktree
/// got in one service, as a line says it.
pub(super) fn own(namespace: &crate::state::NamespaceRecord) -> String {
    match namespace.kind {
        NamespaceKind::Database => format!("own database {}", namespace.name),
        NamespaceKind::Slot => format!("slot {}", namespace.name),
    }
}

/// The server one target lives on, with the login to reach it.
pub(super) fn server_for<'a>(
    paths: &PandoPaths,
    config: &Config,
    target: &'a Target,
) -> Result<namespace::Server<'a>> {
    let file = paths.config_file();
    let login = match namespace::find_login(
        &main_env(paths, config),
        config,
        &target.service,
        &target.keys,
        target.namespace.user,
        &file,
    )
    .map_err(|unresolved| unreadable_login(&target.service, &file, unresolved))?
    {
        Some(login) => login,
        None if !target.namespace.user => Login::none(),
        // Nothing the developer wrote gives one: the administrator the
        // container's own environment keeps does, and nobody is asked.
        None => match admin_server(paths, target) {
            Some(admin) => return Ok(admin),
            None => bail!(
                "nothing gives pando a login for {}'s namespaces — the main checkout's env files \
             have none beside {}. `pando start` on a terminal asks for one; or write \
             [namespaced.{}] with a user and a password in {}",
                target.service,
                target.keys.join(", "),
                target.service,
                file.display()
            ),
        },
    };
    Ok(namespace::Server {
        service: &target.service,
        recipe: &target.namespace,
        host: target.host.clone(),
        port: target.port,
        login,
        bin_dir: paths.home.join("bin"),
        runner: namespace::Runner::Host,
    }
    .reach())
}

/// The login each target needs and nothing gives, asked for now: what
/// `resolve_for_start` does for a namespaced start before anything runs.
///
/// An answer is written to pando's own file, and read back from there into
/// `config`, so the start this is for finds it in the config it is handed.
pub(super) fn ask_for_logins(
    paths: &PandoPaths,
    config: &mut Config,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    for target in plan(paths, config).targets {
        if !target.namespace.user
            || config
                .namespaced
                .get(&target.service)
                .is_some_and(|login| login.has_login())
        {
            continue;
        }
        // The main checkout's own login, or the container's administrator:
        // either is a login nobody has to type.
        let env = main_env(paths, config);
        let found = namespace::find_login(
            &env,
            config,
            &target.service,
            &target.keys,
            true,
            &paths.config_file(),
        );
        if matches!(found, Ok(Some(_))) || container_admin(paths, &target).is_some() {
            continue;
        }
        let login = namespace_login(
            paths,
            config,
            &target.service,
            &target.keys,
            true,
            ask,
            progress,
        )?;
        if login.from.starts_with("[namespaced.")
            && let Some(written) = config::load(paths)?
                .config
                .namespaced
                .get(&target.service)
                .cloned()
        {
            config.namespaced.insert(target.service.clone(), written);
        }
    }
    Ok(())
}

/// This worktree's database on one server: the one state records, found
/// again — or made again, if somebody dropped it by hand — else a new one
/// under the first of its two names the server does not already have.
///
/// A worktree has one database on a server. Another start of it running
/// beside this one — the TUI's and the CLI's — may make and record one
/// after this one read state; that one is the worktree's then, and is
/// never taken for somebody else's.
///
/// Returns it, recorded, and whether it was made just now.
fn ensure_database(
    paths: &PandoPaths,
    name: &str,
    target: &Target,
    server: &namespace::Server<'_>,
    progress: &dyn Fn(&str),
) -> Result<(crate::state::NamespaceRecord, bool)> {
    let wanted = |candidate: &str| crate::state::NamespaceRecord {
        service: target.service.clone(),
        recipe: target.recipe.clone(),
        kind: NamespaceKind::Database,
        host: target.host.clone(),
        port: target.port,
        name: candidate.to_string(),
        main: target.main.clone(),
        mains: target.mains.clone(),
        keys: target.keys.clone(),
        used_at: chrono::Utc::now(),
    };
    let store = crate::state::load(&paths.state_file())?;
    // A server is the machine's, and a second clone of the repository on
    // it names its worktrees' databases the same way. One another project
    // records is never made or taken here: after a server reset, the
    // other clone made this name again, and a start here adopted its live
    // database for this worktree.
    let others = other_projects(paths);
    if let Some(recorded) = recorded_database(&store, name, target) {
        if let Some(whose) = in_another_project(&others, &wanted(&recorded.name)) {
            bail!(
                "{}: {} on {} is recorded for {whose} as well, and pando cannot tell whose it \
                 is, so it runs neither on it — `pando rm {name}` in the project it is not \
                 for forgets it there and drops nothing, and the other goes on using it",
                target.service,
                recorded.name,
                server.address()
            );
        }
        if server.exists(&recorded.name)? {
            return Ok((record(paths, name, wanted(&recorded.name))?, false));
        }
        // Made by pando for this worktree, and gone: somebody dropped it.
        // The same name, made again and empty.
        progress(&format!(
            "{}: {} is gone from {}, so it is made again, empty",
            target.service,
            recorded.name,
            server.address()
        ));
        // Made again only if pando makes it: one that appeared between the
        // two questions was made by something else, and is not this
        // worktree's to run on, nor to drop later.
        if let namespace::Created::AlreadyThere =
            create(server, &recorded.name, &target.main, progress)?
        {
            bail!(
                "{}: {} was gone from {} and is there again, made by something else since — \
                 pando runs this worktree only on a database it made; see what made it, then \
                 start again",
                target.service,
                recorded.name,
                server.address()
            );
        }
        return Ok((record(paths, name, wanted(&recorded.name))?, true));
    }
    let names = namespace::database_names(
        &target.main,
        paths.project_id(),
        name,
        target.namespace.max_name(),
    )?;
    for candidate in &names {
        // Another worktree's, as state knows it, is not this one's to use,
        // nor one another project records.
        if recorded_elsewhere(&store, name, &wanted(candidate)).is_some()
            || in_another_project(&others, &wanted(candidate)).is_some()
        {
            continue;
        }
        match create(server, candidate, &target.main, progress)? {
            namespace::Created::Made => {
                // Recorded only while no other start of this worktree has
                // recorded one since: with two, the app would run on the
                // one whose start spawned first, and the next start would
                // move it to the one recorded first.
                let made = wanted(candidate);
                if record_if(paths, name, &made, |store| {
                    recorded_database(store, name, target).is_none()
                })? {
                    return Ok((made, true));
                }
                let store = crate::state::load(&paths.state_file())?;
                let Some(first) = recorded_database(&store, name, target) else {
                    bail!(
                        "another command changed {name}'s record while this start made \
                         {candidate}, so nothing was started — start it again"
                    );
                };
                progress(&format!(
                    "{}: another start of this worktree recorded {} while this one made \
                     {candidate}, so {} is its database — {candidate} is left on {}, empty, and \
                     `pando doctor` lists it",
                    target.service,
                    first.name,
                    first.name,
                    server.address()
                ));
                return Ok((record(paths, name, wanted(&first.name))?, false));
            }
            namespace::Created::AlreadyThere => {
                // Made and recorded by another start of this worktree since
                // this one read state: its own all the same.
                if let Some(first) =
                    recorded_database(&crate::state::load(&paths.state_file())?, name, target)
                {
                    return Ok((record(paths, name, wanted(&first.name))?, false));
                }
                progress(&format!(
                    "{}: {candidate} is already on {} and pando did not make it, so it is left \
                     alone",
                    target.service,
                    server.address()
                ));
            }
        }
    }
    bail!(
        "{} on {} already has {}, and pando made none of them — drop them there if they are \
         leftovers, and start again",
        target.service,
        server.address(),
        names.join(" and ")
    )
}

/// [`namespace::Server::create`], and when the server refuses the login,
/// the grant the developer would run by hand run instead by the server's
/// own administrator — inside the container that runs it, with the login
/// its environment keeps — and the database made again. With no such
/// administrator the refusal stands, with its grant to run once.
fn create(
    server: &namespace::Server<'_>,
    name: &str,
    main: &str,
    progress: &dyn Fn(&str),
) -> Result<namespace::Created> {
    match server.create(name, main) {
        Err(e) if e.is::<namespace::Denied>() => {
            let Some(admin) = server.admin() else {
                return Err(e);
            };
            let grant = server
                .grant_as(&admin, main)
                .map_err(|grant_failed| e.context(format!("{grant_failed:#}")))?;
            progress(&format!(
                "{}: the login from {} may not make databases, so pando gave it the right, as \
                 {}: {grant}",
                server.service, server.login.from, admin.login.from
            ));
            server.create(name, main)
        }
        made => made,
    }
}

/// A target's server reached as its own administrator, inside the
/// container that runs it: what stands in for a login nobody wrote down,
/// so nobody is asked.
fn admin_server<'a>(paths: &PandoPaths, target: &'a Target) -> Option<namespace::Server<'a>> {
    let probe = namespace::Server {
        service: &target.service,
        recipe: &target.namespace,
        host: target.host.clone(),
        port: target.port,
        login: Login::none(),
        bin_dir: paths.home.join("bin"),
        runner: namespace::Runner::Host,
    }
    .reach();
    probe.admin()
}

/// Whether [`admin_server`] has one for this target.
fn container_admin(paths: &PandoPaths, target: &Target) -> Option<Login> {
    admin_server(paths, target).map(|admin| admin.login)
}

/// The database a worktree's record names for a target, if it names one:
/// the same service and main database, on the same server.
fn recorded_database(
    store: &crate::state::State,
    name: &str,
    target: &Target,
) -> Option<crate::state::NamespaceRecord> {
    store
        .worktrees
        .get(name)?
        .namespaces
        .iter()
        .find(|ns| {
            let here = crate::state::NamespaceRecord {
                host: target.host.clone(),
                port: target.port,
                ..(*ns).clone()
            };
            ns.service == target.service
                && ns.kind == NamespaceKind::Database
                && ns.main == target.main
                && namespace::same_namespace(ns, &here)
        })
        .cloned()
}

/// This worktree's slot on one server: the one state records, else the
/// first one no worktree holds — of this project or of any other pando
/// keeps state for — and the server says is empty.
///
/// Empty, not merely unrecorded: a slot with keys in it that no worktree
/// of this project holds is somebody else's — another project's, or the
/// developer's own — and emptying it later would destroy their data. And
/// unrecorded, not merely empty: another project's worktree whose app has
/// not written yet holds an empty slot too. When every slot is held the
/// start stops and says by whom; a start that can ask has already been
/// asked which one to free.
///
/// The one state records is kept only while it is this worktree's alone
/// and not the main checkout's: one the main checkout's env files name, or
/// one another worktree's record names too — of this project or another —
/// is let go, never emptied, and a new one given, with a line saying why.
/// While the worktree runs on it, it is kept all the same, and the line
/// says how to move it.
fn ensure_slot(
    paths: &PandoPaths,
    name: &str,
    target: &Target,
    server: &namespace::Server<'_>,
    progress: &dyn Fn(&str),
) -> Result<(crate::state::NamespaceRecord, bool)> {
    let slot = |n: &str| crate::state::NamespaceRecord {
        service: target.service.clone(),
        recipe: target.recipe.clone(),
        kind: NamespaceKind::Slot,
        host: target.host.clone(),
        port: target.port,
        name: n.to_string(),
        main: target.main.clone(),
        mains: target.mains.clone(),
        keys: target.keys.clone(),
        used_at: chrono::Utc::now(),
    };
    // Every project's, from reading the others' records to writing this
    // one's: each project's own lock is its own, and two starts in two
    // projects that each read the other's records before either wrote
    // would both be given the same empty slot, or both let go of one
    // their records share. Taken before the project's lock whenever both
    // are held, and never while it is.
    let _slots = crate::state::lock(&paths.slots_lock_file())?;
    let others = other_projects(paths);
    let mut store = crate::state::load(&paths.state_file())?;
    let recorded = store.worktrees.get(name).and_then(|record| {
        record
            .namespaces
            .iter()
            .find(|ns| {
                ns.service == target.service && namespace::same_namespace(ns, &slot(&ns.name))
            })
            .map(|ns| ns.name.clone())
    });
    if let Some(recorded) = recorded {
        let kept = slot(&recorded);
        match hold_on(paths, name, target, &kept, &others)? {
            Held::Kept => return Ok((kept, false)),
            Held::Running(why) => {
                progress(&format!(
                    "{}: slot {recorded} is kept while this worktree runs — {why} — stop it and \
                     start it again, and it gets one of its own",
                    target.service
                ));
                return Ok((kept, false));
            }
            Held::LetGo(why) => progress(&format!(
                "{}: slot {recorded} is let go, not emptied — {why} — and this worktree gets one \
                 of its own",
                target.service
            )),
            Held::Gone => {}
        }
        store = crate::state::load(&paths.state_file())?;
    }
    let holders = slot_holders(&store, target);
    none_while_unread(&others, target)?;
    let elsewhere = slots_elsewhere(&others, target);
    let mut unusable = Unusable::default();
    let mut slots = unheld(target, &holders, &elsewhere);
    while let Some(n) = first_empty(server, &slots, &mut unusable)? {
        // Taken under the lock, against state as it is by now: another
        // worktree's start running beside this one may have been given it
        // first, and neither app has written to it for its size to show.
        let given = slot(&n.to_string());
        if record_if(paths, name, &given, |store| {
            recorded_elsewhere(store, name, &given).is_none()
        })? {
            return Ok((given, true));
        }
        slots.retain(|m| *m > n);
    }
    let store = crate::state::load(&paths.state_file())?;
    let holders = slot_holders(&store, target);
    let held: Vec<String> = holders
        .iter()
        .map(Holder::describe)
        .chain(elsewhere.iter().map(Elsewhere::describe))
        .collect();
    // A terminal start offers only a stopped worktree whose slot the guard
    // would empty, so only then is it said to ask.
    let mains: Vec<&str> = target.mains.iter().map(String::as_str).collect();
    let asks = holders.iter().any(|holder| {
        holder.worktree != name
            && !holder.running
            && namespace::may_drop(&store, &holder.worktree, &holder.namespace, &mains, &others)
                .is_ok()
    });
    let reasons = unusable.reasons();
    if reasons.is_empty() {
        bail!(
            "every slot of {} on {} that pando gives out is held — {}. {}`pando rm` of one you no \
             longer need frees its slot with it{}",
            target.service,
            server.address(),
            held.join(", "),
            match asks {
                true => "`pando start` on a terminal asks which stopped one to free; ",
                false => "",
            },
            elsewhere_hint(paths, &elsewhere)
        );
    }
    bail!(
        "no slot of {} on {} is free: {}{}{}{}",
        target.service,
        server.address(),
        reasons.join("; "),
        match held.is_empty() {
            true => String::new(),
            false => format!("; the rest are held — {}", held.join(", ")),
        },
        match asks {
            true => ". `pando start` on a terminal asks which stopped one to free",
            false => "",
        },
        elsewhere_hint(paths, &elsewhere)
    )
}

/// The slots pando gives out on a target's server that no worktree
/// records, of this project or another, in order.
fn unheld(target: &Target, holders: &[Holder], elsewhere: &[Elsewhere]) -> Vec<u32> {
    allocatable(target)
        .into_iter()
        .filter(|n| {
            !holders.iter().any(|holder| holder.slot == *n)
                && !elsewhere.iter().any(|other| other.slot == *n)
        })
        .collect()
}

/// Why the slots nobody records were not given out, as [`first_empty`]
/// found them.
#[derive(Debug, Default)]
struct Unusable {
    /// Each one the server says holds keys.
    full: Vec<u32>,
    /// The one the server says is out of range, and what it said: a
    /// server with fewer slots than its recipe says has none from there on.
    ended: Option<(u32, String)>,
}

impl Unusable {
    /// One phrase for each reason a slot was not free.
    fn reasons(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.full.is_empty() {
            out.push(format!(
                "{} {} keys no worktree of this project records, and pando never takes a slot \
                 somebody else is using",
                self.full
                    .iter()
                    .map(|n| format!("slot {n}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                match self.full.len() {
                    1 => "holds",
                    _ => "hold",
                }
            ));
        }
        if let Some((n, why)) = &self.ended {
            out.push(format!(
                "slot {n} could not be asked how full it is, so none from it on is given out — \
                 {why}"
            ));
        }
        out
    }
}

/// What a Redis answers for a slot past its last one: `ERR DB index is
/// out of range`.
const PAST_THE_LAST: &str = "out of range";

/// The first of `slots` the server says is empty, asked in order, with
/// every one passed on the way written into `unusable`.
///
/// A slot the server says is out of range is one it does not have — a
/// Redis with fewer databases than its recipe's `slots` — and none after
/// it is asked. Any other failure to size one is the start's: a timeout,
/// a dropped connection or a refused login says nothing about which slots
/// are free, and taking it for the end would ask which stopped worktree's
/// keys to delete while an empty one may be there.
fn first_empty(
    server: &namespace::Server<'_>,
    slots: &[u32],
    unusable: &mut Unusable,
) -> Result<Option<u32>> {
    for &n in slots {
        match server.size(n) {
            Ok(0) => return Ok(Some(n)),
            Ok(_) => unusable.full.push(n),
            Err(e) if format!("{e:#}").contains(PAST_THE_LAST) => {
                unusable.ended = Some((n, format!("{e:#}")));
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// What became of the slot a worktree's record names, as [`hold_on`]
/// found it under the lock.
enum Held {
    /// Still this worktree's alone, and marked as used now.
    Kept,
    /// Not this worktree's alone, and why, but kept and marked as used
    /// all the same, because the worktree runs on it.
    Running(String),
    /// Taken out of this worktree's record, unemptied, and why.
    LetGo(String),
    /// No longer in its record: a start that freed it since has emptied
    /// it, and may be giving it out.
    Gone,
}

/// Whether a worktree goes on with the slot its record names, decided
/// under the lock against state as it is by then.
///
/// Kept only while the record still names it, the main checkout's env
/// files do not, and no other worktree's record does — of this project or
/// of another in `others` — nor another project's main checkout's. Writing
/// back one a start has freed since would hand one slot to two worktrees.
/// One a main checkout names is its data. And one two records name — a
/// state written before slots were taken under the lock, or edited by
/// hand — is shared by two apps already: this worktree lets it go as the
/// other's, rather than go on sharing it or stop every start of both. The
/// caller holds the slots lock `others` was read under, so two projects'
/// starts never both let one go.
///
/// Except while the worktree runs on it. A start that leaves its processes
/// running — one already up, a `restart --only` — cannot move them, and a
/// restart that would could still fail after the slot went; a record
/// naming another slot while they go on using this one leaves this one to
/// the other record alone, and that worktree's `rm` would empty it under
/// them. So it is let go at the first start after the worktree stops. A
/// worktree running shared or isolated is not on it, and the start that
/// makes it namespaced replaces every process, so that start lets it go.
fn hold_on(
    paths: &PandoPaths,
    name: &str,
    target: &Target,
    kept: &crate::state::NamespaceRecord,
    others: &[(String, std::result::Result<crate::state::State, String>)],
) -> Result<Held> {
    let _lock = crate::state::lock(&paths.lock_file())?;
    let mut store = crate::state::load(&paths.state_file())?;
    let own = |ns: &crate::state::NamespaceRecord| {
        ns.service == kept.service && namespace::same_namespace(ns, kept)
    };
    let recorded = store
        .worktrees
        .get(name)
        .is_some_and(|record| record.namespaces.iter().any(own));
    if !recorded {
        return Ok(Held::Gone);
    }
    let held = match not_alone(&store, name, target, kept, others) {
        None => Held::Kept,
        Some(why) if runs_on_its_slots(paths, &store, name) => Held::Running(why),
        Some(why) => Held::LetGo(why),
    };
    let record = store.worktrees.get_mut(name).expect("just read");
    match &held {
        Held::LetGo(_) => record.namespaces.retain(|ns| !own(ns)),
        _ => keep(record, kept),
    }
    crate::state::save(&paths.state_file(), &store)?;
    Ok(held)
}

/// Why the slot a worktree's record names is not its alone, when it is
/// not: the main checkout's env files name it, another worktree's record
/// does too — of this project, or of another in `others` — or another
/// project's records say its main checkout uses it.
fn not_alone(
    store: &crate::state::State,
    name: &str,
    target: &Target,
    slot: &crate::state::NamespaceRecord,
    others: &[(String, std::result::Result<crate::state::State, String>)],
) -> Option<String> {
    let n = slot.name.trim().parse::<u32>().ok();
    let mains = target
        .mains
        .iter()
        .any(|main| n.is_some() && main.trim().parse::<u32>().ok() == n);
    match (mains, recorded_elsewhere(store, name, slot)) {
        (true, _) => Some("the main checkout's env files name it".to_string()),
        (false, Some(other)) => Some(format!("{other}'s record names it too")),
        (false, None) => slots_elsewhere(others, target)
            .into_iter()
            .find(|other| Some(other.slot) == n)
            .map(|other| other.names_it()),
    }
}

/// Whether anything of a worktree runs, as a start decides what it leaves
/// up: a process starting or running, and alive by the rule `status`
/// reads it with — a portless one whose leader backgrounded it and
/// returned among them. A failed one is replaced by the next start.
fn runs(record: &crate::state::WorktreeRecord) -> bool {
    record.processes.values().any(|p| {
        matches!(
            p.phase,
            crate::state::Phase::Starting { .. } | crate::state::Phase::Running { .. }
        ) && p.alive(crate::process::is_alive, crate::process::group_alive)
    })
}

/// Whether anything of a worktree may still be on the slots its record
/// names: a process alive by the rule `status` reads it with, whatever its
/// phase. A start that failed its readiness wait kills nothing, and its
/// app can go on serving. One the sweep has signalled is gone, whatever
/// its pid says now.
fn in_use(record: &crate::state::WorktreeRecord) -> bool {
    record
        .processes
        .values()
        .any(|p| !p.swept && p.alive(crate::process::is_alive, crate::process::group_alive))
}

/// Whether a worktree's app is on the slots its record names, for a
/// namespaced start to leave it there: something of it runs, and in
/// namespaced mode, as a record written for this worktree says. Running
/// shared or isolated it is on other data, and a record left at another
/// path has its processes stopped by the start that finds it.
fn runs_on_its_slots(paths: &PandoPaths, store: &crate::state::State, name: &str) -> bool {
    store.worktrees.get(name).is_some_and(runs)
        && super::lifecycle::recorded_mode(paths, name) == crate::state::ServiceMode::Namespaced
}

/// The slots pando may give a worktree on a target's server: every one it
/// has but 0 — where an app that names no slot keeps its keys — and every
/// one the main checkout's env files name.
fn allocatable(target: &Target) -> Vec<u32> {
    let slots = target.namespace.slots.unwrap_or(0);
    let mains: Vec<u32> = target
        .mains
        .iter()
        .filter_map(|main| main.parse::<u32>().ok())
        .collect();
    (1..slots).filter(|n| !mains.contains(n)).collect()
}

/// A worktree holding a slot on a target's server.
#[derive(Debug, Clone)]
struct Holder {
    worktree: String,
    slot: u32,
    namespace: crate::state::NamespaceRecord,
    /// Something of it may be on the slot: [`in_use`]. Never offered to
    /// free, and never emptied.
    running: bool,
}

impl Holder {
    /// `feat+x (slot 3, running)`.
    fn describe(&self) -> String {
        format!(
            "{} (slot {}, {})",
            self.worktree,
            self.slot,
            match self.running {
                true => "running".to_string(),
                false => format!("last ran {}", since(self.namespace.used_at)),
            }
        )
    }
}

/// Every worktree holding a slot on this target's server, as state says.
fn slot_holders(store: &crate::state::State, target: &Target) -> Vec<Holder> {
    let mut out = Vec::new();
    for (worktree, record) in &store.worktrees {
        let running = in_use(record);
        for ns in &record.namespaces {
            let here = crate::state::NamespaceRecord {
                host: target.host.clone(),
                port: target.port,
                kind: NamespaceKind::Slot,
                ..ns.clone()
            };
            if !namespace::same_namespace(ns, &here) {
                continue;
            }
            if let Ok(slot) = ns.name.parse::<u32>() {
                out.push(Holder {
                    worktree: worktree.clone(),
                    slot,
                    namespace: ns.clone(),
                    running,
                });
            }
        }
    }
    out.sort_by_key(|holder| holder.slot);
    out
}

/// Every other project pando keeps state for on this machine, by id: its
/// state, or why that could not be read.
///
/// A Redis on a port is the machine's, not a project's: a slot another
/// project's worktree holds, or its main checkout uses, is not this one's
/// to give out or to empty. A state file that cannot be read is kept as
/// why, so [`namespace::may_drop`] empties nothing while one cannot.
fn other_projects(
    paths: &PandoPaths,
) -> Vec<(String, std::result::Result<crate::state::State, String>)> {
    let Ok(entries) = std::fs::read_dir(paths.projects_dir()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().to_string();
        let file = entry.path().join("state.json");
        if id == paths.project_id() || !file.is_file() {
            continue;
        }
        out.push((id, crate::state::load(&file).map_err(|e| format!("{e:#}"))));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// A slot on a target's server that another project records: one a
/// worktree of it holds, or the one its main checkout uses.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Elsewhere {
    slot: u32,
    /// `feat+x of project shop-1a2b3c4d`.
    whose: String,
    /// The project's id: its directory under pando's home.
    project: String,
    /// It is the one its main checkout uses, not a worktree's.
    main: bool,
    /// The checkout the slot is recorded for is gone from this machine: a
    /// worktree's directory, or the repository it was linked from.
    gone: bool,
}

impl Elsewhere {
    /// Why a slot this worktree's record names is not its alone, when
    /// another project records it: `feat+x of project shop-1a2b3c4d's
    /// record names it too`, `the main checkout of project shop-1a2b3c4d
    /// uses it`.
    fn names_it(&self) -> String {
        match self.main {
            true => format!("{} uses it", self.whose),
            false => format!("{}'s record names it too", self.whose),
        }
    }

    /// `slot 3 (feat+x of project shop-1a2b3c4d)`, and `, whose checkout
    /// is gone` inside the brackets when it is.
    fn describe(&self) -> String {
        format!(
            "slot {} ({}{})",
            self.slot,
            self.whose,
            match self.gone {
                true => ", whose checkout is gone",
                false => "",
            }
        )
    }
}

/// Where the records of the other projects holding slots are, for the end
/// of an "every slot is held" error: nothing in this project can let them
/// go, and a project whose repository is gone holds its slots until its
/// directory is removed. Empty when no other project holds one.
fn elsewhere_hint(paths: &PandoPaths, elsewhere: &[Elsewhere]) -> String {
    let mut dirs: Vec<String> = elsewhere
        .iter()
        .map(|other| {
            paths
                .projects_dir()
                .join(&other.project)
                .display()
                .to_string()
        })
        .collect();
    dirs.sort();
    dirs.dedup();
    if dirs.is_empty() {
        return String::new();
    }
    format!(
        ". Another project's slots are held by its records in {}: one whose repository is gone \
         holds them until that directory is removed",
        dirs.join(", ")
    )
}

/// Whether the repository the worktree at `path` was linked from is gone:
/// git writes `gitdir: <repository>/.git/worktrees/<name>` into its `.git`
/// file, and that is not there any more.
fn repository_gone(path: &std::path::Path) -> bool {
    crate::worktree::linked_gitdir(path).is_some_and(|gitdir| !gitdir.exists())
}

/// Stops a start that would give out a slot while another project's state
/// cannot be read: it may record any of them, as a worktree's or as its
/// main checkout's, and an empty one given out here would be shared by two
/// apps, with [`namespace::may_drop`] refusing to empty it for either.
fn none_while_unread(
    others: &[(String, std::result::Result<crate::state::State, String>)],
    target: &Target,
) -> Result<()> {
    if let Some((project, why)) = others
        .iter()
        .find_map(|(project, other)| Some((project, other.as_ref().err()?)))
    {
        bail!(
            "no slot of {} on {}:{} is given out while project {project}'s state cannot be read, \
             since it may record any of them — {why}",
            target.service,
            target.host,
            target.port
        );
    }
    Ok(())
}

/// Every slot pando could give out on this target's server that other
/// projects' states record, as a worktree's or as their main checkout's
/// own. A state that could not be read is passed over here: no slot is
/// given out while one cannot be, by [`none_while_unread`].
fn slots_elsewhere(
    others: &[(String, std::result::Result<crate::state::State, String>)],
    target: &Target,
) -> Vec<Elsewhere> {
    let allocatable = allocatable(target);
    let mut out = Vec::new();
    for (project, store) in others {
        let Ok(store) = store else {
            continue;
        };
        for (worktree, record) in &store.worktrees {
            let repository_gone = repository_gone(&record.path);
            for ns in &record.namespaces {
                let here = crate::state::NamespaceRecord {
                    host: target.host.clone(),
                    port: target.port,
                    kind: NamespaceKind::Slot,
                    ..ns.clone()
                };
                if !namespace::same_namespace(ns, &here) {
                    continue;
                }
                if let Ok(slot) = ns.name.trim().parse::<u32>() {
                    out.push(Elsewhere {
                        slot,
                        whose: format!("{worktree} of project {project}"),
                        project: project.clone(),
                        main: false,
                        gone: repository_gone || !record.path.exists(),
                    });
                }
                for main in ns.every_main() {
                    if let Ok(slot) = main.trim().parse::<u32>() {
                        out.push(Elsewhere {
                            slot,
                            whose: format!("the main checkout of project {project}"),
                            project: project.clone(),
                            main: true,
                            gone: repository_gone,
                        });
                    }
                }
            }
        }
    }
    out.retain(|other| allocatable.contains(&other.slot));
    out.sort();
    out.dedup();
    out
}

/// `3 days ago`, `just now` — when a stopped worktree last ran, for a
/// choice about whose data to empty.
fn since(at: chrono::DateTime<chrono::Utc>) -> String {
    let ago = chrono::Utc::now() - at;
    match ago.num_minutes() {
        m if m < 1 => "just now".to_string(),
        m if m < 60 => format!("{m} min ago"),
        m if m < 60 * 24 => format!("{} h ago", m / 60),
        m => format!("{} days ago", m / (60 * 24)),
    }
}

/// Decision 9: when no slot of a target's server is free for this
/// worktree, which has none — every one held, or the ones nobody holds
/// full of somebody else's keys or past what the server has — asks which
/// stopped worktree gives its slot up, and empties and frees the one
/// chosen, through the guard, before anything else of this start happens.
///
/// The server is asked only when a stopped worktree holds one it could
/// offer. A running worktree is never offered: its app is using the slot.
/// Nor is a stopped one whose slot the guard would not empty. When every
/// holder is running, or refused for a reason this start does not clear,
/// the start stops naming them and why. A script gets exit 3 with the
/// list, as for any question.
pub(super) fn free_slots_if_full(
    paths: &PandoPaths,
    config: &Config,
    name: &str,
    ask: Ask<'_>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    for target in plan(paths, config).targets {
        if target.namespace.kind != NamespaceKind::Slot {
            continue;
        }
        let store = crate::state::load(&paths.state_file())?;
        let holders = slot_holders(&store, &target);
        let projects = other_projects(paths);
        // A slot this worktree's record names is one its start goes on
        // with, unless [`hold_on`] will let it go: then the start needs a
        // new one, and is asked now, not only on its next attempt.
        if holders.iter().any(|holder| {
            holder.worktree == name
                && (not_alone(&store, name, &target, &holder.namespace, &projects).is_none()
                    || runs_on_its_slots(paths, &store, name))
        }) {
            continue;
        }
        // The slots it names all the same, which its start lets go.
        let ours: Vec<u32> = holders
            .iter()
            .filter(|holder| holder.worktree == name)
            .map(|holder| holder.slot)
            .collect();
        let holders: Vec<Holder> = holders
            .into_iter()
            .filter(|holder| holder.worktree != name)
            .collect();
        // Before anything is asked: the start this is for gives out no
        // slot then, and freeing one would be refused by the guard anyway.
        none_while_unread(&projects, &target)?;
        let others = slots_elsewhere(&projects, &target);
        let elsewhere: Vec<String> = others.iter().map(Elsewhere::describe).collect();
        // A stopped worktree is offered only where the guard would empty
        // its slot once it is chosen: not one another record names too —
        // this worktree's own among them, until its start lets it go — nor
        // one the main checkout's env files name. One it would not is said
        // why, and is no answer to ask for.
        let mains: Vec<&str> = target.mains.iter().map(String::as_str).collect();
        let mut stopped: Vec<&Holder> = Vec::new();
        let mut refused: Vec<(&Holder, String)> = Vec::new();
        for holder in holders.iter().filter(|holder| !holder.running) {
            match namespace::may_drop(
                &store,
                &holder.worktree,
                &holder.namespace,
                &mains,
                &projects,
            ) {
                Ok(()) => stopped.push(holder),
                Err(why) => refused.push((holder, format!("{why:#}"))),
            }
        }
        let running: Vec<String> = holders
            .iter()
            .filter(|holder| holder.running)
            .map(Holder::describe)
            .collect();
        let unheld = unheld(&target, &holders, &others);
        let mut unusable = Unusable::default();
        if !unheld.is_empty() {
            if stopped.is_empty() {
                continue;
            }
            let server = server_for(paths, config, &target)?;
            server.ping()?;
            if first_empty(&server, &unheld, &mut unusable)?.is_some() {
                continue;
            }
        }
        if stopped.is_empty() && refused.is_empty() {
            bail!(
                "every slot of {} on {}:{} is held by a running worktree{} — {}. Stop one of this \
                 project's, and its slot can be freed{}",
                target.service,
                target.host,
                target.port,
                match elsewhere.is_empty() {
                    true => "",
                    false => " or another project",
                },
                running
                    .iter()
                    .chain(&elsewhere)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                elsewhere_hint(paths, &others)
            );
        }
        // Nothing a question could free. One the guard refuses because
        // this worktree's record names its slot too is offered by the next
        // start, once this one has let that slot go: the start goes on, and
        // says who holds what when it finds none free. Any other refusal
        // holds at every start, so this one stops here, saying why.
        if stopped.is_empty() {
            if refused
                .iter()
                .any(|(holder, _)| ours.contains(&holder.slot))
            {
                continue;
            }
            bail!(
                "every slot of {} on {}:{} is held, and pando may empty none a stopped worktree \
                 holds — {}{}. {}{}",
                target.service,
                target.host,
                target.port,
                refused
                    .iter()
                    .map(|(holder, why)| format!("{}: {why}", holder.describe()))
                    .collect::<Vec<_>>()
                    .join("; "),
                match running.is_empty() && elsewhere.is_empty() {
                    true => String::new(),
                    false => format!(
                        "; the rest are held — {}",
                        running
                            .iter()
                            .chain(&elsewhere)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                },
                match running.is_empty() {
                    true => "`pando rm` of one you no longer need lets its slot go with it",
                    false => "Stop one of this project's running ones, and its slot can be freed",
                },
                elsewhere_hint(paths, &others)
            );
        }
        let reasons = unusable.reasons();
        let question = Question {
            slot: Slot::FreeSlot,
            prompt: format!(
                "{} slot of {} on {}:{} is {}. Which stopped worktree gives up its slot?",
                match reasons.is_empty() {
                    true => "Every",
                    false => "No",
                },
                target.service,
                target.host,
                target.port,
                match reasons.is_empty() {
                    true => "held",
                    false => "free",
                }
            ),
            options: stopped
                .iter()
                .map(|holder| {
                    (
                        holder.worktree.clone(),
                        format!(
                            "slot {}, last ran {}",
                            holder.slot,
                            since(holder.namespace.used_at)
                        ),
                    )
                })
                .collect(),
            preselect: None,
            allow_custom: false,
            allow_none: true,
            multi: false,
            checked: Vec::new(),
            details: std::iter::once(
                "the one chosen has its slot emptied — every key in it deleted — and gets a new, \
                 empty one the next time it starts"
                    .to_string(),
            )
            .chain(reasons)
            .chain(
                (!running.is_empty())
                    .then(|| format!("running, so not offered: {}", running.join(", "))),
            )
            .chain(
                refused
                    .iter()
                    .map(|(holder, why)| format!("not offered: {} — {why}", holder.describe())),
            )
            .chain((!elsewhere.is_empty()).then(|| {
                format!(
                    "another project's, so not offered: {}",
                    elsewhere.join(", ")
                )
            }))
            .collect(),
            answer_file: None,
            snippet: String::new(),
        };
        let (answer, _) = answered_by(ask(&question)?);
        let chosen = match answer {
            Answer::Choice(index) => stopped
                .get(index)
                .copied()
                .ok_or_else(|| anyhow::anyhow!("option {index} is not on offer"))?,
            Answer::None => bail!("no slot was freed, so nothing was started"),
            _ => bail!("the slot to free is chosen from the list, not typed"),
        };
        free_slot(paths, config, &target, chosen, progress)?;
    }
    Ok(())
}

/// Empties and releases one worktree's slot: checked against state again
/// under the lock — still recorded, still stopped — then through
/// [`namespace::may_drop`], then emptied, then forgotten.
fn free_slot(
    paths: &PandoPaths,
    config: &Config,
    target: &Target,
    holder: &Holder,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let server = server_for(paths, config, target)?;
    let _lock = crate::state::lock(&paths.lock_file())?;
    let mut store = crate::state::load(&paths.state_file())?;
    let still = slot_holders(&store, target)
        .into_iter()
        .find(|h| h.worktree == holder.worktree && h.slot == holder.slot);
    match still {
        Some(h) if !h.running => {}
        Some(_) => bail!(
            "{} started again while this start was asking, so its slot was not freed",
            holder.worktree
        ),
        None => bail!("{} no longer holds slot {}", holder.worktree, holder.slot),
    }
    namespace::may_drop(
        &store,
        &holder.worktree,
        &holder.namespace,
        &target.mains.iter().map(String::as_str).collect::<Vec<_>>(),
        &other_projects(paths),
    )?;
    server.drop(&holder.namespace.name, &target.main)?;
    if let Some(record) = store.worktrees.get_mut(&holder.worktree) {
        record.namespaces.retain(|ns| ns != &holder.namespace);
    }
    crate::state::save(&paths.state_file(), &store)?;
    progress(&format!(
        "{}: slot {} emptied and freed — {} gets a new, empty one when it next starts",
        target.service, holder.slot, holder.worktree
    ));
    Ok(())
}

/// Writes a namespace into this worktree's record — the moment after the
/// server made it, which is what makes the record pando's word that pando
/// made it — or marks a recorded one as used now.
fn record(
    paths: &PandoPaths,
    name: &str,
    namespace: crate::state::NamespaceRecord,
) -> Result<crate::state::NamespaceRecord> {
    record_if(paths, name, &namespace, |_| true)?;
    Ok(namespace)
}

/// [`record`], only when `may` says so of state as it is under the lock
/// rather than as the start read it before; whether it was written.
fn record_if(
    paths: &PandoPaths,
    name: &str,
    namespace: &crate::state::NamespaceRecord,
    may: impl FnOnce(&crate::state::State) -> bool,
) -> Result<bool> {
    let worktree = super::worktree::find_worktree(paths, name)?;
    let canonical = std::fs::canonicalize(&worktree.path).unwrap_or(worktree.path);
    let _lock = crate::state::lock(&paths.lock_file())?;
    let mut store = crate::state::load(&paths.state_file())?;
    if !may(&store) {
        return Ok(false);
    }
    let record = store
        .worktrees
        .entry(name.to_string())
        .or_insert_with(|| crate::state::WorktreeRecord::new(canonical, false));
    keep(record, namespace);
    crate::state::save(&paths.state_file(), &store)?;
    Ok(true)
}

/// The worktree other than `name` whose record names this namespace, if
/// any does.
pub(super) fn recorded_elsewhere<'a>(
    store: &'a crate::state::State,
    name: &str,
    namespace: &crate::state::NamespaceRecord,
) -> Option<&'a str> {
    store
        .worktrees
        .iter()
        .find(|(other, record)| {
            other.as_str() != name
                && record
                    .namespaces
                    .iter()
                    .any(|ns| namespace::same_namespace(ns, namespace))
        })
        .map(|(other, _)| other.as_str())
}

/// Puts a namespace into a worktree's record, or marks the one already
/// there as used now, with every name the main checkout's own has there
/// today. The caller holds the lock.
pub(super) fn keep(
    record: &mut crate::state::WorktreeRecord,
    namespace: &crate::state::NamespaceRecord,
) {
    match record
        .namespaces
        .iter_mut()
        .find(|ns| ns.service == namespace.service && namespace::same_namespace(ns, namespace))
    {
        Some(existing) => {
            existing.used_at = namespace.used_at;
            if !namespace.mains.is_empty() {
                existing.mains = namespace.mains.clone();
            }
            // A slot's main is the one the main checkout's env files name
            // first today, which [`namespaced_env`] finds it by. A
            // database's name is made from its main, so that one stays.
            if existing.kind == NamespaceKind::Slot {
                existing.main = namespace.main.clone();
            }
        }
        None => record.namespaces.push(namespace.clone()),
    }
}

/// What a namespaced worktree's processes and hooks are told about the
/// services: the main checkout's own addresses, as a shared start tells
/// them, with every key that names a database or a slot naming the
/// worktree's own instead.
///
/// A target with no namespace among `namespaces` is an error, never a
/// shared service: its keys would go on naming the main checkout's
/// database, and a branch's migrations would run against it.
pub(super) fn namespaced_env(
    paths: &PandoPaths,
    config: &Config,
    plan: &Plan,
    namespaces: &[crate::state::NamespaceRecord],
    worktree: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut env = super::services::shared_service_env(paths, config);
    // The worktree's own name for itself, to every process and hook: an
    // app that puts it in front of its index, topic or key names gets
    // data of its own on any service, with nothing for pando to make.
    let project = paths.project_id();
    env.insert(
        namespace::NAMESPACE_ENV.to_string(),
        namespace::worktree_tag(project, worktree),
    );
    for prefixed in &plan.prefixed {
        env.extend(prefixed.values(project, worktree));
    }
    for target in &plan.targets {
        let Some(namespace) = namespaces.iter().find(|ns| {
            ns.service == target.service
                && ns.main == target.main
                && ns.port == target.port
                && ns.kind == target.namespace.kind
        }) else {
            bail!(
                "{} has no namespace of this worktree's own recorded on {}:{}, so pando will not \
                 point it at the main checkout's data — start it namespaced again to make one",
                target.service,
                target.host,
                target.port
            );
        };
        for tell in &target.tells {
            match tell {
                Tell::Key(key) => {
                    env.insert(key.clone(), namespace.name.clone());
                }
                Tell::Url(key) => {
                    let url = match env.get(key) {
                        Some(url) => Some(url.clone()),
                        None => main_env(paths, config).value(key)?,
                    };
                    let address = target.namespace.address();
                    let rewritten = url.map(|url| url.trim().to_string()).and_then(|url| {
                        match address.url_path {
                            true => crate::services::with_url_path(&url, &namespace.name),
                            false => Some(url),
                        }
                    });
                    let Some(mut rewritten) = rewritten else {
                        bail!(
                            "{key} is not a URL pando can point at {}, so it would go on naming \
                             the main checkout's",
                            namespace.name
                        );
                    };
                    // A client that reads a query parameter before the path
                    // would go on using main's through it.
                    for parameter in &address.url_query {
                        rewritten = crate::services::with_url_query_value(
                            &rewritten,
                            parameter,
                            &namespace.name,
                        );
                    }
                    env.insert(key.clone(), rewritten);
                }
            }
        }
    }
    Ok(env)
}

/// What goes with a worktree when it is removed, one phrase a namespace:
/// `drops database northwind_traders__feat_x`, `empties redis slot 3`.
/// What the remove dialog says before anything happens.
pub fn namespaces_rm_drops(record: &crate::state::WorktreeRecord) -> Vec<String> {
    record
        .namespaces
        .iter()
        .map(|ns| match ns.kind {
            NamespaceKind::Database => format!("drops database {}", ns.name),
            NamespaceKind::Slot => format!("empties {} slot {}", ns.service, ns.name),
        })
        .collect()
}

/// Drops every namespace a removed worktree held — each database dropped,
/// each slot emptied — through [`namespace::may_drop`], on the server it
/// was made on.
///
/// Called by `rm` once the worktree itself is gone, so a removal git
/// refused never costs a worktree its data. One pando may not or cannot
/// drop is said, with the command that drops it by hand, and its record
/// goes with the worktree's. After that `doctor` can find a database
/// again by its name, when its recipe can list them; nothing finds a slot
/// again, a number with nothing of the project in it, so its line says
/// this is the last pando says of it — unless another record still names
/// it, and pando goes on showing it as that one's.
///
/// Returns the lines about the ones it left, for a caller that keeps
/// them beside its result: `pando check` does.
pub(super) fn drop_namespaces(
    paths: &PandoPaths,
    store: &crate::state::State,
    name: &str,
    progress: &dyn Fn(&str),
) -> Vec<String> {
    let mut left: Vec<String> = Vec::new();
    let Some(record) = store.worktrees.get(name) else {
        return left;
    };
    let ran_namespaced = record.mode() == crate::state::ServiceMode::Namespaced;
    if record.namespaces.is_empty() && !ran_namespaced {
        return left;
    }
    let mut progress = |line: &str, dropped: bool| {
        if !dropped {
            left.push(line.to_string());
        }
        progress(line);
    };
    // `rm` works with a config that does not load; the logins it might
    // hold are only a fallback to the main checkout's own.
    let config = config::load(paths)
        .map(|loaded| loaded.config)
        .unwrap_or_else(|_| config::load_without_home(paths).config);
    // A prefix is the app's: pando made nothing under it, so it drops
    // nothing, and says what it leaves.
    if ran_namespaced {
        for prefixed in plan(paths, &config).prefixed {
            progress(
                &format!(
                    "{}: what the app wrote under {} is left as it is — a prefix is the app's, \
                     and pando made nothing there to drop",
                    prefixed.service,
                    prefixed
                        .values(paths.project_id(), name)
                        .into_iter()
                        .map(|(_, value)| value)
                        .next()
                        .unwrap_or_default()
                ),
                false,
            );
        }
    }
    let recipes = crate::recipes::Recipes::load(&paths.recipes_dir());
    let targets = plan(paths, &config).targets;
    let others = other_projects(paths);
    for ns in &record.namespaces {
        let target = targets
            .iter()
            .find(|t| t.service == ns.service && t.port == ns.port && t.namespace.kind == ns.kind);
        // Every name the main checkout's env files give its own today,
        // refused whatever the record says.
        let main_now: Vec<&str> = target
            .iter()
            .flat_map(|t| t.mains.iter().map(String::as_str))
            .collect();
        let what = namespace::describe(ns);
        let recipe = recipes
            .get(&ns.recipe)
            .ok()
            .and_then(|loaded| loaded.recipe.namespace.clone());
        // What a line about one left behind ends with, when nothing will
        // find it again once its record is gone: no recipe lists it, and
        // no other record names it — nor may, from a state unread.
        let last = match (ns.kind == NamespaceKind::Database
            && recipe.as_ref().is_some_and(|recipe| recipe.list.is_some()))
            || named_elsewhere(store, name, ns, &others)
        {
            true => "",
            false => " — nothing records it after this, so pando will not mention it again",
        };
        // What the main checkout names today is half of the guard. Unread
        // — an env value it cannot resolve, a service renamed or no longer
        // namespaced, a port moved — it is not "nothing": main may name
        // this very one now, and a drop on the record's word alone would
        // empty it.
        if target.is_none() {
            progress(
                &format!(
                    "{}: {what} is left as it is — pando cannot read what the main checkout's \
                     env files name for {} today, so it cannot be sure this is not the main \
                     checkout's now{last}",
                    ns.service, ns.service
                ),
                false,
            );
            continue;
        }
        if let Err(e) = namespace::may_drop(store, name, ns, &main_now, &others) {
            progress(
                &format!("{}: {what} is left as it is — {e:#}{last}", ns.service),
                false,
            );
            continue;
        }
        let Some(recipe) = recipe else {
            progress(
                &format!(
                    "{}: {what} is left as it is — the recipe {:?} no longer says how to drop \
                     it{last}",
                    ns.service, ns.recipe
                ),
                false,
            );
            continue;
        };
        let keys = match (&ns.keys[..], target) {
            ([], Some(target)) => target.keys.clone(),
            (keys, _) => keys.to_vec(),
        };
        // A login the env files hold and pando cannot read is none here, as
        // a missing one is: the server says whether it needs one, and the
        // line a refusal gets says how to drop it by hand.
        let login = namespace::find_login(
            &main_env(paths, &config),
            &config,
            &ns.service,
            &keys,
            recipe.user,
            &paths.config_file(),
        )
        .ok()
        .flatten()
        .unwrap_or_else(Login::none);
        let server = namespace::Server {
            service: &ns.service,
            recipe: &recipe,
            host: ns.host.clone(),
            port: ns.port,
            login,
            bin_dir: paths.home.join("bin"),
            runner: namespace::Runner::Host,
        }
        .reach();
        // Made as the container's administrator, with no login written
        // anywhere: dropped as the same one.
        let admin = (recipe.user && server.login.user.is_none())
            .then(|| server.admin())
            .flatten();
        let server = admin.unwrap_or(server);
        // A login that may not drop it is no reason to leave it: the
        // server's own administrator, inside its container, may — past
        // the same guard, which has already passed.
        let dropped = match server.drop(&ns.name, &ns.main) {
            Err(e) if e.is::<namespace::Denied>() => match server.admin() {
                Some(admin) => admin.drop(&ns.name, &ns.main),
                None => Err(e),
            },
            dropped => dropped,
        };
        match dropped {
            Ok(()) => progress(
                &format!(
                    "{}: {}",
                    ns.service,
                    match ns.kind {
                        NamespaceKind::Database => format!("dropped database {}", ns.name),
                        NamespaceKind::Slot => format!("emptied slot {}", ns.name),
                    }
                ),
                true,
            ),
            Err(e) => progress(
                &format!(
                    "{}: {what} could not be dropped — {e:#}{}{last}",
                    ns.service,
                    server
                        .by_hand(&ns.name)
                        .map(|command| format!(" — `{command}` drops it by hand"))
                        .unwrap_or_default()
                ),
                false,
            ),
        }
    }
    left
}

/// Whether a record other than `name`'s names this namespace: another
/// worktree's here, or another project's, as a worktree's or as its main
/// checkout's. A project whose state cannot be read may.
fn named_elsewhere(
    store: &crate::state::State,
    name: &str,
    namespace: &crate::state::NamespaceRecord,
    others: &[(String, std::result::Result<crate::state::State, String>)],
) -> bool {
    recorded_elsewhere(store, name, namespace).is_some()
        || others.iter().any(|(_, other)| other.is_err())
        || in_another_project(others, namespace).is_some()
}

/// Who in another project's state names this namespace — `feat+x of
/// project shop-1a2b3c4d`, or `the main checkout of project …` — as far as
/// those states can be read.
fn in_another_project(
    others: &[(String, std::result::Result<crate::state::State, String>)],
    namespace: &crate::state::NamespaceRecord,
) -> Option<String> {
    others.iter().find_map(|(project, other)| {
        let other = other.as_ref().ok()?;
        other.worktrees.iter().find_map(|(worktree, record)| {
            record.namespaces.iter().find_map(|ns| {
                if namespace::same_namespace(ns, namespace) {
                    return Some(format!("{worktree} of project {project}"));
                }
                ns.every_main()
                    .any(|main| {
                        let main = crate::state::NamespaceRecord {
                            name: main.to_string(),
                            ..ns.clone()
                        };
                        namespace::same_namespace(&main, namespace)
                    })
                    .then(|| format!("the main checkout of project {project}"))
            })
        })
    })
}

/// A database named like a worktree's of this project that no pando
/// project's record holds, as far as their records can be read: one `rm`
/// could not drop, one whose record was lost, or one somebody else made
/// under that name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leftover {
    pub service: String,
    /// `host:port`.
    pub address: String,
    pub name: String,
    /// The command that drops it, for a person: pando never drops what it
    /// has no record of. `None` while `unread` is not.
    pub by_hand: Option<String>,
    /// Another project whose state could not be read, and why: it may
    /// hold this database, as a worktree's, so no command drops it.
    pub unread: Option<(String, String)>,
}

/// Every leftover database of this project on the servers it namespaces
/// in, as far as each server says — asked only of a project that runs
/// namespaced worktrees, or whose last `pando check` ran namespaced,
/// because asking is a login to a server.
///
/// Read-only: a listing, never a drop. A server that does not answer, or
/// a login nothing gives, is skipped rather than reported: this is a look
/// for leftovers, and not being able to look is not one. One another
/// project's record holds — a second clone of the repository on the same
/// server names its worktrees' databases the same way — is that project's,
/// and not listed. While another project's state cannot be read, it may
/// hold any of them: each is listed with that project, and with no
/// command.
pub fn namespace_leftovers(
    paths: &PandoPaths,
    config: &Config,
    store: &crate::state::State,
) -> Vec<Leftover> {
    let namespaced = store.worktrees.values().any(|record| {
        !record.namespaces.is_empty() || record.mode == Some(crate::state::ServiceMode::Namespaced)
    }) || crate::setup::CheckRecord::load(paths)
        .is_some_and(|check| check.mode == crate::setup::CheckMode::Namespaced);
    if !namespaced {
        return Vec::new();
    }
    let others = other_projects(paths);
    let stores: Vec<&crate::state::State> = std::iter::once(store)
        .chain(others.iter().filter_map(|(_, other)| other.as_ref().ok()))
        .collect();
    let unread = others
        .iter()
        .find_map(|(project, other)| Some((project.clone(), other.as_ref().err()?.clone())));
    let mut out = Vec::new();
    for target in plan(paths, config).targets {
        if target.namespace.kind != NamespaceKind::Database || target.namespace.list.is_none() {
            continue;
        }
        let Ok(server) = server_for(paths, config, &target) else {
            continue;
        };
        let Ok(names) = server.list(&target.main) else {
            continue;
        };
        for name in names {
            let listed = crate::state::NamespaceRecord {
                service: target.service.clone(),
                recipe: target.recipe.clone(),
                kind: NamespaceKind::Database,
                host: target.host.clone(),
                port: target.port,
                name: name.clone(),
                main: target.main.clone(),
                mains: target.mains.clone(),
                keys: Vec::new(),
                used_at: chrono::Utc::now(),
            };
            let held = stores.iter().any(|store| {
                store.worktrees.values().any(|record| {
                    record
                        .namespaces
                        .iter()
                        .any(|ns| namespace::same_namespace(ns, &listed))
                })
            });
            // A main checkout's own, here or in another project, that
            // happens to look like a namespace: `shop__test` named beside
            // `shop`, or a project whose main database is `shop__legacy`.
            let own = std::iter::once(target.main.as_str())
                .chain(target.mains.iter().map(String::as_str));
            let mains = own.chain(stores.iter().flat_map(|store| {
                store
                    .worktrees
                    .values()
                    .flat_map(|record| record.namespaces.iter())
                    .flat_map(|ns| ns.every_main())
            }));
            if held
                || mains
                    .into_iter()
                    .any(|main| main.eq_ignore_ascii_case(&name))
            {
                continue;
            }
            out.push(Leftover {
                service: target.service.clone(),
                address: server.address(),
                by_hand: match unread {
                    Some(_) => None,
                    None => server.by_hand(&name),
                },
                unread: unread.clone(),
                name,
            });
        }
    }
    out
}

/// One service's namespaced settings, as a program answers them through
/// `init --answers`: `{"<service>": {"recipe": …, "db_env": […],
/// "prefix_env": […]}}`. Never a login: a password is not an answer a
/// file may carry.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamespacedAnswer {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub db_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefix_env: Vec<String>,
}

/// Every service config declares, by the name `[namespaced.<service>]`
/// knows it by.
fn declared_services(config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for service in &config.services {
        match service {
            config::ServiceConfig::Native { .. } => {
                if let Some(entry) = crate::native::Entry::of(service) {
                    out.push(entry.name.to_string());
                }
            }
            config::ServiceConfig::Compose { include, .. } => out.extend(include.iter().cloned()),
        }
    }
    out
}

/// Writes a program's namespaced settings, one `[namespaced.<service>]`
/// table each, in pando's own file for the project, beside any login
/// there — which it never touches.
///
/// Held, before anything is written, to what each can mean: a service the
/// project declares, a recipe there is that knows a namespace or a prefix,
/// and keys that are env keys. `replacing` takes the settings a named
/// service had away first, its login kept.
pub(super) fn write_namespaced_answer(
    paths: &PandoPaths,
    config: &mut Config,
    services: &std::collections::BTreeMap<String, NamespacedAnswer>,
    by: super::questions::Answerer,
    replacing: bool,
    progress: &dyn Fn(&str),
) -> Result<()> {
    if services.is_empty() {
        return Err(by.refuse(
            "namespaced names no service — it takes {\"<service>\": {\"recipe\": …, \
             \"db_env\": […], \"prefix_env\": […]}}"
                .to_string(),
        ));
    }
    let isolated = declared_services(config);
    let mut declared = isolated.clone();
    for (service, _) in compose_services(paths) {
        if !declared.contains(&service) {
            declared.push(service);
        }
    }
    let recipes = crate::recipes::Recipes::load(&paths.recipes_dir());
    for (service, answer) in services {
        let answered = config.namespaced.get(service).is_some_and(|settings| {
            settings.recipe.is_some()
                || !settings.db_env.is_empty()
                || !settings.prefix_env.is_empty()
        });
        if answered && !replacing {
            return Err(by.refuse(format!(
                "namespaced.{service} is already answered, so your answer was not applied — add \
                 --replace to replace it"
            )));
        }
        if !declared.contains(service) {
            return Err(by.refuse(format!(
                "namespaced names {service:?}, which is not one of this project's services — \
                 they are: {}",
                match declared.is_empty() {
                    true => "none at all".to_string(),
                    false => declared.join(", "),
                }
            )));
        }
        // A database or a slot is reached at the address config maps for
        // the service; one only a compose file names has none.
        if !answer.db_env.is_empty() && !isolated.contains(service) {
            return Err(by.refuse(format!(
                "namespaced.{service}.db_env needs the service in `services`, where its address \
                 is mapped — `prefix_env` needs nothing more"
            )));
        }
        if *answer == NamespacedAnswer::default() {
            return Err(by.refuse(format!(
                "namespaced.{service} says nothing — it takes recipe, db_env and prefix_env"
            )));
        }
        if let Some(name) = &answer.recipe {
            let loaded = recipes
                .get(name)
                .map_err(|e| by.refuse(format!("namespaced.{service}.recipe: {e:#}")))?;
            if !answer.db_env.is_empty() && loaded.recipe.namespace.is_none() {
                return Err(by.refuse(format!(
                    "namespaced.{service}.db_env names a database or slot, and the recipe \
                     {name:?} knows no [namespace] to make one"
                )));
            }
            if loaded.recipe.namespace.is_none() && loaded.recipe.prefix.is_none() {
                return Err(by.refuse(format!(
                    "namespaced.{service}.recipe names {name:?}, which says nothing about a \
                     namespace or a prefix — a recipe with a [namespace] or a [prefix] does"
                )));
            }
        }
        for (field, keys) in [
            ("db_env", &answer.db_env),
            ("prefix_env", &answer.prefix_env),
        ] {
            if let Some(bad) = keys.iter().find(|key| !crate::detect::is_env_name(key)) {
                return Err(by.refuse(format!(
                    "namespaced.{service}.{field} names {bad:?}, which is not an env key"
                )));
            }
        }
    }
    let note = by.note(config::Note::Answered);
    for (service, answer) in services {
        if replacing {
            config::remove_keys(
                paths,
                config::Layer::Project,
                &[
                    &[NAMESPACED_TABLE, service, "recipe"],
                    &[NAMESPACED_TABLE, service, "db_env"],
                    &[NAMESPACED_TABLE, service, "prefix_env"],
                ],
            )?;
        }
        let list = |keys: &[String]| {
            toml_edit::Value::Array(toml_edit::Array::from_iter(keys.iter().cloned()))
        };
        let mut entries: Vec<(String, toml_edit::Value)> = Vec::new();
        if let Some(recipe) = &answer.recipe {
            entries.push((
                "recipe".to_string(),
                toml_edit::Value::from(recipe.as_str()),
            ));
        }
        if !answer.db_env.is_empty() {
            entries.push(("db_env".to_string(), list(&answer.db_env)));
        }
        if !answer.prefix_env.is_empty() {
            entries.push(("prefix_env".to_string(), list(&answer.prefix_env)));
        }
        config::set_detected_table(
            paths,
            config::Layer::Project,
            &[NAMESPACED_TABLE, service],
            entries,
            note.clone(),
        )?;
        let settings = config.namespaced.entry(service.clone()).or_default();
        settings.recipe = answer.recipe.clone();
        settings.db_env = answer.db_env.clone();
        settings.prefix_env = answer.prefix_env.clone();
        progress(&format!(
            "namespaced settings for {service}: {}",
            serde_json::to_string(answer).unwrap_or_default()
        ));
    }
    Ok(())
}

/// The table namespaced settings live under, `[namespaced.<service>]`.
const NAMESPACED_TABLE: &str = "namespaced";

/// What a namespaced start would do with one service, for a program to
/// read before it answers anything: `signals --json`'s `namespaced`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct NamespacedService {
    pub service: String,
    /// `database` or `slot`, which the server makes; `prefix`, which the
    /// app is told; `shared`, on the main checkout's data; or
    /// `undeclared`, a compose service the project's `[[services]]` do not
    /// name, which every worktree reaches as the main checkout's.
    pub how: &'static str,
    /// The recipe whose `[namespace]` it is, for a database or a slot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipe: Option<String>,
    /// The env keys pando points at the worktree's own: where the app
    /// names its database, slot or prefix.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
    /// Why it stays shared, and what would change that.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

/// [`NamespacedService`] for every service of the project, read from
/// config, the recipes and the main checkout's env files — nothing asked
/// of a server, nothing spawned — and for each service of `compose_files`
/// the project does not declare, which a namespaced start never sees.
/// A helper the catalog knows the app keeps nothing in, a mail catcher, is
/// left out, and so is one the compose file builds: the project's own code.
pub fn namespaced_report(
    paths: &PandoPaths,
    config: &Config,
    compose_files: &[String],
) -> Vec<NamespacedService> {
    let declared = declared_services(config);
    let mut undeclared: Vec<NamespacedService> = Vec::new();
    for file in compose_files {
        let Ok(parsed) = crate::compose::read(&paths.root().join(file)) else {
            continue;
        };
        for (service, entry) in &parsed.services {
            let helper = entry
                .image
                .as_deref()
                .and_then(crate::catalog::images::known)
                .is_some_and(|known| known.role == crate::catalog::images::Role::Utility);
            // One the compose file builds rather than pulls is the
            // project's own code, not a store of its data.
            if helper
                || entry.image.is_none()
                || declared.contains(service)
                || config.namespaced.contains_key(service)
                || undeclared.iter().any(|known| known.service == *service)
            {
                continue;
            }
            undeclared.push(NamespacedService {
                service: service.clone(),
                how: "undeclared",
                recipe: None,
                keys: Vec::new(),
                why: Some(format!(
                    "{file} runs it, and nothing says how a worktree gets data of its own in it, \
                     so every worktree reaches the main checkout's — the `namespaced` answer \
                     says how: the recipe its engine is, and the keys its app reads"
                )),
            });
        }
    }
    let plan = plan(paths, config);
    let targets = plan.targets.into_iter().map(|target| NamespacedService {
        how: match target.namespace.kind {
            NamespaceKind::Database => "database",
            NamespaceKind::Slot => "slot",
        },
        recipe: Some(target.recipe),
        keys: target
            .tells
            .into_iter()
            .map(|tell| match tell {
                Tell::Key(key) | Tell::Url(key) => key,
            })
            .collect(),
        why: None,
        service: target.service,
    });
    let prefixed = plan.prefixed.into_iter().map(|prefixed| NamespacedService {
        service: prefixed.service,
        how: "prefix",
        recipe: prefixed.recipe,
        keys: prefixed.keys.into_iter().map(|(key, _)| key).collect(),
        why: None,
    });
    let shared = plan
        .shared
        .into_iter()
        .map(|(service, why)| NamespacedService {
            service,
            how: "shared",
            recipe: None,
            keys: Vec::new(),
            why: Some(why),
        });
    targets
        .chain(prefixed)
        .chain(shared)
        .chain(undeclared)
        .collect()
}

/// What a namespaced start does with a service it makes nothing in: a
/// prefix of the worktree's own, or the main checkout's data, and why.
/// The same for every worktree of a project, so it can be read once per
/// config and said for each worktree by [`PlanRow::line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanRow {
    Prefix(Prefixed),
    Shared { service: String, why: String },
}

impl PlanRow {
    /// The service, a word, and the rest of the line, for this worktree.
    pub fn line(&self, project: &str, worktree: &str) -> (String, &'static str, String) {
        match self {
            PlanRow::Prefix(prefixed) => (
                prefixed.service.clone(),
                "own",
                prefixed.describe(project, worktree),
            ),
            PlanRow::Shared { service, why } => (service.clone(), "shared", why.clone()),
        }
    }
}

/// [`PlanRow`]s of a project's namespaced starts: the prefixed services
/// first, then the shared ones.
pub fn namespace_plan_rows(paths: &PandoPaths, config: &Config) -> Vec<PlanRow> {
    let plan = plan(paths, config);
    plan.prefixed
        .into_iter()
        .map(PlanRow::Prefix)
        .chain(
            plan.shared
                .into_iter()
                .map(|(service, why)| PlanRow::Shared { service, why }),
        )
        .collect()
}

/// What a worktree holds in each service, as `status` says it: the
/// service, a word, and the rest of the line.
///
/// `own` for a namespace it runs on, or a prefix its app is told; `kept`
/// for one it holds while running in another mode, which waits for the
/// way back until `rm`; and, for a namespaced worktree, `shared` for a
/// service that stays on the main checkout's data, with why. `config` is
/// `None` when it does not load, and then the prefixed and shared ones go
/// unsaid.
pub fn namespace_lines(
    paths: &PandoPaths,
    config: Option<&Config>,
    name: &str,
    record: &crate::state::WorktreeRecord,
) -> Vec<(String, &'static str, String)> {
    let running_on_them = record.mode() == crate::state::ServiceMode::Namespaced;
    let mut out: Vec<(String, &'static str, String)> = record
        .namespaces
        .iter()
        .map(|ns| {
            let what = match ns.kind {
                NamespaceKind::Database => format!("database {}", ns.name),
                NamespaceKind::Slot => format!("slot {}", ns.name),
            };
            let (word, until) = match running_on_them {
                true => ("own", ""),
                false => ("kept", ", until rm"),
            };
            (
                ns.service.clone(),
                word,
                format!("{what} on {}:{}{until}", ns.host, ns.port),
            )
        })
        .collect();
    if running_on_them && let Some(config) = config {
        out.extend(
            namespace_plan_rows(paths, config)
                .iter()
                .map(|row| row.line(paths.project_id(), name)),
        );
    }
    out
}
