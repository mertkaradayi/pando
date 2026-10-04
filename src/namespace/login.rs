//! Who pando connects as to make and drop a worktree's namespaces.

use std::path::Path;

use crate::config::Config;
use crate::services::{EnvFiles, Unresolved};

/// The keys an app keeps its user in, beside its address.
const USER_SUFFIXES: [&str; 2] = ["_USER", "_USERNAME"];

/// The keys an app keeps its password in, beside its address.
const PASSWORD_SUFFIXES: [&str; 3] = ["_PASSWORD", "_PASS", "_PWD"];

/// A login for one service's server, and where it was read from.
///
/// The password is private to this type and leaves it one way: through
/// [`Login::env`], into the environment of the one command that needs it.
/// Never an argument, which anyone on the machine can read in `ps`; never a
/// line of output; never a log. `Debug` names it and does not print it.
#[derive(Clone, PartialEq, Eq)]
pub struct Login {
    pub user: Option<String>,
    password: Option<String>,
    /// Where it came from, for a sentence: `DATABASE_USER and
    /// DATABASE_PASSWORD in the main checkout's env files`.
    pub from: String,
}

impl std::fmt::Debug for Login {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Login")
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "(hidden)"))
            .field("from", &self.from)
            .finish()
    }
}

impl Login {
    pub fn new(user: Option<String>, password: Option<String>, from: impl Into<String>) -> Login {
        let nonempty = |value: Option<String>| value.filter(|v| !v.is_empty());
        Login {
            user: nonempty(user),
            password: nonempty(password),
            from: from.into(),
        }
    }

    /// No user and no password: a server that asks for neither, as a
    /// development Redis usually does.
    pub fn none() -> Login {
        Login::new(None, None, "nothing — the server is asked with no login")
    }

    pub fn has_password(&self) -> bool {
        self.password.is_some()
    }

    /// The environment of a command that logs in with it: the password
    /// under the name the engine's own client reads it from —
    /// `MYSQL_PWD`, `REDISCLI_AUTH`. Empty with no password, or when the
    /// engine names no such variable.
    pub fn env(&self, password_env: Option<&str>) -> Vec<(String, String)> {
        match (password_env, &self.password) {
            (Some(var), Some(password)) => vec![(var.to_string(), password.clone())],
            _ => Vec::new(),
        }
    }
}

/// The login the main checkout's env files give a service the app finds
/// through `keys`: the `user:password@` of a URL among them, else the keys
/// beside them — `DATABASE_USER` and `DATABASE_PASSWORD` next to
/// `DATABASE_PORT`. `None` when they carry neither a user nor a password,
/// and [`Unresolved`] when the one they carry holds a reference nothing
/// sets: tried as written, pando logged in as a user called `${DB_USER}`.
///
/// The main checkout's, because namespaced mode is its servers: the app in
/// a worktree logs in the same way, so the login that makes the worktree's
/// database is the one that will use it.
pub fn from_env_files(env: &EnvFiles, keys: &[String]) -> Result<Option<Login>, Unresolved> {
    for key in keys {
        let Some(value) = env.value(key)? else {
            continue;
        };
        let (user, password) = crate::services::url_userinfo(value.trim());
        if user.is_some() || password.is_some() {
            return Ok(Some(Login::new(
                user,
                password,
                format!("the URL in {key}, in the main checkout's env files"),
            )));
        }
    }
    let keys = keys.iter().map(String::as_str);
    let user = env.sibling(keys.clone(), &USER_SUFFIXES)?;
    let password = env.sibling(keys, &PASSWORD_SUFFIXES)?;
    if user.is_none() && password.is_none() {
        return Ok(None);
    }
    let named: Vec<&str> = [&user, &password]
        .into_iter()
        .flatten()
        .map(|(key, _)| key.as_str())
        .collect();
    Ok(Some(Login::new(
        user.as_ref().map(|(_, value)| value.clone()),
        password.as_ref().map(|(_, value)| value.clone()),
        format!("{} in the main checkout's env files", named.join(" and ")),
    )))
}

/// The login pando was given for this service before — the answer to the
/// question, in pando's own file for the project.
pub fn from_config(config: &Config, service: &str, file: &Path) -> Option<Login> {
    let login = config.namespaced.get(service)?;
    Some(Login::new(
        login.user.clone(),
        login.password.clone(),
        format!("[namespaced.{service}] in {}", file.display()),
    ))
}

/// The login a namespaced start uses for one service, without asking: the
/// main checkout's own when its env files carry one that will do, else the
/// one pando was given before.
///
/// `needs_user` is the engine's: MariaDB logs in as somebody, a
/// development Redis asks only for a password, if that. A login from the
/// env files with no user is no login for an engine that needs one, and
/// the one written down for pando is tried next. So is it when the env
/// files' login holds a reference nothing sets, and with nothing written
/// down, that is the [`Unresolved`] error.
pub fn find(
    env: &EnvFiles,
    config: &Config,
    service: &str,
    keys: &[String],
    needs_user: bool,
    file: &Path,
) -> Result<Option<Login>, Unresolved> {
    let usable = |login: &Login| !needs_user || login.user.is_some();
    let written_down = || from_config(config, service, file).filter(usable);
    match from_env_files(env, keys) {
        Ok(login) => Ok(login.filter(usable).or_else(written_down)),
        Err(unresolved) => written_down().map(Some).ok_or(unresolved),
    }
}
