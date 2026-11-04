//! Where a project's repository is, written down in its directory under
//! the home: `project.json`, the one record of a project that does not
//! need its checkout to be read.
//!
//! A project's directory is named by its id, a name and a hash, which
//! cannot be turned back into a path. Everything else in it names a
//! checkout, and a checkout can be gone. So the root is written beside the
//! state whenever pando runs in the project and the file is missing or
//! says something else, and a reader that lists every project — Grove,
//! `ls --all` — reads it without asking git.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::discover::ProjectRef;

/// The registry file's name in a project's directory.
pub const PROJECT_FILE: &str = "project.json";

/// Read and written by pando only, beside a state file that is 0600 too.
const PROJECT_FILE_MODE: u32 = 0o600;

/// What `project.json` holds: where the project's main checkout is, what
/// it is called, and the repositories of a multi-repo project, as the
/// commands that ran there saw them — narrowed by `[repos] members`, the
/// same list `ls --json` publishes as `project.repos`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registered {
    pub root: PathBuf,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<String>,
}

impl Registered {
    pub fn of(project: &ProjectRef) -> Self {
        Self {
            root: project.root.clone(),
            name: project.display_name.clone(),
            repos: project.repos.clone(),
        }
    }

    /// The project this names, as it was where it was registered: its id
    /// is the one its root hashes to now, which is another project's when
    /// the root has moved.
    pub fn project(&self) -> ProjectRef {
        let mut project =
            ProjectRef::multi_repo(&self.root, self.repos.clone()).unwrap_or_default();
        project.display_name = self.name.clone();
        project
    }

    /// Whether the repository it names is not where this project's
    /// directory, `id`, says it is: gone, or now hashing to another id —
    /// moved, renamed, or a symlink pointed elsewhere.
    pub fn moved_from(&self, id: &str) -> bool {
        if !self.root.exists() {
            return true;
        }
        !ProjectRef::from_root(&self.root).is_ok_and(|project| project.id == id)
    }
}

/// The registry in `project_dir`, when one is there and readable.
pub fn registered(project_dir: &Path) -> Option<Registered> {
    let text = std::fs::read_to_string(project_dir.join(PROJECT_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Writes `project` into `project_dir`'s registry when the file is missing
/// or says something else. `true` when it wrote.
///
/// Never makes `project_dir`: a project pando has never written anything
/// for is not one to register. Run on every command, so a run that finds
/// the file as it would write it costs one small read and writes nothing.
pub fn remember(project_dir: &Path, project: &ProjectRef) -> Result<bool> {
    remember_as(project_dir, &Registered::of(project))
}

/// [`remember`] for a registry already worked out: a project recovered
/// from what its directory records.
pub fn remember_as(project_dir: &Path, entry: &Registered) -> Result<bool> {
    if !project_dir.is_dir() {
        return Ok(false);
    }
    if registered(project_dir).as_ref() == Some(entry) {
        return Ok(false);
    }
    let text = serde_json::to_string_pretty(entry).context("serialise project.json")?;
    crate::paths::write_private_atomic(
        &project_dir.join(PROJECT_FILE),
        &format!("{text}\n"),
        PROJECT_FILE_MODE,
    )?;
    Ok(true)
}

/// Every project directory under `home`, as `(id, directory)`, sorted by
/// id. Directories only: whatever else sits in `projects/` is no project.
///
/// None when the home or its `projects/` is not there yet. Any other
/// reason it cannot be read — a directory pando may not read, a
/// `PANDO_HOME` that names a file — is an error: read as none, it would
/// say nothing runs while projects do.
pub fn project_dirs(home: &Path) -> Result<Vec<(String, PathBuf)>> {
    let dir = home.join(super::PROJECTS_DIR);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| Some((entry.file_name().into_string().ok()?, entry.path())))
        .collect();
    out.sort();
    Ok(out)
}
