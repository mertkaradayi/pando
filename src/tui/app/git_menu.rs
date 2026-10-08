//! The git menu, `space g`: where the selected checkout stands, what can be done
//! to it and why not where it cannot, a preview of the exact commands,
//! the run on a worker, and what it did. One modal whose stage changes in
//! place; esc always goes back one stage.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::sync::mpsc;
use std::thread;

use crate::actions;
use crate::actions::git::{GitAction, GitRead, Ran};

use super::App;
use super::background::AppEvent;
use super::dialogs::Modal;
use super::pending::{PendingKind, PendingOutcome};

/// The menu's last row, after its actions: a shell in the checkout, to do
/// it by hand.
pub const BY_HAND_KEY: char = '!';

/// Where the git menu is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitStage {
    /// The read of the checkout is on its way.
    Reading,
    /// What it can do. `selected` is a row of [`actions::git::offers`],
    /// or one past them for doing it by hand.
    Menu { read: Box<GitRead>, selected: usize },
    /// The commands, and what moves: ⏎ runs them. This is the asking.
    Preview {
        read: Box<GitRead>,
        action: GitAction,
    },
    /// On the worker. It finishes, or aborts what it started; nothing
    /// stops it halfway.
    Running {
        read: Box<GitRead>,
        action: GitAction,
    },
    /// What it did. `restart` is offered when the branch of a checkout
    /// that runs moved under it.
    Result {
        read: Box<GitRead>,
        action: GitAction,
        ran: Result<Ran, String>,
        restart: bool,
    },
}

impl GitStage {
    /// The row the cursor starts on: a pull when the upstream has
    /// something new, else a rebase when the base has, else the first
    /// move it can make, else the first row.
    fn first_choice(read: &GitRead) -> usize {
        let offers = actions::git::offers(read);
        let open = |action: GitAction| {
            offers
                .iter()
                .position(|o| o.action == action && o.refused.is_none())
        };
        let behind = |drift: Option<(u32, u32)>| drift.is_some_and(|(_, behind)| behind > 0);
        let wanted = if behind(read.upstream_drift) {
            open(GitAction::Pull)
        } else if behind(read.base_drift) {
            open(GitAction::Rebase)
        } else {
            None
        };
        wanted
            .or_else(|| {
                offers
                    .iter()
                    .position(|o| o.refused.is_none() && o.action != GitAction::Fetch)
            })
            .unwrap_or(0)
    }
}

impl App {
    /// `space g`: the git menu for the selected checkout. The read runs on a
    /// worker; the menu says so until it lands.
    pub(super) fn open_git_menu(&mut self) {
        let Some(wt) = self.selected_worktree().cloned() else {
            return;
        };
        let label = self.label_of(&wt.name);
        if wt.prunable {
            self.set_error(format!(
                "{label} has no working tree for git to move — its directory is gone"
            ));
            return;
        }
        self.modal = Some(Modal::Git {
            name: wt.name.clone(),
            stage: GitStage::Reading,
        });
        self.spawn_git_read(wt.name, wt.path, wt.branch);
    }

    fn spawn_git_read(&self, name: String, path: std::path::PathBuf, branch: Option<String>) {
        // Tests hand the read in as an event: no git runs for them.
        if cfg!(test) {
            return;
        }
        let root = self.paths.root().to_path_buf();
        let config = self.config.clone();
        let main = self.is_main(&name);
        let recorded = self
            .record_for(&name)
            .and_then(|record| record.base.clone());
        let tx = self.event_tx.clone();
        thread::spawn(move || {
            let base =
                actions::git::base_for(&root, &config, branch.as_deref(), recorded.as_deref());
            let read = actions::git::read(&path, main, base.as_deref());
            let _ = tx.send(AppEvent::GitRead(Box::new((name, read))));
        });
    }

    /// A read landed: the menu opens on it, if it is still waiting for it.
    pub(super) fn git_read_arrived(&mut self, name: &str, read: GitRead) -> bool {
        let Some(Modal::Git { name: open, stage }) = self.modal.as_mut() else {
            return false;
        };
        if open != name || *stage != GitStage::Reading {
            return false;
        }
        *stage = GitStage::Menu {
            selected: GitStage::first_choice(&read),
            read: Box::new(read),
        };
        true
    }

    pub(super) fn handle_git_key(&mut self, key: KeyEvent, name: String, stage: GitStage) {
        let code = key.code;
        let back = matches!(code, KeyCode::Esc | KeyCode::Char('q'));
        let next = match stage {
            GitStage::Reading if back => None,
            GitStage::Reading => Some(GitStage::Reading),
            GitStage::Menu { read, selected } => {
                let offers = actions::git::offers(&read);
                let rows = offers.len() + 1;
                match code {
                    _ if back => None,
                    KeyCode::Down | KeyCode::Char('j') => Some(GitStage::Menu {
                        read,
                        selected: (selected + 1) % rows,
                    }),
                    KeyCode::Up | KeyCode::Char('k') => Some(GitStage::Menu {
                        read,
                        selected: (selected + rows - 1) % rows,
                    }),
                    KeyCode::Enter => self.choose_git_row(&name, read, selected),
                    KeyCode::Char(BY_HAND_KEY) => self.choose_git_row(&name, read, offers.len()),
                    KeyCode::Char(c) => match offers.iter().position(|o| o.action.key() == c) {
                        Some(row) => self.choose_git_row(&name, read, row),
                        None => Some(GitStage::Menu { read, selected }),
                    },
                    _ => Some(GitStage::Menu { read, selected }),
                }
            }
            GitStage::Preview { read, action } => match code {
                KeyCode::Esc => {
                    let selected = actions::git::offers(&read)
                        .iter()
                        .position(|o| o.action == action)
                        .unwrap_or(0);
                    Some(GitStage::Menu { read, selected })
                }
                KeyCode::Enter => self.run_git(&name, read, action),
                _ => Some(GitStage::Preview { read, action }),
            },
            running @ GitStage::Running { .. } => {
                if back {
                    self.set_status(
                        "it cannot be stopped halfway: it finishes, or aborts what it started",
                    );
                }
                Some(running)
            }
            GitStage::Result {
                read,
                action,
                ran,
                restart,
            } => {
                let stuck = !matches!(&ran, Ok(Ran::Moved(_) | Ran::Unchanged(_)));
                match code {
                    // The preview was the asking: no second press.
                    KeyCode::Char('r') if restart => {
                        self.modal = None;
                        self.restart_named(name, actions::Mode::Remembered, None);
                        return;
                    }
                    KeyCode::Char(BY_HAND_KEY) if stuck => {
                        self.modal = None;
                        let label = self.label_of(&name);
                        self.open_shell_in(&read.checkout, &label);
                        return;
                    }
                    KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => None,
                    _ => Some(GitStage::Result {
                        read,
                        action,
                        ran,
                        restart,
                    }),
                }
            }
        };
        if let Some(stage) = next {
            self.modal = Some(Modal::Git { name, stage });
        }
    }

    /// A row of the menu, picked: its preview, the reason it will not run,
    /// or the shell.
    fn choose_git_row(&mut self, name: &str, read: Box<GitRead>, row: usize) -> Option<GitStage> {
        let offers = actions::git::offers(&read);
        let Some(offer) = offers.get(row) else {
            let label = self.label_of(name);
            self.open_shell_in(&read.checkout, &label);
            return None;
        };
        match &offer.refused {
            Some(why) => {
                self.set_status(why.clone());
                Some(GitStage::Menu {
                    read,
                    selected: row,
                })
            }
            None => Some(GitStage::Preview {
                read,
                action: offer.action,
            }),
        }
    }

    /// ⏎ on a preview: the action, on the worker every action in flight
    /// uses, so the row says what is being done to it.
    fn run_git(&mut self, name: &str, read: Box<GitRead>, action: GitAction) -> Option<GitStage> {
        let checkout = read.checkout.clone();
        let (main, base) = (read.main, read.base.clone());
        let worker_name = name.to_string();
        let (ptx, prx) = mpsc::channel::<String>();
        let started = self.spawn_pending(name.to_string(), PendingKind::Git(action), move || {
            let progress = |msg: &str| {
                let _ = ptx.send(msg.to_string());
            };
            actions::git::run(&checkout, main, base.as_deref(), action, &progress)
                .map(|ran| PendingOutcome::Git(worker_name, ran))
                .map_err(|e| format!("{e:#}"))
        });
        if !started {
            return Some(GitStage::Preview { read, action });
        }
        if let Some(p) = self.pending.as_mut() {
            p.progress_rx = Some(prx);
        }
        Some(GitStage::Running { read, action })
    }

    /// What a run did, on the status line and in the menu if it is still
    /// open on it. Every row's git state is read again: a fetch moves
    /// every row's counts, a rebase one row's head.
    pub(super) fn git_finished(
        &mut self,
        name: &str,
        label: &str,
        action: GitAction,
        result: Result<Ran, String>,
    ) {
        let restart = self.is_up(name)
            && action != GitAction::Fetch
            && matches!(&result, Ok(ran) if ran.moved());
        match &result {
            Ok(ran @ (Ran::Moved(_) | Ran::Unchanged(_))) => {
                let after = if restart {
                    " — it runs on the old files until it restarts"
                } else {
                    ""
                };
                self.set_success(format!("{label}: {}{after}", ran.summary()));
            }
            Ok(ran) => self.set_error_about(name, format!("{label}: {}", ran.summary())),
            Err(e) => {
                self.set_error_about(name, format!("could not {} {label}: {e}", action.word()))
            }
        }
        if let Some(Modal::Git { name: open, stage }) = self.modal.as_mut()
            && open == name
            && let GitStage::Running { read, action } = stage
        {
            *stage = GitStage::Result {
                read: read.clone(),
                action: *action,
                ran: result,
                restart,
            };
        }
        self.spawn_discovery();
        self.refresh_git(None);
    }
}
