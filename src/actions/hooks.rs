//! Project hooks, and the probes that watch what they touch.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::{self, Config};
use crate::hooks;
use crate::paths::PandoPaths;
use crate::state::{self, WorktreeRecord};
use crate::template;

use super::runtime::with_prelude;
use super::services::service_roles;

/// The name of the built-in install hook, and of its log file.
///
/// It shares `logs/<worktree>/` with every process, so it is one of
/// [`crate::paths::RESERVED_LOG_SOURCES`]; a process allowed to take the
/// name would have its log truncated on every start.
pub const INSTALL_HOOK: &str = "install";

/// What a plain install is keyed on: every `package.json` in the worktree,
/// the root's and each workspace app's. `node_modules` is never walked.
const UNLOCKED_INSTALL_KEY: &str = "**/package.json";

/// `[project].install` expressed as the `[[hooks]]` entry it is: a
/// create-point hook keyed on the lockfiles, because a lockfile changing
/// is what "the dependencies changed" means.
///
/// One mechanism rather than two. The only thing still special about it is
/// the warning below when a "frozen" install rewrites a lockfile.
///
/// A plain install — the one a project that gitignores its lockfile gets —
/// is keyed on the manifests instead: the lockfile is one it writes itself,
/// or none at all, so it says nothing about whether the dependencies
/// changed, and keying on it would call every first install a lockfile
/// rewrite.
pub(super) fn install_hook(config: &Config) -> Option<config::HookConfig> {
    let install = config.project.install.as_deref()?.trim();
    if install.is_empty() {
        return None;
    }
    // The root's lockfiles, and an app directory's one or two levels down:
    // a repository whose root is not an app installs each app in its own
    // directory, `backend/uv.lock` and `apps/mobile/package-lock.json`,
    // and keyed on the root alone it would install again on every start.
    let fingerprint = match crate::catalog::package_managers::is_unlocked_install(install) {
        true => vec![UNLOCKED_INSTALL_KEY.to_string()],
        false => crate::catalog::package_managers::lockfiles()
            .iter()
            .flat_map(|l| [l.to_string(), format!("*/{l}"), format!("*/*/{l}")])
            .collect(),
    };
    Some(config::HookConfig {
        name: INSTALL_HOOK.to_string(),
        after: config::HookPoint::Create,
        fingerprint,
        cmd: install.to_string(),
        cwd: None,
        fallback: None,
        on: None,
    })
}

/// Every hook that runs at one lifecycle point, in the order they run: the
/// built-in install first, then whatever `[[hooks]]` declares, in file
/// order.
fn hooks_at(config: &Config, point: config::HookPoint) -> Vec<config::HookConfig> {
    let mut out: Vec<config::HookConfig> = Vec::new();
    if point == config::HookPoint::Create
        && let Some(install) = install_hook(config)
    {
        out.push(install);
    }
    out.extend(config.hooks.iter().filter(|h| h.after == point).cloned());
    out
}

/// What a hook is given: the worktree it runs in, the ports of its roles,
/// and the environment the processes get.
pub struct HookContext<'a> {
    pub name: &'a str,
    pub branch: Option<&'a str>,
    pub worktree: &'a Path,
    pub ports: &'a BTreeMap<String, u16>,
    /// Service addresses, so a migration talks to this worktree's own
    /// database rather than the shared one.
    pub service_env: &'a BTreeMap<String, String>,
    /// Whether this start runs on data of the worktree's own: private
    /// services, or namespaces of its own. A hook scoped to isolated
    /// starts — every hook after `services`, unless its entry says
    /// otherwise — runs on both and is skipped otherwise, because the data
    /// it would run against is the main checkout's.
    pub own_data: bool,
    /// Why a start that asked for data of its own has none such a hook
    /// could run on — a namespaced one whose database stays the main
    /// checkout's — for the line that says it was not run.
    pub not_own: Option<&'a str>,
}

/// Runs every hook at one lifecycle point whose fingerprint has changed.
///
/// Deliberately not under the state lock. The recorded fingerprint is read
/// without one — the worst a race can do is run an idempotent hook twice —
/// and the lock is taken only to write the result, because a migration can
/// take a minute and nothing else should wait on it.
pub fn run_hooks(
    paths: &PandoPaths,
    config: &Config,
    point: config::HookPoint,
    ctx: &HookContext<'_>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let has_services = !service_roles(config).is_empty();
    for hook in hooks_at(config, point) {
        if !hook.runs_on(ctx.own_data, has_services) {
            if let Some(note) = skipped(&hook, has_services, ctx.not_own) {
                progress(&note);
            }
            continue;
        }
        run_hook(paths, config, &hook, ctx, progress)?;
    }
    Ok(())
}

/// The scope a hook runs in, in this project: [`config::HookConfig::scope`]
/// with whether the project has services read the way a start reads it.
pub fn hook_scope(config: &Config, hook: &config::HookConfig) -> config::HookScope {
    hook.scope(!service_roles(config).is_empty())
}

/// Why a hook did not run, when that is worth a line: one that only runs
/// where the worktree has data of its own, on a start that has none.
/// `never` is the developer's own answer and says nothing.
///
/// `not_own` is why a start that asked for data of its own has none: the
/// line says that, and not a setting that would run the hook against the
/// main checkout's data.
pub(super) fn skipped(
    hook: &config::HookConfig,
    has_services: bool,
    not_own: Option<&str>,
) -> Option<String> {
    if hook.scope(has_services) != config::HookScope::Isolated {
        return None;
    }
    if let Some(not_own) = not_own {
        return Some(format!(
            "{}: not run — {not_own}, and it runs only where the data is the worktree's own",
            hook.name
        ));
    }
    // Only an explicit `on = "isolated"` reaches here in a project with
    // no services, and there is no "shared services" to speak of: nothing
    // in the project can be isolated at all.
    let why = match has_services {
        true => "this start uses the shared services",
        false => "this project has no services pando can isolate, so no start is isolated",
    };
    Some(format!(
        "{}: not run — it runs on isolated and namespaced starts only, and {why} (set \
         `on = \"always\"` in its [[hooks]] entry to run it here too)",
        hook.name
    ))
}

fn run_hook(
    paths: &PandoPaths,
    config: &Config,
    hook: &config::HookConfig,
    ctx: &HookContext<'_>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    let log_file = paths.log_file(ctx.name, &hook.name);
    let template_ctx = template_context(paths, ctx, &log_file);
    let cmd = template::render_shell(&hook.cmd, &template_ctx)
        .with_context(|| format!("in the command for hook {}", hook.name))?;
    let fallback = hook
        .fallback
        .as_deref()
        .map(|f| template::render_shell(f, &template_ctx))
        .transpose()
        .with_context(|| format!("in the fallback for hook {}", hook.name))?;
    let cwd = hook_cwd(ctx.worktree, hook, &template_ctx)?;

    // The command is part of the fingerprint, not only the files: editing
    // what a hook runs is a reason to run it again, and keying on the
    // files alone meant a corrected migration command never ran.
    let (pins, keyed_cmd) = runtime_key(config, hook, &cmd);
    let fingerprint = || hooks::fingerprint_with(ctx.worktree, &hook.fingerprint, pins, &keyed_cmd);
    let current = fingerprint();
    let last_run = state::load(&paths.state_file()).ok().and_then(|store| {
        store
            .worktrees
            .get(ctx.name)
            .and_then(|r| r.hooks.get(&hook.name))
            .cloned()
    });
    let recorded = last_run.as_ref().and_then(|h| h.fingerprint.clone());
    if unchanged(current.as_deref(), recorded.as_deref()) {
        return Ok(());
    }
    // …and a hook that *has* globs and matched nothing with them is the
    // same thing by accident: `fingerprint = ["prisma/migrations"]` instead
    // of `["prisma/migrations/**"]` is the natural typo and costs a full
    // migration on every start. Every guess is visible; so is this.
    //
    // Once per worktree, the first time the hook runs there: after that
    // the recorded run is the proof it was said, and a notice repeated on
    // every `new` and `start` is one nobody reads. `doctor` goes on
    // reporting a `[[hooks]]` entry at rest.
    if !hook.fingerprint.is_empty() && current.is_none() && last_run.is_none() {
        progress(&match hook.name == INSTALL_HOOK {
            // Its globs are pando's list of every lockfile it knows, not
            // anything the developer wrote, so the typo advice is noise.
            true => format!(
                "{INSTALL_HOOK}: there is no lockfile here to key it on, so it runs on every start"
            ),
            false => matched_nothing(ctx.worktree, hook),
        });
    }

    // "running the install step: true" rather than "install: true", which
    // read as a setting being reported rather than a command being run.
    progress(&match hook.name.as_str() {
        INSTALL_HOOK => format!("running the install step: {cmd}"),
        name => format!("running the {name} hook: {cmd}"),
    });
    let mut env = pando_env(paths, ctx.name, ctx.branch, ctx.worktree);
    env.extend(ctx.service_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    // What the worktree looked like before, so anything the hook leaves
    // behind can be named rather than merely counted.
    let before = porcelain_status(ctx.worktree);
    hooks::run_with_fallback(
        &log_file,
        &with_prelude(config, &cmd),
        fallback
            .as_deref()
            .map(|f| with_prelude(config, f))
            .as_deref(),
        &cwd,
        &env,
    )
    .with_context(|| hook_failed(paths, config, hook))?;

    // A hook that rewrites one of its own inputs — a "frozen" install that
    // normalises a lockfile is the classic — has to be reported, because a
    // tracked file changing under a worktree is what Invariant 1 exists to
    // prevent. And the fingerprint recorded is the one the hook *left
    // behind*, or it sees its own change and re-runs on every start.
    let after = fingerprint();
    if after != current {
        progress(&changed_its_inputs(&hook.name, &cmd));
    }
    // And anything it wrote *outside* what it is keyed on, which the
    // fingerprint can say nothing about. `docs/02-principles.md`: a hook
    // that writes a new file into the worktree is misconfigured, and the
    // developer can only know that if pando says which file.
    let appeared = newly_dirty(&before, &porcelain_status(ctx.worktree));
    if !appeared.is_empty() {
        progress(&format!(
            "warning: the {} hook changed this worktree: {} — `git status` there will show \
             that, and pando never writes into your repository",
            hook.name,
            appeared.join(", ")
        ));
    }

    let _lock = state::lock(&paths.lock_file())?;
    let mut store = state::load(&paths.state_file())?;
    // `or_insert_with`, not `get_mut`: on `start` for an adopted worktree
    // there is no record yet — `start` creates it after this returns — and
    // dropping the result on the floor would re-run on every start
    // forever. A record pando did not create is not claimed as its own.
    store
        .worktrees
        .entry(ctx.name.to_string())
        .or_insert_with(|| WorktreeRecord::new(ctx.worktree, false))
        .hooks
        .insert(
            hook.name.clone(),
            state::HookRecord {
                fingerprint: after,
                ran_at: Utc::now(),
            },
        );
    state::save(&paths.state_file(), &store)?;
    Ok(())
}

/// Whether the next start in a worktree runs a hook: the hook runs on the
/// data that start has, and its fingerprint — the files it is keyed on
/// and its command as that start renders it — is not the one recorded.
///
/// Public because `doctor` answers the same question at rest. Its own copy
/// fingerprinted the command as written, so every hook with a placeholder
/// in it read as changed, and it ignored the hook's scope.
pub fn runs_again(
    paths: &PandoPaths,
    config: &Config,
    hook: &config::HookConfig,
    ctx: &HookContext<'_>,
    recorded: Option<&str>,
) -> bool {
    if !hook.runs_on(ctx.own_data, !service_roles(config).is_empty()) {
        return false;
    }
    let log_file = paths.log_file(ctx.name, &hook.name);
    // A command that does not render is one the start stops on rather
    // than skips.
    let Ok(cmd) = template::render_shell(&hook.cmd, &template_context(paths, ctx, &log_file))
    else {
        return true;
    };
    let (pins, keyed_cmd) = runtime_key(config, hook, &cmd);
    let current = hooks::fingerprint_with(ctx.worktree, &hook.fingerprint, pins, &keyed_cmd);
    !unchanged(current.as_deref(), recorded)
}

/// Whether a hook's inputs are what they were when it last ran. No
/// fingerprint at all says nothing of the kind, so such a hook runs every
/// time.
fn unchanged(current: Option<&str>, recorded: Option<&str>) -> bool {
    current.is_some() && current == recorded
}

/// What a hook's command, fallback and cwd render against in one worktree.
fn template_context<'a>(
    paths: &'a PandoPaths,
    ctx: &HookContext<'a>,
    log_file: &'a Path,
) -> template::Context<'a> {
    template::Context {
        name: ctx.name,
        branch: ctx.branch,
        worktree: ctx.worktree,
        root: paths.root(),
        project: paths.project_id(),
        ports: ctx.ports,
        default_role: None,
        log: Some(log_file),
    }
}

/// What a hook's fingerprint covers besides its own globs, and the command
/// it is keyed on.
///
/// The install step is keyed on the runtime it builds for as well: the
/// files that pin the version, and the command with the prelude that picks
/// it. A version manager reads the worktree's own `.nvmrc`, so a branch
/// that moves it, or a corrected prelude, changes the runtime without
/// touching a lockfile, and native modules built for the old one then
/// fail to load. Every other hook is keyed on its globs and command alone:
/// a migration run again because the runtime moved is run for nothing.
fn runtime_key<'a>(
    config: &'a Config,
    hook: &config::HookConfig,
    cmd: &str,
) -> (&'a [String], String) {
    match hook.name == INSTALL_HOOK {
        true => (&config.runtime.version_files, with_prelude(config, cmd)),
        false => (&[], cmd.to_string()),
    }
}

/// Runs the pre-start probes, refusing to start when one of them fails in
/// a way it recognised.
///
/// The asymmetry is the whole design. A probe that fails with the stderr
/// it was told to watch for is a diagnosis: the start stops and the
/// developer gets the one-line fix. A probe that fails any *other* way is
/// ignored, because a check pando does not understand must never be the
/// reason a project it has never seen refuses to start.
pub(super) fn run_probes(
    paths: &PandoPaths,
    config: &Config,
    ctx: &HookContext<'_>,
    progress: &dyn Fn(&str),
) -> Result<()> {
    if config.probes.is_empty() {
        return Ok(());
    }
    let template_ctx = template::Context {
        name: ctx.name,
        branch: ctx.branch,
        worktree: ctx.worktree,
        root: paths.root(),
        project: paths.project_id(),
        ports: ctx.ports,
        default_role: None,
        log: None,
    };
    let mut env = pando_env(paths, ctx.name, ctx.branch, ctx.worktree);
    env.extend(ctx.service_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    for probe in &config.probes {
        let cmd = template::render_shell(&probe.cmd, &template_ctx)
            .with_context(|| format!("in the command for probe {}", probe.name))?;
        let Some(stderr) = hooks::probe(&with_prelude(config, &cmd), ctx.worktree, &env)
            .with_context(|| format!("the probe {} could not be run", probe.name))?
        else {
            continue;
        };
        if !stderr.contains(&probe.match_) {
            // Ignored on purpose — and said out loud, so a probe that has
            // quietly stopped recognising anything is visible.
            progress(&format!(
                "the probe {} failed in a way it does not recognise; ignoring it",
                probe.name
            ));
            continue;
        }
        let reason = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim();
        bail!(
            "the probe {} failed: {reason}\n  {}",
            probe.name,
            probe.hint
        );
    }
    Ok(())
}

/// `git status --porcelain --untracked-files=all` inside a worktree, one
/// entry per line.
///
/// Empty when git cannot answer. A hook that ran is not failed because the
/// check after it could not be made — the check is a warning, not a gate.
///
/// `--no-optional-locks`, so the check never takes `index.lock` from under
/// a `git commit` the developer runs in the worktree while a hook does, and
/// bounded like every other git question pando asks.
pub(super) fn porcelain_status(worktree: &Path) -> Vec<String> {
    let Ok(out) = crate::project::git(
        worktree,
        [
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// The paths in `after` that were not dirty in `before`, without their
/// two-letter status prefix.
fn newly_dirty(before: &[String], after: &[String]) -> Vec<String> {
    after
        .iter()
        .filter(|line| !before.contains(line))
        .map(|line| line.get(3..).unwrap_or(line.as_str()).trim().to_string())
        .collect()
}

/// The first words of a hook's failure, which name the hook: how `pando
/// check` tells which one failed.
pub(super) fn failed_words(hook: &str) -> String {
    format!("the {hook} hook failed")
}

/// A failed install step's error with the way past it: the file that
/// sets `project.install`, and that `""` there skips installing. `None`
/// for any other error. The CLI says it after `new`, `start` and
/// `restart`, and the TUI after `n`.
pub fn install_remedy(paths: &PandoPaths, error: &str) -> Option<String> {
    error.contains(&failed_words(INSTALL_HOOK)).then(|| {
        format!(
            "{error} — `project.install` in {} is the command; fix it there, or set it to \"\" \
             to skip installing",
            config::install_origin(paths).display()
        )
    })
}

/// The context a failing hook carries: which entry it is, and where the one
/// edit that removes it lives.
///
/// For a hook the developer wrote, naming the file is a courtesy. For one
/// *pando* invented — the detected `migrate` this phase adds automatically
/// — it is the difference between "a wrong guess costs one edit" and a
/// mysterious failure at start time.
fn hook_failed(paths: &PandoPaths, config: &Config, hook: &config::HookConfig) -> String {
    let base = failed_words(&hook.name);
    // The install step is synthesised from `[project].install`; there is no
    // `[[hooks]]` entry to point at.
    if hook.name == INSTALL_HOOK || !config.hooks.iter().any(|h| h.name == hook.name) {
        return base;
    }
    format!(
        "{base} — this is the [[hooks]] entry named {:?} in {}; fix or remove it there, or \
         have your coding agent run `pando init --agent` and `pando check`",
        hook.name,
        hook_source_file(paths, &hook.name).display()
    )
}

/// Which config file declares a `[[hooks]]` entry by that name. pando's own
/// first, because that is the layer that wins and the one detection writes.
fn hook_source_file(paths: &PandoPaths, name: &str) -> PathBuf {
    let home = paths.config_file();
    if declares_hook(&home, name) {
        return home;
    }
    let committed = paths.root().join("pando.toml");
    if declares_hook(&committed, name) {
        return committed;
    }
    home
}

fn declares_hook(path: &Path, name: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return false;
    };
    doc.get("hooks")
        .and_then(|item| item.as_array_of_tables())
        .is_some_and(|tables| {
            tables
                .iter()
                .any(|table| table.get("name").and_then(|v| v.as_str()) == Some(name))
        })
}

/// One notice for a hook whose globs match nothing at all.
///
/// Public because `doctor` reports the same thing at rest that a start
/// says while it happens, and two spellings of it would drift.
pub fn matched_nothing(worktree: &Path, hook: &config::HookConfig) -> String {
    let globs: Vec<String> = hook
        .fingerprint
        .iter()
        .map(|glob| format!("{glob:?}"))
        .collect();
    let mut message = format!(
        "warning: the {} hook is keyed on {}, which matches nothing in this worktree, so it \
         runs on every start",
        hook.name,
        globs.join(", ")
    );
    // The common shape of the mistake: a literal that names a directory.
    // A glob matches files, so it needs `/**` to reach into one. A trailing
    // `/` is dropped first: `dir//**` has an empty segment, which matches
    // no name at all, so the advice would keep the warning it answers.
    let directories: Vec<String> = hook
        .fingerprint
        .iter()
        .filter(|glob| !glob.contains(['*', '?']) && worktree.join(glob).is_dir())
        .map(|glob| format!("{}/**", glob.trim_end_matches('/')))
        .collect();
    if !directories.is_empty() {
        message.push_str(&format!(
            " — that is a directory, and a fingerprint matches files; try {}",
            directories.join(", ")
        ));
    }
    message
}

fn changed_its_inputs(hook: &str, cmd: &str) -> String {
    if hook == INSTALL_HOOK {
        return format!(
            "warning: {cmd:?} changed a lockfile in this worktree — that command is not as \
             frozen as it looks, and `git status` there will show it"
        );
    }
    format!(
        "warning: the {hook} hook changed one of the files it is keyed on — `git status` in \
         this worktree will show it, and the hook will run again next time"
    )
}

/// A hook runs in the worktree, or in the subdirectory it names. The same
/// refusals a process's `cwd` gets: a hook that ran outside its own
/// worktree would be writing into a repository.
fn hook_cwd(
    worktree: &Path,
    hook: &config::HookConfig,
    ctx: &template::Context<'_>,
) -> Result<PathBuf> {
    let Some(relative) = hook.cwd.as_deref() else {
        return Ok(worktree.to_path_buf());
    };
    let rendered = template::render(relative, ctx)
        .with_context(|| format!("in the cwd for hook {}", hook.name))?;
    let dir = worktree.join(&rendered);
    if !dir.is_dir() {
        bail!(
            "cwd {rendered:?} does not exist in this worktree ({}) — check the cwd of hook {:?}",
            dir.display(),
            hook.name
        );
    }
    let resolved = crate::paths::resolve_for_compare(&dir);
    let owner = crate::paths::resolve_for_compare(worktree);
    if !resolved.starts_with(&owner) {
        bail!(
            "cwd {rendered:?} for hook {:?} resolves to {}, which is outside the worktree ({})",
            hook.name,
            resolved.display(),
            owner.display()
        );
    }
    Ok(dir)
}

/// The variables every command pando runs gets, whether or not it has ports
/// yet. A hook has to be able to find out which worktree it is in.
pub(super) fn pando_env(
    paths: &PandoPaths,
    name: &str,
    branch: Option<&str>,
    worktree: &Path,
) -> Vec<(String, String)> {
    let mut env = vec![
        ("PANDO_NAME".to_string(), name.to_string()),
        // The git branch, the same string the dev process is given. A hook
        // doing `git checkout "$PANDO_BRANCH"` with the *directory* name
        // checks out the wrong thing, or nothing at all. A detached HEAD
        // has no branch, and then the directory name is all there is.
        (
            "PANDO_BRANCH".to_string(),
            branch.unwrap_or(name).to_string(),
        ),
        ("PANDO_WORKTREE".to_string(), worktree.display().to_string()),
        ("PANDO_ROOT".to_string(), paths.root().display().to_string()),
        ("PANDO_PROJECT".to_string(), paths.project_id().to_string()),
    ];
    // What `pando check` runs is a test of the settings, in a throwaway
    // worktree: a process that reaches outside the worktree — opening an
    // app on the simulator, say — can leave that out, and needs a
    // documented way to know rather than the check's directory name.
    if name == crate::paths::CHECK_WORKTREE {
        env.push((CHECK_ENV.to_string(), "1".to_string()));
    }
    env
}

/// Set to `1` for every process and hook `pando check` starts, and for
/// nothing else.
pub const CHECK_ENV: &str = "PANDO_CHECK";
