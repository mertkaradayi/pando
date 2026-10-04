//! What a config must satisfy before anything runs from it.

use super::schema::Config;
use super::schema::ISOLATION_KINDS;
use super::schema::ServiceConfig;
use crate::project::ProjectRef;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::{Component, Path};

/// `[dev]` is shorthand for `processes.dev`; the two forms may not both be
/// present, which is a validation error rather than a merge.
pub(super) fn normalize(mut config: Config) -> Result<Config> {
    if let Some(dev) = config.dev.take() {
        if !config.processes.is_empty() {
            bail!(
                "[dev] and [processes] may not both be set — [dev] is shorthand for one process named dev"
            );
        }
        config.processes.insert("dev".to_string(), dev);
    }
    Ok(config)
}

pub fn validate(config: &Config, project: &ProjectRef) -> Result<()> {
    if let Some(dir) = config.configured_worktrees_dir(&project.root) {
        // Against the repository root only: the fuller check, which also
        // knows about linked worktrees, needs git and runs once at startup.
        crate::paths::ensure_outside_repository("worktrees_dir", &dir, &project.root, &[])?;
    }
    validate_processes(config)?;
    validate_services(config)?;
    if let Some(prefer) = config.isolation.preferred()
        && !ISOLATION_KINDS.contains(&prefer)
    {
        bail!(
            "[isolation] prefer = {prefer:?} is not something pando can run — it is {}, the \
             same two spellings `[[services]] kind` takes",
            ISOLATION_KINDS.join(" or ")
        );
    }
    if let Some(appearance) = &config.ui.appearance
        && !super::schema::APPEARANCES.contains(&appearance.as_str())
    {
        bail!(
            "[ui] appearance = {appearance:?} is not one pando knows — it is {}",
            super::schema::APPEARANCES.join(", ")
        );
    }
    if let Some(sort) = &config.ui.sort
        && !super::schema::LIST_SORTS.contains(&sort.as_str())
    {
        bail!(
            "[ui] sort = {sort:?} is not one pando knows — it is {}",
            super::schema::LIST_SORTS.join(", ")
        );
    }
    if config.isolation.none && !config.services.is_empty() {
        bail!(
            "[isolation] none = true says this project runs no private services, and \
             [[services]] configures {} — delete whichever of them is out of date",
            config.services.len()
        );
    }
    // A hook writes `logs/<worktree>/<name>.log` under exactly the same
    // rules as a process, and shares the namespace with it. A process of
    // the same name truncates that log when it spawns, erasing what a hook
    // before it appended, and a `dev` hook's lines land in the running
    // process's log.
    for hook in &config.hooks {
        crate::paths::validate_owned_log_source("hook name", &hook.name)?;
        if let Some(process) = config
            .processes
            .keys()
            .find(|process| crate::paths::same_log_source(process, &hook.name))
        {
            bail!(
                "the hook {:?} and the process {process:?} have the same name — both write the \
                 log logs/<worktree>/{process}.log, so the process's start would truncate what \
                 the hook appended; rename one of them",
                hook.name
            );
        }
    }
    for probe in &config.probes {
        if probe.name.trim().is_empty() {
            bail!("a [[probes]] entry needs a name");
        }
        if probe.cmd.trim().is_empty() {
            bail!("probe {:?} has no cmd", probe.name);
        }
    }
    for entry in config.project.provision_paths() {
        validate_repository_relative("provision path", entry)?;
    }
    // Both halves: the destination is written inside a worktree, and the
    // source is read out of the repository. A `..` in either one is pando
    // reaching somewhere it does not own.
    for (destination, source) in &config.project.provision_from {
        validate_repository_relative("provision_from path", destination)?;
        validate_repository_relative("provision_from source", source)?;
    }
    for entry in &config.project.clone {
        validate_repository_relative("clone path", entry)?;
    }
    Ok(())
}

fn validate_repository_relative(label: &str, entry: &str) -> Result<()> {
    let path = Path::new(entry);
    if entry.trim().is_empty() {
        bail!("{label}s must not be empty");
    }
    if path.is_absolute() {
        bail!("{label} {entry:?} must be relative to the repository root");
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("{label} {entry:?} must not escape the repository root");
    }
    Ok(())
}

/// A service is three things at once and has to be legal as all of them.
///
/// It is a **role**, so its port is allocated with the processes' and
/// `{port:postgres}` resolves; two things claiming one role would be handed
/// one number. It is a **log source**, so its container log lands at
/// `logs/<worktree>/<service>.log`, under the same single-path-component
/// rule a process name has had since Phase 2b — a service called `../x`
/// would write outside pando's home. And its `file` is read from inside
/// the worktree, which is also the directory compose resolves every
/// relative path in that file against.
fn validate_services(config: &Config) -> Result<()> {
    let mut role_owner: BTreeMap<String, String> = BTreeMap::new();
    for (process, spec) in &config.processes {
        for role in spec.roles() {
            role_owner.insert(role, format!("process {process:?}"));
        }
    }
    for service in &config.services {
        match service {
            ServiceConfig::Compose {
                file, include, env, ..
            } => {
                // The same refusals `compose::file_in` makes, at load time
                // rather than at the first isolated start.
                crate::compose::file_in(Path::new("/"), file)?;
                for name in include {
                    claim_service_name(config, &mut role_owner, name, "drop it from `include`")?;
                }
                for (key, service_name) in env {
                    if key.trim().is_empty() {
                        bail!("a [[services]] env key must not be empty");
                    }
                    if !include.iter().any(|name| name == service_name) {
                        bail!(
                            "env.{key} points at the service {service_name:?}, which is not in \
                             `include` — pando has no port for a service it does not run"
                        );
                    }
                }
            }
            ServiceConfig::Native {
                name,
                port_env,
                env,
                ..
            } => {
                // A native service is a role, a log tab and a data
                // directory under its own name, so it is held to every
                // rule a compose service's name is.
                claim_service_name(config, &mut role_owner, name, "rename it")?;
                for (key, service_name) in env {
                    if key.trim().is_empty() {
                        bail!("a [[services]] env key must not be empty");
                    }
                    // A native entry runs exactly one service, so the only
                    // thing its env map can point at is itself.
                    if service_name != name {
                        bail!(
                            "env.{key} points at the service {service_name:?}, but this \
                             [[services]] entry runs {name:?} — a native entry is one service, \
                             so its env map can only name that one"
                        );
                    }
                }
                if port_env.as_ref().is_some_and(|key| key.trim().is_empty()) {
                    bail!("[[services]] {name:?} has an empty `port_env`");
                }
                // Which recipe runs it is *not* checked here. Resolving a
                // `preset` means reading the user's recipes directory, and
                // a build that refused a name only because it is not one of
                // the built-ins would be giving built-ins exactly the
                // privilege a user's own file is supposed to have. The
                // start path and `doctor` resolve it, where the directory
                // is in hand.
            }
        }
    }
    Ok(())
}

/// The rules every service name is held to, whichever kind of service it
/// is: it is a log file component, it must not collide with a hook's or a
/// process's log, and it owns a role no process may also own.
///
/// `escape` is what the developer can do about a collision, which differs
/// between the two kinds — a compose service can be dropped from
/// `include`, a native one has to be renamed.
fn claim_service_name(
    config: &Config,
    role_owner: &mut BTreeMap<String, String>,
    name: &str,
    escape: &str,
) -> Result<()> {
    crate::paths::validate_owned_log_source("service name", name)?;
    // A service is a log source, and so is a hook. Two of them with one
    // name both write `logs/<worktree>/<name>.log`: the service's log
    // truncates it, the hook appends to it, and `logs --source <name>`
    // shows a mixture of the two.
    if let Some(hook) = config
        .hooks
        .iter()
        .find(|hook| crate::paths::same_log_source(&hook.name, name))
    {
        bail!(
            "the hook {:?} and the service {name:?} have the same name — both write the \
             log logs/<worktree>/{name}.log, so the service's log would truncate what the \
             hook appended; rename one of them",
            hook.name
        );
    }
    if let Some(owner) = role_owner.get(name) {
        // Two services with one name is its own sentence: "the service
        // \"postgres\" and the service \"postgres\"" reads like a bug in
        // pando rather than a duplicate in the file.
        if owner.starts_with("the service") {
            bail!(
                "two [[services]] entries both run a service called {name:?} — a service is \
                 one role, one port and one log, so it can only be declared once"
            );
        }
        bail!(
            "the service {name:?} and {owner} both claim the role {name:?} — a role is one \
             port and belongs to one thing; rename the process's role, or {escape}"
        );
    }
    // Two services whose names differ only in case are two roles, but on
    // a filesystem that ignores case they are one log, and whichever
    // starts last truncates the other's.
    if let Some((other, _)) = role_owner.iter().find(|(role, owner)| {
        owner.starts_with("the service") && crate::paths::same_log_source(role, name)
    }) {
        bail!(
            "the service {name:?} differs only in case from the service {other:?} — on a \
             filesystem that ignores it, as macOS's does, both write the one log \
             logs/<worktree>/{name}.log; {escape}"
        );
    }
    // A process owning some other role still writes its log under its
    // own name, and whichever of the two starts last truncates the other's.
    if let Some(process) = config
        .processes
        .keys()
        .find(|process| crate::paths::same_log_source(process, name))
    {
        bail!(
            "the service {name:?} and the process {process:?} have the same name — both write \
             the log logs/<worktree>/{name}.log, so whichever starts last would truncate the \
             other's; rename the process, or {escape}"
        );
    }
    role_owner.insert(name.to_string(), format!("the service {name:?}"));
    Ok(())
}

/// Rules that only make sense across every process of a worktree.
///
/// A role names a port, and a port belongs to exactly one process: two
/// processes claiming `web` would both be handed the same number, and the
/// second one to start would die with `EADDRINUSE` for a reason nothing in
/// pando could explain. `{port:<role>}` may still *reference* any role of
/// the worktree — that is how a web process is told the api's port — so
/// only ownership is exclusive, never use.
///
/// The name itself is checked here too: it becomes a path component of the
/// process's log file, and a TOML key may be any quoted string.
fn validate_processes(config: &Config) -> Result<()> {
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for (name, process) in &config.processes {
        crate::paths::validate_owned_log_source("process name", name)?;
        if let Some(other) = config
            .processes
            .keys()
            .find(|other| *other != name && crate::paths::same_log_source(other, name))
        {
            bail!(
                "processes {other:?} and {name:?} differ only in case — on a filesystem that \
                 ignores it, as macOS's does, both write the one log logs/<worktree>/{name}.log; \
                 rename one of them"
            );
        }
        let roles = process.roles();
        for role in &roles {
            // What `{port:<role>}` can spell: anything else is not read as a
            // placeholder at all, and the app is handed the braces verbatim.
            let spellable = !role.is_empty()
                && role
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
            if !spellable {
                bail!(
                    "process {name:?} names the role {role:?}, which `{{port:<role>}}` cannot \
                     spell — use letters, digits, `_`, `-` and `.` only"
                );
            }
            if let Some(first) = owner.get(role) {
                bail!(
                    "processes {first:?} and {name:?} both claim the role {role:?} — a role \
                     belongs to one process, and every process may still reference it with \
                     {{port:{role}}}"
                );
            }
            owner.insert(role.clone(), name.clone());
        }
        if let Some(named) = process.ready.as_ref().and_then(|r| r.role.as_deref())
            && !roles.iter().any(|r| r == named)
        {
            bail!(
                "ready.role = {named:?} in process {name:?} names a role it does not own — it \
                 owns {}",
                if roles.is_empty() {
                    "none".to_string()
                } else {
                    roles.join(", ")
                }
            );
        }
        if let Some(cwd) = process.cwd.as_deref() {
            validate_cwd(name, cwd)?;
        }
    }
    Ok(())
}

/// A process runs inside its own worktree. An absolute path or one that
/// climbs out with `..` would put it somewhere pando does not own — the
/// main checkout, a sibling worktree — and everything it wrote there would
/// be written into a repository, which Invariant 1 forbids.
fn validate_cwd(process: &str, cwd: &str) -> Result<()> {
    if cwd.trim().is_empty() {
        bail!("cwd for process {process:?} must not be empty — leave it out for the worktree root");
    }
    let path = Path::new(cwd);
    if path.is_absolute() {
        bail!("cwd {cwd:?} for process {process:?} must be relative to the worktree");
    }
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("cwd {cwd:?} for process {process:?} must not escape the worktree");
    }
    Ok(())
}
