//! Last-known enrichment and PR state, so the TUI paints a populated list on
//! its first frame instead of an empty one. Lives under pando's home, never
//! in the repository.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::worktree::{EnrichUpdate, Worktree};

const CURRENT_VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CachedEnrich {
    pub branch: Option<String>,
    pub prunable: bool,
    pub head_sha: Option<String>,
    pub head_subject: Option<String>,
    pub head_age: Option<String>,
    // A live `git status` is the only thing that can refresh these, so a
    // stale value beats a blank column for the first seconds after launch.
    #[serde(default)]
    pub dirty: Option<bool>,
    #[serde(default)]
    pub ahead_behind: Option<(u32, u32)>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CacheFile {
    pub version: u32,
    pub entries: BTreeMap<String, CachedEnrich>,
}

impl CacheFile {
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION,
            entries: BTreeMap::new(),
        }
    }
}

impl Default for CacheFile {
    fn default() -> Self {
        Self::new()
    }
}

/// Last-known branch to PR map, persisted on every fetch so a relaunch
/// paints chips on the first frame instead of waiting for the live `gh`
/// answer (which still overwrites this as soon as it lands).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PrCacheFile {
    pub version: u32,
    pub prs: BTreeMap<String, crate::worktree::PrInfo>,
}

impl PrCacheFile {
    pub fn new() -> Self {
        Self {
            version: CURRENT_VERSION,
            prs: BTreeMap::new(),
        }
    }
}

impl Default for PrCacheFile {
    fn default() -> Self {
        Self::new()
    }
}

pub fn load_prs(path: &Path) -> PrCacheFile {
    let Ok(s) = std::fs::read_to_string(path) else {
        return PrCacheFile::new();
    };
    match serde_json::from_str::<PrCacheFile>(&s) {
        Ok(c) if c.version == CURRENT_VERSION => c,
        _ => PrCacheFile::new(),
    }
}

pub fn save_prs(path: &Path, cache: &PrCacheFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string(cache).context("serialize pr cache")?;
    std::fs::write(&tmp, json).with_context(|| format!("write tmp pr cache {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename tmp → {}", path.display()))?;
    Ok(())
}

/// Loads the cache file. Returns an empty cache on miss, IO error, or
/// version mismatch — never propagates errors so callers can degrade
/// silently to live enrichment.
pub fn load(path: &Path) -> CacheFile {
    let Ok(s) = std::fs::read_to_string(path) else {
        return CacheFile::new();
    };
    match serde_json::from_str::<CacheFile>(&s) {
        Ok(c) if c.version == CURRENT_VERSION => c,
        _ => CacheFile::new(),
    }
}

pub fn save(path: &Path, cache: &CacheFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(cache).context("serialize cache")?;
    std::fs::write(&tmp, json).with_context(|| format!("write tmp cache {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("rename tmp → {}", path.display()))?;
    Ok(())
}

pub fn from_update(u: &EnrichUpdate) -> CachedEnrich {
    CachedEnrich {
        branch: u.branch.clone(),
        prunable: u.prunable,
        head_sha: u.head_sha.clone(),
        head_subject: u.head_subject.clone(),
        head_age: u.head_age.clone(),
        dirty: u.dirty,
        ahead_behind: u.ahead_behind,
    }
}

pub fn apply_to_worktree(wt: &mut Worktree, c: &CachedEnrich) {
    wt.branch = c.branch.clone();
    wt.prunable = c.prunable;
    wt.head_sha = c.head_sha.clone();
    wt.head_subject = c.head_subject.clone();
    wt.head_age = c.head_age.clone();
    wt.dirty = c.dirty;
    wt.ahead_behind = c.ahead_behind;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut c = CacheFile::new();
        c.entries.insert(
            "feat+foo".into(),
            CachedEnrich {
                branch: Some("feat/foo".into()),
                prunable: false,
                head_sha: Some("abc1234".into()),
                head_subject: Some("do the thing".into()),
                head_age: Some("2 hours ago".into()),
                dirty: Some(true),
                ahead_behind: Some((3, 141)),
            },
        );
        save(&path, &c).unwrap();
        let back = load(&path);
        assert_eq!(back, c);
    }

    fn sample_pr(number: u32, branch: &str) -> crate::worktree::PrInfo {
        crate::worktree::PrInfo {
            number,
            title: format!("PR {number}"),
            branch: branch.into(),
            author: "dev".into(),
            draft: false,
            state: crate::worktree::PrState::Merged,
            url: format!("https://github.com/org/repo/pull/{number}"),
            cross_repository: false,
            base: "main".into(),
        }
    }

    #[test]
    fn pr_cache_round_trips_through_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache").join("prs.json");
        let mut c = PrCacheFile::new();
        c.prs
            .insert("feat/magic".into(), sample_pr(463, "feat/magic"));
        save_prs(&path, &c).unwrap();
        assert_eq!(load_prs(&path), c);
    }

    #[test]
    fn pr_cache_version_mismatch_or_miss_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("prs.json");
        assert!(load_prs(&path).prs.is_empty(), "missing file → empty");
        std::fs::write(&path, r#"{"version": 99, "prs": {}}"#).unwrap();
        assert!(load_prs(&path).prs.is_empty(), "future version → empty");
    }

    #[test]
    // A cache file written before dirty/ahead_behind existed must still
    // hydrate; the new fields just default to unknown.
    fn cache_without_dirty_fields_still_loads() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let old = r#"{"version":1,"entries":{"feat+foo":{
            "branch":"feat/foo","prunable":false,"head_sha":"abc1234",
            "head_subject":"do the thing","head_age":"2 hours ago"}}}"#;
        std::fs::write(&path, old).unwrap();
        let back = load(&path);
        let entry = back.entries.get("feat+foo").expect("entry survives");
        assert_eq!(entry.head_sha.as_deref(), Some("abc1234"));
        assert_eq!(entry.dirty, None);
        assert_eq!(entry.ahead_behind, None);
    }

    #[test]
    fn version_mismatch_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let bogus = r#"{"version": 99, "entries": {}}"#;
        std::fs::write(&path, bogus).unwrap();
        let back = load(&path);
        assert_eq!(back, CacheFile::new());
        assert!(back.entries.is_empty());
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let back = load(&path);
        assert!(back.entries.is_empty());
        assert_eq!(back.version, CURRENT_VERSION);
    }

    #[test]
    fn malformed_json_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "{not json").unwrap();
        let back = load(&path);
        assert!(back.entries.is_empty());
    }

    #[test]
    fn save_overwrites_without_leaking_tmp() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut a = CacheFile::new();
        a.entries.insert("one".into(), CachedEnrich::default());
        save(&path, &a).unwrap();

        let mut b = CacheFile::new();
        b.entries.insert("two".into(), CachedEnrich::default());
        save(&path, &b).unwrap();

        assert_eq!(load(&path), b);
        assert!(
            !path.with_extension("json.tmp").exists(),
            "tmp file must not leak after rename"
        );
    }
}
