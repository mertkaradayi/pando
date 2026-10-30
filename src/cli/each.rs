//! A command given several worktrees: each name resolved once, the work
//! side by side, and each outcome said under its name as it comes.

use anyhow::{Result, bail};
use std::io::Write;
use std::sync::Mutex;

use super::{notice, to_stderr};

/// How many worktrees are acted on at once. A removal is a `git worktree
/// remove` and perhaps a `docker compose down -v`, a start an install and
/// a dev server: the disk, the daemon and the machine are what they wait
/// on, not pando's lock, and past a few more at once they only queue
/// there.
const AT_ONCE: usize = 8;

/// What became of the worktrees a command was given, as it goes.
pub(super) struct Outcomes {
    /// How many worktrees were asked for, each counted once.
    asked: usize,
    failed: Vec<String>,
    /// What the command did, as "was not …" finishes it: `removed`.
    done: &'static str,
}

impl Outcomes {
    pub(super) fn new(done: &'static str) -> Self {
        Self {
            asked: 0,
            failed: Vec::new(),
            done,
        }
    }

    /// `typed`, resolved by `resolve`, each worktree once — a branch and
    /// its directory are one — in the order typed. A name that does not
    /// resolve is said, and counted as failed.
    pub(super) fn resolve<T>(
        &mut self,
        typed: &[String],
        resolve: impl Fn(&str) -> Result<T>,
        key: impl Fn(&T) -> &str,
    ) -> Vec<T> {
        let mut resolved: Vec<T> = Vec::new();
        for typed in typed {
            match resolve(typed) {
                Ok(item) if resolved.iter().any(|r| key(r) == key(&item)) => {}
                Ok(item) => {
                    self.asked += 1;
                    resolved.push(item);
                }
                Err(e) => {
                    if !self.failed.contains(typed) {
                        self.asked += 1;
                    }
                    self.fail(typed, &e);
                }
            }
        }
        resolved
    }

    /// One of the worktrees asked for that will not be acted on: said
    /// under its name, with every remedy in it spelled as the flag that
    /// takes it.
    pub(super) fn fail(&mut self, name: &str, e: &anyhow::Error) {
        say_not(name, self.done, e);
        if !self.failed.iter().any(|n| n == name) {
            self.failed.push(name.to_string());
        }
    }

    /// `act` for each of `items`, on threads of their own, at most
    /// [`AT_ONCE`] at a time. Each one's notices carry its name; what it
    /// writes is printed in one piece once it is done, and a failure is
    /// said under its name. The first write to `out` that failed — its
    /// reader went away, which `main` tells apart — is returned; the work
    /// goes on regardless.
    pub(super) fn side_by_side<T: Sync, W: Write + Send>(
        &mut self,
        items: &[T],
        name: impl Fn(&T) -> &str + Sync,
        act: impl Fn(&T, &dyn Fn(&str), &mut Vec<u8>) -> Result<()> + Sync,
        out: &mut W,
    ) -> Option<std::io::Error> {
        let queue = Mutex::new(items.iter());
        let failed = Mutex::new(Vec::new());
        let out = Mutex::new(out);
        let unwritten: Mutex<Option<std::io::Error>> = Mutex::new(None);
        let done = self.done;
        std::thread::scope(|scope| {
            for _ in 0..items.len().min(AT_ONCE) {
                scope.spawn(|| {
                    while let Some(item) = next(&queue) {
                        let name = name(item);
                        let progress = |line: &str| notice(&format!("{name}: {line}"));
                        let mut written = Vec::new();
                        let acted = act(item, &progress, &mut written);
                        if !written.is_empty() {
                            let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
                            if let Err(e) = out.write_all(&written) {
                                unwritten
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .get_or_insert(e);
                            }
                        }
                        if let Err(e) = acted {
                            say_not(name, done, &e);
                            failed
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(name.to_string());
                        }
                    }
                });
            }
        });
        self.failed
            .extend(failed.into_inner().unwrap_or_else(|e| e.into_inner()));
        unwritten.into_inner().unwrap_or_else(|e| e.into_inner())
    }

    /// The command's own outcome: a failure naming every worktree that
    /// was not acted on, else the write to stdout that failed, if one did.
    pub(super) fn conclude(self, unwritten: Option<std::io::Error>) -> Result<()> {
        if !self.failed.is_empty() {
            bail!(
                "{} of {} worktrees {} not {}: {}",
                self.failed.len(),
                self.asked,
                if self.failed.len() == 1 {
                    "was"
                } else {
                    "were"
                },
                self.done,
                self.failed.join(", ")
            );
        }
        match unwritten {
            Some(e) => Err(e.into()),
            None => Ok(()),
        }
    }
}

/// The next item a worker takes, if any is left.
fn next<'a, T>(queue: &Mutex<std::slice::Iter<'a, T>>) -> Option<&'a T> {
    queue.lock().unwrap_or_else(|e| e.into_inner()).next()
}

/// One worktree's failure, on stderr under its name.
fn say_not(name: &str, done: &str, e: &anyhow::Error) {
    to_stderr(&format!(
        "pando: {name} was not {done}: {}\n",
        crate::remedy::for_cli(&format!("{e:#}"))
    ));
}
