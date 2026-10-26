//! Turning a choice into config: what is still needed, applying a
//! candidate, and the edits that write it to `pando.toml`.

use std::collections::BTreeMap;

use crate::config::{Config, PortsSpec, ProcessConfig};

use super::proposal::{Candidate, ServiceHint, Slot};
use super::services::{SCHEMA_HOOK, ServiceSource};

/// The process name `[dev]` is shorthand for.
pub const DEV: &str = "dev";

/// Whether a slot still has a question to ask, given what is already
/// decided.
///
/// A command that carries `{port:web}` has answered the port question by
/// existing — Django takes its port on the command line, so there is no
/// environment variable to choose.
pub fn still_needed(slot: Slot, config: &Config) -> bool {
    match slot {
        // A config that already declares any process has answered the
        // question of how many there are.
        Slot::Processes => config.processes.is_empty(),
        Slot::DevCmd => config
            .processes
            .get(DEV)
            .is_none_or(|dev| dev.cmd.trim().is_empty()),
        // Unset, not empty. A process configured with no ports at all — a
        // worker, a watcher, a queue consumer — has answered this question
        // with `ports = []`, and asking again would hand it a port it will
        // never bind and then call it failed for not binding it.
        Slot::PortEnv => config.processes.get(DEV).is_none_or(|p| p.ports.is_none()),
        // Unset, not empty, exactly as for the port slot: `prelude = ""`
        // is "this machine needs nothing", which is an answer, and asking
        // again would be asking a developer to say no twice.
        Slot::Prelude => config.runtime.prelude.is_none(),
        // And again: `provision = []` is "no worktree needs a local file
        // of mine", which a developer may have said by declining.
        Slot::Provision => config.project.provision.is_none(),
        // `clone = []` is "share nothing", an answer like `provision = []`.
        Slot::Clone => config.project.clone.is_none(),
        // Set to a branch, or not set: origin/HEAD is the answer then.
        Slot::Base => config.project.base.is_none(),
        // Anything already in `[[services]]` is an answer about every
        // service: a developer who listed two has said the third is not
        // wanted, and a second run must not offer it again. So is
        // `[isolation] none`, which is how the native half records the
        // answer "none of them" — a compose entry can say that with an
        // empty `include` and a native one, being a single service, has
        // nowhere to put it.
        Slot::Services => config.services.is_empty() && !config.isolation.none,
        // Likewise for hooks: one the developer wrote is their schema
        // step, whatever it is called.
        Slot::SchemaHook => config.hooks.is_empty(),
        _ => true,
    }
}

/// Whether detection may write into the single `dev` process at all, given
/// the config **as it was loaded**.
///
/// Two shapes qualify: a project with no processes configured, and the one
/// a developer writes when they want pando to fill something in — a lone
/// `[dev]` holding a `cwd` or an `env` and no command. Any other process
/// present, or a `[dev]` whose command they wrote themselves, means
/// detection stays out: a `[dev]` a developer wrote is an answer about its
/// ports too, and `[dev]` written beside `[processes]` is a file pando's
/// own loader refuses.
///
/// Asked once, of the config as loaded, and carried through the run:
/// the moment detection writes `[dev].cmd`, the config is indistinguishable
/// from one written by hand, and re-reading it would give the opposite
/// answer.
pub fn may_fill_dev(config: &Config) -> bool {
    match config.processes.len() {
        0 => true,
        1 => config
            .processes
            .get(DEV)
            .is_some_and(|dev| dev.cmd.trim().is_empty()),
        _ => false,
    }
}

/// The same permission, after an answer to [`Slot::Processes`] has been
/// applied: the single-process slots still have work to do when the answer
/// was the single-process form, and nothing to do when it was not.
pub fn fills_one_dev_process(config: &Config) -> bool {
    config.processes.len() == 1 && config.processes.contains_key(DEV)
}

/// A value the developer typed rather than chose.
///
/// A command carrying `{port:web}` brings its roles with it, so the port
/// question that would have followed is already answered.
pub fn custom(slot: Slot, value: &str) -> Candidate {
    let roles = roles_in(value);
    let ports = match slot {
        Slot::DevCmd | Slot::Processes => (!roles.is_empty()).then_some(PortsSpec::List(roles)),
        // What cannot be a list of variables is refused before it gets
        // here; [`port_spec`]'s single variable is the fallback for it.
        Slot::PortEnv => typed_ports(value).ok(),
        _ => None,
    };
    Candidate {
        value: value.to_string(),
        why: String::new(),
        // A command typed at the processes question is one process, named
        // `dev`: every question has a custom answer, and the custom answer
        // to "several processes?" is "no, this one".
        service: None,
        // The schema slot's answer is a whole `[[hooks]]` entry, and
        // without one there is nothing for `edits` or `apply` to write —
        // a typed answer here used to be accepted and then silently
        // dropped. No fingerprint, because a command pando did not
        // propose carries no globs it could key on: an empty one means
        // "every start", which is slow and correct, where a guessed one
        // would be fast and wrong.
        hook: (slot == Slot::SchemaHook).then(|| crate::config::HookConfig {
            name: SCHEMA_HOOK.to_string(),
            after: crate::config::HookPoint::Services,
            fingerprint: Vec::new(),
            cmd: value.to_string(),
            cwd: None,
            fallback: None,
            on: Some(crate::config::HookScope::Isolated),
        }),
        preselected: false,
        // A list the developer typed is a list of files they have. Seeding
        // from an example is an offer, and they did not take it.
        provision_from: BTreeMap::new(),
        // Typed by a human, by definition.
        needs_a_human: false,
        processes: (slot == Slot::Processes).then(|| {
            BTreeMap::from([(
                DEV.to_string(),
                ProcessConfig {
                    cmd: value.to_string(),
                    ports: ports.clone(),
                    ..Default::default()
                },
            )])
        }),
        ports,
    }
}

/// The `ports` a typed answer to the port slot writes, or why it cannot be
/// one.
///
/// Split exactly as the option that names several variables is joined —
/// `WEB_PORT, API_PORT` — so the same text is the same map whether it
/// matched that option or was typed in another order. One variable owns
/// `web` whatever it is called, as a typed answer always has; several each
/// own the role their own name says, which is how the option reads them.
/// Anything that is not an environment variable name is refused: a
/// variable called `PORT, API_PORT` is one nothing will ever read.
pub fn typed_ports(value: &str) -> Result<PortsSpec, String> {
    let vars = split_list(value);
    if let Some(bad) = vars.iter().find(|var| !is_env_name(var)) {
        return Err(format!(
            "{bad:?} is not an environment variable name — letters, digits and `_`, not \
             starting with a digit, and several of them separated by commas"
        ));
    }
    let vars: Vec<&str> = vars.iter().map(String::as_str).collect();
    match vars.as_slice() {
        [] => Err("it names no variable".to_string()),
        [one] => Ok(PortsSpec::Map(BTreeMap::from([(
            one.to_string(),
            one_role(one).to_string(),
        )]))),
        several => super::dev::roles_named_by(several)
            .map(PortsSpec::Map)
            .map_err(|unnamed| {
                format!(
                    "{unnamed} says no role — with several variables each one names its own, \
                     as `PORT` (the web role) or `<ROLE>_PORT` does"
                )
            }),
    }
}

/// Whether a name is one a shell can export: letters, digits and `_`, and
/// not a digit first.
pub fn is_env_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The `ports` an answer to the port slot writes: the whole map a candidate
/// carries, when a rule read several roles out of the project's own env
/// example, else the single variable the candidate named owning `web`.
fn port_spec(candidate: &Candidate) -> PortsSpec {
    candidate.ports.clone().unwrap_or_else(|| {
        PortsSpec::Map(BTreeMap::from([(
            candidate.value.clone(),
            one_role(&candidate.value).to_string(),
        )]))
    })
}

/// The role one port variable owns: `web`, the role a browser opens,
/// except for a bundler a device reads, which has a name of its own —
/// `RCT_METRO_PORT` owns `metro`.
fn one_role(var: &str) -> &'static str {
    crate::catalog::frameworks::device_role(var).unwrap_or(crate::config::WEB_ROLE)
}

/// Writes a chosen candidate into a config. The one place that knows what
/// each slot means, shared by the resolver and the tests.
pub fn apply(slot: Slot, candidate: &Candidate, config: &mut Config) {
    match slot {
        Slot::Install => config.project.install = Some(candidate.value.clone()),
        Slot::VersionFiles => config.runtime.version_files = split_list(&candidate.value),
        Slot::Prelude => config.runtime.prelude = Some(candidate.value.clone()),
        Slot::Base => config.project.base = Some(candidate.value.clone()),
        Slot::Clone => config.project.clone = Some(split_list(&candidate.value)),
        Slot::Provision => {
            config.project.provision = Some(split_list(&candidate.value));
            // Only what this answer brought: an answer with no seeds in it
            // is not a statement that the developer's own mapping is wrong.
            if !candidate.provision_from.is_empty() {
                config.project.provision_from = candidate.provision_from.clone();
            }
        }
        Slot::Processes => {
            for (name, process) in candidate.processes.iter().flatten() {
                config.processes.insert(name.clone(), process.clone());
            }
        }
        Slot::DevCmd => {
            let process = config.processes.entry(DEV.to_string()).or_default();
            process.cmd = candidate.value.clone();
            // Only when nobody has said: a `ports` the developer wrote is
            // an answer, and a command that carries `{port:web}` must not
            // overwrite it.
            if let Some(ports) = &candidate.ports
                && process.ports.is_none()
            {
                process.ports = Some(ports.clone());
            }
            // What the framework's rule brings besides the command, on the
            // same terms: an `env` or a `ready` the developer wrote is
            // theirs, and one written whole over it would lose their keys.
            if let Some(rule) = candidate.processes.as_ref().and_then(|p| p.get(DEV)) {
                if process.env.is_empty() {
                    process.env = rule.env.clone();
                }
                if process.ready.is_none() {
                    process.ready = rule.ready.clone();
                }
            }
        }
        Slot::PortEnv => {
            let process = config.processes.entry(DEV.to_string()).or_default();
            process.ports = Some(port_spec(candidate));
        }
        Slot::SchemaHook => {
            if let Some(hook) = &candidate.hook {
                config.hooks.push(hook.clone());
            }
        }
        // A set, not a value: it goes through [`apply_services`].
        Slot::Services => {}
        // A login is one service's, and only the question that asked for
        // it knows which: `actions::namespace_login` writes it. A slot to
        // free writes nothing. Namespaced settings are tables per service,
        // which `actions` writes from the answer itself.
        Slot::Login | Slot::FreeSlot | Slot::Namespaced => {}
    }
}

/// Writes a chosen *set* of compose services as one `[[services]]` entry.
///
/// An empty set is written too, as an entry with an empty `include`:
/// "none of them" is an answer, and an answer nothing records is asked
/// again on every start. It is the same shape the port slot uses for a
/// process that really has no ports.
pub fn apply_native_services(chosen: &[&Candidate], config: &mut Config) {
    for candidate in chosen {
        let hint = candidate.service.as_ref();
        let recipe = hint
            .and_then(|h| h.recipe())
            .filter(|recipe| *recipe != candidate.value)
            .map(str::to_string);
        let env = hint
            .and_then(|h| h.env_key.clone())
            .map(|key| BTreeMap::from([(key, candidate.value.clone())]))
            .unwrap_or_default();
        config.services.push(crate::config::ServiceConfig::Native {
            name: candidate.value.clone(),
            preset: recipe,
            port_env: None,
            init: None,
            cmd: None,
            ready: None,
            ready_timeout_s: None,
            env,
        });
    }
}

pub fn apply_services(file: &str, chosen: &[&Candidate], config: &mut Config) {
    let file = file.to_string();
    let include: Vec<String> = chosen.iter().map(|c| c.value.clone()).collect();
    let env: BTreeMap<String, String> = chosen
        .iter()
        .filter_map(|c| {
            let hint = c.service.as_ref()?;
            Some((hint.env_key.clone()?, c.value.clone()))
        })
        .collect();
    config.services.push(crate::config::ServiceConfig::Compose {
        file,
        include,
        env,
        ready_timeout_s: None,
    });
}

/// The keys of the one `[[services]]` entry a set answer appends.
///
/// Takes the file rather than reading it off a candidate, because the empty
/// answer has no candidates to read it from and still has to be written:
/// "none of them" is about a compose file that exists.
pub fn service_entry(
    file: &str,
    chosen: &[&Candidate],
) -> (&'static str, Vec<(String, toml_edit::Value)>) {
    let array = Slot::Services
        .array()
        .expect("the services slot appends a [[table]] entry");
    let mut entries: Vec<(String, toml_edit::Value)> = vec![
        ("kind".to_string(), "compose".into()),
        ("file".to_string(), file.to_string().into()),
        (
            "include".to_string(),
            toml_edit::Value::Array(toml_edit::Array::from_iter(
                chosen.iter().map(|c| c.value.clone()),
            )),
        ),
    ];
    let mut env = toml_edit::InlineTable::new();
    for candidate in chosen {
        if let Some(key) = candidate.service.as_ref().and_then(|h| h.env_key.as_ref()) {
            env.insert(key, candidate.value.as_str().into());
        }
    }
    if !env.is_empty() {
        entries.push(("env".to_string(), toml_edit::Value::InlineTable(env)));
    }
    (array, entries)
}

/// The keys of one `[[services]] kind = "native"` entry.
///
/// One entry per service, because a native entry runs one server — which
/// is also why the recorded negative for this mechanism is not an empty
/// `include` but `[isolation] none`.
pub fn native_entry(chosen: &Candidate) -> (&'static str, Vec<(String, toml_edit::Value)>) {
    let array = Slot::Services
        .array()
        .expect("the services slot appends a [[table]] entry");
    let recipe = chosen
        .service
        .as_ref()
        .and_then(ServiceHint::recipe)
        .unwrap_or(chosen.value.as_str());
    let mut entries: Vec<(String, toml_edit::Value)> = vec![
        ("kind".to_string(), "native".into()),
        ("name".to_string(), chosen.value.clone().into()),
    ];
    // Only when it differs: `name = "postgres"` already names the recipe,
    // and a second line saying so is noise in a file people read.
    if recipe != chosen.value {
        entries.push(("preset".to_string(), recipe.to_string().into()));
    }
    if let Some(key) = chosen.service.as_ref().and_then(|h| h.env_key.as_ref()) {
        let mut env = toml_edit::InlineTable::new();
        env.insert(key, chosen.value.as_str().into());
        entries.push(("env".to_string(), toml_edit::Value::InlineTable(env)));
    }
    (array, entries)
}

/// The keys of the one `[[table]]` entry a set answer appends, or `None`
/// for a slot whose answer is a key in a table.
pub fn array_edits(
    slot: Slot,
    chosen: &[&Candidate],
) -> Option<(&'static str, Vec<(String, toml_edit::Value)>)> {
    let array = slot.array()?;
    let mut entries: Vec<(String, toml_edit::Value)> = Vec::new();
    match slot {
        Slot::Services => {
            let hint = chosen.iter().find_map(|c| c.service.as_ref())?;
            return Some(match &hint.source {
                ServiceSource::Compose { file } => service_entry(file, chosen),
                // One entry per native service, so this path is only ever
                // reached for the first of them; `apply_service_answer`
                // writes them one at a time.
                ServiceSource::Native { .. } => native_entry(chosen.first()?),
            });
        }
        Slot::SchemaHook => {
            let hook = chosen.first()?.hook.as_ref()?;
            entries.push(("name".to_string(), hook.name.clone().into()));
            entries.push(("after".to_string(), "services".into()));
            if !hook.fingerprint.is_empty() {
                entries.push((
                    "fingerprint".to_string(),
                    toml_edit::Value::Array(toml_edit::Array::from_iter(
                        hook.fingerprint.iter().cloned(),
                    )),
                ));
            }
            entries.push(("cmd".to_string(), hook.cmd.clone().into()));
            if let Some(on) = hook.on {
                entries.push(("on".to_string(), on.as_str().into()));
            }
        }
        _ => return None,
    }
    Some((array, entries))
}

/// The same choice, as the keys to patch into `pando.toml`.
///
/// Separate from [`apply`] because the file is edited in place rather than
/// re-serialised from the struct: the developer's comments and ordering
/// survive, and only the keys pando decided are touched.
pub fn edits(slot: Slot, candidate: &Candidate) -> Vec<Edit> {
    // Owned, not `&'static`: `[processes.<app>]` is a table whose name
    // detection only learns by reading the repository.
    let single = |table: &[&str], key: &str, value: toml_edit::Value| Edit {
        table: table.iter().map(|t| t.to_string()).collect(),
        key: key.to_string(),
        value,
    };
    let keyed = slot.key();
    match slot {
        Slot::Prelude | Slot::Base => {
            let (table, key) = keyed.expect("this slot writes one key");
            vec![single(table, key, candidate.value.clone().into())]
        }
        Slot::Install | Slot::DevCmd => {
            let (table, key) = keyed.expect("this slot writes one key");
            let mut out = vec![single(table, key, candidate.value.clone().into())];
            if let Some(PortsSpec::List(roles)) = &candidate.ports {
                out.push(single(
                    &["dev"],
                    "ports",
                    toml_edit::Value::Array(toml_edit::Array::from_iter(roles.iter().cloned())),
                ));
            }
            // The rest of `[dev]` a framework's rule proposes with its
            // command: Expo's longer wait.
            if let Some(rule) = candidate.processes.as_ref().and_then(|p| p.get(DEV)) {
                if !rule.env.is_empty() {
                    out.push(single(&["dev"], "env", env_value(&rule.env)));
                }
                if let Some(ready) = &rule.ready {
                    out.push(single(&["dev"], "ready", ready_value(ready)));
                }
            }
            out
        }
        Slot::VersionFiles | Slot::Clone => {
            let (table, key) = keyed.expect("this slot writes one key");
            vec![single(
                table,
                key,
                toml_edit::Value::Array(toml_edit::Array::from_iter(split_list(&candidate.value))),
            )]
        }
        Slot::Provision => {
            let (table, key) = keyed.expect("this slot writes one key");
            let mut out = vec![single(
                table,
                key,
                toml_edit::Value::Array(toml_edit::Array::from_iter(split_list(&candidate.value))),
            )];
            // The list says which files a worktree gets; this says where
            // the ones the main checkout does not have come from. Two keys,
            // because the second is only true of some of the first.
            if !candidate.provision_from.is_empty() {
                let mut inline = toml_edit::InlineTable::new();
                for (destination, source) in &candidate.provision_from {
                    inline.insert(destination, source.as_str().into());
                }
                out.push(single(
                    &["project"],
                    "provision_from",
                    toml_edit::Value::InlineTable(inline),
                ));
            }
            out
        }
        Slot::PortEnv => {
            let (table, key) = keyed.expect("this slot writes one key");
            let value = match port_spec(candidate) {
                PortsSpec::Map(map) => {
                    let mut inline = toml_edit::InlineTable::new();
                    for (var, role) in &map {
                        inline.insert(var, role.as_str().into());
                    }
                    toml_edit::Value::InlineTable(inline)
                }
                PortsSpec::List(roles) => {
                    toml_edit::Value::Array(toml_edit::Array::from_iter(roles.iter().cloned()))
                }
            };
            vec![single(table, key, value)]
        }
        Slot::Processes => process_edits(candidate),
        // Both of these append a whole `[[table]]` entry; [`array_edits`]
        // is what knows how to write one.
        Slot::Services | Slot::SchemaHook => Vec::new(),
        // Written by `actions::namespace_login`, into a table named for
        // the service it was asked about; a slot to free writes nothing;
        // namespaced settings are a table per service the answer names.
        Slot::Login | Slot::FreeSlot | Slot::Namespaced => Vec::new(),
    }
}

/// One key pando is about to write, and the table it belongs in.
#[derive(Debug, Clone)]
pub struct Edit {
    pub table: Vec<String>,
    pub key: String,
    pub value: toml_edit::Value,
}

/// A whole `[processes]` table, written out.
///
/// One process named `dev` takes the `[dev]` shorthand instead: that is
/// what the shorthand is for, and it keeps the single-process file the
/// shape every example in the docs has.
fn process_edits(candidate: &Candidate) -> Vec<Edit> {
    let mut out = Vec::new();
    let processes = candidate.processes.clone().unwrap_or_default();
    let shorthand = processes.len() == 1 && processes.contains_key(DEV);
    for (name, process) in &processes {
        let table: Vec<String> = if shorthand {
            vec![DEV.to_string()]
        } else {
            vec!["processes".to_string(), name.clone()]
        };
        let mut push = |key: &str, value: toml_edit::Value| {
            out.push(Edit {
                table: table.clone(),
                key: key.to_string(),
                value,
            })
        };
        push("cmd", process.cmd.clone().into());
        if let Some(cwd) = &process.cwd {
            push("cwd", cwd.clone().into());
        }
        match &process.ports {
            Some(PortsSpec::List(roles)) => push(
                "ports",
                toml_edit::Value::Array(toml_edit::Array::from_iter(roles.iter().cloned())),
            ),
            Some(PortsSpec::Map(map)) => {
                let mut inline = toml_edit::InlineTable::new();
                for (var, role) in map {
                    inline.insert(var, role.as_str().into());
                }
                push("ports", toml_edit::Value::InlineTable(inline));
            }
            None => {}
        }
        if !process.env.is_empty() {
            push("env", env_value(&process.env));
        }
        if let Some(ready) = &process.ready {
            push("ready", ready_value(ready));
        }
        // Said either way when said at all: `page = true` over a device
        // framework's default is as much an answer as `page = false`.
        if let Some(page) = process.page {
            push("page", page.into());
        }
    }
    out
}

/// A process's `env`, as the inline table it is written as.
fn env_value(env: &BTreeMap<String, String>) -> toml_edit::Value {
    let mut inline = toml_edit::InlineTable::new();
    for (var, value) in env {
        inline.insert(var, value.as_str().into());
    }
    toml_edit::Value::InlineTable(inline)
}

/// A process's `ready`, as the inline table it is written as.
fn ready_value(ready: &crate::config::ReadySpec) -> toml_edit::Value {
    let mut inline = toml_edit::InlineTable::new();
    if let Some(role) = &ready.role {
        inline.insert("role", role.as_str().into());
    }
    if let Some(timeout) = ready.timeout_s {
        inline.insert("timeout_s", (timeout as i64).into());
    }
    toml_edit::Value::InlineTable(inline)
}

/// The roles a command asks for by carrying `{port:<role>}`.
///
/// A developer who types their own command with a placeholder in it has
/// declared the role by using it; there is no second question to ask.
pub fn roles_in(cmd: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in cmd.split("{port:").skip(1) {
        let Some((role, _)) = piece.split_once('}') else {
            continue;
        };
        if !role.is_empty() && !out.contains(&role.to_string()) {
            out.push(role.to_string());
        }
    }
    out
}

/// Multi-valued slots carry their list as one comma-separated string, so a
/// `Candidate` stays one value whatever the slot holds.
/// The one-value form of a list slot's answer, which [`split_list`] takes
/// apart again. A program answering through `init --answers` sends a JSON
/// array and never has to know the separator.
pub fn join_list(values: &[String]) -> String {
    values.join(",")
}

fn split_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// The value `init --answers` takes for this candidate at this slot: what
/// a command pando prints puts in the file, so that running it chooses
/// exactly this candidate.
///
/// The shapes are the answers file's own: a JSON array for a list slot
/// and for the set question, the whole process tables at the process
/// list — its option's text is a summary, and a summary typed back is a
/// command nobody wrote — and the option's own text everywhere else.
pub fn answers_value(slot: Slot, candidate: &Candidate) -> serde_json::Value {
    let strings = |values: Vec<String>| {
        serde_json::Value::Array(values.into_iter().map(serde_json::Value::String).collect())
    };
    if slot.is_list() {
        return strings(split_list(&candidate.value));
    }
    if slot.is_multi() {
        return strings(vec![candidate.value.clone()]);
    }
    if slot == Slot::Processes
        && let Some(tables) = &candidate.processes
        && let Ok(value) = serde_json::to_value(tables)
    {
        return value;
    }
    serde_json::Value::String(candidate.value.clone())
}

/// Keeps the first of each repeated value: the Makefile target and the Go
/// rule both say `go run .`, and that is one candidate, not two.
pub(super) fn dedup_by_value(candidates: &mut Vec<Candidate>) {
    let mut seen: Vec<String> = Vec::new();
    candidates.retain(|c| {
        if seen.contains(&c.value) {
            return false;
        }
        seen.push(c.value.clone());
        true
    });
}

/// The TOML an answer would be, ready to paste: what pando would write for
/// the first option, or the slot's key with a placeholder when the rules
/// offered nothing.
///
/// Built from [`edits`] and [`array_edits`] — the same functions that
/// write the answer — so the snippet a question prints cannot drift from
/// the file an answer produces.
pub fn snippet(slot: Slot, chosen: &[&Candidate]) -> String {
    let mut doc = toml_edit::DocumentMut::new();
    if let Some((array, entries)) = array_edits(slot, chosen) {
        let mut table = toml_edit::Table::new();
        for (key, value) in entries {
            table.insert(&key, toml_edit::value(value));
        }
        let mut tables = toml_edit::ArrayOfTables::new();
        tables.push(table);
        doc.insert(array, toml_edit::Item::ArrayOfTables(tables));
        return doc.to_string();
    }
    let edits: Vec<Edit> = match chosen.first() {
        Some(candidate) => edits(slot, candidate),
        None => match slot.key() {
            Some((table, key)) => vec![Edit {
                table: table.iter().map(|t| t.to_string()).collect(),
                key: key.to_string(),
                value: match slot.is_list() {
                    true => toml_edit::Value::Array(toml_edit::Array::from_iter([format!(
                        "<{}>",
                        slot.custom_noun()
                    )])),
                    false => format!("<{}>", slot.custom_noun()).into(),
                },
            }],
            None => Vec::new(),
        },
    };
    for edit in edits {
        let mut table = doc.as_table_mut();
        for (depth, name) in edit.table.iter().enumerate() {
            let item = table
                .entry(name)
                .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()));
            let next = item
                .as_table_mut()
                .expect("every table on an edit's path is a table");
            // `[processes.web]` rather than an empty `[processes]` above it.
            next.set_implicit(depth + 1 < edit.table.len());
            table = next;
        }
        table.insert(&edit.key, toml_edit::value(edit.value));
    }
    doc.to_string()
}
