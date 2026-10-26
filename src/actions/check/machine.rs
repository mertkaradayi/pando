//! Machine first: before a check spawns anything, the servers it will run
//! on have to be up. A shared start uses the developer's own, and one that
//! is not running is the machine's to fix, not the settings'.

use crate::config::Config;
use crate::paths::PandoPaths;
use crate::ports;
use crate::services;

use super::super::services::{env_dirs, shared_service_keys};

/// A shared service the check found not answering, and what starts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Down {
    pub(super) service: String,
    pub(super) reason: String,
}

/// The first of the project's shared services that nothing answers for on
/// this machine, where the main checkout's env files say it is: the
/// root's, then those of the directories the processes run in. Each one
/// is asked once, with the bounded connect readiness uses, over both
/// loopbacks: `localhost` is `[::1]` first for some servers.
///
/// A service whose key has no value there has nothing to ask, and one on
/// another host is not this machine's: each is passed over, the second
/// said through `progress`.
pub(super) fn first_down(
    paths: &PandoPaths,
    config: &Config,
    progress: &dyn Fn(&str),
) -> Option<Down> {
    let dirs = env_dirs(&paths.project, config);
    for shared in shared_service_keys(paths, config) {
        let Some((file, value)) = services::env_value_below(paths.root(), &dirs, &shared.key)
        else {
            continue;
        };
        let Some(port) = services::port_of_value(&value) else {
            continue;
        };
        // A bare port, or a URL whose login holds a reference, is on this
        // machine as far as anything written says.
        let host = services::url_host(&value).unwrap_or_else(|| "localhost".to_string());
        if !ports::is_loopback_host(&host) {
            progress(&format!(
                "{} is on {host}, not this machine — not checked",
                shared.service
            ));
            continue;
        }
        if ports::something_is_listening(port) {
            continue;
        }
        return Some(Down {
            reason: format!(
                "nothing answers on {host}:{port}, where {} in the main checkout's {file} puts \
                 {} — the check runs on your own services, as a shared start does, so start it \
                 first: {}",
                shared.key,
                shared.service,
                starts(paths, &shared.service, shared.compose_file.as_deref())
            ),
            service: shared.service,
        });
    }
    None
}

/// What starts a shared service: the compose command, run in the main
/// checkout, for one of its compose file's services; for a native one,
/// whatever the developer runs it with, which pando cannot know.
fn starts(paths: &PandoPaths, service: &str, compose_file: Option<&str>) -> String {
    match compose_file {
        Some(file) => format!(
            "`docker compose -f {file} up -d {service}` in {}",
            paths.root().display()
        ),
        None => format!("your own {service}, however you run it (`brew services start`, systemd…)"),
    }
}
