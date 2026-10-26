//! The modals: create, pull requests, remove, the confirmations, and the
//! question a worker asks.

use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::sync::mpsc::Sender;

use crate::actions;
use crate::worktree::{BranchEntry, PrInfo, PrState};

use super::App;

#[derive(Debug, Clone)]
pub enum Modal {
    Create {
        input: String,
        branches: BranchLoadState,
        selected: usize,
        /// What a new branch forks from, once tab has chosen it. Until
        /// then, none: it forks from the base the config gives its name.
        base: Option<String>,
    },
    /// `p`: the project's open pull requests, narrowed by what is typed.
    /// The list itself is the app's, read at every paint, so the answer to
    /// the fetch opening this started lands in it while it is open.
    PullRequests {
        input: String,
        selected: usize,
    },
    /// What would stop the removal, or what it would take with it, is not
    /// kept here: it is read off the app at every paint and every key, so
    /// a git re-read that lands while the dialog is open is what it shows.
    Remove {
        name: String,
        /// Whether pando created it; drives the extra warning line.
        created_by_pando: bool,
    },
    /// Confirming that a public URL goes away. Somebody may be looking at
    /// it right now, so it is not a bare keypress.
    Unshare {
        name: String,
        url: String,
    },
    /// `t` on a worktree that is not shared: its dev server goes on the
    /// internet, which is the one step here that cannot be taken back —
    /// a link, once given out, has been given out.
    Share {
        name: String,
    },
    /// A mode key on a worktree running in another mode — `i` on one
    /// running shared, `S` on one running isolated: every process
    /// restarts, on services it was not using a moment ago.
    SwitchMode {
        name: String,
        /// The mode it is switching to.
        to: crate::state::ServiceMode,
    },
    /// ⏎: which services it runs on — shared, namespaced (experimental),
    /// isolated. On a stopped worktree the mode it last ran in is under
    /// the cursor; on a running one the mode it runs in, and choosing
    /// another switches it, with no second dialog: this was the asking.
    Mode {
        name: String,
        /// The row under the cursor, in [`crate::state::ServiceMode::ALL`].
        selected: usize,
    },
    /// `X`: every worktree with anything up, stopped at once, as
    /// `pando stop --all` does. Lists what goes down before it does.
    StopAll {
        names: Vec<String>,
    },
    /// Something pando needs to know before it can start. A worker thread is
    /// waiting on `reply`; closing this modal without answering has to send
    /// something, or that thread waits forever.
    Question {
        question: actions::Question,
        selected: usize,
        /// `Some` while a command is being typed instead of chosen.
        custom: Option<String>,
        reply: Sender<Result<actions::Answer, String>>,
    },
    /// `T`: every colour theme. The one under the cursor paints the
    /// screen while it is there; `before` is what esc puts back.
    Theme {
        selected: usize,
        before: crate::theme::Palette,
    },
    /// `space g`: the git menu on a checkout, at whichever stage it is.
    Git {
        name: String,
        stage: super::GitStage,
    },
    Help,
    /// What pando has said this session, in full and newest first: the
    /// header has one row, and a long error does not fit in it.
    Messages,
}

/// Stated before the user confirms, rather than discovered after: what
/// would refuse the removal, and what it would take down with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoveBlocker {
    Locked(Option<String>),
    /// Uncommitted changes or untracked files: git refuses without force,
    /// and with it they are gone.
    Dirty,
    /// Git has not been read yet, so nobody knows whether it is dirty.
    DirtyUnknown,
    /// Something runs in it, which the removal stops first.
    Running,
    NotOurs,
    /// The namespaces it holds in the project's own servers, which go with
    /// it: `drops database northwind_traders__feat_x`, `empties redis slot
    /// 3`.
    Drops(Vec<String>),
    /// What a set's directory holds beside its checkouts, which only a
    /// forced removal takes with it.
    Beside(Vec<String>),
    /// A git checkout in a set's directory that is not one of the set's,
    /// which no removal takes.
    Foreign(Vec<String>),
}

impl RemoveBlocker {
    pub fn line(&self) -> String {
        match self {
            RemoveBlocker::Locked(Some(reason)) => {
                format!("locked ({reason}) — pando always refuses; unlock it first")
            }
            RemoveBlocker::Locked(None) => {
                "locked — pando always refuses; unlock it first".to_string()
            }
            RemoveBlocker::Dirty => {
                "it has uncommitted changes or untracked files — F removes it and discards them"
                    .to_string()
            }
            RemoveBlocker::DirtyUnknown => {
                "git has not been read yet — uncommitted changes, if any, stop y; F removes anyway"
                    .to_string()
            }
            RemoveBlocker::Running => "it is running — removing stops it first".to_string(),
            RemoveBlocker::NotOurs => {
                "pando did not create this worktree — confirming removes it anyway".to_string()
            }
            RemoveBlocker::Drops(what) => format!("{} with it", what.join(", ")),
            RemoveBlocker::Beside(what) => format!(
                "{} sits beside its repositories' checkouts — F removes it with the set",
                what.join(", ")
            ),
            RemoveBlocker::Foreign(what) => format!(
                "{} is a git checkout that is not one of the set's — pando never removes one; \
                 move it out of the set first",
                what.join(", ")
            ),
        }
    }

    /// A locked worktree can never be removed, nor a set holding a git
    /// checkout of its own; the others are confirmable.
    pub fn is_fatal(&self) -> bool {
        matches!(self, RemoveBlocker::Locked(_) | RemoveBlocker::Foreign(_))
    }

    /// Whether only a forced removal gets past it: git's refusal over
    /// changes, and what sits beside a set's checkouts.
    pub fn needs_force(&self) -> bool {
        matches!(self, RemoveBlocker::Dirty | RemoveBlocker::Beside(_))
    }
}

#[derive(Debug, Clone)]
pub enum BranchLoadState {
    Loading,
    Ready(Vec<BranchEntry>),
}

impl BranchLoadState {
    pub fn as_slice(&self) -> &[BranchEntry] {
        match self {
            BranchLoadState::Loading => &[],
            BranchLoadState::Ready(b) => b.as_slice(),
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, BranchLoadState::Loading)
    }
}

/// What `n` may fork a new branch from: `leading` first — the base it
/// forks from when none is chosen, and the repository's default — then
/// every branch the picker read, each once: for a multi-repo project,
/// every branch all its repositories have.
pub fn base_choices(leading: &[&str], branches: &[BranchEntry]) -> Vec<String> {
    let mut choices: Vec<String> = Vec::new();
    let names = leading.iter().map(|name| name.to_string()).chain(
        branches
            .iter()
            .filter(|entry| entry.in_every_repo)
            .map(|entry| match entry.source {
                crate::worktree::BranchSource::Local => entry.name.clone(),
                crate::worktree::BranchSource::Remote => format!("origin/{}", entry.name),
            }),
    );
    for name in names {
        if !choices.contains(&name) {
            choices.push(name);
        }
    }
    choices
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateRow {
    /// Fork a new branch of this name from the default base.
    NewBranch(String),
    /// Check out a branch that already exists.
    Existing(BranchEntry),
}

/// Rows of the create picker: the typed name as a new branch (unless it
/// exactly matches an existing one), then every branch that contains it.
pub fn create_rows(input: &str, branches: &[BranchEntry]) -> Vec<CreateRow> {
    let trimmed = input.trim();
    let lower = trimmed.to_lowercase();
    let exact_match = !trimmed.is_empty() && branches.iter().any(|b| b.name == trimmed);
    let mut rows = Vec::new();
    if !trimmed.is_empty() && !exact_match {
        rows.push(CreateRow::NewBranch(trimmed.to_string()));
    }
    for b in branches {
        if !lower.is_empty() && !b.name.to_lowercase().contains(&lower) {
            continue;
        }
        rows.push(CreateRow::Existing(b.clone()));
    }
    rows
}

/// Rows of the pull request picker: every open one, newest first as `gh`
/// lists them, that the typed text finds in its number, title, branch or
/// author.
pub fn pr_rows<'a>(prs: &'a [PrInfo], input: &str) -> Vec<&'a PrInfo> {
    let needle = input.trim().trim_start_matches('#').to_lowercase();
    prs.iter()
        .filter(|pr| pr.state == PrState::Open)
        .filter(|pr| {
            needle.is_empty()
                || pr.number.to_string().starts_with(&needle)
                || pr.title.to_lowercase().contains(&needle)
                || pr.branch.to_lowercase().contains(&needle)
                || pr.author.to_lowercase().contains(&needle)
                || pr
                    .repo
                    .as_deref()
                    .is_some_and(|repo| repo.to_lowercase().contains(&needle))
        })
        .collect()
}

impl App {
    pub(super) fn handle_pull_request_key(
        &mut self,
        key: KeyEvent,
        mut input: String,
        mut selected: usize,
    ) {
        let count = pr_rows(&self.pr_list, &input).len();
        // The list can shrink under the cursor when a fetch lands; the
        // paint clamps it, and enter must take the row the paint showed.
        selected = selected.min(count.saturating_sub(1));
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Down => selected = (selected + 1).min(count.saturating_sub(1)),
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Backspace => {
                input.pop();
                selected = 0;
            }
            KeyCode::Char(c) => {
                input.push(c);
                selected = 0;
            }
            KeyCode::Enter => {
                let chosen = pr_rows(&self.pr_list, &input)
                    .get(selected)
                    .map(|pr| (*pr).clone());
                let Some(pr) = chosen else {
                    if self.pr_fetching {
                        self.set_status("still asking GitHub…");
                    } else {
                        self.set_error("no open pull request to make a worktree for");
                    }
                    self.modal = Some(Modal::PullRequests { input, selected });
                    return;
                };
                let branch = pr.local_branch();
                if self.is_main_branch(&branch) {
                    self.set_error(format!(
                        "#{}'s branch {branch} is checked out in the main checkout",
                        pr.number
                    ));
                } else if let Some(existing) = self.worktree_for_branch(&branch) {
                    // The same as `n` on a branch that has one: enter goes
                    // to it.
                    self.filter.clear();
                    self.mode = super::Mode::Normal;
                    self.refilter_keeping(Some(existing), 0);
                    self.tail_index = 0;
                    self.tail_scroll = 0;
                    self.set_status(format!(
                        "#{} already has a worktree — selected it",
                        pr.number
                    ));
                    return;
                } else if self.spawn_create_pr(pr) {
                    return; // closes only once the work is under way
                }
            }
            _ => {}
        }
        self.modal = Some(Modal::PullRequests { input, selected });
    }

    /// Puts what a fetch brought in place of the pull request list. The
    /// picker's cursor is a row, and a pull request opened or closed since
    /// the last fetch moves the rows below it, so the cursor follows the
    /// pull request it was on; enter then takes the one that was chosen.
    pub(super) fn replace_pr_list(&mut self, prs: Vec<PrInfo>) {
        let before = std::mem::replace(&mut self.pr_list, prs);
        let Some(Modal::PullRequests { input, selected }) = self.modal.as_mut() else {
            return;
        };
        let rows = pr_rows(&before, input);
        // The paint clamps a cursor past the end, so that row is the one
        // it showed.
        let Some(number) = rows
            .get((*selected).min(rows.len().saturating_sub(1)))
            .map(|pr| pr.number)
        else {
            return;
        };
        if let Some(row) = pr_rows(&self.pr_list, input)
            .iter()
            .position(|pr| pr.number == number)
        {
            *selected = row;
        }
    }

    pub(super) fn open_pull_requests(&mut self) {
        self.modal = Some(Modal::PullRequests {
            input: String::new(),
            selected: 0,
        });
        // What the last fetch said shows at once; this brings it up to date.
        self.spawn_pr_fetch();
    }

    pub(super) fn handle_create_key(
        &mut self,
        key: KeyEvent,
        mut input: String,
        branches: BranchLoadState,
        mut selected: usize,
        mut base: Option<String>,
    ) {
        let rows = create_rows(&input, branches.as_slice());
        match key.code {
            KeyCode::Esc => return,
            // The base a new branch forks from, walked in place: the one
            // it forks from untouched first, then the repository's
            // default, then every branch the picker read. Whatever tab
            // lands on is chosen, the first included: the untouched base
            // follows the name as it is typed, and a choice stays put.
            KeyCode::Tab | KeyCode::BackTab => {
                let implied = self.implied_base(&input);
                let leading: Vec<&str> = implied
                    .iter()
                    .chain(&self.default_base)
                    .map(String::as_str)
                    .collect();
                let choices = base_choices(&leading, branches.as_slice());
                if !choices.is_empty() {
                    let current = base
                        .as_ref()
                        .or(implied.as_ref())
                        .and_then(|b| choices.iter().position(|c| c == b));
                    let forward = key.code == KeyCode::Tab;
                    // With no base yet — no default the repository could
                    // name — the first tab lands on the first choice, not
                    // the second.
                    let next = match current {
                        Some(at) => {
                            let step: isize = if forward { 1 } else { -1 };
                            (at as isize + step).rem_euclid(choices.len() as isize) as usize
                        }
                        None if forward => 0,
                        None => choices.len() - 1,
                    };
                    base = Some(choices[next].clone());
                }
            }
            KeyCode::Down => selected = (selected + 1).min(rows.len().saturating_sub(1)),
            KeyCode::Up => selected = selected.saturating_sub(1),
            KeyCode::Backspace => {
                input.pop();
                selected = 0;
            }
            KeyCode::Char(c) => {
                input.push(c);
                selected = 0;
            }
            KeyCode::Enter => {
                let Some(row) = rows.get(selected) else {
                    self.set_error("type a branch name first");
                    self.modal = Some(Modal::Create {
                        input,
                        branches,
                        selected,
                        base,
                    });
                    return;
                };
                let (branch, new_branch) = match row {
                    CreateRow::NewBranch(name) => (name.clone(), true),
                    CreateRow::Existing(entry) => (entry.name.clone(), false),
                };
                // Git keeps one checkout per branch, and the main checkout
                // has this one. Said here, not by a worker that fails.
                if self.is_main_branch(&branch) {
                    self.set_error(format!(
                        "{branch} is checked out in the main checkout — pick another branch, \
                         or type a new name to fork one from it"
                    ));
                    self.modal = Some(Modal::Create {
                        input,
                        branches,
                        selected,
                        base,
                    });
                    return;
                }
                // The picker already said this one has a worktree; enter
                // goes to it, which is what somebody typing its name wants.
                if let Some(existing) = self.worktree_for_branch(&branch) {
                    self.filter.clear();
                    self.mode = super::Mode::Normal;
                    self.refilter_keeping(Some(existing), 0);
                    self.tail_index = 0;
                    self.tail_scroll = 0;
                    self.set_status(format!("{branch} already has a worktree — selected it"));
                    return;
                }
                // The base only means something to a branch being made.
                let fork_from = if new_branch { base.clone() } else { None };
                if self.spawn_create(branch, fork_from) {
                    return; // modal closes only once the work is under way
                }
                self.modal = Some(Modal::Create {
                    input,
                    branches,
                    selected,
                    base,
                });
                return;
            }
            _ => {}
        }
        self.modal = Some(Modal::Create {
            input,
            branches,
            selected,
            base,
        });
    }

    /// The base a new branch called `branch` forks from when none is
    /// chosen: the config's, read as `new` reads it, else the
    /// repository's default. A multi-repo project's repositories each
    /// fork from their own, so it has one only when a `[branches]` rule
    /// names one for every repository.
    pub fn implied_base(&self, branch: &str) -> Option<String> {
        let branch = branch.trim();
        match self.paths.project.is_multi_repo() {
            true => self.config.rule_base(branch).map(str::to_string),
            false => self
                .config
                .base_for_branch(branch)
                .map(str::to_string)
                .or_else(|| self.default_base.clone()),
        }
    }

    /// Whether `branch` is the one the main checkout has — for a
    /// multi-repo project, the one any repository's main checkout has: git
    /// checks no branch out twice in one repository.
    pub fn is_main_branch(&self, branch: &str) -> bool {
        self.main.as_ref().is_some_and(|main| {
            main.checkouts()
                .iter()
                .any(|checkout| checkout.branch.as_deref() == Some(branch))
        })
    }

    /// Whether the worktree a dialog names was removed from under it — by
    /// `pando rm` in another pane, or by hand — said on the status line if
    /// so. There is nothing left to confirm, and a worker sent after it
    /// would only come back with git's complaint.
    pub(super) fn gone_from_under_dialog(&mut self, name: &str) -> bool {
        if self.worktrees.iter().any(|w| w.name == name) {
            return false;
        }
        let label = self.label_of(name);
        self.set_status(format!("{label} is already gone"));
        true
    }

    /// `y` removes what git would let go of; `F` removes it whatever it
    /// holds — `rm --force`. Anything else closes the dialog.
    pub(super) fn handle_remove_key(
        &mut self,
        key: KeyEvent,
        name: String,
        created_by_pando: bool,
    ) {
        let force = match key.code {
            KeyCode::Char('y') | KeyCode::Enter => false,
            KeyCode::Char('F') => true,
            _ => return,
        };
        if self.gone_from_under_dialog(&name) {
            return;
        }
        let blockers = self.remove_blockers(&name);
        if let Some(fatal) = blockers.iter().find(|b| b.is_fatal()) {
            let label = self.label_of(&name);
            self.set_error(format!("{label}: {}", fatal.line()));
            return;
        }
        // Known to be dirty, or holding files beside a set's checkouts:
        // the removal would refuse, so the dialog stays and says which key
        // does it.
        if !force && let Some(blocker) = blockers.iter().find(|b| b.needs_force()) {
            let label = self.label_of(&name);
            let what = match blocker {
                RemoveBlocker::Beside(what) => format!("holds {}", what.join(", ")),
                _ => "has uncommitted changes".to_string(),
            };
            self.set_error_about(
                &name,
                format!("{label} {what} — F removes it anyway, esc keeps it"),
            );
            self.modal = Some(Modal::Remove {
                name,
                created_by_pando,
            });
            return;
        }
        self.spawn_remove(name, !created_by_pando, force);
    }

    /// Everything the remove dialog states up front, most serious first:
    /// what refuses it, then the data that goes for good, then the rest.
    /// A dialog too short for all of them loses the last.
    pub fn remove_blockers(&self, name: &str) -> Vec<RemoveBlocker> {
        let mut out = Vec::new();
        let Some(wt) = self.worktrees.iter().find(|w| w.name == name) else {
            return out;
        };
        if wt.locked {
            out.push(RemoveBlocker::Locked(wt.lock_reason.clone()));
        }
        let foreign = actions::foreign_checkouts(wt);
        let beside: Vec<String> = actions::beside_checkouts(wt)
            .into_iter()
            .filter(|name| !foreign.contains(name))
            .collect();
        if !foreign.is_empty() {
            out.push(RemoveBlocker::Foreign(foreign));
        }
        if !beside.is_empty() {
            out.push(RemoveBlocker::Beside(beside));
        }
        match wt.dirty {
            Some(true) => out.push(RemoveBlocker::Dirty),
            Some(false) => {}
            None => out.push(RemoveBlocker::DirtyUnknown),
        }
        let drops = self
            .record_for(name)
            .map(actions::namespaces_rm_drops)
            .unwrap_or_default();
        if !drops.is_empty() {
            out.push(RemoveBlocker::Drops(drops));
        }
        if self.phase_of(name).is_some() {
            out.push(RemoveBlocker::Running);
        }
        if !self.created_by_pando.get(name).copied().unwrap_or(false) {
            out.push(RemoveBlocker::NotOurs);
        }
        out
    }

    pub(super) fn handle_question_key(
        &mut self,
        key: KeyEvent,
        question: actions::Question,
        mut selected: usize,
        mut custom: Option<String>,
        reply: Sender<Result<actions::Answer, String>>,
    ) {
        // Whatever happens, the worker gets an answer: it is blocked on this
        // channel, and a modal that closes without sending would leave it
        // there forever.
        //
        // A set question is its own little mode: space ticks the row under
        // the cursor, enter takes whatever is ticked, and there is no
        // command to type in place of "which of these containers".
        if question.multi {
            let mut checked = std::mem::take(&mut self.question_checked);
            match key.code {
                KeyCode::Esc => {
                    let _ = reply.send(Err("cancelled".to_string()));
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(question.options.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                KeyCode::Char(' ') | KeyCode::Char('x') => {
                    if checked.contains(&selected) {
                        checked.retain(|i| *i != selected);
                    } else {
                        checked.push(selected);
                        checked.sort_unstable();
                    }
                }
                KeyCode::Char('n') => checked.clear(),
                KeyCode::Enter => {
                    let _ = reply.send(Ok(if checked.is_empty() {
                        actions::Answer::None
                    } else {
                        actions::Answer::Many(checked)
                    }));
                    return;
                }
                _ => {}
            }
            self.question_checked = checked;
            self.modal = Some(Modal::Question {
                question,
                selected,
                custom,
                reply,
            });
            return;
        }
        if let Some(text) = custom.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    // Back to the list of options, unless there were none.
                    if question.options.is_empty() {
                        let _ = reply.send(Err("cancelled".to_string()));
                        return;
                    }
                    custom = None;
                }
                KeyCode::Enter => {
                    let answer = text.trim().to_string();
                    if answer.is_empty() {
                        self.set_error(format!(
                            "type the {}, or esc to go back",
                            question.slot.custom_noun()
                        ));
                    } else {
                        let _ = reply.send(Ok(actions::Answer::Custom(answer)));
                        return;
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Esc => {
                    let _ = reply.send(Err("cancelled".to_string()));
                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    selected = (selected + 1).min(question.options.len().saturating_sub(1));
                }
                KeyCode::Up | KeyCode::Char('k') => selected = selected.saturating_sub(1),
                // Only where a typed answer is one: a slot to free is
                // chosen from the list, never typed.
                KeyCode::Char('c') if question.allow_custom => custom = Some(String::new()),
                // The CLI prompt's `n`: a slot that may have no answer —
                // no schema hook, no port variable — records that, the
                // same `Answer::None` the prompt sends.
                KeyCode::Char('n') if question.allow_none => {
                    let _ = reply.send(Ok(actions::Answer::None));
                    return;
                }
                KeyCode::Enter => {
                    if question.options.is_empty() {
                        custom = Some(String::new());
                    } else {
                        let _ = reply.send(Ok(actions::Answer::Choice(selected)));
                        return;
                    }
                }
                _ => {}
            }
        }
        self.modal = Some(Modal::Question {
            question,
            selected,
            custom,
            reply,
        });
    }

    /// The worktree a branch already has, by its branch or by the name a
    /// new one would be given — `new` refuses both.
    pub fn worktree_for_branch(&self, branch: &str) -> Option<String> {
        let dir_name = actions::sanitize_branch_to_dir(branch);
        self.worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(branch) || w.name == dir_name)
            .map(|w| w.name.clone())
    }

    pub(super) fn open_create(&mut self) {
        self.modal = Some(Modal::Create {
            input: String::new(),
            branches: BranchLoadState::Loading,
            selected: 0,
            base: None,
        });
        self.spawn_branch_fetch();
    }

    pub(super) fn open_remove(&mut self) {
        let Some(wt) = self.selected_worktree() else {
            self.set_error("nothing selected");
            return;
        };
        let name = wt.name.clone();
        // No dialog: there is nothing to confirm.
        if self.is_main(&name) {
            let label = self.label_of(&name);
            self.set_error(format!(
                "{label} is the main checkout — pando runs it, but never removes it"
            ));
            return;
        }
        let created_by_pando = self.created_by_pando.get(&name).copied().unwrap_or(false);
        self.modal = Some(Modal::Remove {
            name: name.clone(),
            created_by_pando,
        });
        // What git said may be minutes old; the dialog is where it matters.
        // The answer repaints the dialog when it lands.
        self.refresh_git(Some(vec![name]));
    }
}
