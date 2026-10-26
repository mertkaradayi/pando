//! The services section: isolation, native services and their engines, and
//! compose entries.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::actions::Machine;
use crate::config::{Config, ServiceConfig};
use crate::paths::PandoPaths;
use crate::process as proc;
use crate::{actions, detect, native, recipes};

use super::report::{
    ComposeEntryReport, EngineBinary, Finding, IncludedService, IsolationReport, NativeInstance,
    NativeServiceReport, Section, ServicesReport,
};
use super::worktrees::declared_services;

pub(super) fn services_report(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
    findings: &mut Vec<Finding>,
) -> ServicesReport {
    let mut compose = Vec::new();
    let recipes = recipes::Recipes::load(&paths.recipes_dir());
    let isolation = isolation_report(paths, config, machine, &recipes);
    let native = native_report(paths, config, machine, findings);
    for service in &config.services {
        let ServiceConfig::Compose {
            file, include, env, ..
        } = service
        else {
            continue;
        };
        let path = paths.root().join(file);
        let parsed = path.is_file().then(|| crate::compose::read(&path));
        if parsed.is_none() {
            findings.push(Finding::problem(
                Section::Services,
                format!(
                    "the compose file {file:?} is not in this repository — `start --isolated` \
                     has nothing to bring up"
                ),
                "point `[[services]] file` at a file that is there, or drop the entry",
            ));
        }
        let read = match &parsed {
            Some(Ok(read)) => Some(read),
            Some(Err(e)) => {
                findings.push(Finding::problem(
                    Section::Services,
                    format!("the compose file {file:?} could not be read: {e:#}"),
                    "fix the file, or drop the `[[services]]` entry that names it",
                ));
                None
            }
            None => None,
        };
        let mut services = Vec::new();
        for name in include {
            let declared = read.is_some_and(|r| r.services.contains_key(name));
            let healthcheck = read
                .and_then(|r| r.services.get(name))
                .is_some_and(|s| s.healthcheck);
            if read.is_some() && !declared {
                findings.push(Finding::problem(
                    Section::Services,
                    format!(
                        "`include` names the service {name:?}, which {file} does not declare — \
                         `start --isolated` would fail"
                    ),
                    format!("drop {name:?} from `include`, or add it to {file}"),
                ));
            }
            if declared && !healthcheck {
                findings.push(
                    Finding::note(
                        Section::Services,
                        format!(
                            "the service {name:?} declares no healthcheck, so readiness is a \
                             connect that only proves something is behind the port — a database \
                             still initialising can pass it"
                        ),
                    )
                    .with_fix(format!("add a `healthcheck:` to {name} in {file}")),
                );
            }
            services.push(IncludedService {
                name: name.clone(),
                ready: if healthcheck {
                    "healthcheck"
                } else {
                    "connect"
                },
                declared,
                env_key: env
                    .iter()
                    .find(|(_, service)| *service == name)
                    .map(|(key, _)| key.clone()),
            });
        }
        let unresolved = read.map(|r| r.unresolved.clone()).unwrap_or_default();
        if let Some(described) = unresolved.describe() {
            findings.push(
                Finding::note(
                    Section::Services,
                    format!(
                        "{file} carries {described}, which pando's own reader does not follow — \
                         the ports and volumes it read may not be the ones compose would use"
                    ),
                )
                .with_fix(
                    "with Docker installed pando asks `docker compose config`, which resolves \
                     them; without it, inline what the key brings in",
                ),
            );
        }
        compose.push(ComposeEntryReport {
            file: file.clone(),
            file_exists: parsed.is_some(),
            services,
            extends: unresolved.extends.clone(),
            include: unresolved.include,
            error: match &parsed {
                Some(Err(e)) => Some(format!("{e:#}")),
                _ => None,
            },
        });
    }
    name_collisions(paths, config, findings);
    super::workers::shared_queue_findings(paths, config, findings);
    ServicesReport {
        compose,
        native,
        isolation,
    }
}

/// Which mechanism this project's private services would use, and the
/// evidence that decided it.
///
/// Asked of the same function the start path asks, through the shell
/// doctor was given, so the report is the decision rather than a second
/// opinion about it. Once config has answered, a start runs what config
/// says and asks detection nothing, so that is what is reported; what
/// detection would choose is a line of its own, and only where it would
/// now choose differently.
fn isolation_report(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
    recipes: &recipes::Recipes,
) -> IsolationReport {
    let answered = !config.services.is_empty() || config.isolation.none;
    let signals = detect::signals_of(&paths.project);
    let (script, names) = actions::machine_evidence_script(paths, recipes);
    let evidence = match (machine.shell)(&script) {
        Some(text) => actions::machine_evidence_from(&text, &names),
        None => detect::MachineEvidence::unknown(),
    };
    let choice = detect::service_choice_for(paths.root(), &signals, &evidence, config);
    let (mechanism, lines) = match configured_isolation(config) {
        Some((mechanism, mut lines)) => {
            if let Some(detected) = choice.mechanism
                && !config
                    .services
                    .iter()
                    .any(|s| s.brings_anything_up() && service_kind(s) == detected)
            {
                lines.push(format!(
                    "detection alone would choose {detected} now: {}",
                    choice.evidence.join("; ")
                ));
            }
            (mechanism, lines)
        }
        None => (choice.mechanism, choice.evidence),
    };
    IsolationReport {
        mechanism: mechanism.map(str::to_string),
        prefer: config.isolation.prefer.clone(),
        answered,
        evidence: lines
            .iter()
            .map(|line| super::config::with_real_user_config(paths, line))
            .collect(),
    }
}

/// What config already says about isolation: the mechanism a start runs,
/// named after the first `[[services]]` entry that brings anything up, and
/// one line per entry that says it. An entry that includes none of a
/// compose file's services is an answer that runs nothing, so a config of
/// only those has nothing to isolate. `None` while config says nothing.
fn configured_isolation(config: &Config) -> Option<(Option<&'static str>, Vec<String>)> {
    if config.isolation.none {
        return Some((
            None,
            vec!["`[isolation] none` says this project has nothing to isolate".to_string()],
        ));
    }
    if config.services.is_empty() {
        return None;
    }
    let lines = config
        .services
        .iter()
        .map(|service| match service {
            ServiceConfig::Compose { file, include, .. } => match include.is_empty() {
                true => format!(
                    "`[[services]]` names the compose file {file}, for none of its services"
                ),
                false => format!(
                    "`[[services]]` names the compose file {file}, for {}",
                    include.join(", ")
                ),
            },
            ServiceConfig::Native { name, .. } => {
                format!("`[[services]]` names the native service {name}")
            }
        })
        .collect();
    let mechanism = config
        .services
        .iter()
        .find(|service| service.brings_anything_up())
        .map(service_kind);
    Some((mechanism, lines))
}

/// A `[[services]]` entry's `kind`, spelled the way the file spells it.
fn service_kind(service: &ServiceConfig) -> &'static str {
    match service {
        ServiceConfig::Compose { .. } => "compose",
        ServiceConfig::Native { .. } => "native",
    }
}

/// What each `[[services]] kind = "native"` block resolved to, and
/// whether this machine can run it.
///
/// A missing engine is a line, not a crash: doctor reports, and pando
/// never installs one. The same goes for a recipe file that does not
/// parse — it is reported once, as a problem, and the recipes it shadows
/// are named.
fn native_report(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
    findings: &mut Vec<Finding>,
) -> Vec<NativeServiceReport> {
    let entries = native::Entry::all(config);
    if entries.is_empty() {
        return Vec::new();
    }
    let recipes = recipes::Recipes::load(&paths.recipes_dir());
    for (name, broken) in recipes.broken() {
        findings.push(Finding::problem(
            Section::Services,
            format!(
                "the recipe file {} does not load: {}",
                broken.path.display(),
                broken.error
            ),
            format!(
                "fix it, or delete it — until then nothing can run the recipe {name:?}, not \
                 even a built-in of that name, because a file that shadows one must not fail \
                 quietly back to it"
            ),
        ));
    }
    let bin_dir = paths.home.join("bin");
    let mut out = Vec::new();
    for entry in &entries {
        let datadir = paths.service_data_dir(WORKTREE_PLACEHOLDER, entry.name);
        let socket_root = paths
            .service_socket_dir(WORKTREE_PLACEHOLDER, entry.name)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let mut report = NativeServiceReport {
            name: entry.name.to_string(),
            preset: entry.preset().to_string(),
            source: None,
            overrides: Vec::new(),
            datadir: datadir.display().to_string(),
            socket_root,
            engine: Vec::new(),
            engine_asked: true,
            version: None,
            install: None,
            env_key: None,
            notes: None,
            untested: false,
            instances: native_instances(paths, entry.name),
            error: None,
        };
        let resolved = match native::resolve(&recipes, entry) {
            Ok(resolved) => resolved,
            Err(e) => {
                report.error = Some(format!("{e:#}"));
                findings.push(Finding::problem(
                    Section::Services,
                    format!("{e:#}"),
                    format!(
                        "name a `preset` that exists, give the entry its own `cmd`, or write \
                         {}.toml in {}",
                        entry.preset(),
                        paths.recipes_dir().display()
                    ),
                ));
                out.push(report);
                continue;
            }
        };
        report.source = Some(resolved.source.describe());
        report.overrides = resolved.overrides.iter().map(|o| o.to_string()).collect();
        report.install = resolved.recipe.install.clone();
        report.notes = resolved.recipe.notes.clone();
        report.untested = resolved.recipe.untested;
        if resolved.recipe.untested {
            // A note, because nothing is wrong: the recipe may work
            // perfectly. What a developer needs to know is which thing
            // to suspect first when it does not, and that is not
            // guessable from a failure.
            findings.push(
                Finding::note(
                    Section::Services,
                    format!(
                        "the {:?} recipe has never been run against a real server — pando \
                         ships it so a developer who has the engine can try it, not because \
                         it is proven",
                        resolved.recipe.name
                    ),
                )
                .with_fix(format!(
                    "if it does not work, the recipe is the first thing to suspect: copy it \
                     into {}/{}.toml and fix it there",
                    paths.recipes_dir().display(),
                    resolved.recipe.name
                )),
            );
        }
        let (mapping, _) = entry.env_map(Some(&resolved.recipe));
        report.env_key = mapping.keys().next().cloned();
        let Some((engine, version)) = probe_engine(machine, &bin_dir, &resolved.recipe) else {
            // The shell did not answer. That is not evidence about the
            // engine, so its binaries are listed and nothing is claimed
            // about them.
            findings.push(Finding::note(
                Section::Services,
                format!(
                    "pando could not ask the shell where the {:?} recipe's engine is: the shell \
                     did not answer inside its deadline",
                    resolved.recipe.name
                ),
            ));
            report.engine = resolved
                .recipe
                .binaries
                .iter()
                .map(|name| EngineBinary {
                    name: name.clone(),
                    path: None,
                })
                .collect();
            report.engine_asked = false;
            out.push(report);
            continue;
        };
        let missing: Vec<&str> = engine
            .iter()
            .filter(|b| b.path.is_none())
            .map(|b| b.name.as_str())
            .collect();
        if !missing.is_empty() {
            // A note, not a problem, for the same reason a missing Docker
            // is one: only `start --isolated` needs this engine, and a
            // project whose developer runs against a shared database is
            // not broken by not having a private one. `doctor` exits 1 on
            // a problem, and "you could not run a mode you are not using"
            // is not that.
            findings.push(
                Finding::note(
                    Section::Services,
                    format!(
                        "the service {:?} runs the {:?} recipe, and {} not on PATH — \
                         `start --isolated` cannot run it, though a plain `start` still can",
                        entry.name,
                        resolved.recipe.name,
                        match missing.len() {
                            1 => format!("{} is", missing[0]),
                            _ => format!("{} are", missing.join(", ")),
                        }
                    ),
                )
                .with_fix(match &resolved.recipe.install {
                    Some(hint) => format!("install it: {hint} — pando never will"),
                    None => format!(
                        "install it, or put a shim in {} — pando never installs an engine",
                        bin_dir.display()
                    ),
                }),
            );
        }
        report.engine = engine;
        report.version = version;
        out.push(report);
    }
    out
}

/// Stands where a worktree's name goes, so the data directory can be
/// reported for a project rather than for one worktree.
const WORKTREE_PLACEHOLDER: &str = "<worktree>";

/// Marks the engine probe's two answers, the way the tools probe marks
/// its three.
pub(super) const NATIVE_BIN_MARK: &str = "pando-native-bin ";
pub(super) const NATIVE_VERSION_MARK: &str = "pando-native-version ";

/// Where each of a recipe's binaries resolved, and what the first of them
/// says its version is, or `None` when the shell did not answer.
///
/// Asked through the same injected shell the tools probe uses, and with
/// the same PATH the start path would give the recipe — pando's own `bin`
/// first, so a shim a developer put there is what doctor reports.
fn probe_engine(
    machine: &Machine<'_>,
    bin_dir: &Path,
    recipe: &recipes::Recipe,
) -> Option<(Vec<EngineBinary>, Option<String>)> {
    let binaries = recipe.binaries.clone();
    if binaries.is_empty() {
        return Some((Vec::new(), None));
    }
    let mut script = format!(
        "export PATH={}:\"$PATH\"\n",
        proc::shell_quote(&bin_dir.display().to_string())
    );
    for binary in &binaries {
        let quoted = proc::shell_quote(binary);
        let _ = writeln!(
            script,
            "if __pando_b=$(command -v {quoted} 2>/dev/null); then \
             printf '{NATIVE_BIN_MARK}%s %s\\n' {quoted} \"$__pando_b\"; fi"
        );
    }
    if let Some(version) = recipe.version_cmd() {
        let _ = writeln!(
            script,
            "printf '{NATIVE_VERSION_MARK}%s\\n' \"$({version} 2>&1 | head -n 1)\""
        );
    }
    let text = (machine.shell)(&script)?;
    let mut found: BTreeMap<String, String> = BTreeMap::new();
    let mut version = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(NATIVE_BIN_MARK)
            && let Some((name, path)) = rest.split_once(' ')
        {
            found.insert(name.to_string(), path.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix(NATIVE_VERSION_MARK)
            && !rest.trim().is_empty()
        {
            version = Some(rest.trim().to_string());
        }
    }
    let engine = binaries
        .iter()
        .map(|name| EngineBinary {
            name: name.clone(),
            path: found.get(name).cloned(),
        })
        .collect();
    // A version printed by a binary that is not there is the shell
    // reporting its own "command not found", not an engine.
    let version = version.filter(|_| found.contains_key(&binaries[0]));
    Some((engine, version))
}

/// The worktrees that already have data for this service, and what their
/// data directories say about themselves.
fn native_instances(paths: &PandoPaths, service: &str) -> Vec<NativeInstance> {
    let Ok(entries) = std::fs::read_dir(paths.project_dir().join("data")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter(|worktree| paths.service_data_dir(worktree, service).is_dir())
        .map(|worktree| {
            let datadir = paths.service_data_dir(&worktree, service);
            let marker = native::marker(&datadir);
            NativeInstance {
                initialised: marker.map(|marker| {
                    format!(
                        "{} {}",
                        match marker.adopted {
                            true => "adopted",
                            false => "initialised",
                        },
                        marker.at.format("%Y-%m-%d")
                    )
                }),
                socket_dir: paths
                    .service_socket_dir(&worktree, service)
                    .display()
                    .to_string(),
                datadir: datadir.display().to_string(),
                worktree,
            }
        })
        .collect()
}

/// A compose service whose name is already a role of one of this project's
/// processes.
///
/// A role is one port and belongs to one thing, so the two cannot both
/// have it: `validate_services` refuses the pair, and a config that holds
/// it does not load. pando now refuses to *write* that pair — the answer is
/// checked against the loader before a key reaches disk — but the two
/// halves are still sitting in the repository waiting to be offered, and a
/// developer who answers the services question is told "no" without ever
/// having been told why. This is the why, at rest, before the question.
///
/// Only for services that have not been answered for yet: once one is in
/// `include` the config does not load at all, and that is the Config
/// section's headline problem rather than a second copy of it here.
///
/// And only in a compose file pando takes services from: the root's — in
/// a folder of repositories, the one detection proposes from a
/// repository's root — or one a `[[services]]` table names.
/// `docker/compose.yml` below the root is never offered, so a name in it
/// collides with nothing.
///
/// The fix is pando's side of the pair, the role: the compose file is the
/// repository's, and nobody should edit a committed file to suit pando.
fn name_collisions(paths: &PandoPaths, config: &Config, findings: &mut Vec<Finding>) {
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for (process, spec) in &config.processes {
        for role in spec.roles() {
            owner.insert(role, format!("the process {process:?}"));
        }
    }
    if owner.is_empty() {
        return;
    }
    let answered = declared_services(config);
    // A folder's is read from its repositories, which costs the git calls
    // a repository's own root does not need.
    let proposed = match paths.project.is_multi_repo() {
        true => detect::compose_file(paths.root(), &detect::signals_of(&paths.project)),
        false => crate::compose::find(paths.root()),
    };
    let mut offered: Vec<String> = proposed.into_iter().collect();
    for service in &config.services {
        if let ServiceConfig::Compose { file, .. } = service
            && !offered.contains(file)
        {
            offered.push(file.clone());
        }
    }
    for file in &offered {
        let Ok(read) = crate::compose::read(&paths.root().join(file)) else {
            continue;
        };
        for name in read.services.keys() {
            if answered.contains(name) {
                continue;
            }
            let Some(owner) = owner.get(name) else {
                continue;
            };
            findings.push(
                Finding::note(
                    Section::Services,
                    format!(
                        "{file} declares a service called {name:?}, and {owner} already \
                         owns the role {name:?} — a role is one port and belongs to one \
                         thing, so pando will refuse to run a private copy of it under \
                         that name"
                    ),
                )
                .with_fix(format!(
                    "give that process's role {name:?} another name in its `ports` — \
                     answering the services question with {name:?} is refused until it moves"
                )),
            );
        }
    }
}
