//! The orchestration layer. Everything user-facing — the CLI and the TUI —
//! calls into here; those two stay thin wrappers.
//!
//! Invariant 1 is enforced at this level: the only things written inside a
//! worktree are paths the project's own gitignore already ignores, checked
//! with `git check-ignore` before anything is created.
//!
//! One file per concern: worktrees, a worktree's copy-on-write checkout,
//! hooks, questions, `init`, the runtime
//! check, start/stop/restart, when a start is ready, private services,
//! namespaced starts, share, reading state, an app on a device's links
//! and opening it there, the name its config written as code gives it,
//! the builds installed on the booted simulators, `pando check`,
//! trying pando's own guess for the setup screen, and the git menu's
//! moves, which callers name as `actions::git::…` because `read` and
//! `run` alone would say nothing.

mod app_config;
mod check;
mod checkout;
mod device;
pub mod git;
mod hooks;
mod init;
mod installed;
mod lifecycle;
mod namespaced;
mod opening;
mod questions;
mod readiness;
mod refresh;
mod runtime;
mod services;
mod share;
mod trying;
mod worktree;

pub use check::{
    CHECK_RAN_BY_ENV, Checked, LeftoverCheck, Narration, catch_check_interrupts, check, check_at,
    leftover_check, ran_by,
};
pub use device::{
    NativeChanges, ReadManifest, app_links, app_links_installed, app_links_with, native_changes,
    read_manifest,
};
pub use hooks::{
    CHECK_ENV, HookContext, INSTALL_HOOK, hook_scope, install_remedy, matched_nothing, run_hooks,
    runs_again,
};
pub use init::{
    ALL_SLOTS, InitReport, SlotSummary, init, init_dry_run, machine_evidence,
    machine_evidence_from, machine_evidence_script, runs_nothing, slot_value,
};
pub use installed::{InstalledBuild, Simulators, installed_builds, openable_apps};
pub use lifecycle::{
    Mode, StartReport, StartedProcess, StopAllReport, StopOutcome, process_names,
    refuse_only_on_a_mode_change, restart, start, stop, stop_all, stop_all_listed, stop_all_with,
};
pub use namespaced::{
    Leftover, login_question, namespace_leftovers, namespace_lines, namespace_login,
    namespaced_not_own_data, namespaces_rm_drops,
};
pub use opening::{
    BOOT_WAIT, NotOpened, Opener, RETRY_WAIT, Ran, RunCommand, open_app, open_commands, run_command,
};
pub use questions::{
    Answer, Answering, Ask, NEW_SLOTS, NeedsAnswer, Question, RefusedAnswer, START_SLOTS,
    Volunteered, question_for, recommended, resolve, resolve_for_new, resolve_for_start,
    resolve_on, resolve_process, resolve_silencing, settled,
};
pub use readiness::{
    NO_PORT_WATCH, NO_PORT_WATCH_MAX, ReadyVerdict, no_port_watch, ready_limit, ready_line,
    ready_verdict, still_watched, watched_processes,
};
pub use refresh::{FAILURE_SHOWN_LINES, Refreshed, failure_tail, inspect, refresh};
pub use runtime::{
    MACHINE_WIDE, Machine, Offer, prelude_needed, prelude_offers, runs_through_runner,
    runtime_shell, user_home, with_prelude,
};
pub use services::{
    ServiceStatus, env_dirs, export_lines, recorded_service_statuses, resolved_env, service_roles,
    service_statuses, shared_service_status, shared_service_statuses, url_owner_not_running,
    worktree_url,
};
pub use share::{ENV_SHARE_PORT, ShareOutcome, SpawnProxy, share, share_with, unshare};
pub use trying::{OwnGuess, try_on_its_own, try_on_its_own_on};
pub use worktree::{
    CREATED_BUT_INSTALL_FAILED, KEPT_OVER_RACED_RECORD, MAIN_RUNS_ONLY, Ownership, Unprovisioned,
    created_by_pando, guard_write_locations, ls, ls_all, new, new_for_pr, ownership, path, path_in,
    rm, sanitize_branch_to_dir, unprovisioned,
};

#[cfg(test)]
mod tests;
