//! The project and config sections: where the project is, and every config
//! layer key by key.

use std::path::Path;

use crate::actions::Machine;
use crate::config::{Config, HIDDEN, is_password};
use crate::paths::PandoPaths;
use crate::{ports, state};

use super::report::{ConfigReport, Finding, KeyReport, LayerReport, ProjectReport, Section};

pub(super) fn project_report(
    paths: &PandoPaths,
    config: &Config,
    machine: &Machine<'_>,
    findings: &mut Vec<Finding>,
) -> ProjectReport {
    if let Some(wsl) = crate::wsl::Wsl::at(&machine.system) {
        windows_drive_findings(paths, config, &wsl, findings);
    }
    let home_mode = mode_of(&paths.home);
    if let Some(mode) = &home_mode
        && mode != "700"
    {
        findings.push(
            Finding::note(
                Section::Project,
                format!(
                    "pando's home is mode {mode}, not 700 — it holds command lines, and a \
                     project's config can carry a credential"
                ),
            )
            .with_fix(format!("chmod 700 {}", paths.home.display())),
        );
    }
    // Only while nothing names the base: a project whose own does has
    // said where work starts, whatever origin/HEAD says.
    if config.project.base.is_none()
        && let Some(drift) = crate::worktree::base_drift(paths.root())
    {
        findings.push(base_drift_finding(&drift));
    }
    // Read straight, with no lock and no save: `actions::refresh` would
    // take the lock, advance phases and write the file back, and doctor
    // writes nothing.
    let windows_held = state::load(&paths.state_file())
        .map(|store| {
            store
                .worktrees
                .values()
                .filter(|record| !record.ports.is_empty())
                .count()
        })
        .unwrap_or(0);
    ProjectReport {
        id: paths.project_id().to_string(),
        root: paths.root().display().to_string(),
        home: paths.home.display().to_string(),
        worktrees_dir: config.worktrees_dir(paths).display().to_string(),
        home_mode,
        port_min: ports::PORT_MIN,
        port_max: ports::PORT_MAX,
        base_step: ports::BASE_STEP,
        bases_in_range: ports::BASE_COUNT,
        windows_held,
    }
}

/// The repository, or the directory its worktrees go in, on a Windows
/// drive under WSL. WSL reaches a drive over a network filesystem: on one
/// fixture `ls` took ten times as long there as in WSL's own, `new` two
/// and a half, and inotify sent no event at all, for an edit from Windows
/// or from Linux, so a dev server that reloads on an edit never does. A
/// note: it all works, slowly.
fn windows_drive_findings(
    paths: &PandoPaths,
    config: &Config,
    wsl: &crate::wsl::Wsl,
    findings: &mut Vec<Finding>,
) {
    let slow = "WSL reaches a Windows drive over a network filesystem, where git is several times \
                slower and an edit sends no inotify event";
    if let Some(drive) = wsl.drive_of(paths.root()) {
        findings.push(
            Finding::note(
                Section::Project,
                format!(
                    "the repository is on the Windows drive at {} — {slow}, so the main \
                     checkout's dev server does not reload on an edit",
                    drive.display()
                ),
            )
            .with_fix(
                "clone the repository into WSL's own filesystem, under ~, and run pando from \
                 there",
            ),
        );
    }
    let worktrees = config.worktrees_dir(paths);
    if let Some(drive) = wsl.drive_of(&worktrees) {
        let fix = match config.project.worktrees_dir {
            Some(_) => "set `[project] worktrees_dir` to a directory under ~ in WSL, or leave it \
                        unset for pando's home"
                .to_string(),
            None => format!(
                "point PANDO_HOME at a directory under ~ in WSL; pando's home is {}",
                paths.home.display()
            ),
        };
        findings.push(
            Finding::note(
                Section::Project,
                format!(
                    "this project's worktrees go on the Windows drive at {} ({}) — {slow}, so a \
                     worktree's dev server does not reload on an edit",
                    drive.display(),
                    worktrees.display()
                ),
            )
            .with_fix(fix),
        );
    }
}

/// origin/HEAD far behind the main checkout's branch: `new` and `check`
/// fork from it all the same, so a check can fail on a commit nobody
/// works on. A note, not a problem: which branch work starts from is the
/// developer's to say, and either way out is theirs.
fn base_drift_finding(drift: &crate::worktree::BaseDrift) -> Finding {
    let crate::worktree::BaseDrift {
        default,
        current,
        ahead,
        days_older,
        ..
    } = drift;
    Finding::note(
        Section::Project,
        format!(
            "origin/HEAD is {default}, last committed {days_older} days before {current}, the \
             main checkout's branch, which has {ahead} commits it lacks — `pando new` and \
             `pando check` fork from {default}"
        ),
    )
    .with_fix(format!(
        "if work starts from {current}, make it this project's base: answer `base` with it \
         through `pando init --answers -` — or point origin/HEAD at it in your repository: \
         `git remote set-head origin {current}`"
    ))
}

/// How the default spelling of the machine-wide config is written in text
/// pando's lower layers compose, before they know which home is in use.
const DEFAULT_USER_CONFIG: &str = "~/.pando/config.toml";

/// The machine-wide config file as doctor names it: the one this run
/// actually reads — under `PANDO_HOME` when that is set — with the
/// developer's home written as `~`.
pub(super) fn user_config_shown(paths: &PandoPaths) -> String {
    let file = paths.user_config_file().display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => {
            let home = home.trim_end_matches('/');
            match file.strip_prefix(home) {
                Some(rest) if rest.starts_with('/') => format!("~{rest}"),
                _ => file,
            }
        }
        _ => file,
    }
}

/// `text` with the default spelling of the machine-wide config replaced by
/// the file this run reads: a line composed below doctor says
/// `~/.pando/config.toml` whatever `PANDO_HOME` is, and a fix pointing at
/// a file pando will not read is no fix.
pub(super) fn with_real_user_config(paths: &PandoPaths, text: &str) -> String {
    text.replace(DEFAULT_USER_CONFIG, &user_config_shown(paths))
}

fn mode_of(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).ok()?;
    Some(format!("{:o}", meta.permissions().mode() & 0o777))
}

pub(super) fn config_report(
    paths: &PandoPaths,
    error: Option<String>,
    warnings: Vec<String>,
    findings: &mut Vec<Finding>,
) -> ConfigReport {
    if let Some(error) = &error {
        findings.push(Finding::problem(
            Section::Config,
            format!("the config does not load: {error}"),
            "fix the file the message names — `new`, `start`, `restart` and the TUI need it, \
             and every other command is running without it",
        ));
    }
    for warning in &warnings {
        findings.push(Finding::note(Section::Config, warning.clone()));
    }
    let layers = vec![
        layer_report("committed", &paths.root().join("pando.toml"), true),
        layer_report("user", &paths.user_config_file(), true),
        layer_report("project", &paths.config_file(), false),
    ];
    ConfigReport { layers, error }
}

/// The two keys only pando's own layer may set. A committed file belongs to
/// a team and a user file to a machine; neither gets to decide where pando
/// writes for this project.
const STRIPPED_BELOW_PROJECT: [&str; 2] = ["project.root", "project.worktrees_dir"];

fn layer_report(layer: &'static str, path: &Path, strips: bool) -> LayerReport {
    let display = path.display().to_string();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return LayerReport {
                layer,
                path: display,
                present: false,
                keys: Vec::new(),
                error: None,
            };
        }
        Err(e) => {
            return LayerReport {
                layer,
                path: display,
                present: true,
                keys: Vec::new(),
                error: Some(format!("cannot be read: {e}")),
            };
        }
    };
    let doc: toml_edit::DocumentMut = match text.parse() {
        Ok(doc) => doc,
        Err(e) => {
            return LayerReport {
                layer,
                path: display,
                present: true,
                keys: Vec::new(),
                error: Some(format!("is not valid TOML: {e}")),
            };
        }
    };
    let mut keys = Vec::new();
    walk_table(doc.as_table(), "", &mut keys);
    if strips {
        for key in &mut keys {
            key.ignored = STRIPPED_BELOW_PROJECT.contains(&key.key.as_str())
                || key.key.starts_with("namespaced.");
        }
    }
    LayerReport {
        layer,
        path: display,
        present: true,
        keys,
        error: None,
    }
}

/// Every key of a document, with the comment the file itself carries beside
/// it.
///
/// Read out of the file rather than re-derived: `set_detected` puts its note
/// in a value's trailing decor, `set_detected_table` on a table's own
/// header, and `set_detected_array_entry` on an entry's. Reading them back
/// shows what is there — including a comment a developer wrote by hand,
/// which no model of what pando would have written could produce.
fn walk_table(table: &toml_edit::Table, prefix: &str, out: &mut Vec<KeyReport>) {
    for (key, item) in table.iter() {
        let path = match prefix.is_empty() {
            true => key.to_string(),
            false => format!("{prefix}.{key}"),
        };
        match item {
            // A login's password is named and never shown: this report is
            // printed, and printed JSON is logged and piped into things.
            toml_edit::Item::Value(value) if is_password(&path) => out.push(KeyReport {
                key: path,
                value: Some(HIDDEN.to_string()),
                raw: None,
                note: comment(value.decor().suffix().and_then(|s| s.as_str())),
                ignored: false,
            }),
            // A login written inline is walked key by key, the way one
            // written as a table is, so its password is hidden too.
            toml_edit::Item::Value(toml_edit::Value::InlineTable(inline))
                if path == "namespaced" || path.starts_with("namespaced.") =>
            {
                if let Some(note) = comment(inline.decor().suffix().and_then(|s| s.as_str())) {
                    out.push(KeyReport {
                        key: path.clone(),
                        value: None,
                        raw: None,
                        note: Some(note),
                        ignored: false,
                    });
                }
                walk_table(&inline.clone().into_table(), &path, out);
            }
            toml_edit::Item::Value(value) => out.push(KeyReport {
                key: path,
                value: Some(value_repr(value)),
                raw: Some(value.clone()),
                note: comment(value.decor().suffix().and_then(|s| s.as_str())),
                ignored: false,
            }),
            toml_edit::Item::Table(inner) => {
                if let Some(note) = comment(inner.decor().suffix().and_then(|s| s.as_str())) {
                    out.push(KeyReport {
                        key: path.clone(),
                        value: None,
                        raw: None,
                        note: Some(note),
                        ignored: false,
                    });
                }
                walk_table(inner, &path, out);
            }
            toml_edit::Item::ArrayOfTables(entries) => {
                for (index, entry) in entries.iter().enumerate() {
                    let path = format!("{path}[{index}]");
                    out.push(KeyReport {
                        key: path.clone(),
                        value: None,
                        raw: None,
                        note: comment(entry.decor().suffix().and_then(|s| s.as_str())),
                        ignored: false,
                    });
                    walk_table(entry, &path, out);
                }
            }
            toml_edit::Item::None => {}
        }
    }
}

/// A value without the whitespace and comments around it: the report lines
/// those up itself.
pub(super) fn value_repr(value: &toml_edit::Value) -> String {
    let mut bare = value.clone();
    bare.decor_mut().clear();
    bare.to_string().trim().to_string()
}

/// A decor suffix as a comment, or `None` when there is nothing but
/// whitespace in it.
fn comment(suffix: Option<&str>) -> Option<String> {
    let text = suffix?.trim();
    (!text.is_empty()).then(|| text.to_string())
}
