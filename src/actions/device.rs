//! How the app of a process a phone, a tablet or a simulator runs is
//! opened: its links, from the settings, the worktree's record and the
//! app's own manifests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::catalog::frameworks::{self, AppLinks, AppManifest, Device};
use crate::config::Config;
use crate::state::WorktreeRecord;

use super::installed::InstalledBuild;
use super::services::observed_port_for_role;

/// The address the links name. The iOS simulator shares this machine's;
/// a device's is the developer's network's, which the device note tells
/// them to supply, and pando never guesses.
const HOST: &str = "127.0.0.1";

/// How a caller reads an app's manifests: from disk, or from what it
/// read before.
pub type ReadManifest<'a> = &'a dyn Fn(&Device, &Path) -> AppManifest;

/// The links that open each process's app, by process: every process the
/// settings run whose framework's app runs on a device, once the worktree
/// holds its port, with the app's manifests read from the worktree.
pub fn app_links(config: &Config, record: &WorktreeRecord) -> BTreeMap<String, AppLinks> {
    app_links_with(config, record, &read_manifest)
}

/// [`app_links`], with the manifests read by `read`: the TUI's paint reads
/// no file, and hands in what it read once.
pub fn app_links_with(
    config: &Config,
    record: &WorktreeRecord,
    read: ReadManifest<'_>,
) -> BTreeMap<String, AppLinks> {
    device_processes(config, record)
        .into_iter()
        .map(|app| {
            let links = app
                .device
                .links(HOST, app.port, &read(app.device, &app.dir));
            (app.name.to_string(), links)
        })
        .collect()
}

/// [`app_links`], with the scheme of an app whose config does not say it
/// taken from its development build on a booted simulator, from
/// [`super::installed_builds`]: the scheme that build registers, unless
/// it was made for another SDK and needs replacing first.
pub fn app_links_installed(
    config: &Config,
    record: &WorktreeRecord,
    installed: &BTreeMap<String, InstalledBuild>,
) -> BTreeMap<String, AppLinks> {
    device_processes(config, record)
        .into_iter()
        .map(|app| {
            let mut manifest = read_manifest(app.device, &app.dir);
            if manifest.scheme.is_none()
                && let Some(build) = installed.get(app.name).filter(|build| !build.stale())
            {
                manifest.scheme = build.scheme.clone();
            }
            let links = app.device.links(HOST, app.port, &manifest);
            (app.name.to_string(), links)
        })
        .collect()
}

/// What a branch changes of the native code a development build compiles
/// in, for one process's app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeChanges {
    /// What the branch was measured against.
    pub base: String,
    /// The files, relative to the worktree, changed since the branch left
    /// `base`: committed, uncommitted, or new.
    pub changed: Vec<String>,
    /// The commands, run in the app's directory, that make this worktree
    /// a development build of its own, as `(platform, command)` in the
    /// catalog's order: the iOS simulator's first.
    pub builds: Vec<(&'static str, String)>,
}

/// Each device app's native code the worktree's branch changes against
/// `base`, by process, for the processes whose app has any. A build of
/// another branch lacks what these add, and the bundle this worktree's
/// bundler serves reaches for it.
///
/// Empty when git cannot say: a base the worktree does not have, say.
///
/// `base_of` names the base of each repository of the project, by name,
/// or of the worktree itself, by none: a multi-repo project's set is a
/// checkout per repository, each with its own base and its own git, and
/// an app is measured in the repository its directory is in.
pub fn native_changes(
    config: &Config,
    project: &crate::project::ProjectRef,
    record: &WorktreeRecord,
    base_of: &dyn Fn(Option<&str>) -> Option<String>,
) -> BTreeMap<String, NativeChanges> {
    let apps = device_processes(config, record);
    if apps.is_empty() {
        return BTreeMap::new();
    }
    // What changed in each checkout, under its path in the worktree.
    let mut changes: BTreeMap<Option<&str>, (String, Vec<String>)> = BTreeMap::new();
    for (repo, dir) in project.repo_checkouts_in(&record.path) {
        let Some(base) = base_of(repo) else {
            continue;
        };
        let Some(changed) = changed_since(&dir, &base) else {
            continue;
        };
        let changed = changed
            .into_iter()
            .map(|path| match repo {
                Some(repo) => format!("{repo}/{path}"),
                None => path,
            })
            .collect();
        changes.insert(repo, (base, changed));
    }
    apps.into_iter()
        .filter_map(|app| {
            let repo = match project.is_multi_repo() {
                true => app.cwd.and_then(|cwd| cwd.split('/').next()),
                false => None,
            };
            let (base, changed) = changes.get(&repo)?;
            let native: Vec<String> = changed
                .iter()
                .filter(|path| {
                    let within = match app.cwd {
                        Some(cwd) => path.strip_prefix(cwd).and_then(|p| p.strip_prefix('/')),
                        None => Some(path.as_str()),
                    };
                    within.is_some_and(|rel| app.device.native.is_native(rel))
                })
                .cloned()
                .collect();
            (!native.is_empty()).then(|| {
                let changes = NativeChanges {
                    base: base.clone(),
                    changed: native,
                    builds: app
                        .device
                        .native
                        .builds
                        .iter()
                        .map(|(platform, build)| {
                            (*platform, build.replace("{port}", &app.port.to_string()))
                        })
                        .collect(),
                };
                (app.name.to_string(), changes)
            })
        })
        .collect()
}

/// Every file of the worktree at `dir` that differs from where its branch
/// left `base`: committed or not, and new files git does not ignore.
fn changed_since(dir: &Path, base: &str) -> Option<Vec<String>> {
    let git = |args: &[&str]| -> Option<String> {
        let out = crate::project::git(dir, args).ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    };
    let fork = git(&["merge-base", base, "HEAD"])?;
    let fork = fork.trim();
    let mut changed: Vec<String> = git(&["diff", "--name-only", "-z", fork, "--"])?
        .split('\0')
        .chain(git(&["ls-files", "--others", "--exclude-standard", "-z"])?.split('\0'))
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect();
    changed.sort();
    changed.dedup();
    Some(changed)
}

/// A process whose app a device runs, as the worktree holds it.
pub(super) struct DeviceProcess<'a> {
    pub(super) name: &'a str,
    pub(super) device: &'static Device,
    /// Its bundler's port: the one it listens on, else the one assigned.
    pub(super) port: u16,
    /// Its directory, as config names it relative to the worktree.
    cwd: Option<&'a str>,
    /// And where that is.
    pub(super) dir: PathBuf,
}

/// Every process the settings run whose framework's app runs on a device,
/// once the worktree holds its port.
///
/// Recognised from the settings and the catalog alone, the way
/// `setup::device_note` recognises one: by the framework's port variable
/// or its server in the command.
pub(super) fn device_processes<'a>(
    config: &'a Config,
    record: &WorktreeRecord,
) -> Vec<DeviceProcess<'a>> {
    config
        .runnable_processes()
        .filter_map(|(name, process)| {
            let vars = process.port_vars();
            let names: Vec<&str> = vars.keys().map(String::as_str).collect();
            let (device, var) = frameworks::device(&names, &process.cmd)?;
            // The role whose port the framework's variable holds, else the
            // process's one role: a command that says `--port {port:app}`.
            let role = match vars.get(var) {
                Some(Some(role)) => role.clone(),
                _ => {
                    let owned = record
                        .roles
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| process.roles());
                    let [role] = owned.as_slice() else {
                        return None;
                    };
                    role.clone()
                }
            };
            let assigned = *record.ports.get(&role)?;
            let cwd = process
                .cwd
                .as_deref()
                .map(|cwd| cwd.trim_start_matches("./").trim_end_matches('/'))
                .filter(|cwd| !cwd.is_empty() && *cwd != ".");
            Some(DeviceProcess {
                name,
                device,
                port: observed_port_for_role(record, &role).unwrap_or(assigned),
                dir: cwd.map_or_else(|| record.path.clone(), |cwd| record.path.join(cwd)),
                cwd,
            })
        })
        .collect()
}

/// What the app in `dir` says about opening it: the scheme its
/// development build registers, from the name in its config, and whether
/// it depends on the development client.
///
/// The name is its manifest's, else the one string literal its config
/// written as code gives it: an `app.config.ts` is code, and pando does
/// not run it to read a name. Where neither says, the links keep the
/// scheme's placeholder.
pub fn read_manifest(device: &Device, dir: &Path) -> AppManifest {
    let json = |file: &str| -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(dir.join(file)).ok()?).ok()
    };
    let scheme =
        config_value(device, dir, device.scheme.key).and_then(|name| device.scheme.of(&name));
    let development_client = json("package.json").is_some_and(|package| {
        ["dependencies", "devDependencies"]
            .iter()
            .any(|table| package[table].get(device.development_client).is_some())
    });
    AppManifest {
        scheme,
        development_client,
    }
}

/// The string the app in `dir` gives `key`, a path below its config's
/// root: its manifest's, else the one literal its config written as code
/// gives the key's last name.
pub(super) fn config_value(device: &Device, dir: &Path, key: &[&str]) -> Option<String> {
    let scheme = &device.scheme;
    let manifest: Option<serde_json::Value> = std::fs::read_to_string(dir.join(scheme.manifest))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    manifest
        .as_ref()
        .and_then(|manifest| {
            scheme
                .root
                .iter()
                .chain(key)
                .try_fold(manifest, |value, key| value.get(key))?
                .as_str()
                .map(str::to_string)
        })
        .or_else(|| super::app_config::literal(&config_code(device, dir), key.last()?))
}

/// The source of each config the app in `dir` writes as code.
pub(super) fn config_code(device: &Device, dir: &Path) -> Vec<String> {
    device
        .scheme
        .code
        .iter()
        .filter_map(|file| std::fs::read_to_string(dir.join(file)).ok())
        .collect()
}
