//! The run settings' fingerprint: what a passed check vouches for.

use crate::config::{
    Config, HookConfig, ProbeConfig, ProcessConfig, ProvisionMode, RuntimeSection, ServiceConfig,
};
use serde::Serialize;
use std::collections::BTreeMap;

/// Bumped when what the fingerprint covers changes, which makes every
/// recorded check stale on purpose: a test of settings pando no longer
/// reads the same way is not a test of today's.
pub const FINGERPRINT_VERSION: u32 = 1;

/// The fingerprint of what a check ran, from the merged config.
///
/// The rule: a key is in when changing it can change whether a start in
/// the check's mode (shared, the only one this phase checks) comes up.
/// So it covers
///
/// - `[project] install`, `provision` (as the list a start copies, so
///   "nobody said" and "nothing" are one), `provision_from` and
///   `provision_mode`;
/// - `[runtime]`, the prelude and the version files;
/// - the processes (`[dev]` too, though loading folds it into them);
/// - `[[services]]`, `[[hooks]]` and `[[probes]]`;
/// - and [`FINGERPRINT_VERSION`].
///
/// And it leaves out
///
/// - `[ui]`: a theme change must not make every project stale;
/// - `[share]` and `[branches]`: neither is used by a start;
/// - `[namespaced]`: logins are never hashed into a file;
/// - `[isolation]`: it chooses how an isolated start runs services, and
///   the check starts shared;
/// - `root`, `worktrees_dir` and `base`: where things go and which commit
///   is tested, not how the project runs. A new commit on main does not
///   make a check stale either;
/// - `copy_on_write` and `clone`: the first changes how many bytes a
///   checkout takes, never which files it has, and a check never clones,
///   so the second is not part of what it ran;
/// - pando's version: `check.json` records it apart, and an upgrade never
///   invalidates a test.
///
/// The hash is of the covered keys as canonical JSON: maps have sorted
/// keys, lists keep their order (the order of services and hooks is the
/// order they run in), and a key left unset is left out. So the same
/// settings give the same fingerprint in any process and on any day, and
/// a key added to the config later changes no existing fingerprint until
/// somebody sets it.
pub fn fingerprint(config: &Config) -> String {
    format!("{:x}", md5::compute(hashed_text(config)))
}

/// The text the fingerprint is the hash of. Through a `serde_json::Value`,
/// whose objects are sorted maps, so no struct's field order or map's
/// insertion order reaches it.
pub(super) fn hashed_text(config: &Config) -> String {
    let run = RunSettings {
        version: FINGERPRINT_VERSION,
        install: config.project.install.as_deref(),
        provision: config.project.provision_paths(),
        provision_from: &config.project.provision_from,
        provision_mode: config.project.provision_mode,
        runtime: &config.runtime,
        dev: config.dev.as_ref(),
        processes: &config.processes,
        services: &config.services,
        hooks: &config.hooks,
        probes: &config.probes,
    };
    serde_json::to_value(&run)
        .expect("run settings serialise: every key is a string")
        .to_string()
}

#[derive(Serialize)]
struct RunSettings<'a> {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    install: Option<&'a str>,
    #[serde(skip_serializing_if = "<[String]>::is_empty")]
    provision: &'a [String],
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    provision_from: &'a BTreeMap<String, String>,
    provision_mode: ProvisionMode,
    runtime: &'a RuntimeSection,
    #[serde(skip_serializing_if = "Option::is_none")]
    dev: Option<&'a ProcessConfig>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    processes: &'a BTreeMap<String, ProcessConfig>,
    #[serde(skip_serializing_if = "<[ServiceConfig]>::is_empty")]
    services: &'a [ServiceConfig],
    #[serde(skip_serializing_if = "<[HookConfig]>::is_empty")]
    hooks: &'a [HookConfig],
    #[serde(skip_serializing_if = "<[ProbeConfig]>::is_empty")]
    probes: &'a [ProbeConfig],
}
