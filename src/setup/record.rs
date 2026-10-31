//! The two files pando keeps about a project's setup: the last check's
//! record (`check.json`) and what the setup screen remembers
//! (`setup.json`).
//!
//! Both are read tolerantly: a file that is missing, unreadable or of a
//! shape this pando does not know reads as "none", never as an error, so a
//! damaged file costs a retest and nothing else. Both are written the way
//! `state.json` is, to a temp file renamed over the old one, so a reader
//! on the TUI's tick never sees half a record.

use crate::paths::PandoPaths;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The last `pando check`, or the one running now.
///
/// pando's own record, not a published shape: `check --json` is the
/// contract, and this file is free to change with it. Every field a later
/// version might add is defaulted, so an older record still reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckRecord {
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    /// The pando that ran it. Shown ("tested with pando 0.5.0"), never
    /// compared: an upgrade does not invalidate a test.
    pub pando_version: String,
    /// The run settings' fingerprint after the check's own resolve pass.
    pub fingerprint_before: String,
    /// The same, taken again at the end. A check that finished with a
    /// different one tested settings that changed under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_after: Option<String>,
    /// The commit the throwaway worktree was made at, once it is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The ref that commit was taken from; none when it fell back to HEAD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_ref: Option<String>,
    /// The base the project's settings named when the check ran:
    /// `Some(None)` when they named none, and `new` forked from
    /// origin/HEAD. Its result speaks for the settings only while they
    /// name the same one. `None` in a record from before pando kept it,
    /// which is compared with nothing.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub settings_base: Option<Option<String>>,
    pub outcome: CheckOutcome,
    /// One entry per process the check started, in start order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<ProcessResult>,
    /// The process, or the hook (`install`), whose failure ended the
    /// check, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_process: Option<String>,
    /// The failed process's last lines, already redacted by whoever wrote
    /// the record: this file is read into the agent's job.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_tail: Vec<String>,
    /// What a running check has done so far, one line per step ("made a
    /// test worktree", "installing", "starting web, api…"), for the screen
    /// that watches it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<String>,
    pub ran_by: RanBy,
    /// Where the check ran the project's data: on the shared services,
    /// with the hooks after them left out, or in namespaces of its own,
    /// where they ran. A record from before there was a choice was shared.
    #[serde(default)]
    pub mode: CheckMode,
    /// Anything the check wants said beside its result, such as the hooks
    /// it skipped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// What the check's worktree cost on disk, measured once it passed and
    /// before it was taken down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskCost>,
}

/// What one worktree costs on disk beyond its tracked files, whatever the
/// project's tools are: the gitignored trees its install and processes
/// left in it, and what they wrote outside it. A check never clones, so
/// every tree is the one a worktree builds from nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskCost {
    /// The gitignored directories it ended with, biggest first, the small
    /// ones left out.
    #[serde(default)]
    pub trees: Vec<DiskTree>,
    /// The free space the disk lost while the check ran beyond what the
    /// trees hold: what the tools wrote outside the worktree, such as a
    /// build cache, give or take whatever else the machine wrote then.
    /// `None` where the OS was not asked.
    #[serde(default)]
    pub outside_bytes: Option<u64>,
}

/// One gitignored directory of the check's worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskTree {
    /// Relative to the worktree, as `clone` names a path.
    pub path: String,
    /// Its blocks on disk.
    pub bytes: u64,
    /// The main checkout has a directory there too, which `clone` could
    /// share with every new worktree.
    pub main_has: bool,
    /// `clone` already names it.
    pub cloned: bool,
}

/// How a check ended, or that it has not yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CheckOutcome {
    Running,
    Passed,
    Failed {
        kind: FailureKind,
        reason: String,
    },
    /// A run question was still open (exit 3); `slot` names it.
    NotSetUp {
        slot: String,
    },
    /// Stopped before it finished: a signal, or a `stop` of the check's
    /// own worktree.
    Interrupted,
}

/// Where a check ran the project's data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckMode {
    /// On the main checkout's own services, as a plain start runs: the
    /// hooks after the services are not run, because they would run
    /// against the developer's own data.
    #[default]
    Shared,
    /// In a database, and a Redis slot, of the check's own in the main
    /// checkout's servers, as `start --namespaced` runs: the hooks after
    /// the services run there, and the namespaces are dropped with the
    /// check's worktree.
    Namespaced,
}

/// Whose a failure is to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FailureKind {
    /// pando's settings for the project are wrong: the agent fixes them.
    Settings,
    /// The machine is not ready — a server not running, Docker stopped, a
    /// runtime missing: the developer's, and no setting changes it.
    Machine,
    /// The commit tested lacks a file the step needed, which the main
    /// checkout's branch has: the base is the wrong one, and loosening a
    /// setting to get past it would test the wrong commit. The developer's
    /// to choose, with `check --base` or the `base` answer.
    Base,
}

/// One process of a check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessResult {
    pub name: String,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// What the HTTP probe got, for the one process it asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_status: Option<u16>,
    /// How long it took to be ready, or to fail.
    #[serde(default)]
    pub secs: f64,
}

/// Who ran a check, so the screen can say "your agent is probably on it"
/// only when a program did, and only of a failure that is the settings'.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RanBy {
    Tui,
    Terminal,
    /// stderr was not a terminal.
    Program,
}

impl CheckRecord {
    /// A check that has just begun, by this pando, with its first
    /// fingerprint.
    pub fn begin(fingerprint: String, ran_by: RanBy) -> Self {
        Self {
            started_at: Utc::now(),
            finished_at: None,
            pando_version: env!("CARGO_PKG_VERSION").to_string(),
            fingerprint_before: fingerprint,
            fingerprint_after: None,
            commit: None,
            base_ref: None,
            settings_base: None,
            outcome: CheckOutcome::Running,
            processes: Vec::new(),
            failed_process: None,
            failed_tail: Vec::new(),
            progress: Vec::new(),
            ran_by,
            mode: CheckMode::Shared,
            notes: Vec::new(),
            disk: None,
        }
    }

    /// The fingerprint the result stands for: the one taken last.
    pub fn fingerprint(&self) -> &str {
        self.fingerprint_after
            .as_deref()
            .unwrap_or(&self.fingerprint_before)
    }

    /// Whether the settings changed while the check ran, so its result
    /// speaks for neither the settings before nor after.
    pub fn changed_while_running(&self) -> bool {
        self.fingerprint_after
            .as_deref()
            .is_some_and(|after| after != self.fingerprint_before)
    }

    /// The project's record, or none when there is no readable one.
    pub fn load(paths: &PandoPaths) -> Option<Self> {
        read_json(&paths.check_file())
    }

    pub fn save(&self, paths: &PandoPaths) -> Result<()> {
        write_json(&paths.check_file(), self)
    }
}

/// What the setup screen and the CLI's tip remember about a project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SetupMemory {
    /// `esc` on the setup screen: never shown it again, only a hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_at: Option<DateTime<Utc>>,
    /// "Let pando try on its own", so the ready view can say whose guess
    /// the settings were.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tried_by_pando_at: Option<DateTime<Utc>>,
    /// The CLI's first-time tip, which prints once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tip_shown_at: Option<DateTime<Utc>>,
}

impl SetupMemory {
    /// The project's memory; nothing remembered when there is no readable
    /// file.
    pub fn load(paths: &PandoPaths) -> Self {
        read_json(&paths.setup_file()).unwrap_or_default()
    }

    pub fn save(&self, paths: &PandoPaths) -> Result<()> {
        write_json(&paths.setup_file(), self)
    }
}

/// A field that is there, `null` included, as `Some`: serde reads a
/// `null` as a missing one otherwise, and `Some(None)` is an answer.
fn present<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(deserializer).map(Some)
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(value)
        .with_context(|| format!("serialize {}", path.display()))?;
    std::fs::write(&tmp, json).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename tmp → {}", path.display()))?;
    Ok(())
}
