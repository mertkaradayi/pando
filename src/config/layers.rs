//! Reading the three layers and merging them into one `Config`, with
//! what each lower layer is and is not allowed to decide.

use super::schema::Config;
use super::suggest::{KeyError, toml_error_line};
use super::validate::normalize;
use super::validate::validate;
use crate::paths::PandoPaths;
use crate::project::ProjectRef;
use anyhow::{Context, Result, bail};
use std::path::Path;
use toml::{Table, Value};

/// Keys only pando's own project layer may set, because they decide where
/// pando writes for one project on one machine.
const PROJECT_LAYER_ONLY: [&str; 2] = ["root", "worktrees_dir"];

/// The table of namespaced starts' settings, whose logins only pando's own
/// project layer may hold.
pub(super) const NAMESPACED: &str = "namespaced";

/// Why a layer beneath the project one may not decide where pando writes.
/// Each layer says it in its own terms, because the two reasons differ: one
/// file is inside the repository, the other is shared by every project.
/// The two layers beneath pando's own, which are hand-written and so are
/// held to what each of them is *for*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LowerLayer {
    /// A `pando.toml` a team committed: shared by everyone who clones.
    Committed,
    /// `~/.pando/config.toml`: shared by every project on one laptop.
    User,
}

const COMMITTED_REASON: &str = "a committed config may not decide where pando writes";
const USER_REASON: &str = "a machine-wide config may not decide where pando writes for one project";

/// A loaded config plus anything pando decided to ignore. There is no
/// `doctor` yet, so the warnings have to reach the caller some other way.
#[derive(Debug, Clone, Default)]
pub struct Loaded {
    pub config: Config,
    pub warnings: Vec<String>,
}

pub fn load(paths: &PandoPaths) -> Result<Loaded> {
    load_layers(paths, true)
}

/// Everything except pando's own layer.
///
/// A home `pando.toml` that cannot be parsed, deserialised or validated is
/// fatal for the commands that act on it — `new`, `start`, `restart` and
/// the TUI, which can do both. Every other command needs none of it, and
/// `stop` is the one you need most when that file is broken, so those run
/// on what is left.
pub fn load_without_home(paths: &PandoPaths) -> Loaded {
    load_layers(paths, false).unwrap_or_default()
}

fn load_layers(paths: &PandoPaths, use_home: bool) -> Result<Loaded> {
    let mut warnings = Vec::new();
    let committed_path = paths.root().join("pando.toml");
    let user_path = paths.user_config_file();
    let home_path = paths.config_file();

    // Lowest precedence first, and both hand-written: a file that does not
    // parse, deserialise or validate is dropped with a warning rather than
    // bricking every command. A committed file belongs to the team, and to
    // whichever pando wrote it — a key this build has never heard of is
    // what a *newer* pando's config looks like. The user layer is one file
    // for every project on the machine, so a mistake in it has an even
    // wider blast radius.
    let committed = read_lower_layer(
        paths,
        &committed_path,
        LowerLayer::Committed,
        COMMITTED_REASON,
        &mut warnings,
    );
    let user = read_lower_layer(
        paths,
        &user_path,
        LowerLayer::User,
        USER_REASON,
        &mut warnings,
    );
    // Strictly, and only when it is wanted: a file pando wrote and cannot
    // parse is not one to carry on past the way the others are. The caller
    // decides whether this command can do without it.
    let home = if use_home {
        read_home_table(&home_path)?
    } else {
        None
    };

    let mut merged = Table::new();
    for table in [committed.clone(), user.clone(), home.clone()]
        .into_iter()
        .flatten()
    {
        merge_tables(&mut merged, table);
    }

    match build(merged, &paths.project) {
        Ok(config) => Ok(Loaded { config, warnings }),
        // The project layer is pando's own file, so it still fails hard —
        // but when each layer is fine alone and only the combination is
        // not, no single file explains it and every one of them is named.
        Err(e) => {
            let present: Vec<String> = [
                committed
                    .as_ref()
                    .map(|_| committed_path.display().to_string()),
                user.as_ref().map(|_| user_path.display().to_string()),
                home.as_ref().map(|_| home_path.display().to_string()),
            ]
            .into_iter()
            .flatten()
            .collect();
            let home_alone_is_fine = home.is_none_or(|table| build(table, &paths.project).is_ok());
            if present.len() > 1 && home_alone_is_fine {
                bail!("{e:#} — {} cannot all apply", listed(&present));
            }
            bail!("{}", in_file(&e, &home_path))
        }
    }
}

/// A layer pando did not write: read it, strip the keys it may not set, and
/// drop the whole thing with a warning if what is left will not build.
fn read_lower_layer(
    paths: &PandoPaths,
    path: &Path,
    layer: LowerLayer,
    reason: &str,
    warnings: &mut Vec<String>,
) -> Option<Table> {
    let mut table = read_table(path, warnings)?;
    strip_keys_only_pando_may_set(&mut table, path, layer, reason, warnings);
    if let Err(e) = build(table.clone(), &paths.project) {
        // A key error's Display is already one line.
        warnings.push(format!("ignoring {}: {e:#}", path.display()));
        return None;
    }
    Some(table)
}

/// pando's own layer's error as one line naming its file: a key it does
/// not take says where the file is in the sentence, anything else is
/// followed by it.
fn in_file(e: &anyhow::Error, path: &Path) -> String {
    match e.downcast_ref::<KeyError>() {
        Some(key) => key.render(Some(path)),
        None => {
            // toml puts the table it was in on a line of its own.
            let text = format!("{e:#}");
            let flat: Vec<&str> = text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            format!("{} — in {}", flat.join(" "), path.display())
        }
    }
}

/// `a`, `a and b`, `a, b and c` — every file a refusal has to name.
fn listed(names: &[String]) -> String {
    match names.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// One layer on its own: deserialise, normalise, validate. A layer that
/// cannot survive this is not merged into anything.
fn build(table: Table, project: &ProjectRef) -> Result<Config> {
    let config: Config = Value::Table(table)
        .try_into()
        .map_err(|e: toml::de::Error| {
            // A key a table does not take is the common mistake, and it gets
            // one line with what was probably meant; anything else keeps
            // toml's words.
            match KeyError::parse(&e.to_string()) {
                Some(key) => anyhow::Error::new(key),
                None => anyhow::Error::new(e).context("parse pando.toml"),
            }
        })?;
    let config = normalize(config)?;
    validate(&config, project)?;
    Ok(config)
}

/// pando's own layer. A file that is not there is not an error; one that is
/// there and does not parse is.
fn read_home_table(path: &Path) -> Result<Option<Table>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let table = toml::from_str::<Table>(&text)
        .map_err(|e| anyhow::anyhow!("{}", toml_error_line(&e.to_string())))
        .with_context(|| format!("parse {}", path.display()))?;
    Ok(Some(table))
}

fn read_table(path: &Path, warnings: &mut Vec<String>) -> Option<Table> {
    let text = std::fs::read_to_string(path).ok()?;
    match toml::from_str::<Table>(&text) {
        Ok(t) => Some(t),
        Err(e) => {
            // A broken file — ours or the project's — must not brick every
            // command; it is reported and skipped.
            warnings.push(format!(
                "ignoring {}: {}",
                path.display(),
                toml_error_line(&e.to_string())
            ));
            None
        }
    }
}

/// Removes the keys this layer may not set, warning by name for each
/// one, with the reason it may not.
///
/// Four kinds of key, and the reason differs for each. `project.root`
/// and `project.worktrees_dir` decide where pando writes, which only
/// pando's own file may say. `[isolation] none` is a fact about one
/// repository, so a machine-wide file may not answer it for every
/// project at once. `[isolation] prefer` is a fact about one laptop, so
/// a file the team shares may not answer it for everyone's. And a
/// `[namespaced]` login is a password, which belongs in pando's own file;
/// its `db_env` is not, and stays.
fn strip_keys_only_pando_may_set(
    table: &mut Table,
    path: &Path,
    layer: LowerLayer,
    reason: &str,
    warnings: &mut Vec<String>,
) {
    if let Some(Value::Table(project)) = table.get_mut("project") {
        for key in PROJECT_LAYER_ONLY {
            if project.remove(key).is_some() {
                warnings.push(format!(
                    "ignoring project.{key} in {}: {reason}",
                    path.display()
                ));
            }
        }
    }
    let (key, why) = match layer {
        // A machine-wide file saying "this project has no private
        // services" says it about every project on the laptop.
        LowerLayer::User => (
            "none",
            "a machine-wide config may not answer that for every project",
        ),
        // A file the team shares saying "prefer native" imposes one
        // developer's laptop on everyone who clones the repository.
        LowerLayer::Committed => (
            "prefer",
            "which mechanism to use is a property of a machine, not of a repository",
        ),
    };
    if let Some(Value::Table(isolation)) = table.get_mut("isolation")
        && isolation.remove(key).is_some()
    {
        warnings.push(format!(
            "ignoring isolation.{key} in {}: {why}",
            path.display()
        ));
    }
    // A login is a password: kept in pando's own file for the project,
    // which is 0600 and never committed, and nowhere else. Which keys name
    // the app's database is no secret, and a team may write it down.
    if strip_logins(table) {
        warnings.push(format!(
            "ignoring [{NAMESPACED}] logins in {}: {}",
            path.display(),
            match layer {
                LowerLayer::Committed => "a login may not live in a file the team shares",
                LowerLayer::User => "a login belongs to one project, in pando's own file for it",
            }
        ));
    }
}

/// Removes every login from a lower layer's `[namespaced]`, keeping each
/// service's `db_env`; whether there was one to remove. Anything there
/// that is not a table of tables goes whole.
fn strip_logins(table: &mut Table) -> bool {
    let Some(Value::Table(namespaced)) = table.get_mut(NAMESPACED) else {
        return table.remove(NAMESPACED).is_some();
    };
    let mut stripped = false;
    namespaced.retain(|_, service| {
        let Value::Table(service) = service else {
            stripped = true;
            return false;
        };
        for key in ["user", "password"] {
            stripped |= service.remove(key).is_some();
        }
        !service.is_empty()
    });
    if namespaced.is_empty() {
        table.remove(NAMESPACED);
    }
    stripped
}

/// Tables merge per key; everything else, arrays of tables included, is
/// replaced whole by the higher layer.
fn merge_tables(base: &mut Table, over: Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(base_table)), Value::Table(over_table)) => {
                merge_tables(base_table, over_table);
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}
