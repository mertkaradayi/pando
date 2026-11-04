//! The projects pando knows: every directory under the home that holds a
//! state file or a registry, with where its repository is.

use std::path::{Path, PathBuf};

use crate::project::{self, Registered};

use super::recover::recover_registry;

/// A project pando has run in, as its directory under the home says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownProject {
    /// The directory's name: the id the project had when pando ran there.
    pub id: String,
    /// Its directory under the home.
    pub dir: PathBuf,
    /// Where its repository is, what it is called, its repositories.
    pub registered: Registered,
    /// Whether that repository is gone, or now hashes to another id: a
    /// project shown and never acted on.
    pub moved: bool,
    /// Whether pando may not read its directory: listed as moved, never
    /// acted on, and said to be unreadable rather than to have moved.
    pub unreadable: bool,
}

impl KnownProject {
    pub fn state_file(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.dir.join("state.lock")
    }
}

/// Every project under `home`, by id.
///
/// A directory with neither a state file nor a registry is none: pando
/// made it and ran nothing there, or something else put it there. One
/// with a state file and no registry was registered before registries
/// existed; its root is recovered from the checkouts it records and its
/// registry written then, once. One whose root cannot be worked out —
/// every checkout it records is gone — is still listed, as moved, under
/// the name its directory carries and an empty root, and nothing is
/// written: running pando in its repository registers it again while the
/// repository is where it was, and `pando doctor` in its new place adopts
/// it once it moved, under the same folder name. A directory pando cannot
/// read is listed the same way, and its summary says why: its processes
/// may hold ports all the same.
///
/// An error when the home's `projects/` cannot be read, which is not the
/// same as none.
pub fn known(home: &Path) -> anyhow::Result<Vec<KnownProject>> {
    Ok(project::project_dirs(home)?
        .into_iter()
        .filter_map(|(id, dir)| known_at(id, dir))
        .collect())
}

/// Whether `dir` may hold a state file: it does, or pando cannot tell
/// because it may not look.
fn state_file_may_exist(dir: &Path) -> bool {
    match std::fs::metadata(dir.join("state.json")) {
        Ok(meta) => meta.is_file(),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Whether pando may not read `dir` or the registry in it.
fn unreadable(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_err()
        || std::fs::File::open(dir.join(project::PROJECT_FILE))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
}

fn known_at(id: String, dir: PathBuf) -> Option<KnownProject> {
    let unreadable = unreadable(&dir);
    let registered = match project::registered(&dir) {
        Some(registered) => registered,
        None if state_file_may_exist(&dir) => match recover_registry(&id, &dir) {
            Some(recovered) => {
                // Not worth failing a listing over: the next one recovers
                // it again.
                let _ = project::remember_as(&dir, &recovered);
                recovered
            }
            None => Registered {
                root: PathBuf::new(),
                name: name_of(&id),
                repos: Vec::new(),
            },
        },
        None => return None,
    };
    // Never acted on, whether it is where it was or not.
    let moved = unreadable || registered.moved_from(&id);
    Some(KnownProject {
        id,
        dir,
        registered,
        moved,
        unreadable,
    })
}

/// The display name in a project's id, `<name>-<8 hex>`: the whole id
/// when it has no such tail.
fn name_of(id: &str) -> String {
    match id.rsplit_once('-') {
        Some((name, hash))
            if !name.is_empty()
                && hash.len() == 8
                && hash.bytes().all(|b| b.is_ascii_hexdigit()) =>
        {
            name.to_string()
        }
        _ => id.to_string(),
    }
}
