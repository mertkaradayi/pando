//! What the check's worktree cost on disk: the gitignored trees the
//! project's own install and processes left in it, and what they wrote
//! outside it, measured once it passed and before the teardown.
//!
//! Nothing here knows an ecosystem. A tree is whatever git ignores; a cache
//! outside is whatever the free space says went missing. Which of them a
//! worktree could share, and how, is the reader's to judge: `clone` for a
//! tree, a process's `env` for a cache keyed by the checkout's path.

use crate::setup::{DiskCost, DiskTree};
use std::path::Path;

/// A tree smaller than this is left out: a worktree's logs and stamps are
/// not what fills a disk.
pub(super) const MIN_TREE_BYTES: u64 = 1 << 20;

/// Below this, what went outside is no line: the logs and state pando
/// and the tools keep, and the noise of everything else writing.
pub(super) const OUTSIDE_WORTH_A_LINE: u64 = 50 * MIN_TREE_BYTES;

/// The most trees a check lists, biggest first.
pub(super) const MAX_TREES: usize = 12;

/// The disk's free space before anything was made, read on the main
/// checkout's filesystem, where worktrees and their caches live.
pub(super) struct Before {
    free: Option<u64>,
}

impl Before {
    pub(super) fn read(root: &Path) -> Before {
        Before {
            free: crate::platform::files::free_bytes(root),
        }
    }
}

/// Measures the check's worktree at `worktree` against the main checkout at
/// `main`. `repos` are a folder of repositories' members, whose trees are
/// named under them; `clones` is what `clone` names.
pub(super) fn measure(
    before: &Before,
    main: &Path,
    worktree: &Path,
    repos: &[String],
    clones: &[String],
) -> DiskCost {
    let mut trees: Vec<DiskTree> = crate::detect::ignored_dirs(worktree, repos)
        .into_iter()
        .filter_map(|path| {
            let at = worktree.join(&path);
            // A provisioned link is the main checkout's, not this
            // worktree's.
            let meta = std::fs::symlink_metadata(&at).ok()?;
            (meta.is_dir() && !meta.file_type().is_symlink()).then(|| DiskTree {
                bytes: tree_bytes(&at),
                main_has: main.join(&path).is_dir(),
                cloned: clones
                    .iter()
                    .any(|clone| clone.trim_end_matches('/') == path),
                path,
            })
        })
        .collect();
    let inside: u64 = trees.iter().map(|tree| tree.bytes).sum();
    let outside_bytes = before
        .free
        .zip(crate::platform::files::free_bytes(main))
        .map(|(then, now)| then.saturating_sub(now).saturating_sub(inside));
    trees.retain(|tree| tree.bytes >= MIN_TREE_BYTES);
    trees.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    trees.truncate(MAX_TREES);
    DiskCost {
        trees,
        outside_bytes,
    }
}

/// The blocks of everything under `dir`, links not followed and each file
/// counted where it is, as `du` counts them.
pub(super) fn tree_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            total += crate::platform::files::allocated_bytes(&meta);
            if meta.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    total
}

/// Where a reader learns what to do about a tree to share or a cache
/// outside: the line says it only when there is one, so the setup job
/// itself spends nothing on it.
pub(super) const WHAT_TO_DO: &str =
    "how to judge it: \"What a worktree costs on disk\" in `pando init --agent --reference brief`";

/// The line a person watching the check reads, and the agent setting the
/// project up: the trees a new worktree builds, the ones `clone` could
/// share marked, what went outside, and where to read what to do about
/// them. `None` when there is nothing worth a line.
pub(super) fn summary(cost: &DiskCost) -> Option<String> {
    let mut parts: Vec<String> = cost
        .trees
        .iter()
        .map(|tree| {
            let mark = match (tree.main_has, tree.cloned) {
                (true, false) => " (the main checkout has one: `clone` could share it)",
                _ => "",
            };
            format!("{} {}{mark}", tree.path, size(tree.bytes))
        })
        .collect();
    if let Some(outside) = cost
        .outside_bytes
        .filter(|bytes| *bytes >= OUTSIDE_WORTH_A_LINE)
    {
        parts.push(format!("about {} written outside it", size(outside)));
    }
    if parts.is_empty() {
        return None;
    }
    let mut line = format!("a worktree holds {}", parts.join(" · "));
    let to_share = cost.trees.iter().any(|tree| tree.main_has && !tree.cloned);
    let outside = cost
        .outside_bytes
        .is_some_and(|bytes| bytes >= OUTSIDE_WORTH_A_LINE);
    if to_share || outside {
        line.push_str(&format!(" — {WHAT_TO_DO}"));
    }
    Some(line)
}

/// `512 KB`, `48 MB`, `1.2 GB`: decimal, as a disk is sold and `df -H`
/// prints.
pub(super) fn size(bytes: u64) -> String {
    const KB: f64 = 1e3;
    const MB: f64 = 1e6;
    const GB: f64 = 1e9;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else {
        format!("{:.0} KB", b / KB)
    }
}
