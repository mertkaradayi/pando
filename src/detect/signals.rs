//! Signals: everything tier 1 can see in the main checkout.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::catalog::artifacts;
use crate::catalog::frameworks;
use crate::catalog::package_managers;
use crate::services::{ENV_EXAMPLES, LOCAL_ENV_FILES};

/// Everything tier 1 can see. Serialisable because `pando signals` prints it
/// for an agent to read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signals {
    /// `package.json` scripts: name to body.
    pub scripts: BTreeMap<String, String>,
    /// Makefile or justfile targets: name to the target.
    pub targets: BTreeMap<String, Target>,
    /// Lockfiles present at the root, in the order pando checks them.
    pub lockfiles: Vec<String>,
    pub workspace_markers: Vec<String>,
    /// Which files that pin a runtime exist. What they *say* is
    /// `runtime_requirements`; this stays the list `doctor` shows.
    pub version_files: Vec<String>,
    /// What those files, `.tool-versions`, `mise.toml` (or `.mise.toml`) and `engines` in
    /// `package.json` actually ask for: one entry per (language, spec,
    /// source), a pin before a range.
    ///
    /// Defaulted rather than required, so a dump written by an older pando
    /// still deserialises.
    #[serde(default)]
    pub runtime_requirements: Vec<crate::runtime::Requirement>,
    /// `.env.example` entries, in file order. The values matter as well as
    /// the keys: a value that is a localhost URL is how one app says where
    /// another one listens. Below a root that is not an app, each app
    /// directory's example follows, for keys the ones before it lack.
    pub env_example: Vec<(String, String)>,
    /// Framework marker files that exist.
    pub markers: Vec<String>,
    /// The compose files at the root, or, when it has none, those one
    /// directory below it, under their path: `docker/compose.yml`.
    pub compose_files: Vec<String>,
    /// Root-level files that are gitignored and present — what a new
    /// worktree would be missing — then each app directory's local env
    /// files, under its path: `backend/.env`.
    pub ignored_present: Vec<String>,
    /// Local files the repository does not have but ships an example of:
    /// `(destination, source)`, sorted by destination, the root's first and
    /// then each app directory's.
    ///
    /// A fresh clone is the case. `.env` is gitignored, so it never arrives
    /// with the clone, and there is nothing for a worktree to be given a
    /// copy of — while `.env.example` sits beside it, tracked and unused.
    /// A pair is here only when the destination is gitignored in the main
    /// checkout and really absent, so nothing pando offers from this could
    /// ever show as untracked.
    ///
    /// Defaulted rather than required, so a dump written by an older pando
    /// still deserialises.
    #[serde(default)]
    pub provision_seeds: Vec<(String, String)>,
    /// Workspace apps that would read an env file from their own directory
    /// and have none: `(destination, source)`, the root's local `.env`
    /// given to `apps/<app>/.env`.
    ///
    /// A monorepo commonly keeps one `.env` at the root, and an app that
    /// loads `.env` from its working directory — dotenv's default — finds
    /// nothing when it runs in its own. A pair is here only when the root
    /// file is gitignored and present, the app directory has no env file of
    /// its own at all, and the destination is gitignored in the main
    /// checkout. Defaulted so an older dump still deserialises.
    #[serde(default)]
    pub workspace_env_links: Vec<(String, String)>,
    /// The apps below a root that is not one: sibling directories such as
    /// `backend` and `frontend`, and the conventional `apps/*` and
    /// `packages/*`, each with a manifest or a lockfile of its own.
    ///
    /// Read only when the root has no manifest, no lockfile, no workspace
    /// marker and no framework marker — a polyglot repository whose apps
    /// each keep their own toolchain, where reading the root alone finds
    /// nothing at all. Everywhere else it is empty, and the root is what
    /// pando reads. Defaulted so an older dump still deserialises.
    #[serde(default)]
    pub app_dirs: Vec<AppDir>,
    /// Dependency trees present in the main checkout and gitignored there —
    /// `node_modules`, `apps/api/node_modules` — at the root and up to two
    /// directories below it, outside any hidden directory. What `[project]
    /// clone` can share with a new worktree. Defaulted so an older dump
    /// still deserialises.
    #[serde(default)]
    pub dependency_dirs: Vec<String>,
}

/// One app directory below a root that has no manifest: what detection
/// reads there to propose its install and its process.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppDir {
    /// Relative to the repository root: `backend`, `apps/mobile`.
    pub dir: String,
    /// Its lockfiles, in the order pando checks them.
    #[serde(default)]
    pub lockfiles: Vec<String>,
    /// Its own `package.json` scripts.
    #[serde(default)]
    pub scripts: BTreeMap<String, String>,
    /// Its framework and toolchain markers.
    #[serde(default)]
    pub markers: Vec<String>,
}

const WORKSPACE_MARKERS: [&str; 3] = ["pnpm-workspace.yaml", "turbo.json", "nx.json"];

/// Every file that pins a runtime version. Spelled out rather than derived
/// from [`crate::runtime::LANGUAGES`] because the order is written into
/// config as `runtime.version_files`, and doctor compares that array in
/// order: a reordering would call every config written before it stale. A
/// test holds this list to the table instead.
pub(super) const VERSION_FILES: [&str; 8] = [
    ".nvmrc",
    ".node-version",
    ".tool-versions",
    "mise.toml",
    ".mise.toml",
    ".python-version",
    "rust-toolchain.toml",
    ".ruby-version",
];

/// The compose file names `signals` reports, in the order it reports them.
/// The same names as [`crate::compose::COMPOSE_FILES`], whose order is
/// compose's own precedence instead; `signals --json` publishes this order,
/// so the two stay separate lists and a test holds them to one set.
pub(super) const COMPOSE_FILES: [&str; 4] = [
    "docker-compose.yml",
    "docker-compose.yaml",
    "compose.yml",
    "compose.yaml",
];

/// Reads every tier 1 signal from the main checkout.
pub fn signals(root: &Path) -> Signals {
    let ignored = Ignored::read(root);
    let manifest = std::fs::read_to_string(root.join("package.json")).unwrap_or_default();
    let mut signals = Signals {
        scripts: parse_scripts(&manifest),
        targets: parse_targets(root),
        lockfiles: present(root, &package_managers::lockfiles()),
        workspace_markers: present(root, &WORKSPACE_MARKERS),
        version_files: present(root, &VERSION_FILES),
        runtime_requirements: crate::runtime::requirements(root),
        env_example: env_example(root),
        markers: present(root, &frameworks::marker_files()),
        compose_files: compose_files(root),
        ignored_present: ignored_files(&ignored),
        provision_seeds: provision_seeds(root),
        workspace_env_links: Vec::new(),
        app_dirs: Vec::new(),
        dependency_dirs: dependency_dirs(&ignored),
    };
    // Last, because which directories are apps is itself read from the
    // signals above.
    signals.workspace_env_links = super::workspaces::workspace_env_links(root, &signals);
    if !has_manifest(root, &signals) {
        signals.app_dirs = app_dirs(root);
        read_app_dirs(root, &mut signals);
    }
    signals
}

/// What the app directories add to the root's own signals, each path
/// under its directory: their env examples, their local env files, and
/// the examples of the ones a fresh clone lacks.
///
/// An env example's keys join the root's, the first spelling of a key
/// winning, which is the root's own rule for one file: it is how the
/// services an app talks to are found. Of the gitignored files present in
/// an app directory only the env files are taken — `backend/.env` is the
/// file a worktree is missing, and everything else an app directory
/// ignores is the output of its own tools.
fn read_app_dirs(root: &Path, signals: &mut Signals) {
    let dirs: Vec<String> = signals.app_dirs.iter().map(|app| app.dir.clone()).collect();
    for dir in dirs {
        let path = root.join(&dir);
        let under = |name: &str| format!("{dir}/{name}");
        // After the root's, which keeps the root's own order the one a
        // config written before this compares against.
        signals.version_files.extend(
            present(&path, &VERSION_FILES)
                .iter()
                .map(|name| under(name)),
        );
        signals
            .runtime_requirements
            .extend(crate::runtime::requirements_in(root, &dir));
        for (key, value) in env_example(&path) {
            if !signals.env_example.iter().any(|(held, _)| *held == key) {
                signals.env_example.push((key, value));
            }
        }
        signals.ignored_present.extend(
            ignored_present(&path)
                .iter()
                .filter(|name| is_local_env(name))
                .map(|name| under(name)),
        );
        signals.provision_seeds.extend(
            provision_seeds(&path)
                .iter()
                .map(|(destination, source)| (under(destination), under(source))),
        );
    }
}

/// Whether a file name is an env file an app reads, `.env` or `.env.local`,
/// rather than the example of one.
fn is_local_env(name: &str) -> bool {
    name.starts_with(".env") && !EXAMPLE_SUFFIXES.iter().any(|s| name.ends_with(s))
}

/// Whether the root says anything about how the project is built: a
/// `package.json`, a lockfile, a workspace marker, or a framework or
/// toolchain marker. A root with none of them is not an app, and its apps
/// are in the directories below it.
fn has_manifest(root: &Path, signals: &Signals) -> bool {
    root.join("package.json").is_file()
        || !signals.lockfiles.is_empty()
        || !signals.workspace_markers.is_empty()
        || !signals.markers.is_empty()
}

/// Where the apps of a root with no manifest are looked for: each
/// directory directly below it, and each one below `apps` and `packages`,
/// the two conventional homes. One level, on purpose: a deeper walk finds
/// vendored copies and fixtures as readily as apps.
const APP_PARENTS: [&str; 2] = ["apps", LIBRARY_PARENT];

/// The conventional home of the libraries a project's apps build on: read
/// like the others, and never asked to be started, since nothing runs a
/// library on its own.
pub(super) const LIBRARY_PARENT: &str = "packages";

/// The app directories below `root`, sorted by path.
///
/// A hidden directory, a cache or dependency tree
/// ([`artifacts::is_artifact`]) and anything the project gitignores is
/// skipped, and so is a directory with nothing pando recognises in it:
/// `docker/`, `docs/` and `scripts/` are not apps.
pub(super) fn app_dirs(root: &Path) -> Vec<AppDir> {
    let mut dirs: Vec<String> = subdirs(root, "");
    for parent in APP_PARENTS {
        dirs.extend(subdirs(root, parent));
    }
    dirs.sort();
    let apps: Vec<AppDir> = dirs
        .into_iter()
        .filter_map(|dir| app_dir(root, dir))
        .collect();
    let ignored = gitignored(
        root,
        &apps.iter().map(|a| a.dir.clone()).collect::<Vec<_>>(),
    );
    apps.into_iter()
        .filter(|app| !ignored.contains(&app.dir))
        .collect()
}

/// The directories directly below `root/parent`, as paths relative to
/// `root`, sorted: never a hidden one, nor a cache or dependency tree.
fn subdirs(root: &Path, parent: &str) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(root.join(parent)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| !name.starts_with('.') && !artifacts::is_artifact(name))
        .map(|name| match parent {
            "" => name,
            parent => format!("{parent}/{name}"),
        })
        .collect();
    out.sort();
    out
}

/// The compose files the root has, or, when it has none, the ones a
/// directory directly below it has: `docker/compose.yml`.
///
/// Reported, so an agent and doctor see the file, and nothing more: the
/// services a worktree runs are proposed from a compose file at the root
/// only. One a directory keeps beside the deployment it describes is as
/// often the production stack as the development one, and pando has no
/// way to tell which.
fn compose_files(root: &Path) -> Vec<String> {
    let at_root = present(root, &COMPOSE_FILES);
    if !at_root.is_empty() {
        return at_root;
    }
    let below: Vec<String> = subdirs(root, "")
        .into_iter()
        .flat_map(|dir| {
            present(&root.join(&dir), &COMPOSE_FILES)
                .into_iter()
                .map(move |file| format!("{dir}/{file}"))
        })
        .collect();
    let ignored = gitignored(root, &below);
    below
        .into_iter()
        .filter(|file| !ignored.contains(file))
        .collect()
}

/// What one directory holds, when it is an app: a manifest or a lockfile
/// pando knows.
fn app_dir(root: &Path, dir: String) -> Option<AppDir> {
    let path = root.join(&dir);
    let manifest = std::fs::read_to_string(path.join("package.json")).ok();
    let app = AppDir {
        lockfiles: present(&path, &package_managers::lockfiles()),
        scripts: parse_scripts(manifest.as_deref().unwrap_or_default()),
        markers: present(&path, &frameworks::marker_files()),
        dir,
    };
    (manifest.is_some() || !app.lockfiles.is_empty() || !app.markers.is_empty()).then_some(app)
}

/// Which of `paths` the project gitignores, in one `git check-ignore`
/// rather than one per directory. A git that could not run ignores
/// nothing, which is the answer that reads more rather than less.
fn gitignored(root: &Path, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return Vec::new();
    }
    let args = ["check-ignore", "--"]
        .into_iter()
        .map(str::to_string)
        .chain(paths.iter().cloned());
    let Ok(out) = crate::project::git(root, args) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|line| line.trim_end_matches('/').to_string())
        .collect()
}

pub(super) fn present(root: &Path, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|name| root.join(name).exists())
        .map(|name| (*name).to_string())
        .collect()
}

/// The `scripts` object of a `package.json`.
///
/// Hand-rolled rather than a JSON dependency pulled in for one object: the
/// shape is fixed, and anything it cannot read is simply not a signal.
pub(super) fn parse_scripts(manifest: &str) -> BTreeMap<String, String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(manifest) else {
        return BTreeMap::new();
    };
    let Some(scripts) = value.get("scripts").and_then(|s| s.as_object()) else {
        return BTreeMap::new();
    };
    scripts
        .iter()
        .filter_map(|(name, body)| Some((name.clone(), body.as_str()?.to_string())))
        .collect()
}

/// A `Makefile` or `justfile` target, as much of it as deciding how to run
/// it takes.
///
/// The whole recipe and the prerequisites, not one line: which of the two
/// ways to start a target is right — `make <target>` or the command itself
/// — cannot be decided from a fragment. See `target_candidates`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    /// The tool that runs it: `make` or `just`.
    pub tool: String,
    /// What follows the colon: the targets that run *first*.
    #[serde(default)]
    pub prereqs: Vec<String>,
    /// Every command line of the recipe, in order, with continuations
    /// joined and the runner's own `@`, `-` and `+` prefixes left on —
    /// they are part of what the recipe says. Blank lines and comment-only
    /// lines are dropped: they are not commands.
    #[serde(default)]
    pub recipe: Vec<String>,
    /// Whether the file sets something at its top that every recipe runs
    /// with: make's `export` or `include`, a justfile's `set` or `export`.
    /// Not published, so a target keeps the shape `agent/json.md` gives it.
    #[serde(skip)]
    pub file_sets_env: bool,
}

impl Target {
    /// The command line this target *is*, when proposing it instead of the
    /// runner loses nothing. See `target_candidates` for why the bar is
    /// this high.
    pub fn sole_command(&self) -> Option<&str> {
        if !self.prereqs.is_empty() || self.file_sets_env {
            return None;
        }
        let [only] = self.recipe.as_slice() else {
            return None;
        };
        // `-` and `+` are directives about how the runner treats the
        // command rather than part of it; `@` only suppresses the echo,
        // which is what running the line directly does anyway.
        let body = only.trim_start_matches(['@', '-', '+']);
        if only[..only.len() - body.len()].contains(['-', '+']) {
            return None;
        }
        let runner_expands = match self.tool.as_str() {
            // just passes `$VAR` through to the shell unchanged; `{{ … }}`
            // is its own interpolation.
            "just" => body.contains("{{"),
            // In a makefile every `$` is make's, including `$$`, which is
            // how a makefile escapes one *for* the shell.
            _ => body.contains('$'),
        };
        (!runner_expands).then_some(body.trim())
    }

    /// What starts this target: its own command when it is representable by
    /// one, otherwise the runner and the target's name.
    pub fn command(&self, name: &str) -> String {
        match self.sole_command() {
            Some(command) => command.to_string(),
            None => format!("{} {name}", self.tool),
        }
    }
}

/// Targets of a `Makefile` or `justfile` that might start something.
pub(super) fn parse_targets(root: &Path) -> BTreeMap<String, Target> {
    let mut out: BTreeMap<String, Target> = BTreeMap::new();
    for (file, tool) in [
        ("Makefile", "make"),
        ("makefile", "make"),
        ("justfile", "just"),
        ("Justfile", "just"),
    ] {
        let Ok(text) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        let lines: Vec<&str> = text.lines().collect();
        let file_sets_env = sets_env(&lines, tool);
        for (i, line) in lines.iter().enumerate() {
            // A target starts at column zero and its name is one word.
            if line.starts_with([' ', '\t']) {
                continue;
            }
            let Some((name, rest)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            {
                continue;
            }
            // `.PHONY`, `.DEFAULT_GOAL` and the rest are directives to make
            // about other targets, never something to run.
            if name.starts_with('.') {
                continue;
            }
            // `x := 1` is an assignment, not a target.
            if rest.trim_start().starts_with('=') {
                continue;
            }
            // `dev:: …` is make's double-colon rule. The second colon is
            // part of the operator, not a prerequisite.
            let rest = rest.strip_prefix(':').unwrap_or(rest);
            // What follows the colon is the prerequisite list — the targets
            // that run *first* — in both make and just, never the recipe.
            // Only make's one-liner form, a semicolon after the
            // prerequisites, puts a command on the target line.
            let (before, inline) = match rest.split_once(';') {
                Some((before, command)) => (before, command.trim()),
                None => (rest, ""),
            };
            let mut recipe: Vec<String> = Vec::new();
            if !inline.is_empty() {
                recipe.push(inline.to_string());
            }
            recipe.extend(recipe_below(&lines, i));
            if recipe.is_empty() {
                // A target whose next line is not indented has no recipe.
                continue;
            }
            out.entry(name.to_string()).or_insert(Target {
                tool: tool.to_string(),
                prereqs: before.split_whitespace().map(str::to_string).collect(),
                recipe,
                file_sets_env,
            });
        }
    }
    out
}

/// Whether a makefile or justfile has a line at its top level that changes
/// the environment or the shell of every recipe in it.
///
/// For make: `export` and `.EXPORT_ALL_VARIABLES`, which hand variables to
/// each recipe's shell; `include`, `-include` and `sinclude`, because the
/// file included is usually `.env` beside an `export`, or a makefile that
/// exports on its own; and an assignment to `SHELL` or `.SHELLFLAGS`. For
/// just: any `set`, among them `dotenv-load`, `export`, `shell` and
/// `working-directory`, and `export NAME := …`. A recipe line lifted out of
/// such a file runs without any of it.
fn sets_env(lines: &[&str], tool: &str) -> bool {
    lines
        .iter()
        .filter(|line| !line.starts_with([' ', '\t']))
        .any(|line| {
            let head = line
                .split(|c: char| c.is_whitespace() || matches!(c, ':' | '=' | '?' | '+' | '!'))
                .next()
                .unwrap_or("");
            match tool {
                "just" => matches!(head, "set" | "export"),
                _ => matches!(
                    head,
                    "export"
                        | ".EXPORT_ALL_VARIABLES"
                        | "include"
                        | "-include"
                        | "sinclude"
                        | "SHELL"
                        | ".SHELLFLAGS"
                ),
            }
        })
}

/// The indented recipe under the target on line `at`, one entry per command.
///
/// Four details of the format, each of which the one-line version got
/// wrong. A trailing `\` continues the command onto the next physical line,
/// however many times. Blank lines and comment-only lines sit *among*
/// recipe lines without ending the recipe, and are not commands — except a
/// `#!` shebang, which is just's way of handing the whole recipe to another
/// interpreter and is very much part of it. A comment may also be at column
/// zero and still not end the recipe. Everything else at column zero does.
fn recipe_below(lines: &[&str], at: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut continued: Option<String> = None;
    for line in lines.iter().skip(at + 1) {
        let trimmed = line.trim();
        // A continuation swallows the next physical line whatever it is,
        // including one that would otherwise end the recipe.
        if let Some(mut started) = continued.take() {
            started.push(' ');
            match trimmed.strip_suffix('\\') {
                Some(head) => {
                    started.push_str(head.trim_end());
                    continued = Some(started);
                }
                None => {
                    started.push_str(trimmed);
                    out.push(started);
                }
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if !line.starts_with([' ', '\t']) {
            if trimmed.starts_with('#') {
                continue;
            }
            break;
        }
        if is_comment_line(trimmed) {
            continue;
        }
        match trimmed.strip_suffix('\\') {
            Some(head) => continued = Some(head.trim_end().to_string()),
            None => out.push(trimmed.to_string()),
        }
    }
    out.extend(continued);
    out
}

/// A recipe line that is only a comment, `@#` included. A `#!` shebang is
/// not one: just runs the recipe through it.
fn is_comment_line(trimmed: &str) -> bool {
    let body = trimmed.trim_start_matches(['@', '-', '+']);
    body.starts_with('#') && !body.starts_with("#!")
}

impl Signals {
    /// The env example's keys, for the rules that only care about names.
    pub fn env_keys(&self) -> impl Iterator<Item = &str> {
        self.env_example.iter().map(|(key, _)| key.as_str())
    }
}

/// Entries of the first env example file that exists, in file order.
///
/// Read the way the app's env file is, with `export`, quotes and a
/// trailing comment dropped: `API_PORT="4000"` is port 4000, and
/// `export PORT=3000` declares `PORT`.
pub(super) fn env_example(root: &Path) -> Vec<(String, String)> {
    for name in ENV_EXAMPLES {
        let Ok(text) = std::fs::read_to_string(root.join(name)) else {
            continue;
        };
        return text
            .lines()
            .filter_map(crate::services::parse_env_line)
            .collect();
    }
    Vec::new()
}

/// The value `key` has in a directory's own env files: its local ones
/// first, which are what the app really reads in the main checkout, then
/// its env example.
///
/// Read for a port number and nothing else: an app whose framework has no
/// default of its own to say, `node server.js`, listens where its `.env`
/// puts it. The value itself is never proposed or written anywhere.
pub(super) fn env_file_value(dir: &Path, key: &str) -> Option<String> {
    LOCAL_ENV_FILES
        .iter()
        .chain(ENV_EXAMPLES.iter())
        .filter_map(|name| std::fs::read_to_string(dir.join(name)).ok())
        .find_map(|text| {
            text.lines()
                .filter_map(crate::services::parse_env_line)
                .find(|(held, _)| held == key)
                .map(|(_, value)| value)
        })
}

/// Root-level files that git ignores and that exist — the local files a
/// fresh worktree would be missing.
///
/// Only the root, and only files: an ignored directory is build output or a
/// dependency tree, which a worktree builds for itself. `--directory` has
/// git name such a directory once, as `name/`, rather than list every file
/// under `node_modules` or `target` for the filter below to throw away.
pub(super) fn ignored_present(root: &Path) -> Vec<String> {
    ignored_files(&Ignored::read(root))
}

/// What `git ls-files --others --ignored --directory` lists under a root:
/// each gitignored file, and each wholly ignored directory once, with a
/// trailing slash. Empty when git cannot say.
struct Ignored {
    root: std::path::PathBuf,
    entries: Vec<String>,
}

impl Ignored {
    fn read(root: &Path) -> Ignored {
        let out = crate::project::git(
            root,
            [
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
                "--no-empty-directory",
                "-z",
            ],
        );
        let entries = match out {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .split('\0')
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        Ignored {
            root: root.to_path_buf(),
            entries,
        }
    }
}

/// The root-level files among them, sorted.
fn ignored_files(ignored: &Ignored) -> Vec<String> {
    let mut found: Vec<String> = ignored
        .entries
        .iter()
        .filter(|p| !p.contains('/'))
        .filter(|p| !artifacts::is_artifact(p))
        .filter(|p| ignored.root.join(p).is_file())
        .cloned()
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The ignored directories among them that a package manager installs
/// into, sorted: the root's, then each app's. Three components at most —
/// `apps/api/node_modules` — and none hidden, so a worktree kept inside
/// the checkout, under `.claude/` or the like, is never offered.
fn dependency_dirs(ignored: &Ignored) -> Vec<String> {
    let names = package_managers::dependency_dirs();
    let mut found: Vec<String> = ignored
        .entries
        .iter()
        .filter_map(|entry| entry.strip_suffix('/'))
        .filter(|dir| {
            let parts: Vec<&str> = dir.split('/').collect();
            parts.len() <= 3
                && parts.last().is_some_and(|last| names.contains(last))
                && !parts.iter().any(|part| part.starts_with('.'))
        })
        .filter(|dir| ignored.root.join(dir).is_dir())
        .map(str::to_string)
        .collect();
    // The root's first: it is the one every workspace shares.
    found.sort_by_key(|dir| (dir.contains('/'), dir.clone()));
    found.dedup();
    found
}

/// Suffixes that make a tracked file the example of a local one.
const EXAMPLE_SUFFIXES: [&str; 3] = [".example", ".sample", ".template"];

/// Root-level example files whose real file is gitignored and not here:
/// `(destination, source)`.
///
/// The fresh-clone case. `.env.example` is tracked and arrives with the
/// clone; `.env` is gitignored and never does, so `ignored_present` — which
/// only lists files that exist — has nothing to offer and a worktree is
/// created without the file the app needs.
///
/// Both conditions are checked here rather than at the question, because
/// proposing a seed that `new` would then refuse is worse than proposing
/// none: `git check-ignore` is what authorises a write into a worktree, and
/// a destination that is not ignored can never be one.
pub(super) fn provision_seeds(root: &Path) -> Vec<(String, String)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let source = entry.file_name().to_string_lossy().to_string();
        let Some(destination) = EXAMPLE_SUFFIXES
            .iter()
            .find_map(|suffix| source.strip_suffix(suffix))
        else {
            continue;
        };
        if destination.is_empty()
            || artifacts::is_artifact(destination)
            || root.join(destination).exists()
            || !is_gitignored(root, destination)
        {
            continue;
        }
        out.push((destination.to_string(), source));
    }
    out.sort();
    out
}

/// Whether the project's own gitignore covers a path, whether or not it
/// exists. Exit 0 means ignored; anything else — including a git that could
/// not run — means it is not, because only a definite yes may authorise a
/// write.
pub fn is_gitignored(root: &Path, rel: &str) -> bool {
    crate::project::git(root, ["check-ignore", "-q", "--", rel])
        .map(|out| out.status.code() == Some(0))
        .unwrap_or(false)
}
