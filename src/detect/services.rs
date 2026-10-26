//! Services and the schema hook: what a project talks to, where each
//! service can come from, and how its schema is prepared.

use std::collections::BTreeMap;
use std::path::Path;

use crate::catalog::images::{self, Role};
use crate::catalog::package_managers;
use crate::config::Config;

use super::dev::{listed, lockfiles, python_runner, script_runner};
use super::proposal::{Candidate, ComposeResolver, Proposal, ServiceHint, Slot};
use super::signals::Signals;

/// Key suffixes that hold an address pando can rewrite. `_NAME`, `_USER`
/// and `_PASSWORD` are about the same service and hold nothing pando can
/// point anywhere, so they are not candidates.
///
/// `_HOST` is not one either, and deliberately: `services::rewrite` can put
/// a port into a URL or replace a bare number, and a bare `localhost` has
/// nowhere to put one. Proposing it wrote a mapping that could never be
/// satisfied and failed the start that used it.
///
/// In preference order: a URL carries everything, a DSN nearly as much, and
/// a port is a number pando can simply replace.
const ADDRESS_SUFFIXES: [&str; 3] = ["_URL", "_DSN", "_PORT"];

/// Key suffixes that hold the host half of a split address, beside a
/// `_PORT` of the same stem: `POSTGRES_SERVER=localhost` with
/// `POSTGRES_PORT=5432` is `postgres://localhost:5432` in two keys. Never
/// a candidate themselves, for the reason `_HOST` is not an address
/// suffix; only evidence that the port beside them is a service's.
const HOST_SUFFIXES: [&str; 3] = ["_HOST", "_HOSTNAME", "_SERVER"];

/// What a rule can say about which env key names one compose service.
#[derive(Debug, Clone, PartialEq, Eq)]
enum EnvKey {
    /// This key, and no other service is using it.
    Found(String),
    /// The only key a rule would have used is already pointed at another
    /// service of the same file. Two databases from one image is the
    /// ordinary case; pando cannot tell which one the app means, so it asks.
    TakenBy { key: String, service: String },
    /// Nothing in the env example names this service at all.
    Nothing,
}

/// The env key the app reads to find one compose service, when a rule can
/// say which.
///
/// `claimed` is every key an earlier service of the same file already owns.
/// An env map with one key pointing at two services is not a thing that can
/// be written down: the later one silently wins, the earlier one runs with
/// nothing addressing it, and the app talks to whichever pando happened to
/// write last.
fn env_key_for(
    service: &str,
    image: Option<&str>,
    env: &[(String, String)],
    claimed: &BTreeMap<String, String>,
) -> EnvKey {
    let mut prefixes: Vec<String> = vec![service.to_uppercase()];
    if let Some(known) = image.and_then(images::known) {
        prefixes.extend(known.env_prefixes.iter().map(|k| (*k).to_string()));
    }
    // By suffix first, then by prefix: the best *kind* of key wins over
    // the best-matching name, because a `_PORT` that merely belongs to a
    // differently named prefix for the same service is a worse answer than
    // the `_URL` that carries the credentials too.
    let mut taken: Option<EnvKey> = None;
    for suffix in ADDRESS_SUFFIXES {
        for prefix in &prefixes {
            let Some((key, _)) = env
                .iter()
                .find(|(key, _)| key == &format!("{prefix}{suffix}"))
            else {
                continue;
            };
            match claimed.get(key) {
                None => return EnvKey::Found(key.clone()),
                // Remembered rather than returned: a later suffix may still
                // find this service a key of its own, and only when none
                // does is "somebody else has it" the answer.
                Some(owner) if taken.is_none() => {
                    taken = Some(EnvKey::TakenBy {
                        key: key.clone(),
                        service: owner.clone(),
                    });
                }
                Some(_) => {}
            }
        }
    }
    taken.unwrap_or(EnvKey::Nothing)
}

fn is_app_service(image: Option<&str>) -> bool {
    image
        .and_then(images::known)
        .is_some_and(|known| known.role == Role::App)
}

/// Where a service a proposal offers would come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceSource {
    /// Declared by the project's own compose file.
    Compose { file: String },
    /// Run by a recipe, because nothing in the repository says how.
    Native { recipe: String },
}

/// What this machine can actually run, as evidence rather than as a guess.
///
/// Injected rather than probed here: `detect` is pure file reading and
/// runs on the start path before anything is spawned. `actions` probes it
/// through the shell a real spawn uses and `doctor` through the one it
/// was given, so what each of them reports is what the start path would
/// have found. An empty one means "nobody looked", which is not the same
/// as "nothing is installed" — see [`MachineEvidence::unknown`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineEvidence {
    /// Whether anything looked at all.
    pub probed: bool,
    /// Whether a `docker` that answers is on PATH.
    pub docker: bool,
    /// Recipe name to whether every binary it needs is on PATH.
    pub engines: BTreeMap<String, bool>,
}

impl MachineEvidence {
    /// Nobody looked. Every question about this machine answers "maybe",
    /// which is what keeps detection's own tests independent of the host
    /// they run on.
    pub fn unknown() -> MachineEvidence {
        MachineEvidence::default()
    }

    /// Whether this machine can run the recipe, or `None` when nobody
    /// looked.
    pub fn can_run(&self, recipe: &str) -> Option<bool> {
        if !self.probed {
            return None;
        }
        Some(self.engines.get(recipe).copied().unwrap_or(false))
    }

    pub fn has_docker(&self) -> Option<bool> {
        self.probed.then_some(self.docker)
    }
}

/// What a project's env example says it talks to, by the one thing that
/// names an engine unambiguously: the scheme of a URL, or the port a bare
/// number defaults to.
///
/// Not the key's prefix. `DATABASE_URL` is `DATABASE` for Postgres, MySQL
/// and MongoDB alike in [`images::IMAGES`] — good enough to *match* a key
/// to a compose service whose image already named the engine, and no use
/// at all for working out which engine a project wants when nothing else
/// says. `postgres://` says it; `5432` says it.
const SERVICE_ADDRESSES: [(&str, &[&str], u16); 4] = [
    ("postgres", &["postgres", "postgresql", "pgsql"], 5432),
    ("mariadb", &["mysql", "mariadb"], 3306),
    ("redis", &["redis", "rediss", "valkey"], 6379),
    ("mongodb", &["mongodb", "mongodb+srv"], 27017),
];

/// The recipe an env value names, by its URL scheme or its port.
pub(super) fn recipe_for_address(value: &str) -> Option<&'static str> {
    let value = value.trim();
    if let Some(scheme) = value.split("://").next().filter(|s| *s != value) {
        let scheme = scheme.to_ascii_lowercase();
        if let Some((recipe, _, _)) = SERVICE_ADDRESSES
            .iter()
            .find(|(_, schemes, _)| schemes.contains(&scheme.as_str()))
        {
            return Some(recipe);
        }
    }
    // A bare number, or the port at the end of a URL whose scheme said
    // nothing. The default port is weaker evidence than a scheme and is
    // only ever reached when the scheme was silent.
    let port = port_of(value)?;
    SERVICE_ADDRESSES
        .iter()
        .find(|(_, _, default)| *default == port)
        .map(|(recipe, _, _)| *recipe)
}

/// The host key that makes a `<STEM>_PORT` one half of a split address,
/// as `(key, value)`: a `<STEM>_HOST`, `_HOSTNAME` or `_SERVER` with a
/// value, in [`HOST_SUFFIXES`] order.
fn paired_host<'a>(env: &'a [(String, String)], port_key: &str) -> Option<&'a (String, String)> {
    let stem = port_key.strip_suffix("_PORT")?;
    HOST_SUFFIXES.iter().find_map(|suffix| {
        let host = format!("{stem}{suffix}");
        env.iter()
            .find(|(key, value)| *key == host && !value.trim().is_empty())
    })
}

/// The recipe a split address names by its stem, where the stem's first
/// word is an engine's own URL scheme: `POSTGRES_PORT`, `REDIS_QUEUE_PORT`.
/// The stem is to the pair what the scheme is to a URL. A family word
/// such as `DATABASE` or `DB` names no engine, as it does not in a URL's
/// key either.
fn recipe_for_stem(port_key: &str) -> Option<&'static str> {
    let word = port_key.split('_').next()?.to_ascii_lowercase();
    SERVICE_ADDRESSES
        .iter()
        .find(|(_, schemes, _)| schemes.contains(&word.as_str()))
        .map(|(recipe, _, _)| *recipe)
}

fn port_of(value: &str) -> Option<u16> {
    let value = value.trim();
    if let Ok(port) = value.parse::<u16>() {
        return Some(port);
    }
    let rest = value.split("://").nth(1)?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    hostport.rsplit(':').next()?.parse().ok()
}

/// Every service this project appears to need that no file in it explains.
///
/// One candidate per recipe, not per key: two keys naming the same engine
/// are one server. Sorted by the recipe name so two runs agree.
///
/// An address split over two keys, `REDIS_QUEUE_HOST=localhost` and
/// `REDIS_QUEUE_PORT=6379`, is the same evidence a URL is: the port key
/// is the candidate's key, as a bare `_PORT` is, and the stem names the
/// engine where the port is not its default, as a scheme would.
pub(super) fn native_candidates(signals: &Signals, evidence: &MachineEvidence) -> Vec<Candidate> {
    let mut by_recipe: BTreeMap<&str, (String, String)> = BTreeMap::new();
    let env = &signals.env_example;
    for (key, value) in env {
        if !ADDRESS_SUFFIXES.iter().any(|s| key.ends_with(s)) {
            continue;
        }
        let host = paired_host(env, key);
        let Some(recipe) =
            recipe_for_address(value).or_else(|| host.and_then(|_| recipe_for_stem(key)))
        else {
            continue;
        };
        let said = match host {
            Some((host_key, host)) => format!("{host_key}={host} and {key}={value}"),
            None => format!("{key}={value}"),
        };
        // The first key wins, which is file order: a project that names
        // a database twice means one database.
        by_recipe
            .entry(recipe)
            .or_insert_with(|| (key.clone(), said));
    }
    by_recipe
        .into_iter()
        .map(|(recipe, (key, said))| {
            let runnable = evidence.can_run(recipe);
            // The file it came from by its path: in a folder of
            // repositories, `backend/.env.example`.
            let file = signals.env_example_file(&key);
            let why = match runnable {
                Some(true) => format!("{file} {said}; {recipe} is on this machine"),
                Some(false) => format!(
                    "{file} {said}; {recipe} is not installed here, so starting it will say \
                     what to install"
                ),
                None => format!("{file} {said}"),
            };
            Candidate {
                value: recipe.to_string(),
                why,
                // Ticked when this machine can actually run it: an engine
                // that is not installed is still offered, because the
                // project plainly wants one, but it is not taken silently.
                preselected: runnable != Some(false),
                service: Some(ServiceHint {
                    source: ServiceSource::Native {
                        recipe: recipe.to_string(),
                    },
                    env_key: Some(key),
                }),
                ..Candidate::default()
            }
        })
        .collect()
}

/// The engines the env example in `dir` addresses, by recipe name, read
/// as [`native_candidates`] reads them: for `doctor`, which asks whether
/// an app talks to Redis at all, of the root and of an app's own
/// directory alike.
pub fn addressed_engines(dir: &Path) -> Vec<&'static str> {
    let signals = Signals {
        env_example: super::signals::env_example(dir),
        ..Signals::default()
    };
    native_candidates(&signals, &MachineEvidence::unknown())
        .iter()
        .filter_map(|candidate| {
            SERVICE_ADDRESSES
                .iter()
                .map(|(recipe, _, _)| *recipe)
                .find(|recipe| *recipe == candidate.value)
        })
        .collect()
}

/// Which mechanism a project's private services would use, and the
/// evidence that decided it.
///
/// Three inputs, in this order: what the project itself declares, what
/// this machine can actually run, and what the developer said they
/// prefer. Never a bare verdict — `evidence` is what `doctor` prints
/// under the answer, because "pando will use native" with no reason
/// behind it is a thing to argue with rather than a thing to act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceChoice {
    /// `compose`, `native`, or `None` when the project declares nothing
    /// to isolate.
    pub mechanism: Option<&'static str>,
    /// Whether the project's own compose file offers anything worth a
    /// private copy.
    pub compose_declared: bool,
    /// Whether the env example names a dependency a recipe could run.
    pub native_declared: bool,
    /// One line per fact that went into the answer, in the order they
    /// were weighed.
    pub evidence: Vec<String>,
}

/// [`service_choice`] for a project, reading the two halves the way
/// `propose` reads them.
///
/// The one entry point a caller outside detection uses: `doctor` reports
/// the answer, the proposal machinery acts on it, and neither of them
/// gets its own copy of how the decision is made.
pub fn service_choice_for(
    root: &Path,
    signals: &Signals,
    evidence: &MachineEvidence,
    config: &Config,
) -> ServiceChoice {
    let compose = compose_services_proposal(root, signals, None);
    let native = native_candidates(signals, evidence);
    let file = compose
        .as_ref()
        .and_then(|p| p.service_file())
        .map(str::to_string)
        .or_else(|| compose_file(root, signals));
    let choice = service_choice(
        file.as_deref(),
        compose.as_ref().map(|p| p.candidates.len()).unwrap_or(0),
        &native,
        evidence,
        config.isolation.preferred(),
    );
    noted_below_root(choice, file.as_deref(), signals)
}

/// The compose file services are proposed from, relative to `root`, the
/// form config holds: the one at the root of a repository, and in a
/// folder of repositories the first of those at a repository's own root,
/// `infra/docker-compose.yml`, which is where a set has it too.
pub fn compose_file(root: &Path, signals: &Signals) -> Option<String> {
    match signals.repos.is_empty() {
        true => crate::compose::find(root),
        false => signals.compose_files.first().cloned(),
    }
}

/// A choice that found no compose file at the root, with the ones below
/// it named: a developer told "no compose file" while `docker/compose.yml`
/// sits in the repository would rightly stop trusting the rest. In a
/// folder of repositories, the ones in another repository than the file
/// proposed from are named instead, since only one file is.
fn noted_below_root(
    mut choice: ServiceChoice,
    compose_file: Option<&str>,
    signals: &Signals,
) -> ServiceChoice {
    if !signals.repos.is_empty() {
        let repo_of = |file: &str| file.split('/').next().unwrap_or_default().to_string();
        match compose_file {
            None => {
                if let Some(first) = choice.evidence.first_mut() {
                    *first =
                        "no repository of this folder has a compose file at its root".to_string();
                }
            }
            Some(file) => {
                let others: Vec<&str> = signals
                    .compose_files
                    .iter()
                    .filter(|other| repo_of(other) != repo_of(file))
                    .map(String::as_str)
                    .collect();
                if !others.is_empty() {
                    let at = choice.evidence.len().min(1);
                    choice.evidence.insert(
                        at,
                        format!(
                            "services are proposed from {file}, not from {} — add a \
                             `[[services]]` entry for another by hand",
                            listed(&others)
                        ),
                    );
                }
            }
        }
        return choice;
    }
    if compose_file.is_none() && !signals.compose_files.is_empty() {
        let at = choice.evidence.len().min(1);
        choice.evidence.insert(
            at,
            format!(
                "services are proposed from a compose file at the root, not from {} below it",
                listed(&signals.compose_files)
            ),
        );
    }
    choice
}

/// What pando says when both mechanisms are real and nobody has chosen.
///
/// Named, because `agent/brief.md` quotes it twice — it is the line an
/// agent is told to show a developer instead of answering a machine-wide
/// preference on their behalf. A reword here with the brief left alone
/// would leave the brief quoting a sentence pando no longer says, and a
/// fenced block in a document is not something the compiler can see.
/// [`the_brief_quotes_the_preference_line_pando_actually_says`] is what
/// makes the two fail together.
pub const NO_PREFERENCE_EVIDENCE: &str = "nobody has said which to prefer, so the project's own compose file wins — set \
     `[isolation] prefer = \"native\"` in ~/.pando/config.toml to run the recipes instead";

/// Decides between the two mechanisms from evidence.
///
/// `compose_candidates` is what the compose file offers after the
/// build-from-this-repository filter; `native` is what the env example
/// names. The preference only ever breaks a tie — it cannot conjure a
/// mechanism the project does not declare, and it cannot pick one this
/// machine has been shown not to have.
pub fn service_choice(
    compose_file: Option<&str>,
    compose_candidates: usize,
    native: &[Candidate],
    evidence: &MachineEvidence,
    prefer: Option<&str>,
) -> ServiceChoice {
    let compose_declared = compose_candidates > 0;
    let native_declared = !native.is_empty();
    let mut why: Vec<String> = Vec::new();
    match compose_file {
        Some(file) if compose_declared => why.push(format!(
            "{file} declares {compose_candidates} service{} this project depends on",
            if compose_candidates == 1 { "" } else { "s" }
        )),
        Some(file) => why.push(format!("{file} declares nothing this project depends on")),
        None => why.push("this repository has no compose file at its root".to_string()),
    }
    match native_declared {
        true => why.push(format!(
            "its env example addresses {}",
            listed(&native.iter().map(|c| c.value.as_str()).collect::<Vec<_>>())
        )),
        false => why
            .push("nothing in its env example names an engine pando has a recipe for".to_string()),
    }

    let mechanism = match (compose_declared, native_declared) {
        (false, false) => None,
        // One option is not a choice, and saying so is shorter than
        // explaining a preference that could not have applied.
        (true, false) => Some("compose"),
        (false, true) => {
            if let Some(unrunnable) = native
                .iter()
                .find(|c| evidence.can_run(&c.value) == Some(false))
            {
                why.push(format!(
                    "{} is not installed here, and there is no compose file to fall back to — \
                     starting it will say what to install",
                    unrunnable.value
                ));
            }
            Some("native")
        }
        (true, true) => {
            let docker = evidence.has_docker();
            let every_engine = native
                .iter()
                .all(|c| evidence.can_run(&c.value) != Some(false));
            if let Some(prefer) = prefer {
                why.push(format!(
                    "this machine prefers {prefer} (~/.pando/config.toml)"
                ));
                match prefer {
                    "native" if every_engine => Some("native"),
                    // A preference this machine cannot honour is not
                    // overruled quietly: the other mechanism is used and
                    // the evidence says which engine was missing.
                    "native" => {
                        let missing: Vec<&str> = native
                            .iter()
                            .filter(|c| evidence.can_run(&c.value) == Some(false))
                            .map(|c| c.value.as_str())
                            .collect();
                        why.push(format!(
                            "but {} is not installed here, so compose it is",
                            listed(&missing)
                        ));
                        Some("compose")
                    }
                    // Only when the recipes could actually run. A
                    // machine with neither docker nor the engines gets
                    // the compose it asked for and the refusal that
                    // comes with it, rather than an evidence line
                    // promising recipes that are not there either.
                    "compose" if docker == Some(false) && every_engine => {
                        why.push(
                            "but docker is not on this machine, so the recipes it is".to_string(),
                        );
                        Some("native")
                    }
                    _ => Some("compose"),
                }
            } else if docker == Some(false) && every_engine {
                // Not a preference and not a guess: the mechanism the
                // project declares is one this machine has been shown
                // not to have.
                why.push("docker is not on this machine".to_string());
                Some("native")
            } else {
                // The project's own compose file is the project's own
                // statement about how to run its services, and nobody
                // has said otherwise. The line below is how a developer
                // learns there was a choice at all.
                why.push(NO_PREFERENCE_EVIDENCE.to_string());
                Some("compose")
            }
        }
    };
    ServiceChoice {
        mechanism,
        compose_declared,
        native_declared,
        evidence: why,
    }
}

/// Every service the project's compose file declares, with the env key
/// that names it where a rule found one.
///
/// Decided — no question at all — only when every service is resolved:
/// either an env key points at it, or its image is not one an application
/// talks to. One redis with nothing pointing at it is enough to ask,
/// because pando cannot tell whether the project wants a private copy.
pub(super) fn compose_services_proposal(
    root: &Path,
    signals: &Signals,
    resolve: Option<ComposeResolver<'_>>,
) -> Option<Proposal> {
    let file = compose_file(root, signals)?;
    let path = root.join(&file);
    let mut parsed = crate::compose::read(&path).ok()?;
    // Half a file read is not a proposal. Compose resolves `extends:`, a
    // top-level `include:` and YAML aliases; ask it when it is available.
    if parsed.unresolved.any()
        && let Some(resolve) = resolve
        && let Some(resolved) = resolve(&path)
    {
        parsed = resolved;
    }
    if parsed.services.is_empty() {
        return None;
    }
    let mut candidates = Vec::new();
    let mut resolved = true;
    // Which service owns which key so far, in file order. A key belongs to
    // the first service a rule gave it to; the second one is a question.
    let mut claimed: BTreeMap<String, String> = BTreeMap::new();
    // The services that are this project rather than something it depends
    // on, kept so the refusal can name them.
    let mut built_here: Vec<String> = Vec::new();
    for (name, service) in &parsed.services {
        // A service compose builds out of this repository is the
        // application. Running a private copy of it per worktree is not
        // isolation, it is a second copy of the thing being developed, and
        // it is the one answer that is certainly wrong — so it is not on
        // offer at all.
        if let Some(context) = &service.build
            && built_from_project(root, &file, context)
        {
            built_here.push(name.clone());
            continue;
        }
        let image = service.image.as_deref();
        let found = env_key_for(name, image, &signals.env_example, &claimed);
        let app_service = is_app_service(image);
        if !matches!(found, EnvKey::Found(_)) && app_service {
            resolved = false;
        }
        let of_the_image = match image {
            Some(image) => format!("{file}, {image}"),
            None => file.clone(),
        };
        let why = match &found {
            EnvKey::Found(key) => format!("{of_the_image} → {key}"),
            EnvKey::TakenBy { key, service } => {
                format!("{of_the_image}; {key} already points at {service}")
            }
            EnvKey::Nothing => format!("{of_the_image}; nothing in the env example names it"),
        };
        let env_key = match found {
            EnvKey::Found(key) => {
                claimed.insert(key.clone(), name.clone());
                Some(key)
            }
            _ => None,
        };
        candidates.push(Candidate {
            value: name.clone(),
            why,
            preselected: env_key.is_some(),
            service: Some(ServiceHint {
                source: ServiceSource::Compose { file: file.clone() },
                env_key,
            }),
            ..Candidate::default()
        });
    }
    // A file that could not be read whole — no Docker to ask, or compose
    // refused it — is never decided: the ports and images here may not be
    // the ones compose uses, and an `include:` brings in services this list
    // does not even have. It still proposes, because a project with a
    // compose file silently looking like a project with none is worse than
    // a question.
    if let Some(keys) = parsed.unresolved.describe() {
        resolved = false;
        for candidate in &mut candidates {
            if parsed.unresolved.include || parsed.unresolved.extends.contains(&candidate.value) {
                candidate.preselected = false;
            }
            candidate.why = format!(
                "{}; pando does not follow {keys} in this file, so check this one",
                candidate.why
            );
        }
    }
    // Nothing to offer, for one of two reasons: every service in the file
    // is this project's own, or nothing in the env example addresses any
    // of them and none of their images is one an app talks to. Either way
    // there is nothing to ask — a question whose only answer is wrong is
    // worse than silence — and either way the *answer* still has to be
    // written down, or the next start works it out again and a developer
    // reading the file never learns that pando looked. It is the same
    // empty answer a developer gives by declining the question, recorded
    // the same way: an entry naming the file with an empty `include`. One
    // shape for "none", not two.
    //
    // Unless pando could not read the file whole. "Half a file read is not
    // a proposal" applies hardest here: a top-level `include:` brings in
    // services this list does not have, and recording "none of them" about
    // a file pando has not finished reading would answer the slot for good
    // — silencing, permanently, whatever the next run with Docker present
    // would have found. Nothing is written, and it is asked again.
    // `resolved` is already false when a key went unfollowed, so the
    // empty-candidates case is spelled out rather than folded in: a file
    // whose every service is built here *and* carries an `extends:` must
    // still reach the refusal below, not fall through to a proposal with
    // nothing in it.
    if candidates.is_empty() || (candidates.iter().all(|c| !c.preselected) && resolved) {
        if parsed.unresolved.any() {
            return None;
        }
        let built = listed(&built_here.iter().map(String::as_str).collect::<Vec<_>>());
        let unaddressed = listed(
            &candidates
                .iter()
                .map(|c| c.value.as_str())
                .collect::<Vec<_>>(),
        );
        let mut proposal = Proposal::of(Slot::Services, Vec::new(), true);
        proposal.none_because = Some(match (candidates.is_empty(), built_here.is_empty()) {
            (true, _) => format!("{file} declares only {built}, built from this repository"),
            (false, true) => format!("nothing in the env example addresses {unaddressed}"),
            (false, false) => format!(
                "nothing in the env example addresses {unaddressed}, and {built} is built \
                 from this repository"
            ),
        });
        proposal.file = Some(file);
        return Some(proposal);
    }
    Some(Proposal::of(Slot::Services, candidates, resolved))
}

/// Which services this project runs private copies of, and by which
/// mechanism.
///
/// The compose half and the native half are worked out independently —
/// each is a fact about the repository — and [`service_choice`] decides
/// between them from those facts, this machine, and the developer's
/// preference. Only the chosen half becomes a question.
pub(super) fn services_proposal(
    root: &Path,
    signals: &Signals,
    resolve: Option<ComposeResolver<'_>>,
    evidence: &MachineEvidence,
    prefer: Option<&str>,
) -> Option<Proposal> {
    let compose = compose_services_proposal(root, signals, resolve);
    let native = native_candidates(signals, evidence);
    let compose_file = compose
        .as_ref()
        .and_then(|p| p.service_file())
        .map(str::to_string)
        .or_else(|| compose_file(root, signals));
    let compose_candidates = compose.as_ref().map(|p| p.candidates.len()).unwrap_or(0);
    let choice = service_choice(
        compose_file.as_deref(),
        compose_candidates,
        &native,
        evidence,
        prefer,
    );
    let choice = noted_below_root(choice, compose_file.as_deref(), signals);
    match choice.mechanism {
        Some("native") => {
            // Decided when this machine can run every one of them.
            // An engine that is not installed is still offered — the
            // project plainly wants one — but it is not taken silently.
            let decided = native.iter().all(|c| c.preselected);
            let mut proposal = Proposal::of(Slot::Services, native, decided);
            proposal.mechanism = Some("native");
            proposal.evidence = choice.evidence;
            Some(proposal)
        }
        _ => {
            let mut proposal = compose?;
            proposal.mechanism = choice.mechanism;
            proposal.evidence = choice.evidence;
            Some(proposal)
        }
    }
}

/// Whether a compose service's build context points inside the project.
///
/// Resolved the way compose resolves it: relative to the directory the
/// compose file is in, which is where `build: ./api` looks. A context
/// `docker compose config` already made absolute is used as it stands.
///
/// [`crate::paths::resolve_for_compare`] rather than `canonicalize`: a
/// context directory that does not exist still says where compose *would*
/// look, and on macOS the repository's own `/var/...` and the canonical
/// `/private/var/...` are the same directory and have to compare equal.
pub(super) fn built_from_project(root: &Path, file: &str, context: &str) -> bool {
    let compose_file = root.join(file);
    let dir = compose_file.parent().unwrap_or(root);
    let context = Path::new(context);
    let resolved = if context.is_absolute() {
        context.to_path_buf()
    } else {
        dir.join(context)
    };
    crate::paths::resolve_for_compare(&resolved)
        .starts_with(crate::paths::resolve_for_compare(root))
}

/// The command that brings a fresh database up to the current schema.
///
/// Each rule carries the globs whose change means it has to run again,
/// because a hook without them runs on every start and one with the wrong
/// ones never runs at all.
pub(super) fn schema_hook_proposal(root: &Path, signals: &Signals) -> Option<Proposal> {
    let candidates = schema_candidates(root, signals);
    if candidates.is_empty() {
        return None;
    }
    // Always a question, even with one candidate: it is the one question
    // that touches data, and a developer says yes or no to it once.
    // `--yes` still takes the first option, and what it writes runs on
    // isolated starts only — against this worktree's private services.
    Some(Proposal::of(Slot::SchemaHook, candidates, false))
}

/// The name every proposed schema hook gets. One name, so a second run
/// recognises the hook it wrote rather than appending another one.
pub const SCHEMA_HOOK: &str = "migrate";

fn schema_candidates(root: &Path, signals: &Signals) -> Vec<Candidate> {
    let mut out = Vec::new();
    let mut push = |cmd: String, globs: &[&str], why: &str| {
        out.push(Candidate {
            value: cmd.clone(),
            why: why.to_string(),
            hook: Some(crate::config::HookConfig {
                name: SCHEMA_HOOK.to_string(),
                after: crate::config::HookPoint::Services,
                fingerprint: globs.iter().map(|g| (*g).to_string()).collect(),
                cmd,
                cwd: None,
                fallback: None,
                // Explicit rather than defaulted, so the entry shows how
                // to change it.
                on: Some(crate::config::HookScope::Isolated),
            }),
            ..Candidate::default()
        })
    };
    let exec = package_runner(signals);
    if root.join("prisma/schema.prisma").is_file() {
        push(
            format!("{exec} prisma migrate deploy"),
            &["prisma/migrations/**"],
            "prisma/schema.prisma",
        );
    }
    if root.join("drizzle.config.ts").is_file() || root.join("drizzle.config.js").is_file() {
        push(
            format!("{exec} drizzle-kit migrate"),
            &["drizzle/**"],
            "drizzle.config",
        );
    }
    if root.join("manage.py").is_file() {
        // `python_runner` carries its own trailing space, or is empty for
        // a project whose interpreter is simply on PATH. Every app's
        // migrations, at any depth: `core/` beside `apps/billing/`, or a
        // project's apps all one level down.
        push(
            format!("{}python manage.py migrate", python_runner(signals)),
            &["**/migrations/*.py"],
            "manage.py",
        );
    }
    if root.join("alembic.ini").is_file() {
        push(
            format!("{}alembic upgrade head", python_runner(signals)),
            &["**/versions/*.py"],
            "alembic.ini",
        );
    }
    if root.join("config/database.yml").is_file() {
        push(
            "bin/rails db:prepare".to_string(),
            &["db/migrate/*.rb"],
            "config/database.yml",
        );
    }
    // Last, after every tool pando knows by its files: the project's own
    // name for the step. A migration runner nobody has a rule for — knex,
    // a hand-written script over a directory of SQL — is still spelled as
    // a package script, and that spelling is the project saying what it
    // runs against a fresh database.
    let watched: Vec<String> = MIGRATION_DIRS
        .iter()
        .filter(|dir| root.join(dir).is_dir())
        .map(|dir| format!("{dir}/**"))
        .collect();
    let watched: Vec<&str> = if watched.is_empty() {
        vec!["package.json"]
    } else {
        watched.iter().map(String::as_str).collect()
    };
    for name in SCHEMA_SCRIPTS {
        let Some(body) = signals.scripts.get(name) else {
            continue;
        };
        if generates_files(body) {
            continue;
        }
        push(
            format!("{}{name}", script_runner(signals)),
            &watched,
            &format!("package.json scripts.{name}"),
        );
    }
    out
}

/// Package scripts that bring a database to its schema, in the order a
/// project most likely means them. Seeding and resetting are not on it:
/// neither is the schema, and a reset on every isolated start would be a
/// hook that destroys what the last one made.
const SCHEMA_SCRIPTS: [&str; 7] = [
    "db:migrate",
    "migrate",
    "db:deploy",
    "migrate:deploy",
    "db:setup",
    "db:init",
    "db:push",
];

/// Where a project keeps its migrations, when it is not an ORM pando has a
/// rule for. What a script-named schema hook is keyed on, so it runs again
/// when a migration is added rather than on every start.
const MIGRATION_DIRS: [&str; 5] = [
    "migrations",
    "db/migrations",
    "database/migrations",
    "src/migrations",
    "db/migrate",
];

/// A script that writes migration files rather than applying them — the
/// generating variants principles forbid in a hook.
fn generates_files(body: &str) -> bool {
    [
        "migrate dev",
        "generate",
        "makemigrations",
        "migrate:make",
        "--create-only",
    ]
    .iter()
    .any(|marker| body.contains(marker))
}

/// How this project runs a binary from its dependencies: the first
/// lockfile's manager decides, and `npx` is what is left.
fn package_runner(signals: &Signals) -> &'static str {
    lockfiles(signals)
        .next()
        .and_then(package_managers::for_lockfile)
        .and_then(|manager| manager.exec)
        .unwrap_or("npx")
}
