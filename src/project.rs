//! The repository pando is looking at, and the identity it derives from it.
//!
//! Discovery is from `git worktree list --porcelain`, whose first entry is
//! always the main checkout — from any cwd, including inside a linked
//! worktree. `git rev-parse --git-common-dir` is deliberately not used: from
//! a subdirectory it prints a relative path such as `../.git`.
//!
//! A project may also be several repositories: a plain folder whose
//! immediate subdirectories are git repositories (`product/backend`,
//! `product/frontend`) is one project, and a worktree of it is a *set*, a
//! directory holding a linked worktree of every repository under the same
//! name. [`ProjectRef::repos`] says which it is.

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

/// The folder of repositories `project`, a single repository, sits in
/// beside others: the multi-repo project pando would run from that folder.
/// `None` for a repository with no sibling repository, for one inside a
/// repository, and for a multi-repo project itself.
pub fn folder_of(project: &ProjectRef) -> Option<ProjectRef> {
    if project.is_multi_repo() {
        return None;
    }
    let folder = project.root.parent()?;
    if folder.join(".git").exists() {
        return None;
    }
    let repos = repos_in(folder);
    let name = project.root.file_name()?.to_str()?;
    (repos.len() >= FIRST_SIGHT_REPOS && repos.iter().any(|repo| repo == name))
        .then(|| ProjectRef::multi_repo(folder, repos).ok())
        .flatten()
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
fn first_entry(porcelain: &str) -> Option<(PathBuf, bool)> {
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
fn path_hash8(root: &Path) -> String {
    let digest = md5::compute(crate::platform::files::path_bytes(root));
    format!("{digest:x}")[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{git, init_repo};
    use tempfile::tempdir;

    /// A pando home that knows no project.
    fn no_home() -> &'static Path {
        Path::new("/pando-test-does-not-exist/home")
    }

    // A project's id names its directory under the pando home: a hash that
    // changed with a refactor would orphan every project's state, config
    // and logs. Pinned to the bytes of the path as the OS stores them.
    #[test]
    fn the_path_hash_of_a_project_never_changes() {
        assert_eq!(path_hash8(Path::new("/home/me/code/app")), "1fb718a9");
        assert_eq!(
            path_hash8(Path::new("/Users/me/Code/ünïcode app")),
            "30d92acf"
        );
    }

    #[test]
    fn discovers_the_root_from_the_repository_root() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("acme-shop");
        init_repo(&repo);

        let project = discover(&repo, no_home()).unwrap();
        assert_eq!(project.root, std::fs::canonicalize(&repo).unwrap());
        assert_eq!(project.display_name, "acme-shop");
        assert!(project.id.starts_with("acme-shop-"));
        assert_eq!(project.id.len(), "acme-shop-".len() + 8);
    }

    #[test]
    fn subdirectory_and_linked_worktree_resolve_to_the_same_project() {
        let dir = tempdir().unwrap();
        let repo = dir.path().join("acme-shop");
        init_repo(&repo);
        let nested = repo.join("apps").join("web");
        std::fs::create_dir_all(&nested).unwrap();
        let linked = dir.path().join("elsewhere").join("feat+x");
        std::fs::create_dir_all(linked.parent().unwrap()).unwrap();
        git(
            &repo,
            &["worktree", "add", "-b", "feat/x", linked.to_str().unwrap()],
        );

        let from_root = discover(&repo, no_home()).unwrap();
        let from_sub = discover(&nested, no_home()).unwrap();
        let from_linked = discover(&linked, no_home()).unwrap();

        assert_eq!(from_root, from_sub);
        assert_eq!(
            from_root, from_linked,
            "a linked worktree must resolve to the main checkout's project"
        );
    }

    #[test]
    fn the_id_is_stable_and_path_dependent() {
        let dir = tempdir().unwrap();
        let a = dir.path().join("one").join("shop");
        let b = dir.path().join("two").join("shop");
        init_repo(&a);
        init_repo(&b);

        let first = discover(&a, no_home()).unwrap();
        assert_eq!(
            first,
            discover(&a, no_home()).unwrap(),
            "id must be deterministic"
        );
        let second = discover(&b, no_home()).unwrap();
        assert_eq!(first.display_name, second.display_name);
        assert_ne!(
            first.id, second.id,
            "same directory name at different paths must not share a project id"
        );
    }

    #[test]
    fn outside_a_repository_is_an_error() {
        let dir = tempdir().unwrap();
        let err = discover(dir.path(), no_home()).unwrap_err();
        assert!(
            format!("{err:#}").contains("not inside a git repository"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn a_bare_repository_is_refused() {
        let dir = tempdir().unwrap();
        let bare = dir.path().join("bare.git");
        git(
            dir.path(),
            &[
                "init",
                "--bare",
                "--quiet",
                "--initial-branch=main",
                bare.to_str().unwrap(),
            ],
        );

        let err = discover(&bare, no_home()).unwrap_err();
        assert!(
            format!("{err:#}").contains("bare repositories are not supported"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn first_entry_reads_the_main_checkout_only() {
        let text = "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n\
                    worktree /elsewhere/feat+x\nHEAD def\nbranch refs/heads/feat/x\n\n";
        assert_eq!(
            first_entry(text),
            Some((PathBuf::from("/repo"), false)),
            "only the first entry describes the main checkout"
        );
    }

    #[test]
    fn first_entry_reports_a_bare_main_checkout() {
        assert_eq!(
            first_entry("worktree /repo.git\nbare\n\n"),
            Some((PathBuf::from("/repo.git"), true))
        );
    }

    #[test]
    fn first_entry_ignores_unknown_lines_and_empty_output() {
        assert_eq!(
            first_entry("worktree /repo\nsomething-new-in-git 1\nHEAD abc\n\n"),
            Some((PathBuf::from("/repo"), false))
        );
        assert_eq!(first_entry(""), None);
    }

    /// `product/` holding the repositories `backend` and `frontend`.
    fn folder_of_repos(dir: &Path) -> PathBuf {
        let folder = dir.join("product");
        init_repo(&folder.join("backend"));
        init_repo(&folder.join("frontend"));
        std::fs::create_dir_all(folder.join("docs")).unwrap();
        std::fs::canonicalize(folder).unwrap()
    }

    /// A pando home that knows `project`, as one `new` has made.
    fn home_knowing(dir: &Path, project: &ProjectRef) -> PathBuf {
        let home = dir.join("pando-home");
        std::fs::create_dir_all(home.join("projects").join(&project.id)).unwrap();
        home
    }

    #[test]
    fn a_folder_of_repositories_is_one_project_of_them() {
        let dir = tempdir().unwrap();
        let folder = folder_of_repos(dir.path());

        let project = discover(&folder, no_home()).unwrap();
        assert_eq!(project.root, folder);
        assert_eq!(project.display_name, "product");
        assert_eq!(project.repos, ["backend", "frontend"]);
        assert!(project.id.starts_with("product-"));
        assert_eq!(
            project.checkouts_in(Path::new("/set")),
            [
                PathBuf::from("/set/backend"),
                PathBuf::from("/set/frontend")
            ]
        );
    }

    #[test]
    fn a_folder_of_one_repository_is_no_project_until_pando_knows_it() {
        let dir = tempdir().unwrap();
        let folder = dir.path().join("product");
        init_repo(&folder.join("backend"));

        let err = discover(&folder, no_home()).unwrap_err();
        assert!(
            format!("{err:#}").contains("not inside a git repository, nor a folder of them"),
            "{err:#}"
        );
        let project = ProjectRef::multi_repo(&folder, vec!["backend".into()]).unwrap();
        let home = home_knowing(dir.path(), &project);
        assert_eq!(discover(&folder, &home).unwrap(), project);
    }

    #[test]
    fn a_plain_folder_below_a_known_one_finds_it_and_an_unknown_one_does_not() {
        let dir = tempdir().unwrap();
        let folder = folder_of_repos(dir.path());
        let docs = folder.join("docs");
        assert!(discover(&docs, no_home()).is_err());

        let project = discover(&folder, no_home()).unwrap();
        let home = home_knowing(dir.path(), &project);
        assert_eq!(discover(&docs, &home).unwrap(), project);
    }

    #[test]
    fn a_main_checkout_in_the_folder_is_still_its_own_repository() {
        let dir = tempdir().unwrap();
        let folder = folder_of_repos(dir.path());
        let project = discover(&folder, no_home()).unwrap();
        let home = home_knowing(dir.path(), &project);

        let backend = discover(&folder.join("backend"), &home).unwrap();
        assert_eq!(backend.root, folder.join("backend"));
        assert!(!backend.is_multi_repo());
    }

    #[test]
    fn a_set_and_its_checkouts_are_the_folders_project() {
        let dir = tempdir().unwrap();
        let folder = folder_of_repos(dir.path());
        let project = discover(&folder, no_home()).unwrap();
        let home = home_knowing(dir.path(), &project);
        let set = dir.path().join("trees").join("feat+x");
        std::fs::create_dir_all(&set).unwrap();
        for repo in ["backend", "frontend"] {
            let at = set.join(repo);
            git(
                &folder.join(repo),
                &["worktree", "add", "-b", "feat/x", at.to_str().unwrap()],
            );
        }
        std::fs::create_dir_all(set.join("backend").join("src")).unwrap();

        assert_eq!(discover(&set, &home).unwrap(), project);
        assert_eq!(discover(&set.join("frontend"), &home).unwrap(), project);
        assert_eq!(
            discover(&set.join("backend").join("src"), &home).unwrap(),
            project
        );
        // Unknown to this home, the set is nobody's, and each checkout is
        // its repository's.
        assert!(discover(&set, no_home()).is_err());
        assert_eq!(
            discover(&set.join("frontend"), no_home()).unwrap().root,
            folder.join("frontend")
        );
    }

    #[test]
    fn a_worktree_named_for_its_repository_is_not_taken_for_a_set() {
        let dir = tempdir().unwrap();
        let folder = folder_of_repos(dir.path());
        let project = discover(&folder, no_home()).unwrap();
        let home = home_knowing(dir.path(), &project);
        // One repository's worktrees, one of them called `backend`.
        let trees = dir.path().join("trees");
        std::fs::create_dir_all(&trees).unwrap();
        let backend = folder.join("backend");
        for name in ["backend", "feat+y"] {
            let at = trees.join(name);
            git(
                &backend,
                &["worktree", "add", "-b", name, at.to_str().unwrap()],
            );
        }

        let found = discover(&trees.join("backend"), &home).unwrap();
        assert_eq!(found.root, backend);
        assert!(!found.is_multi_repo());
    }

    #[test]
    fn a_path_is_asked_about_in_the_repository_it_starts_with() {
        let project = ProjectRef {
            id: "product-00000000".into(),
            root: PathBuf::from("/product"),
            display_name: "product".into(),
            repos: vec!["backend".into(), "frontend".into()],
            ..Default::default()
        };
        let set = Path::new("/set");
        assert_eq!(
            project.repo_path(set, "backend/.env"),
            Some((PathBuf::from("/set/backend"), ".env".into()))
        );
        assert_eq!(
            project.repo_path(set, "frontend/node_modules/"),
            Some((PathBuf::from("/set/frontend"), "node_modules/".into()))
        );
        assert_eq!(project.repo_path(set, ".env"), None);
        assert_eq!(project.repo_path(set, "backend"), None);
        assert_eq!(project.repo_path(set, "docs/notes.md"), None);
        let one = ProjectRef::from_root("/repo").unwrap();
        assert_eq!(
            one.repo_path(Path::new("/wt"), ".env"),
            Some((PathBuf::from("/wt"), ".env".into()))
        );
    }
}
