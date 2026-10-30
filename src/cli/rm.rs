//! `rm` of several worktrees at once: each on a thread of its own, each
//! outcome said as it comes.

use anyhow::{Result, bail};
use std::io::Write;
use std::sync::Mutex;

use crate::actions;
use crate::paths::PandoPaths;

use super::{names, notice, to_stderr};

/// How many removals run at once. Each is a `git worktree remove`, and
/// perhaps a `docker compose down -v`: the disk and the daemon, not the
/// lock, are what they wait on, and past a few more at once only queue
/// there.
const AT_ONCE: usize = 8;

/// `pando rm` of every name typed.
///
/// One name is the command it always was, word for word. Several are
/// resolved first, each once — `feat/a` and `feat+a` are one worktree —
/// and removed side by side, which [`actions::rm`] is safe to be: each
/// one's notices carry its name, `removed <name>` is printed as each
/// finishes, and one that fails is said under its name and leaves the
/// others to finish. The command then fails, naming the ones that stay.
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
    let mut failed: Vec<String> = Vec::new();
    let mut resolved: Vec<String> = Vec::new();
    for typed in typed {
        match names::resolve(paths, typed) {
            Ok(name) if resolved.contains(&name) => {}
            Ok(name) => resolved.push(name),
            Err(e) => {
                say_not_removed(typed, &e);
                failed.push(typed.clone());
            }
        }
    }
    let asked = resolved.len() + failed.len();
    let queue = Mutex::new(resolved.iter());
    let failed = Mutex::new(failed);
    let out = Mutex::new(out);
    // The first write to stdout that failed: the reader went away, which
    // `main` tells apart. The removals go on regardless.
    let unwritten: Mutex<Option<std::io::Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..resolved.len().min(AT_ONCE) {
            scope.spawn(|| {
                while let Some(name) = next(&queue) {
                    let progress = |line: &str| notice(&format!("{name}: {line}"));
                    match actions::rm(paths, name, yes, force, &progress) {
                        Ok(()) => {
                            let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
                            if let Err(e) = writeln!(out, "removed {name}") {
                                let mut first = unwritten.lock().unwrap_or_else(|e| e.into_inner());
                                first.get_or_insert(e);
                            }
                        }
                        Err(e) => {
                            say_not_removed(name, &e);
                            failed
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(name.clone());
                        }
                    }
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap_or_else(|e| e.into_inner());
    if !failed.is_empty() {
        bail!(
            "{} of {asked} worktrees {} not removed: {}",
            failed.len(),
            if failed.len() == 1 { "was" } else { "were" },
            failed.join(", ")
        );
    }
    match unwritten.into_inner().unwrap_or_else(|e| e.into_inner()) {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// The next name a worker takes, if any is left.
fn next<'a>(queue: &Mutex<std::slice::Iter<'a, String>>) -> Option<&'a String> {
    queue.lock().unwrap_or_else(|e| e.into_inner()).next()
}

/// One name's failure, on stderr under its name, with every remedy in it
/// spelled as the flag that takes it.
fn say_not_removed(name: &str, e: &anyhow::Error) {
    to_stderr(&format!(
        "pando: {name} was not removed: {}\n",
        crate::remedy::for_cli(&format!("{e:#}"))
    ));
}
