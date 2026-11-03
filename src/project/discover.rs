//! The project a directory is in: a repository or a folder of them, and
//! the identity pando derives from it.

use anyhow::{Context, Result, bail};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

/// How long one git question may take before pando stops waiting for it.
///
/// git is the one external call every command makes before it reaches
/// dispatch — [`discover`] runs first — so a git that hangs (waiting on
/// a wedged process, or on a network mount that stopped
/// answering) is a pando that hangs, with no line on the screen saying
/// why. Generous, because a status on a very large tree is slow and still
/// an answer; bounded, because forever is not.
pub const GIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `git -C <dir> <args…>` to completion, output captured, bounded by
/// [`GIT_TIMEOUT`].
///
/// An [`std::io::ErrorKind::TimedOut`] error when git did not answer in
/// time, with everything it started killed; every other failure is the
/// same `io::Error` `Command::output` would have given.
pub fn git<I, S>(dir: &Path, args: I) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    git_within(dir, args, GIT_TIMEOUT)
}

/// [`git`] with the deadline supplied.
pub fn git_within<I, S>(dir: &Path, args: I, timeout: Duration) -> std::io::Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new("git");
    command.arg("-C").arg(dir).args(args);
    crate::platform::process::output_within(command, timeout)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectRef {
    /// `<display_name>-<8 hex chars of a hash of the canonical root path>`.
    pub id: String,
    /// Canonical path of the main checkout.
    pub root: PathBuf,
    /// The main checkout's directory name.
    pub display_name: String,
    /// The repositories of a multi-repo project, by their directory names
    /// under `root`, sorted. Empty for a project that is one repository,
    /// which `root` then is.
    pub repos: Vec<String>,
    /// The base config names for each of [`ProjectRef::repos`] that has
    /// one, for the reads that run without config — `ls`, the TUI's
    /// enrichment — to measure a checkout against what `new` forked it
    /// from. Empty for a project that is one repository.
    pub repo_bases: std::collections::BTreeMap<String, String>,
}

impl ProjectRef {
    /// Builds a reference from a main-checkout path. Public so tests and
    /// later phases (doctor adopting a moved project) can construct one
    /// without shelling out.
    pub fn from_root(root: impl AsRef<Path>) -> Result<Self> {
        let root = canonicalize(root.as_ref());
        let display_name = root
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "repo".to_string());
        let id = format!("{display_name}-{}", path_hash8(&root));
        Ok(Self {
            id,
            root,
            display_name,
            repos: Vec::new(),
            repo_bases: Default::default(),
        })
    }

    /// A multi-repo project: the plain folder `root`, made of the
    /// repositories under it named by `repos`. Its id is a single
    /// repository's would be, from the folder's path.
    pub fn multi_repo(root: impl AsRef<Path>, mut repos: Vec<String>) -> Result<Self> {
        repos.sort();
        repos.dedup();
        Ok(Self {
            repos,
            ..Self::from_root(root)?
        })
    }

    /// The repository `rel`, a path relative to a checkout of this project
    /// such as a process's `cwd`, is in: its first part, when that is one
    /// of [`ProjectRef::repos`]. `None` for a project that is one
    /// repository, and for a path in none of them.
    pub fn repo_of(&self, rel: &str) -> Option<&str> {
        let first = rel
            .split('/')
            .find(|part| !part.is_empty() && *part != ".")?;
        self.repos
            .iter()
            .find(|repo| repo.as_str() == first)
            .map(String::as_str)
    }

    /// A repository a message can name as an example: the first. Empty
    /// for a project that is one repository, which has none to name.
    pub fn example_repo(&self) -> &str {
        self.repos.first().map_or("", String::as_str)
    }

    /// Whether this project is several repositories.
    pub fn is_multi_repo(&self) -> bool {
        !self.repos.is_empty()
    }

    /// The git checkouts in `dir`, a checkout of this project — the main
    /// one or a worktree: `dir` itself for one repository, and each
    /// repository's directory under it for several, whether or not it is
    /// there.
    pub fn checkouts_in(&self, dir: &Path) -> Vec<PathBuf> {
        self.repo_checkouts_in(dir)
            .into_iter()
            .map(|(_, checkout)| checkout)
            .collect()
    }

    /// [`ProjectRef::checkouts_in`], each beside the repository it is of:
    /// none for a project that is one repository.
    pub fn repo_checkouts_in(&self, dir: &Path) -> Vec<(Option<&str>, PathBuf)> {
        match self.is_multi_repo() {
            false => vec![(None, dir.to_path_buf())],
            true => self
                .repos
                .iter()
                .map(|repo| (Some(repo.as_str()), dir.join(repo)))
                .collect(),
        }
    }

    /// The checkout of `repo` in `dir`, a checkout of this project: `dir`
    /// itself when `repo` is none.
    pub fn checkout_of(dir: &Path, repo: Option<&str>) -> PathBuf {
        match repo {
            Some(repo) => dir.join(repo),
            None => dir.to_path_buf(),
        }
    }

    /// Where `rel`, a path relative to a checkout of this project, is asked
    /// about in git: the checkout and `rel` as they are for one repository,
    /// and for several the repository's checkout under `dir` and the rest
    /// of the path. `None` when `rel` is in none of the repositories — at
    /// the set's own root, or the repository's directory itself.
    pub fn repo_path(&self, dir: &Path, rel: &str) -> Option<(PathBuf, String)> {
        if !self.is_multi_repo() {
            return Some((dir.to_path_buf(), rel.to_string()));
        }
        let (repo, rest) = rel.trim_start_matches("./").split_once('/')?;
        if rest.trim_matches('/').is_empty() || !self.repos.iter().any(|r| r == repo) {
            return None;
        }
        Some((dir.join(repo), rest.to_string()))
    }
}

/// The multi-repo project `project`, a single repository, is one
/// repository of: the folder it sits in, when pando knows that folder as a
/// project of its own. A folder pando has never run is not said to be
/// one — `~/code` holds every repository a developer has, and naming them
/// all beside one would be noise, and a list nobody asked to publish.
/// `None` for a multi-repo project itself.
pub fn folder_of(project: &ProjectRef, home: &Path) -> Option<ProjectRef> {
    if project.is_multi_repo() {
        return None;
    }
    let folder = known_multi_repo(project.root.parent()?, home)?;
    let name = project.root.file_name()?.to_str()?;
    folder
        .repos
        .iter()
        .any(|repo| repo == name)
        .then_some(folder)
}

/// The repositories directly under `dir`: its subdirectories whose `.git`
/// is a directory, by name, sorted. A `.git` file is a linked worktree or
/// a submodule, which is never a repository of a project of its own; a
/// hidden directory and a symlink are not looked into.
pub fn repos_in(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut repos: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.'))
        .filter(|name| dir.join(name).join(".git").is_dir())
        .collect();
    repos.sort();
    repos
}

/// The directory under pando's home that holds a directory per project,
/// named by its id.
pub const PROJECTS_DIR: &str = "projects";

/// How many repositories a folder pando has never seen needs before
/// `pando` there makes it a project: one repository in a folder is that
/// repository's project, and `pando` inside it says so.
const FIRST_SIGHT_REPOS: usize = 2;

/// The project containing `cwd`, or an error naming why not.
///
/// A repository, as git finds it from `cwd`; or a multi-repo project: from
/// its folder, from inside one of its sets, or from inside one of a set's
/// checkouts. `home` is pando's: a folder or a set is known by the project
/// directory there, which `new` made, so that a folder of repositories
/// that only one of them is used from never becomes a project of its own,
/// and a single repository's worktree that happens to share the
/// repository's name is never taken for a set.
///
/// From a repository's main checkout it is that repository, even inside a
/// folder pando knows as a multi-repo project: a developer who used pando
/// there before keeps what they had.
pub fn discover(cwd: &Path, home: &Path) -> Result<ProjectRef> {
    let out = git(cwd, ["worktree", "list", "--porcelain"]);
    let out = match out {
        Ok(o) if o.status.success() => o,
        Ok(_) => match multi_repo_above(cwd, home) {
            Some(project) => return Ok(project),
            None => bail!(
                "not inside a git repository, nor a folder of them: {}",
                cwd.display()
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => bail!(
            "`git worktree list` did not answer in {}s in {} — a hung git process or an \
             unresponsive disk is the usual cause; run it yourself to see what git is waiting on",
            GIT_TIMEOUT.as_secs(),
            cwd.display()
        ),
        Err(e) => bail!("could not run git ({e}) — is git installed?"),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let (root, bare) = first_entry(&text)
        .with_context(|| format!("git listed no worktrees for {}", cwd.display()))?;
    if bare {
        bail!(
            "bare repositories are not supported: {} has no working tree to manage",
            root.display()
        );
    }
    if let Some(project) = set_of_checkout(cwd, &root, home) {
        return Ok(project);
    }
    ProjectRef::from_root(&root)
}

/// The multi-repo project whose set holds the checkout `cwd` is in, when
/// it is in one: a linked worktree of `main` under `main`'s own name, in a
/// directory beside which no repository is, of a folder pando knows.
fn set_of_checkout(cwd: &Path, main: &Path, home: &Path) -> Option<ProjectRef> {
    let cwd = canonicalize(cwd);
    let checkout = cwd.ancestors().find(|dir| dir.join(".git").exists())?;
    if !checkout.join(".git").is_file() {
        return None;
    }
    let set = checkout.parent()?;
    let root = set_root(set, home)?;
    (canonicalize(main).parent() == Some(root.as_path())).then_some(())?;
    known_multi_repo(&root, home)
}

/// The multi-repo project `cwd` is in when git finds no repository there:
/// the first of `cwd` and its parents that is a set of a known folder, or
/// a folder of repositories — any that pando knows, and `cwd` itself when
/// it holds [`FIRST_SIGHT_REPOS`] or more.
fn multi_repo_above(cwd: &Path, home: &Path) -> Option<ProjectRef> {
    let cwd = canonicalize(cwd);
    for dir in cwd.ancestors() {
        if let Some(root) = set_root(dir, home) {
            return known_multi_repo(&root, home);
        }
        if let Some(project) = known_multi_repo(dir, home) {
            return Some(project);
        }
        if dir == cwd {
            let repos = repos_in(dir);
            if repos.len() >= FIRST_SIGHT_REPOS {
                return ProjectRef::multi_repo(dir, repos).ok();
            }
        }
    }
    None
}

/// `dir` as a project pando already knows as several repositories: its
/// project directory is in `home`, and repositories are under it.
fn known_multi_repo(dir: &Path, home: &Path) -> Option<ProjectRef> {
    if dir.join(".git").exists() {
        return None;
    }
    let repos = repos_in(dir);
    if repos.is_empty() {
        return None;
    }
    let project = ProjectRef::multi_repo(dir, repos).ok()?;
    home.join(PROJECTS_DIR)
        .join(&project.id)
        .is_dir()
        .then_some(project)
}

/// The folder `dir` is a set of, when it is one of a folder pando knows.
fn set_root(dir: &Path, home: &Path) -> Option<PathBuf> {
    let root = set_folder(dir)?;
    known_multi_repo(&root, home).map(|_| root)
}

/// The folder `dir` looks like a set of: no checkout itself, and every
/// checkout directly in it a linked worktree of the repository of its own
/// name, all of them in one folder.
pub fn set_folder(dir: &Path) -> Option<PathBuf> {
    if dir.join(".git").exists() {
        return None;
    }
    let mut root: Option<PathBuf> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let checkout = entry.path();
        let dotgit = checkout.join(".git");
        if !dotgit.exists() {
            continue;
        }
        // A repository of its own in it: a folder of repositories, not a
        // set.
        if dotgit.is_dir() {
            return None;
        }
        // `<repository>/.git/worktrees/<name>`, three levels below the
        // repository's own directory.
        let main = linked_gitdir(&checkout)?
            .parent()?
            .parent()?
            .parent()?
            .to_path_buf();
        if main.file_name() != checkout.file_name() {
            return None;
        }
        let folder = canonicalize(main.parent()?);
        match &root {
            Some(seen) if *seen != folder => return None,
            Some(_) => {}
            None => root = Some(folder),
        }
    }
    root
}

/// The git directory a linked checkout's `.git` file names —
/// `<repository>/.git/worktrees/<name>` — as an absolute path. `None` when
/// its `.git` is not such a file.
///
/// Git writes it absolute, or relative to the checkout when
/// `worktree.useRelativePaths` is set (git 2.48 and later), and resolves a
/// relative one from the checkout's own directory, so that is where it is
/// resolved here. The `..` in it are folded by hand, because what a caller
/// wants to know is often that the directory is no longer there.
pub fn linked_gitdir(checkout: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(checkout.join(".git")).ok()?;
    let named = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    if named.is_empty() {
        return None;
    }
    let named = Path::new(named);
    if named.is_absolute() {
        return Some(named.to_path_buf());
    }
    let mut out = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
    for part in named.components() {
        match part {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            part => out.push(part),
        }
    }
    Some(out)
}

/// Path and bare-ness of the porcelain output's first entry — the main
/// checkout. Entries are separated by a blank line; unknown lines are
/// ignored, as new git versions add them.
pub(super) fn first_entry(porcelain: &str) -> Option<(PathBuf, bool)> {
    let mut path: Option<PathBuf> = None;
    let mut bare = false;
    for line in porcelain.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("worktree ") {
            if path.is_some() {
                break;
            }
            path = Some(PathBuf::from(rest));
        } else if line == "bare" {
            bare = true;
        }
    }
    path.map(|p| (p, bare))
}

/// macOS prints `/var/...` from the shell and `/private/var/...` from git, so
/// every path is canonicalised before it is hashed or compared. Falls back to
/// the input when the path does not exist, so error messages still name it.
pub(crate) fn canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// First eight hex characters of md5 over the canonical root path. Not
/// security — just a short, stable discriminator so two checkouts of the same
/// repository name do not share a project directory.
pub(super) fn path_hash8(root: &Path) -> String {
    let digest = md5::compute(crate::platform::files::path_bytes(root));
    format!("{digest:x}")[..8].to_string()
}
