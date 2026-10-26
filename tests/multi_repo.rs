//! A multi-repo project from the binary's side (issue #18): a plain folder
//! of sibling repositories is one project, `new` makes a set of them, and
//! `start` runs processes from more than one repository in one port window.

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{git, status_porcelain};
use tempfile::TempDir;

struct Folder {
    _dir: TempDir,
    home: PathBuf,
    /// `product/`, holding `backend` and `frontend`.
    root: PathBuf,
}

impl Drop for Folder {
    fn drop(&mut self) {
        if self.home.exists() {
            let _ = self.pando(&["stop"]);
        }
    }
}

impl Folder {
    fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let root = dir.path().join("product");
        for repo in ["backend", "frontend"] {
            let at = root.join(repo);
            std::fs::create_dir_all(&at).unwrap();
            git(&at, &["init", "--quiet"]);
            std::fs::write(at.join(".gitignore"), ".env\n").unwrap();
            std::fs::write(at.join("README.md"), format!("# {repo}\n")).unwrap();
            git(&at, &["add", "."]);
            git(&at, &["commit", "--quiet", "-m", "root"]);
        }
        std::fs::write(root.join("backend").join(".env"), "SECRET=1\n").unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let folder = Self {
            home: dir.path().join("pando-home"),
            root,
            _dir: dir,
        };
        std::fs::create_dir_all(&folder.home).unwrap();
        std::fs::write(
            folder.home.join("config.toml"),
            "[runtime]\nprelude = \"\"\n",
        )
        .unwrap();
        folder
    }

    fn project_dir(&self) -> PathBuf {
        let project = pando::project::ProjectRef::multi_repo(
            &self.root,
            vec!["backend".into(), "frontend".into()],
        )
        .unwrap();
        self.home.join("projects").join(project.id)
    }

    fn write_config(&self, toml: &str) {
        std::fs::create_dir_all(self.project_dir()).unwrap();
        std::fs::write(self.project_dir().join("pando.toml"), toml).unwrap();
    }

    fn pando(&self, args: &[&str]) -> Output {
        self.pando_in(&self.root, args)
    }

    fn pando_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &self.home)
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("run pando")
    }
}

fn ok(out: &Output) -> String {
    assert_eq!(
        out.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_str(&ok(out)).expect("json")
}

/// The project's config: `api` in the backend, and `web` in the frontend
/// running `web` on `PORT`, told the api's port; `backend/.env` provisioned.
fn config_with_web(web: &str) -> String {
    let mut config = String::from("[project]\nprovision = [\"backend/.env\"]\n\n");
    config.push_str("[processes.api]\ncwd = \"backend\"\ncmd = '''");
    config.push_str(&common::listener_on_port_env());
    config.push_str("'''\nports = [\"api\"]\nenv = { PORT = \"{port:api}\" }\n");
    config.push_str("ready = { role = \"api\" }\n\n");
    config.push_str("[processes.web]\ncwd = \"frontend\"\ncmd = '''");
    config.push_str(web);
    config.push_str("'''\nports = [\"web\"]\n");
    config.push_str("env = { PORT = \"{port:web}\", API_URL = \"http://localhost:{port:api}\" }\n");
    config.push_str("ready = { role = \"web\" }\n");
    config
}

#[test]
fn a_folder_of_repositories_lists_as_one_project() {
    let f = Folder::new();
    let listed = json(&f.pando(&["ls", "--json"]));
    assert_eq!(listed["project"]["name"], "product");
    assert_eq!(
        listed["project"]["repos"],
        serde_json::json!(["backend", "frontend"])
    );
    let main = &listed["worktrees"][0];
    assert_eq!(main["main"], true);
    assert_eq!(main["branch"], "main");
    assert_eq!(main["repos"][1]["name"], "frontend");
    assert_eq!(main["repos"][1]["dirty"], false);
}

#[test]
fn a_set_is_made_run_across_its_repositories_and_removed() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not on PATH");
        return;
    }
    let f = Folder::new();
    f.write_config(&config_with_web(&common::listener_printing("API_URL")));

    let made = ok(&f.pando(&["new", "feat/x"]));
    assert!(made.contains("feat+x"), "{made}");
    let set = PathBuf::from(ok(&f.pando(&["path", "feat/x"])).trim());
    assert_eq!(
        std::fs::read_to_string(set.join("backend").join(".env")).unwrap(),
        "SECRET=1\n"
    );

    // From inside one of the set's checkouts, it is the same project.
    let from_inside = json(&f.pando_in(&set.join("frontend"), &["ls", "--json"]));
    assert_eq!(from_inside["project"]["root"], f.root.display().to_string());
    let listed = &from_inside["worktrees"][1];
    assert_eq!(listed["name"], "feat+x");
    assert_eq!(listed["branch"], "feat/x");
    assert_eq!(listed["repos"][0]["branch"], "feat/x");

    ok(&f.pando(&["start", "feat/x", "--wait"]));
    let status = json(&f.pando(&["status", "feat/x", "--json"]));
    let ports = &status["worktrees"][0]["ports"];
    let api = ports["api"].as_u64().expect("an api port");
    assert!(ports["web"].as_u64().is_some(), "{status}");
    let log = f.project_dir().join("logs").join("feat+x").join("web.log");
    let printed = std::fs::read_to_string(&log).unwrap();
    assert!(
        printed.contains(&format!("API_URL=http://localhost:{api}")),
        "the frontend's process was told the backend's port: {printed}"
    );

    // `status` says which repository each process runs in, and each
    // repository's branch; `--source` takes a repository for its logs.
    let said = ok(&f.pando(&["status", "feat/x"]));
    assert!(
        said.contains("api (backend)") && said.contains("web (frontend)"),
        "{said}"
    );
    assert!(said.contains("frontend  repo"), "{said}");
    let logged = ok(&f.pando(&["logs", "feat/x", "--source", "frontend"]));
    assert!(logged.contains("API_URL="), "{logged}");

    ok(&f.pando(&["stop", "feat/x"]));
    ok(&f.pando(&["rm", "feat/x"]));
    assert!(!set.exists());
    for repo in ["backend", "frontend"] {
        assert_eq!(status_porcelain(&f.root.join(repo)), "", "{repo}");
    }

    // `check` tests a throwaway set, every repository at its own base,
    // and leaves nothing behind in either. Its web process serves a page,
    // which is what a check asks for.
    f.write_config(&config_with_web(&common::answering(404)));
    let checked = json(&f.pando(&["check", "--json"]));
    assert_eq!(checked["result"], "passed", "{checked}");
    let commits = checked["commit"].as_str().unwrap();
    assert_eq!(commits.split(' ').count(), 2, "{commits}");
    for repo in ["backend", "frontend"] {
        assert_eq!(
            common::git_raw(&f.root.join(repo), &["worktree", "list", "--porcelain"])
                .stdout
                .split(|b| *b == b'\n')
                .filter(|line| line.starts_with(b"worktree "))
                .count(),
            1,
            "{repo} lists its main checkout alone"
        );
        assert_eq!(status_porcelain(&f.root.join(repo)), "", "{repo}");
    }
}

#[test]
fn a_plain_folder_with_one_repository_is_still_refused() {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("product").join("backend");
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    let out = Command::new(env!("CARGO_BIN_EXE_pando"))
        .env("PANDO_HOME", dir.path().join("pando-home"))
        .current_dir(dir.path().join("product"))
        .args(["ls"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("nor a folder of them"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn detection_and_the_agents_job_read_every_repository_of_the_folder() {
    let f = Folder::new();
    // `backend` is no app detection knows, and its `.env` is still the
    // file its processes read.
    let signals = json(&f.pando(&["signals"]));
    assert_eq!(
        signals["signals"]["ignored_present"],
        serde_json::json!(["backend/.env"]),
        "{}",
        signals["signals"]
    );
    let job = ok(&f.pando(&["init", "--agent"]));
    assert!(
        job.contains("a folder of 2 git repositories: backend (on main), frontend (on main)"),
        "{job}"
    );
}

#[test]
fn doctor_names_what_in_the_config_no_repository_of_the_folder_holds() {
    let f = Folder::new();
    f.write_config(
        "[repos]\nmembers = [\"backend\", \"frontend\", \"gone\"]\n\n\
         [project]\nclone = [\"node_modules\"]\n\n\
         [processes.docs]\ncwd = \"docs\"\ncmd = \"true\"\n",
    );
    let out = f.pando(&["doctor"]);
    let said = String::from_utf8_lossy(&out.stdout);
    for line in [
        "`[repos] members` names \"gone\"",
        "process \"docs\" runs in \"docs\", which is in none of the repositories",
        "`project.clone` names \"node_modules\", which is in none of the repositories",
    ] {
        assert!(said.contains(line), "{line}\n{said}");
    }
}

#[test]
fn from_inside_one_repository_the_agents_job_names_the_folder_beside_it() {
    let f = Folder::new();
    let job = ok(&f.pando_in(&f.root.join("backend"), &["init", "--agent"]));
    assert!(
        job.contains(&format!(
            "This repository sits in {} beside frontend.",
            f.root.display()
        )),
        "{job}"
    );
    // From the folder, the job is the folder's, and says what a set is.
    let job = ok(&f.pando(&["init", "--agent"]));
    assert!(
        job.contains("each git branch of these repositories as one worktree set"),
        "{job}"
    );
    assert!(job.contains("inside each repository's .git"), "{job}");
    assert!(!job.contains("This repository sits in"), "{job}");
}

// The services of a set: the compose file at a repository's root, named
// under it, and the value rewritten read from that repository's env
// files. Before, detection never proposed the file, and an isolated start
// looked for DATABASE_URL at the set's root, which is a plain directory.
#[test]
fn a_set_runs_private_services_from_a_repositorys_compose_file() {
    if !common::python3_available() {
        eprintln!("skipping: python3 is not on PATH");
        return;
    }
    let f = Folder::new();
    let backend = f.root.join("backend");
    std::fs::write(
        backend.join("docker-compose.yml"),
        "services:\n  postgres:\n    image: postgres:16\n    ports:\n      - \"5432:5432\"\n",
    )
    .unwrap();
    std::fs::write(
        backend.join(".env.example"),
        "DATABASE_URL=postgres://app:app@localhost:5432/shop\n",
    )
    .unwrap();
    git(&backend, &["add", "."]);
    git(&backend, &["commit", "--quiet", "-m", "services"]);
    common::docker::install(&f.home);

    let signals = json(&f.pando(&["signals"]));
    assert_eq!(
        signals["signals"]["compose_files"],
        serde_json::json!(["backend/docker-compose.yml"]),
        "{}",
        signals["signals"]
    );

    let mut config = String::from("[project]\nprovision = [\"backend/.env\"]\n\n");
    config.push_str("[processes.web]\ncwd = \"frontend\"\ncmd = '''");
    config.push_str(&common::listener_printing("DATABASE_URL"));
    config.push_str("'''\nports = [\"web\"]\nready = { role = \"web\" }\n\n");
    config.push_str(
        "[[services]]\nkind = \"compose\"\nfile = \"backend/docker-compose.yml\"\n\
         include = [\"postgres\"]\nenv = { DATABASE_URL = \"postgres\" }\n",
    );
    f.write_config(&config);

    ok(&f.pando(&["new", "feat/x"]));
    let project =
        pando::project::ProjectRef::multi_repo(&f.root, vec!["backend".into(), "frontend".into()])
            .unwrap();
    let compose = pando::compose::project_name(&project.id, "feat+x");
    let _down = common::docker::down_on_drop(&f.home, &compose);
    ok(&f.pando(&["start", "feat/x", "--isolated", "--wait"]));

    let status = json(&f.pando(&["status", "feat/x", "--json"]));
    let postgres = status["worktrees"][0]["ports"]["postgres"]
        .as_u64()
        .expect("a postgres port");
    assert_eq!(
        common::docker::services_up(&f.home, &compose),
        [("postgres".to_string(), postgres as u16)]
    );
    let up = common::docker::invocations_for(&f.home, &compose);
    assert!(
        up.iter()
            .any(|line| line.contains("-f docker-compose.yml") && line.contains(" up ")),
        "{up:?}"
    );
    let log = f.project_dir().join("logs").join("feat+x").join("web.log");
    let printed = std::fs::read_to_string(&log).unwrap();
    assert!(
        printed.contains(&format!(
            "DATABASE_URL=postgres://app:app@localhost:{postgres}/shop"
        )),
        "the frontend was pointed at the set's own postgres: {printed}"
    );

    ok(&f.pando(&["stop", "feat/x"]));
    ok(&f.pando(&["rm", "feat/x"]));
    assert!(common::docker::was_downed(&f.home, &compose));
}
