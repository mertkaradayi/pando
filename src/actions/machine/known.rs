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
/// registry written then, once. One whose root cannot be worked out is
/// left out, since there is nowhere to say it is.
pub fn known(home: &Path) -> Vec<KnownProject> {
    project::project_dirs(home)
        .into_iter()
        .filter_map(|(id, dir)| known_at(id, dir))
        .collect()
}

fn known_at(id: String, dir: PathBuf) -> Option<KnownProject> {
    let registered = match project::registered(&dir) {
        Some(registered) => registered,
        None if dir.join("state.json").is_file() => {
            let recovered = recover_registry(&id, &dir)?;
            // Not worth failing a listing over: the next one recovers it
            // again.
            let _ = project::remember_as(&dir, &recovered);
            recovered
        }
        None => return None,
    };
    let moved = registered.moved_from(&id);
    Some(KnownProject {
        id,
        dir,
        registered,
        moved,
    })
}
