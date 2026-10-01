//! pando: one repo, every branch alive.
//!
//! The dependency direction is inner to outer, with no upward imports:
//!
//! ```text
//! catalog · paths · term · remedy · theme · art · wsl → compose → project · config · ports · process · runtime · state
//!       · env_command · template · recipes
//!       → detect · hooks · worktree · cache · log_tail · observe · services
//!         · native · namespace · decisions · setup
//!       → tunnel · share_proxy
//!       → actions
//!       → doctor
//!       → cli · tui
//! ```
//!
//! A module that grew past one concern is a directory: its `mod.rs` holds
//! the module doc and re-exports every public item, so a caller always
//! writes `crate::actions::start`, never the file it lives in, and each
//! file below it is one concern with its own `//!` line. Tests sit in a
//! `tests.rs` beside them.
//!
//! # Where to add something
//!
//! | To add | Edit |
//! |---|---|
//! | a package manager or lockfile | a row in [`catalog::package_managers`] |
//! | a framework | a row in [`catalog::frameworks::RULES`] |
//! | somewhere a device app is opened (a simulator, an emulator) | a row in [`catalog::devices::TARGETS`], its open command in the framework's `Device` row |
//! | a service image a compose file uses | a row in [`catalog::images::IMAGES`] |
//! | a tool cache or artifact `provision` must never offer | a row in [`catalog::artifacts::ARTIFACTS`] |
//! | a job queue whose worker `doctor` recognises | a row in [`catalog::queue_workers::QUEUE_WORKERS`] |
//! | a program pando runs itself, and how to get it | a row in [`catalog::tools::TOOLS`] |
//! | a language or version manager | `runtime/languages.rs` |
//! | a native service (postgres, redis…) | a TOML file in `recipes/builtin/`, and a row in [`recipes::BUILT_IN`] |
//! | a CLI verb | `cli/mod.rs` (`Command`, `dispatch`), its output in a file of its own under `cli/`, the behaviour in `actions/`, and the verb list in `CLAUDE.md`, which a test holds to clap |
//! | a question pando asks | [`detect::Slot`], its proposal in `detect/`, its config edit in `detect/apply.rs`, and [`actions::ALL_SLOTS`]; the slot names are a published contract with a test that pins them |
//! | a `doctor` section | [`doctor::Section`], its report type in `doctor/report.rs`, a file under `doctor/`, its renderer in `doctor/render.rs`, and `agent/json.md`, which a test holds to the enum |
//! | a way out of an error that a CLI flag would name | a row in [`remedy`]: errors say what to do, the CLI rewrites it to the flag |
//! | a colour theme | a TOML file in `theme/builtin/` and a row in [`theme::BUILT_IN`]; a colour for a new meaning is a role in `theme/palette.rs` and a row in the `theme` module's table |
//! | a TUI key or panel | `tui/app/` for state and keys, with the key's row in `tui/app/keymap.rs` (help prints it, a test holds it to the handler); `tui/render/` for drawing |
//!
//! Tables in `catalog` are data: a fact lives in one row, and every module
//! that needs it reads the row. Where two lists must differ in order, both
//! exist and a test holds them to one set.

pub mod actions;
pub mod art;
pub mod cache;
pub mod catalog;
pub mod cli;
pub mod compose;
pub mod config;
pub mod decisions;
pub mod detect;
pub mod doctor;
pub mod env_command;
pub mod hooks;
pub mod log_tail;
pub mod namespace;
pub mod native;
pub mod observe;
pub mod paths;
pub mod ports;
pub mod process;
pub mod project;
pub mod recipes;
pub mod remedy;
pub mod runtime;
pub mod services;
pub mod setup;
pub mod share_proxy;
pub mod state;
pub mod template;
pub mod term;
#[cfg(test)]
pub(crate) mod testutil;
pub mod theme;
pub mod tui;
pub mod tunnel;
pub mod worktree;
pub mod wsl;
