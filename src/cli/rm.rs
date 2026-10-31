//! `rm`: one worktree as it always was, several side by side.

use anyhow::Result;
use std::io::Write;

use crate::actions;
use crate::paths::PandoPaths;

use super::each::Outcomes;
use super::{names, notice};

/// `pando rm` of every name typed.
///
/// One name is the command it always was, word for word. Several are
/// resolved first and removed side by side, which [`actions::rm`] is safe
/// to be: `removed <name>` is printed as each finishes, and one that fails
/// leaves the others to finish. The command then fails, naming the ones
/// that stay.
pub(super) fn rm<W: Write + Send>(
    paths: &PandoPaths,
    typed: &[String],
    yes: bool,
    force: bool,
    out: &mut W,
) -> Result<()> {
    if let [one] = typed {
        let name = names::resolve(paths, one)?;
        actions::rm(paths, &name, yes, force, &notice)?;
        writeln!(out, "removed {name}")?;
        return Ok(());
    }
    let mut outcomes = Outcomes::new("removed");
    let resolved = outcomes.resolve(typed, |t| names::resolve(paths, t), String::as_str);
    let unwritten = outcomes.side_by_side(
        &resolved,
        String::as_str,
        |name, progress, out| {
            actions::rm(paths, name, yes, force, progress)?;
            writeln!(out, "removed {name}")?;
            Ok(())
        },
        out,
    );
    outcomes.conclude(unwritten)
}
