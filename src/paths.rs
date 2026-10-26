//! Every location pando writes to, as a pure function of the home directory
//! and the project id.
//!
//! Invariant 1 lives here: nothing in this module ever points inside the
//! repository. Tests inject a temp `home`, so no test can touch the real
//! `~/.pando`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::project::ProjectRef;

/// Directory mode for pando's home. Later phases copy env files in, so the
/// tree is locked down from the first commit rather than tightened later.
const HOME_MODE: u32 = 0o700;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PandoPaths {
    pub home: PathBuf,
    pub project: ProjectRef,
}

impl PandoPaths {
    pub fn new(home: impl Into<PathBuf>, project: ProjectRef) -> Self {
        Self {
            home: home.into(),
            project,
        }
    }

    pub fn project_id(&self) -> &str {
        &self.project.id
    }

    pub fn root(&self) -> &Path {
        &self.project.root
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.home.join(crate::project::PROJECTS_DIR)
    }

    pub fn project_dir(&self) -> PathBuf {
        self.projects_dir().join(&self.project.id)
    }

    /// The only path pando ever writes config to. A `pando.toml` committed
    /// inside the repository is read-only, and is not this file.
    pub fn config_file(&self) -> PathBuf {
        self.project_dir().join("pando.toml")
    }

    pub fn state_file(&self) -> PathBuf {
        self.project_dir().join("state.json")
    }

    /// Every question a program answered for this project, one JSON object
    /// per line, appended and never rewritten.
    ///
    /// Beside the config rather than inside the cache directory: a cache is
    /// something pando may throw away and rebuild, and this is the opposite
    /// — a record of decisions nothing else remembers.
    pub fn decisions_file(&self) -> PathBuf {
        self.project_dir().join("decisions.jsonl")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.project_dir().join("state.lock")
    }

    /// The last `pando check`'s result, and while one runs, its progress.
    /// pando's own record, not a published shape: `check --json` is.
    pub fn check_file(&self) -> PathBuf {
        self.project_dir().join("check.json")
    }

    /// Held for the whole of a `pando check`, so two never run at once and
    /// anything that reads the check can tell one is running.
    pub fn check_lock_file(&self) -> PathBuf {
        self.project_dir().join("check.lock")
    }

    /// What the setup screen remembers about this project: a skip, the
    /// time pando tried on its own, the CLI's one-time tip.
    pub fn setup_file(&self) -> PathBuf {
        self.project_dir().join("setup.json")
    }

    /// What an agent keeps about running this project with pando, under
    /// the two names coding agents read: `CLAUDE.md` and `AGENTS.md`.
    /// Claude Code reads a `CLAUDE.md` in every directory above the one it
    /// runs in, and this one is above [`Self::worktrees_dir`].
    pub fn agent_memory_files(&self) -> [PathBuf; 2] {
        [
            self.project_dir().join("CLAUDE.md"),
            self.project_dir().join("AGENTS.md"),
        ]
    }

    /// The lock a namespaced start hands out a slot under, the same for
    /// every project on this machine: a Redis on a port is the machine's,
    /// and each project's own lock covers only its own records.
    pub fn slots_lock_file(&self) -> PathBuf {
        self.home.join("slots.lock")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.project_dir().join("cache")
    }

    pub fn enrich_cache_file(&self) -> PathBuf {
        self.cache_dir().join("enrich.json")
    }

    pub fn pr_cache_file(&self) -> PathBuf {
        self.cache_dir().join("prs.json")
    }

    /// Runtime probes that came back satisfied, so a start costs one extra
    /// spawn when the requirement or the prelude changes and none when
    /// neither has.
    pub fn runtime_cache_file(&self) -> PathBuf {
        self.cache_dir().join("runtime.json")
    }

    /// Where `new` puts worktrees unless config overrides it. Callers must go
    /// through the config-aware helper in `actions` rather than reading this
    /// directly, so a configured `worktrees_dir` is honoured everywhere.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.project_dir().join("worktrees")
    }

    pub fn worktree_path(&self, name: &str) -> PathBuf {
        self.worktrees_dir().join(name)
    }

    pub fn logs_dir(&self, name: &str) -> PathBuf {
        self.project_dir().join("logs").join(name)
    }

    /// `source` names one log of one worktree: `dev`, a process, a hook,
    /// `tunnel`, `proxy`. The TUI builds its log tabs from the files that
    /// exist, so nothing here enumerates the set.
    ///
    /// It is a path component, so every name that reaches this has to have
    /// been through [`validate_log_source`] first — at config load time for
    /// a process or a hook, at the argument for `logs --source`.
    pub fn log_file(&self, name: &str, source: &str) -> PathBuf {
        self.logs_dir(name).join(format!("{source}.log"))
    }

    /// Held for the whole of one share of a worktree. Beside the tunnel's
    /// log, which is what two shares at once would both write, and gone
    /// with it when the worktree is removed.
    pub fn share_lock_file(&self, name: &str) -> PathBuf {
        self.logs_dir(name).join("share.lock")
    }

    pub fn data_dir(&self, name: &str) -> PathBuf {
        self.project_dir().join("data").join(name)
    }

    /// One native service's data directory, beside what compose mode uses
    /// so `rm`'s existing `remove_dir_all(data_dir)` already takes it.
    pub fn service_data_dir(&self, name: &str, service: &str) -> PathBuf {
        self.data_dir(name).join(service)
    }

    /// Where a native service puts its Unix socket, if it insists on one.
    ///
    /// The one thing pando writes outside its own home, and it has no
    /// choice: `sockaddr_un.sun_path` is 104 bytes on macOS, and
    /// `~/.pando/projects/<id>/data/<worktree>/<service>/.s.PGSQL.65432`
    /// is past that before the branch name is interesting. So the path is
    /// a *fixed length* — the temporary directory, then `pando-` and eight
    /// hex characters of a hash over the project, the worktree and the
    /// service — and it stays the same however long those three are.
    /// Clients connect over TCP regardless; this exists because Postgres
    /// and MariaDB create a socket whether anyone asked for one or not.
    ///
    /// Nothing about the repository is written here, so Invariant 1 is
    /// untouched: it is a socket directory in the temporary directory,
    /// removed when the service stops.
    pub fn service_socket_dir(&self, name: &str, service: &str) -> PathBuf {
        let mut context = md5::Context::new();
        for part in [self.project_id(), name, service] {
            context.consume(part.as_bytes());
            context.consume([0]);
        }
        let digest = format!("{:x}", context.finalize());
        socket_base().join(format!("pando-{}", &digest[..8]))
    }

    pub fn compose_dir(&self) -> PathBuf {
        self.project_dir().join("compose")
    }

    pub fn compose_override_file(&self, name: &str) -> PathBuf {
        self.compose_dir().join(format!("{name}.override.yml"))
    }

    /// Shared by every pando-spawned tunnel, and passed with `--config` so a
    /// user's own ingress rules never apply to a quick tunnel.
    pub fn tunnel_config_file(&self) -> PathBuf {
        self.home.join("tunnel-config.yml")
    }

    /// The machine-wide config layer: one file for every project on this
    /// laptop, beneath the project layer and above a committed one.
    ///
    /// It sits directly under the home rather than under a project because
    /// what it holds is a property of the machine — which version manager
    /// this shell needs initialised — and not of any one repository.
    pub fn user_config_file(&self) -> PathBuf {
        self.home.join("config.toml")
    }

    /// The developer's own service and language recipes, merged over the
    /// ones compiled into the binary.
    ///
    /// Beside `config.toml` under the home rather than under a project,
    /// and for the same reason: how this laptop runs Postgres is a
    /// property of the laptop, not of one repository. A file here replaces
    /// the built-in whose name it shares.
    pub fn recipes_dir(&self) -> PathBuf {
        self.home.join("recipes")
    }

    /// The developer's own colour themes, one TOML file each, listed after
    /// the built-ins; a file here replaces the built-in whose name it shares.
    pub fn themes_dir(&self) -> PathBuf {
        self.home.join("themes")
    }

    /// Creates the home directory 0700 and the project directory under it.
    /// Idempotent; called before the first write of any run.
    pub fn ensure_home(&self) -> Result<()> {
        // The last gate: whatever computed this home, nothing is created
        // until it is proven to sit outside the repository.
        ensure_outside_repository("pando home", &self.home, self.root(), &[])?;
        ensure_dir_private(&self.home)?;
        std::fs::create_dir_all(self.project_dir())
            .with_context(|| format!("create {}", self.project_dir().display()))?;
        Ok(())
    }
}

/// The longest name an engine is assumed to give a socket inside its
/// socket directory. Postgres's is `.s.PGSQL.65535.lock`, nineteen
/// characters; MariaDB's compiled-in default is `mysql.sock`, ten. Twenty
/// four leaves room without pretending to know every engine.
pub const SOCKET_NAME_BUDGET: usize = 24;

/// `sockaddr_un.sun_path` on macOS, which is the smaller of the two
/// platforms pando targets. Linux allows 108.
pub const SUN_PATH_MAX: usize = 104;

/// The longest socket path that can be bound, leaving room for the
/// terminating NUL byte the kernel counts.
pub const MAX_SOCKET_PATH: usize = SUN_PATH_MAX - 1;

/// What [`PandoPaths::service_socket_dir`] hangs its fixed-length name
/// off.
///
/// The temporary directory, unless that is itself so long that a socket
/// under it could not be bound — on macOS `$TMPDIR` is a per-user path in
/// `/var/folders`, about fifty characters, and a shell can set it to
/// anything. `/tmp` is the fallback because it is the one directory that
/// is short on every Unix.
fn socket_base() -> PathBuf {
    let base = std::env::temp_dir();
    // `/pando-xxxxxxxx` plus `/` plus the longest socket name.
    let overhead = 1 + 14 + 1 + SOCKET_NAME_BUDGET;
    if base.as_os_str().len() + overhead > MAX_SOCKET_PATH {
        return PathBuf::from("/tmp");
    }
    base
}

/// The directory name of `pando check`'s throwaway worktree, under the
/// worktrees directory, and of the log directory its processes write in.
/// A dot first: git refuses a ref component that starts with one, so no
/// branch's directory can be this, and the TUI's watcher already passes
/// over dot names.
pub const CHECK_WORKTREE: &str = ".pando-check";

/// Where a `pando check --base` probe's logs are kept once it ends, under
/// `logs/`: beside the last check's own, which a probe leaves alone, and
/// replaced by the next probe's.
pub const CHECK_PROBE_LOGS: &str = ".pando-check-probe";

/// Where the last check's logs wait, under `logs/`, while a probe writes
/// in their place.
pub const CHECK_HELD_LOGS: &str = ".pando-check-held";

/// Every log directory a check keeps. None of them is a worktree a
/// command takes by name.
pub const CHECK_LOG_DIRS: [&str; 3] = [CHECK_WORKTREE, CHECK_PROBE_LOGS, CHECK_HELD_LOGS];

/// Log names pando keeps for its own use: the install hook's log, and the
/// `tunnel` and `proxy` logs `share` writes.
///
/// One list, in one place, because a process, a hook and `share` all write
/// into the same `logs/<worktree>/` directory and whichever of them starts
/// last truncates the other's log. Phase 3's `[[hooks]]` and Phase 4's
/// `share` extend this rather than keeping a second list somewhere else.
pub const RESERVED_LOG_SOURCES: [&str; 3] = ["install", "tunnel", "proxy"];

/// Refuses a name that would not be exactly one component of
/// `logs/<worktree>/<name>.log`.
///
/// A TOML key may be any quoted string, so `[processes."../../x"]` parses
/// happily; used as a path component it escapes the log directory, and
/// starting it creates and truncates whatever `.log` file it lands on.
/// Aimed back at the checkout that is Invariant 1 broken — pando writing
/// into the repository — and aimed anywhere else it is still a file pando
/// had no business touching.
///
/// `kind` names what is being checked, so the message reads as a sentence:
/// `process name`, `hook name`, `log source`.
pub fn validate_log_source(kind: &str, name: &str) -> Result<()> {
    const WHERE: &str = "a log lives at logs/<worktree>/<name>.log";
    if name.trim().is_empty() {
        anyhow::bail!("a {kind} must not be empty — {WHERE}");
    }
    if name == "." || name == ".." {
        anyhow::bail!("{kind} {name:?} is a directory, not a name — {WHERE}");
    }
    if name.contains('/') || name.contains('\\') {
        let mut message = format!("{kind} {name:?} must be a single name, not a path — {WHERE}");
        if let Some(suggestion) = suggest_log_source(name) {
            message.push_str(&format!("; try {suggestion:?}"));
        }
        anyhow::bail!("{message}");
    }
    Ok(())
}

/// Whether two log names open one file. The same name in another case
/// does: macOS's filesystem ignores case by default, so a process called
/// `Install` writes `install.log`, and `Web` the log of `web`.
pub fn same_log_source(a: &str, b: &str) -> bool {
    a == b || a.to_lowercase() == b.to_lowercase()
}

fn is_reserved_log_source(name: &str) -> bool {
    RESERVED_LOG_SOURCES
        .iter()
        .any(|reserved| same_log_source(reserved, name))
}

/// The same check plus [`RESERVED_LOG_SOURCES`], in any case.
///
/// Process and hook names go through this one. `logs --source` does not:
/// `--source install` is how the install hook's log is read, and refusing
/// to read a log pando itself wrote would be absurd.
pub fn validate_owned_log_source(kind: &str, name: &str) -> Result<()> {
    validate_log_source(kind, name)?;
    if is_reserved_log_source(name) {
        anyhow::bail!(
            "{kind} {name:?} is reserved for pando's own logs ({}) — pando would truncate that \
             log on every start; pick another name",
            RESERVED_LOG_SOURCES.join(", ")
        );
    }
    Ok(())
}

/// The last component of a path-shaped name, when that is itself usable.
/// `apps/web` is a name a developer plausibly writes, and `web` is what
/// they meant.
fn suggest_log_source(name: &str) -> Option<&str> {
    let last = name
        .rsplit(['/', '\\'])
        .find(|part| !part.trim().is_empty())?;
    (last != "." && last != ".." && !is_reserved_log_source(last)).then_some(last)
}

/// Refuses a location pando would write to that lies inside the repository,
/// or inside any of the worktrees it was given. `label` names what the user
/// set, so the message points at the knob they can turn.
///
/// Invariant 1 has no exceptions for a path the user configured: a home or a
/// `worktrees_dir` in the working tree would fill it with state, caches, and
/// whole worktrees.
pub fn ensure_outside_repository(
    label: &str,
    path: &Path,
    root: &Path,
    worktrees: &[PathBuf],
) -> Result<()> {
    let resolved = resolve_for_compare(path);
    if resolved.starts_with(resolve_for_compare(root)) {
        anyhow::bail!(
            "{label} {} is inside the repository {} — pando never writes into your repository",
            path.display(),
            root.display()
        );
    }
    for worktree in worktrees {
        if resolved.starts_with(resolve_for_compare(worktree)) {
            anyhow::bail!(
                "{label} {} is inside the worktree {} — pando never writes into your repository",
                path.display(),
                worktree.display()
            );
        }
    }
    Ok(())
}

/// Canonicalises the deepest existing ancestor and re-appends the rest, so a
/// not-yet-created path still compares correctly against a canonical root —
/// on macOS `/var/...` and `/private/var/...` are the same directory.
pub fn resolve_for_compare(path: &Path) -> PathBuf {
    let mut suffix = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        if let Ok(canonical) = std::fs::canonicalize(&cursor) {
            let mut out = canonical;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        let Some(name) = cursor.file_name().map(|n| n.to_os_string()) else {
            return path.to_path_buf();
        };
        suffix.push(name);
        if !cursor.pop() {
            return path.to_path_buf();
        }
    }
}

fn ensure_dir_private(dir: &Path) -> Result<()> {
    use crate::platform::files;
    std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    let meta = std::fs::metadata(dir).with_context(|| format!("stat {}", dir.display()))?;
    if files::permission_bits(&meta).is_some_and(|bits| bits != HOME_MODE) {
        files::set_permission_bits(dir, HOME_MODE)
            .with_context(|| format!("chmod 0700 {}", dir.display()))?;
    }
    Ok(())
}

/// `$PANDO_HOME` when set and non-empty, else `~/.pando`. The only place in
/// the crate that reads the environment for a path — tests inject a home
/// instead, because an env var set inside parallel tests is racy.
pub fn default_home() -> PathBuf {
    if let Some(v) = std::env::var_os("PANDO_HOME")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    let home = crate::platform::dirs::home().unwrap_or_else(|| PathBuf::from("/"));
    home.join(".pando")
}

/// Where the shell running a detached process records the status it exits
/// with: beside its log, under the same name.
///
/// Derived from the log rather than stored, so a record written by an
/// earlier pando finds its own sidecar without a migration. Not a `.log`,
/// deliberately: both log viewers build their tab list from the `.log`
/// files in the directory, so this is invisible to them.
pub fn exit_status_file(log_file: &Path) -> PathBuf {
    log_file.with_extension("status")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> PandoPaths {
        PandoPaths::new(
            "/tmp/pando-home",
            ProjectRef {
                id: "acme-shop-3f9a2c1d".into(),
                root: PathBuf::from("/tmp/code/acme-shop"),
                display_name: "acme-shop".into(),
                ..Default::default()
            },
        )
    }

    #[test]
    fn computes_expected_subpaths() {
        let p = paths();
        let base = PathBuf::from("/tmp/pando-home/projects/acme-shop-3f9a2c1d");
        assert_eq!(p.project_dir(), base);
        assert_eq!(p.config_file(), base.join("pando.toml"));
        assert_eq!(p.state_file(), base.join("state.json"));
        assert_eq!(p.lock_file(), base.join("state.lock"));
        assert_eq!(p.check_file(), base.join("check.json"));
        assert_eq!(p.check_lock_file(), base.join("check.lock"));
        assert_eq!(p.setup_file(), base.join("setup.json"));
        assert_eq!(p.cache_dir(), base.join("cache"));
        assert_eq!(p.enrich_cache_file(), base.join("cache/enrich.json"));
        assert_eq!(p.pr_cache_file(), base.join("cache/prs.json"));
        assert_eq!(p.runtime_cache_file(), base.join("cache/runtime.json"));
        assert_eq!(p.worktrees_dir(), base.join("worktrees"));
        assert_eq!(p.worktree_path("feat+x"), base.join("worktrees/feat+x"));
        assert_eq!(p.logs_dir("feat+x"), base.join("logs/feat+x"));
        assert_eq!(
            p.log_file("feat+x", "dev"),
            base.join("logs/feat+x/dev.log")
        );
        assert_eq!(
            p.share_lock_file("feat+x"),
            base.join("logs/feat+x/share.lock")
        );
        assert_eq!(p.data_dir("feat+x"), base.join("data/feat+x"));
        assert_eq!(p.compose_dir(), base.join("compose"));
        assert_eq!(
            p.compose_override_file("feat+x"),
            base.join("compose/feat+x.override.yml")
        );
        assert_eq!(
            p.tunnel_config_file(),
            PathBuf::from("/tmp/pando-home/tunnel-config.yml")
        );
        assert_eq!(
            p.user_config_file(),
            PathBuf::from("/tmp/pando-home/config.toml"),
            "the machine-wide layer is one file for every project"
        );
    }

    #[test]
    fn every_location_is_under_home_and_never_under_the_repository() {
        let p = paths();
        let locations = [
            p.project_dir(),
            p.config_file(),
            p.state_file(),
            p.lock_file(),
            p.check_file(),
            p.check_lock_file(),
            p.setup_file(),
            p.agent_memory_files()[0].clone(),
            p.agent_memory_files()[1].clone(),
            p.cache_dir(),
            p.enrich_cache_file(),
            p.pr_cache_file(),
            p.runtime_cache_file(),
            p.worktrees_dir(),
            p.worktree_path("feat+x"),
            p.logs_dir("feat+x"),
            p.log_file("feat+x", "dev"),
            p.share_lock_file("feat+x"),
            p.data_dir("feat+x"),
            p.compose_dir(),
            p.compose_override_file("feat+x"),
            p.tunnel_config_file(),
            p.user_config_file(),
        ];
        for loc in locations {
            assert!(loc.starts_with(&p.home), "{} escapes home", loc.display());
            assert!(
                !loc.starts_with(p.root()),
                "{} is inside the repository — invariant 1 forbids it",
                loc.display()
            );
        }
    }

    // The memory files are found by walking up from a worktree, so they
    // sit in the directory that holds the worktrees, under the names the
    // agents read.
    #[test]
    fn the_agent_memory_files_sit_above_every_worktree() {
        let p = paths();
        let names: Vec<_> = p
            .agent_memory_files()
            .iter()
            .map(|file| {
                assert_eq!(file.parent(), Some(p.project_dir().as_path()));
                assert!(
                    p.worktree_path("feat+x")
                        .starts_with(file.parent().unwrap())
                );
                file.file_name().unwrap().to_string_lossy().to_string()
            })
            .collect();
        assert_eq!(names, ["CLAUDE.md", "AGENTS.md"]);
    }

    // A process name is a path component of its log file, so anything that
    // is not one component is a write outside the logs directory waiting to
    // happen. Both separators, because a name with a backslash in it is one
    // component here and not on every filesystem pando's logs might be read
    // from.
    #[test]
    fn a_log_source_that_is_not_one_path_component_is_refused() {
        for bad in [
            "../../../../../escaped-log",
            "apps/web",
            "a\\b",
            "/absolute",
            ".",
            "..",
            "",
            "   ",
        ] {
            let err = validate_log_source("process name", bad)
                .unwrap_err()
                .to_string();
            assert!(
                err.contains("logs/<worktree>"),
                "{bad:?} was refused without saying why: {err}"
            );
        }
    }

    #[test]
    fn a_plain_name_is_a_log_source_whatever_alphabet_it_is_in() {
        for good in ["dev", "web", "api", "wörker", "install", ".hidden", "a b"] {
            validate_log_source("process name", good)
                .unwrap_or_else(|e| panic!("{good:?} should be a usable log source: {e:#}"));
        }
    }

    #[test]
    fn the_names_pando_writes_its_own_logs_under_are_refused_to_processes() {
        for reserved in RESERVED_LOG_SOURCES {
            let err = validate_owned_log_source("process name", reserved)
                .unwrap_err()
                .to_string();
            assert!(err.contains("reserved"), "{err}");
            assert!(err.contains(reserved), "{err}");
        }
        // Only the exact names; `installer` is a perfectly good process.
        validate_owned_log_source("process name", "installer").unwrap();
        // And `logs --source install` still reads the hook's own log.
        validate_log_source("log source", "install").unwrap();
    }

    // macOS ignores case by default: a process called `Install` opened
    // install.log and truncated the install step's output on every start.
    #[test]
    fn a_reserved_log_name_is_refused_in_any_case() {
        for name in ["Install", "TUNNEL", "Proxy"] {
            let err = validate_owned_log_source("process name", name)
                .unwrap_err()
                .to_string();
            assert!(err.contains("reserved"), "{err}");
        }
        assert!(same_log_source("Web", "web"));
        assert!(!same_log_source("web", "webs"));
    }

    // `[processes."apps/web"]` is a name a developer plausibly writes, so
    // the refusal says what they meant rather than only that they are wrong.
    #[test]
    fn a_path_shaped_name_suggests_its_last_component() {
        let err = validate_log_source("process name", "apps/web")
            .unwrap_err()
            .to_string();
        assert!(err.contains("try \"web\""), "{err}");
        // Nothing usable to suggest, so nothing is suggested.
        let err = validate_log_source("process name", "../..")
            .unwrap_err()
            .to_string();
        assert!(!err.contains("try "), "{err}");
    }

    #[test]
    fn locations_depend_only_on_home_and_project_id() {
        let a = paths();
        let mut project = a.project.clone();
        project.root = PathBuf::from("/somewhere/else");
        let b = PandoPaths::new(a.home.clone(), project);
        assert_eq!(a.project_dir(), b.project_dir());
        assert_eq!(a.state_file(), b.state_file());
        assert_eq!(a.worktree_path("n"), b.worktree_path("n"));
    }

    #[test]
    fn ensure_home_creates_a_private_home_and_project_dir() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("pando-home");
        let p = PandoPaths::new(&home, paths().project);
        p.ensure_home().unwrap();
        p.ensure_home().unwrap(); // idempotent

        assert!(p.project_dir().is_dir());
        let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, HOME_MODE,
            "pando home must be 0700 — later phases copy env files into it"
        );
    }

    // `ensure_home` is the last gate before pando's home is created, so it
    // refuses a home inside the repository even when nothing checked earlier.
    #[test]
    fn ensure_home_refuses_a_home_inside_the_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("acme-shop");
        std::fs::create_dir_all(&root).unwrap();
        let project = ProjectRef {
            id: "acme-shop-3f9a2c1d".into(),
            root: root.clone(),
            display_name: "acme-shop".into(),
            ..Default::default()
        };
        let home = root.join(".pando");
        let p = PandoPaths::new(&home, project);

        let err = p.ensure_home().unwrap_err();
        assert!(
            format!("{err:#}").contains("inside the repository"),
            "unexpected error: {err:#}"
        );
        assert!(!home.exists(), "a refused home must not be created");
    }

    // The only test that changes the environment mid-run, and on a variable
    // nothing else reads: `default_home` is the one function in the crate
    // that reads PANDO_HOME. (`testutil::no_auto_maintenance` sets git's
    // GIT_CONFIG_* once, before any fixture, and never changes them.)
    #[test]
    fn default_home_honours_pando_home_then_falls_back() {
        let previous = std::env::var_os("PANDO_HOME");
        unsafe { std::env::set_var("PANDO_HOME", "/tmp/injected-pando") };
        assert_eq!(default_home(), PathBuf::from("/tmp/injected-pando"));

        unsafe { std::env::set_var("PANDO_HOME", "") };
        assert!(
            default_home().ends_with(".pando"),
            "an empty PANDO_HOME must fall back to ~/.pando"
        );

        unsafe { std::env::remove_var("PANDO_HOME") };
        assert!(default_home().ends_with(".pando"));

        if let Some(v) = previous {
            unsafe { std::env::set_var("PANDO_HOME", v) };
        }
    }
}
