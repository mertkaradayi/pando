//! The shape of `pando.toml`: every section and what it holds.

use crate::paths::PandoPaths;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default, skip_serializing_if = "ProjectSection::is_empty")]
    pub project: ProjectSection,
    #[serde(default, skip_serializing_if = "RuntimeSection::is_empty")]
    pub runtime: RuntimeSection,
    #[serde(default, skip_serializing_if = "IsolationSection::is_empty")]
    pub isolation: IsolationSection,
    /// Shorthand for a single process named `dev`. Normalised into
    /// `processes` at load time; never both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dev: Option<ProcessConfig>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub processes: BTreeMap<String, ProcessConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<ServiceConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub probes: Vec<ProbeConfig>,
    #[serde(default, skip_serializing_if = "BranchesSection::is_empty")]
    pub branches: BranchesSection,
    #[serde(default, skip_serializing_if = "ShareSection::is_empty")]
    pub share: ShareSection,
    #[serde(default, skip_serializing_if = "UiSection::is_empty")]
    pub ui: UiSection,
    /// Who pando connects as to make and drop a worktree's namespaces, by
    /// `[[services]]` name, for a service the main checkout's env files
    /// give no login for. Written when a namespaced start asks, and read
    /// from pando's own project layer only: it is a password.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub namespaced: BTreeMap<String, LoginConfig>,
}

/// One service's login for namespaced starts.
///
/// Its `Debug` never prints the password: a config is printed whole in
/// more than one error path, and a password in a terminal's scrollback is
/// a password somebody else can read.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

impl std::fmt::Debug for LoginConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginConfig")
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "(hidden)"))
            .finish()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees_dir: Option<PathBuf>,
    /// Default base branch for `new`. `None` means "resolve it from the repo".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// Paths linked or copied into each new worktree. Every entry must be
    /// gitignored in the main checkout; `new` refuses otherwise.
    ///
    /// `None` is "nobody has said yet", which detection may answer;
    /// `Some([])` is "no worktree needs a local file of mine", which is an
    /// answer a developer gave and which is never asked about again. The
    /// same distinction `[dev].ports` makes, for the same reason: a
    /// question nothing can record is asked on every `new`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provision: Option<Vec<String>>,
    /// Where a provisioned path comes from when the main checkout has no
    /// file to link: destination to source, both relative to the
    /// repository root.
    ///
    /// A fresh clone has no `.env` — it is gitignored, so it never arrives
    /// — while `.env.example` is right there, tracked. Seeding from it is
    /// an answer to the provision question, never an automatic behaviour,
    /// and the file that lands in the worktree is always a **copy**: a
    /// symlink to the tracked example would make the worktree's own edits
    /// writes into the repository.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub provision_from: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "ProvisionMode::is_default")]
    pub provision_mode: ProvisionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<String>,
    /// Whether `new` fills a worktree with copy-on-write clones of the main
    /// checkout's files, letting git write only the ones that differ.
    /// `None` is on: the result is the same tree git would write, git
    /// checks it, and a filesystem that cannot clone gets git's checkout.
    /// `Some(false)` always checks out with git.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub copy_on_write: Option<bool>,
    /// Ignored paths `new` clones from the main checkout, copy-on-write,
    /// before the install runs, so the install only fixes what differs:
    /// `node_modules`, and a gitignored lockfile with it. Never written as
    /// a full copy: a filesystem that cannot clone leaves them to the
    /// install. Every entry must be gitignored, like `provision`'s.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clone: Vec<String>,
}

impl ProjectSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The paths to provision, with "nobody has said" and "nothing, on
    /// purpose" both reading as the empty list.
    pub fn provision_paths(&self) -> &[String] {
        self.provision.as_deref().unwrap_or_default()
    }

    /// Whether `new` may check out by copy-on-write.
    pub fn copy_on_write(&self) -> bool {
        self.copy_on_write.unwrap_or(true)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionMode {
    #[default]
    Link,
    Copy,
}

impl ProvisionMode {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSection {
    /// Sourced before every command pando runs for this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prelude: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub version_files: Vec<String>,
}

impl RuntimeSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How this worktree's private services are run, and whether it runs any.
///
/// Two keys in two layers, the same split `[runtime]` makes. Which
/// mechanism a developer wants is a property of their laptop — whether
/// they have Docker running all day, whether they already have Postgres
/// installed — so `prefer` is written to the user layer and answered once
/// per machine. Whether this project has private services at all is a
/// property of the repository, so `none` is written to the project layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IsolationSection {
    /// `native` or `compose`, spelled the way `[[services]] kind` is,
    /// because the answer literally selects which kind gets written.
    ///
    /// `None` is "nobody has said", and pando then takes what the project
    /// itself declares — a compose file is the project's own statement
    /// about how to run its services, and preferring it keeps the common
    /// path unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefer: Option<String>,
    /// The recorded form of "this project runs no private services".
    ///
    /// A compose entry can say that with an empty `include`; a native
    /// entry is one service and has nowhere to put it. Without a place to
    /// record the negative the question returns on every isolated start,
    /// with nowhere to answer it but the TOML by hand — the same gap
    /// `ports = []` and `provision = []` each closed for their own slot.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub none: bool,
}

impl IsolationSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Which mechanism this machine asks for, if it asks for one.
    pub fn preferred(&self) -> Option<&str> {
        self.prefer.as_deref()
    }
}

/// The two spellings `[isolation] prefer` takes, which are the two
/// spellings `[[services]] kind` takes.
pub const ISOLATION_KINDS: [&str; 2] = ["compose", "native"];

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessConfig {
    /// The command that starts the process. Defaulted rather than required
    /// so that a `[dev]` table holding only `cwd` or `env` — the shape a
    /// developer writes when they want pando to fill the command in — is a
    /// file every other command can still read. `start` is the one that
    /// refuses, by name.
    #[serde(default)]
    pub cmd: String,
    /// Roles this process owns, when it says. `None` is "nobody has said
    /// yet", which detection may answer; `Some([])` is "this process has no
    /// ports", which it may not. A worker with a port it never binds is
    /// reported as failed for the whole of its healthy life.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<PortsSpec>,
    /// Relative to the worktree. `None` means the worktree root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<ReadySpec>,
    /// Whether its port serves a page a browser opens: `false` for a
    /// bundler a device reads, or an API nobody browses. `None` leaves it
    /// to the catalog: a framework whose app runs on a device serves none,
    /// everything else does. See [`ProcessConfig::serves_page`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<bool>,
}

/// Roles a process owns. The map form `{ ENV = "role" }` is sugar for the
/// list plus an env template, expanded when a process is started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PortsSpec {
    List(Vec<String>),
    Map(BTreeMap<String, String>),
}

impl Default for PortsSpec {
    fn default() -> Self {
        PortsSpec::List(Vec::new())
    }
}

impl ProcessConfig {
    /// The roles this process owns; none when nothing has said.
    pub fn roles(&self) -> Vec<String> {
        self.ports
            .as_ref()
            .map(PortsSpec::roles)
            .unwrap_or_default()
    }

    /// The environment the map form of `ports` is sugar for.
    pub fn port_env(&self) -> BTreeMap<String, String> {
        self.ports
            .as_ref()
            .map(PortsSpec::env_templates)
            .unwrap_or_default()
    }

    /// Whether a browser opens this process's port: what `page` says, and
    /// where it says nothing, whether its framework's app runs in a
    /// browser at all. Expo's Metro serves a bundle to a device, and its
    /// root is no page anybody wants.
    pub fn serves_page(&self) -> bool {
        self.page.unwrap_or_else(|| {
            let vars = self.port_vars();
            let vars: Vec<&str> = vars.keys().map(String::as_str).collect();
            crate::catalog::frameworks::device(&vars, &self.cmd).is_none()
        })
    }

    /// Each variable a port reaches this process through: the map form of
    /// `ports`, or a `{port:<role>}` in `env`, which is how a workspace
    /// app gets one. With the role, when the variable holds that role's
    /// port and nothing else; an address around a port, such as a
    /// backend's URL, has none.
    pub fn port_vars(&self) -> BTreeMap<String, Option<String>> {
        let own_port = |value: &str| {
            value
                .trim()
                .strip_prefix("{port:")
                .and_then(|rest| rest.strip_suffix('}'))
                .filter(|role| !role.is_empty() && !role.contains(['{', '}']))
                .map(str::to_string)
        };
        let templated = self
            .env
            .iter()
            .filter(|(_, value)| value.contains("{port"))
            .map(|(var, value)| (var.clone(), own_port(value)));
        let mapped = self
            .port_env()
            .into_iter()
            .map(|(var, value)| (var, own_port(&value)));
        templated.chain(mapped).collect()
    }
}

impl PortsSpec {
    /// Role names in declaration order, whichever form was written.
    ///
    /// Deduplicated: two environment variables may point at the same role
    /// (`PORT` and `NEXT_PUBLIC_PORT` both meaning `web`), and that is one
    /// port, not two.
    pub fn roles(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let names: Vec<String> = match self {
            PortsSpec::List(v) => v.clone(),
            PortsSpec::Map(m) => m.values().cloned().collect(),
        };
        for name in names {
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }

    /// The environment the map form is sugar for: `ports = { PORT = "web" }`
    /// means `env.PORT = "{port:web}"`.
    ///
    /// Expanded here, once, so the rest of pando only ever sees roles plus
    /// env templates and never has to know which form was written.
    pub fn env_templates(&self) -> BTreeMap<String, String> {
        match self {
            PortsSpec::List(_) => BTreeMap::new(),
            PortsSpec::Map(m) => m
                .iter()
                .map(|(var, role)| (var.clone(), format!("{{port:{role}}}")))
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadySpec {
    /// Role whose port must bind. A process with no ports is ready once alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum ServiceConfig {
    Compose {
        file: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        include: Vec<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
        /// How long each of these services gets to become ready. Sixty
        /// seconds by default; a database that restores a dump on first
        /// boot needs to be able to say so.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ready_timeout_s: Option<u64>,
    },
    Native {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preset: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        port_env: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        init: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cmd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ready: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ready_timeout_s: Option<u64>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        env: BTreeMap<String, String>,
    },
}

impl ServiceConfig {
    /// Whether a start brings anything up for this entry. A native entry
    /// is its one service; a compose entry is the services its `include`
    /// names, and an empty one is the written-down answer "none of them".
    pub fn brings_anything_up(&self) -> bool {
        match self {
            ServiceConfig::Compose { include, .. } => !include.is_empty(),
            ServiceConfig::Native { .. } => true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookConfig {
    pub name: String,
    pub after: HookPoint,
    /// Globs relative to the worktree; the hook runs when their content hash
    /// changes. Empty means "every start".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fingerprint: Vec<String>,
    pub cmd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback: Option<String>,
    /// Which starts run it: `isolated`, `always`, or `never`. Unset means
    /// `isolated` for a hook after `services` in a project with services,
    /// and `always` for the rest — see [`HookConfig::scope`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<HookScope>,
}

/// Which starts a hook runs on.
///
/// A hook after `services` is almost always a migration, and on a start
/// with no data of its own the services it runs after are the developer's
/// shared ones: one branch's migrations applied to the database every
/// other worktree uses. So such a hook runs on isolated starts only unless
/// its entry says `on = "always"` — and a namespaced start is one of
/// those: its database is the worktree's own, in the shared server.
/// `never` is the recorded "no" to the schema question — the command
/// pando found stays visible, switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookScope {
    Isolated,
    Always,
    Never,
}

impl HookScope {
    pub fn as_str(self) -> &'static str {
        match self {
            HookScope::Isolated => "isolated",
            HookScope::Always => "always",
            HookScope::Never => "never",
        }
    }
}

impl HookConfig {
    /// The scope in force: the entry's own `on`, or the default for its
    /// lifecycle point.
    ///
    /// `has_services` is whether the project has any service pando can run
    /// a private copy of. Without one, no start is ever isolated, and the
    /// database a hook after `services` runs against is the only one there
    /// is — an external one the project points at itself. Defaulting that
    /// hook to isolated would mean it never runs at all, so it defaults to
    /// `always`, which is what it did before scopes existed.
    pub fn scope(&self, has_services: bool) -> HookScope {
        self.on.unwrap_or(match self.after {
            HookPoint::Services if has_services => HookScope::Isolated,
            _ => HookScope::Always,
        })
    }

    /// Whether a start that has (or has no) data of its own — isolated or
    /// namespaced — runs this hook, in a project that has (or has no)
    /// services; see [`HookConfig::scope`].
    pub fn runs_on(&self, own_data: bool, has_services: bool) -> bool {
        match self.scope(has_services) {
            HookScope::Always => true,
            HookScope::Isolated => own_data,
            HookScope::Never => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookPoint {
    Create,
    Install,
    Services,
    Dev,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeConfig {
    pub name: String,
    pub cmd: String,
    /// Stderr substring that makes a failure fatal. A non-matching failure is
    /// ignored, so a probe never blocks a project it does not understand.
    #[serde(rename = "match")]
    pub match_: String,
    pub hint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchesSection {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<BranchRule>,
}

impl BranchesSection {
    fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BranchRule {
    #[serde(rename = "match")]
    pub match_: String,
    pub base: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShareSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Prints a header value injected into proxied requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_cmd: Option<String>,
}

impl ShareSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// How the TUI looks. A property of the person rather than the project, so
/// it belongs in the user layer, which is where the theme picker writes it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiSection {
    /// A theme by name: a built-in, or a file in `<pando home>/themes/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// A file whose first line names the theme, followed while the TUI
    /// runs: the state file of a terminal theme switcher, so pando changes
    /// with the terminal. Over `theme` while it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme_from: Option<PathBuf>,
    /// `auto` (the default: the system's), `dark` or `light`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<String>,
    /// How the TUI orders its worktrees, one of [`LIST_SORTS`]: `pr` (the
    /// default) when nothing says. `,` in the TUI cycles it and saves it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<String>,
}

/// The spellings `[ui] appearance` takes.
pub const APPEARANCES: [&str; 3] = ["auto", "dark", "light"];

/// The spellings `[ui] sort` takes, in the order `,` cycles them. The
/// TUI's `ListSort` is held to this list by a test.
pub const LIST_SORTS: [&str; 4] = ["pr", "newest", "run", "name"];

impl UiSection {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What the theme module needs from it, with `~` expanded.
    pub fn theme_settings(&self) -> crate::theme::Settings {
        crate::theme::Settings {
            theme: self.theme.clone(),
            theme_from: self.theme_from.as_deref().map(expand_tilde),
            appearance: self.appearance.clone(),
        }
    }
}

impl Config {
    /// The processes a start can run: those with a command. None means
    /// nothing is set up to run yet — the setup state's "new", and what
    /// the welcome screen calls "not settled yet". Blank counts as none,
    /// as it does for `start`, which refuses a process without one.
    pub fn runnable_processes(&self) -> impl Iterator<Item = (&String, &ProcessConfig)> {
        self.processes
            .iter()
            .filter(|(_, process)| !process.cmd.trim().is_empty())
    }

    /// Where worktrees are created: the configured directory if any, else
    /// pando's own. The single helper every caller uses, so a configured
    /// value is honoured by `actions` and the TUI watcher alike.
    pub fn worktrees_dir(&self, paths: &PandoPaths) -> PathBuf {
        self.configured_worktrees_dir(paths.root())
            .unwrap_or_else(|| paths.worktrees_dir())
    }

    /// Where `pando check` makes its throwaway worktree: beside the real
    /// ones, so it is on the same volume and at the same depth.
    pub fn check_worktree_path(&self, paths: &PandoPaths) -> PathBuf {
        self.worktrees_dir(paths).join(crate::paths::CHECK_WORKTREE)
    }

    /// The configured `worktrees_dir`, with `~` expanded and a relative
    /// value taken from the repository root — where `git -C <root>
    /// worktree add` puts it. Resolved against the directory pando was run
    /// from instead, the checks and the checkout looked at two different
    /// places, and a value that named a directory inside the repository
    /// was let through wherever it did not exist yet.
    pub(super) fn configured_worktrees_dir(&self, root: &Path) -> Option<PathBuf> {
        let dir = expand_tilde(self.project.worktrees_dir.as_deref()?);
        Some(if dir.is_relative() {
            root.join(dir)
        } else {
            dir
        })
    }

    /// The base branch `new` forks from for `branch`, if config decides it:
    /// `[branches].rules` first, then `[project].base`. `None` leaves the
    /// choice to the repository's own default.
    pub fn base_for_branch(&self, branch: &str) -> Option<&str> {
        for rule in &self.branches.rules {
            if glob_match(&rule.match_, branch) {
                return Some(&rule.base);
            }
        }
        self.project.base.as_deref()
    }
}

pub(super) fn expand_tilde(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else {
        return path.to_path_buf();
    };
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"));
    home.join(rest)
}

/// Minimal glob for `[branches].rules`: `*` matches any run of characters,
/// `?` exactly one. Small enough not to be worth a dependency.
pub(super) fn glob_match(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let v: Vec<char> = value.chars().collect();
    let (mut pi, mut vi) = (0usize, 0usize);
    let (mut star, mut star_vi) = (None, 0usize);
    while vi < v.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == v[vi]) {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            star_vi = vi;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            star_vi += 1;
            vi = star_vi;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}
