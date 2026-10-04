//! What a worktree's own database is called.

use anyhow::{Result, bail};

/// The longest database name pando ever makes or drops, in characters:
/// MariaDB's and MySQL's. A recipe may lower it for its engine —
/// Postgres's is 63 bytes — and never raise it, so the guard that holds
/// every drop to it holds for every engine.
pub const MAX_NAME: usize = 64;

/// What stands between the main database's name and the worktree's, so a
/// namespace can always be told from the database it was named after:
/// `northwind_traders__feat_x`.
pub const MARKER: &str = "__";

/// How many hex digits of a hash tell two names apart that would
/// otherwise be the same, or that had to be cut to fit.
const HASH_LEN: usize = 8;

/// Whether a name may be put between backticks in a statement pando runs,
/// and read back from the server's own listing: letters, digits, `_` and
/// `-`. Nothing that could end a quoted identifier, and no `.`, which a
/// database name may not hold at all.
pub fn is_plain(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

/// The two names a worktree's database may have in a server whose main
/// database is `main`, in the order they are tried.
///
/// The first is the one a developer reads: `<main>__<worktree>`, the
/// worktree's name lowercased with everything but letters and digits made
/// `_` — `feat+x` is `northwind_traders__feat_x`. Past `max` — the
/// engine's longest name, at most [`MAX_NAME`] — it is cut and a short
/// hash is put on the end, so it still fits and two long names that share
/// a beginning still differ. An engine that cut it itself, as Postgres
/// does without an error, would make a database of another name than the
/// one pando records.
///
/// The second always carries the hash, of the project and the worktree
/// together. It is for when the first is somebody else's already: `feat+x`
/// and `feat-x` read the same, and a second clone of the repository on the
/// same server has worktrees of the same names. pando never takes over a
/// database it did not make, so the start moves on to this one instead.
///
/// Deterministic: the same main database, project and worktree always give
/// the same two names. Every name starts with `<main>__`, is longer than
/// that, at most `max` long, and is never `main` itself.
pub fn database_names(
    main: &str,
    project: &str,
    worktree: &str,
    max: usize,
) -> Result<[String; 2]> {
    let max = max.min(MAX_NAME);
    if !is_plain(main) {
        bail!(
            "the main database is called {main:?}, which is not a plain name — pando names a \
             worktree's database after it, so it has to be letters, digits, `_` or `-`"
        );
    }
    let prefix = format!("{main}{MARKER}");
    // Room for the prefix, a separator, a hash, and at least one character
    // of the worktree's own name in front of it.
    if prefix.len() + 1 + HASH_LEN + 1 > max {
        bail!(
            "the main database's name {main:?} is too long to name a worktree's database after \
             it — the server allows {max} characters, and `{prefix}` leaves no room for a \
             worktree"
        );
    }
    let slug = slug(worktree);
    let digest = format!("{:x}", md5::compute(format!("{project}/{worktree}")));
    let hash = &digest[..HASH_LEN];
    let hashed = |cut: &str| match cut.is_empty() {
        true => format!("{prefix}{hash}"),
        false => format!("{prefix}{cut}_{hash}"),
    };
    // What is left for the worktree's own part once the hash is on it.
    let room = max - prefix.len() - 1 - HASH_LEN;
    let cut = cut_at(&slug, room);
    let readable = match slug.is_empty() || prefix.len() + slug.len() > max {
        true => hashed(cut),
        false => format!("{prefix}{slug}"),
    };
    Ok([readable, hashed(cut)])
}

/// A worktree's name as the tail of a database name: lowercase letters and
/// digits, every run of anything else one `_`, none at either end.
fn slug(worktree: &str) -> String {
    let mut out = String::new();
    for c in worktree.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

/// At most `room` characters of a slug, with no `_` left dangling where it
/// was cut.
fn cut_at(slug: &str, room: usize) -> &str {
    let cut = &slug[..slug.len().min(room)];
    cut.trim_end_matches('_')
}
