//! `pando ls --all` through the binary: every project in one home, from a
//! directory that is no repository, with the registry each project keeps.

use crate::common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::{Kind, build};
use tempfile::TempDir;

const SLEEPER: &str = "[dev]\ncmd = \"echo started-ok && sleep 30\"\nports = { PORT = \"web\" }\n";

/// Two fixture projects, `a/` and `b/`, sharing one pando home, and a
/// directory beside them that is in no repository.
struct Machine {
    _dir: TempDir,
    home: PathBuf,
    a: PathBuf,
    b: PathBuf,
    nowhere: PathBuf,
}

impl Machine {
    fn new() -> Self {
        common::hermetic_home();
        let dir = TempDir::new().unwrap();
        let a = build(Kind::Plain, &dir.path().join("a")).root;
        let b = build(Kind::Plain, &dir.path().join("b")).root;
        let home = dir.path().join("pando-home");
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::create_dir_all(&home).unwrap();
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::write(home.join("config.toml"), "[runtime]\nprelude = \"\"\n").unwrap();
        }
        let nowhere = dir.path().join("nowhere");
        std::fs::create_dir_all(&nowhere).unwrap();
        let machine = Self {
            home,
            a,
            b,
            nowhere,
            _dir: dir,
        };
        for root in [&machine.a, &machine.b] {
            let config = machine.project_dir(root).join("pando.toml");
            std::fs::create_dir_all(config.parent().unwrap()).unwrap();
            std::fs::write(config, SLEEPER).unwrap();
        }
        machine
    }

    fn pando_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_pando"))
            .env("PANDO_HOME", &self.home)
            .current_dir(cwd)
            .args(args)
            .output()
            .expect("run pando")
    }

    fn project_dir(&self, root: &Path) -> PathBuf {
        let project = pando::project::ProjectRef::from_root(root).unwrap();
        self.home.join("projects").join(project.id)
    }

    fn id(&self, root: &Path) -> String {
        pando::project::ProjectRef::from_root(root).unwrap().id
    }

    /// `ls --all --json` from the directory that is no repository.
    fn all(&self) -> serde_json::Value {
        let out = self.pando_in(&self.nowhere, &["ls", "--all", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        for root in [&self.a, &self.b] {
            if root.exists() {
                let _ = self.pando_in(root, &["stop", "--all"]);
            }
        }
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn ok(out: Output) -> Output {
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    out
}

#[test]
fn ls_all_lists_every_project_from_outside_any_repository() {
    let m = Machine::new();
    ok(m.pando_in(&m.a, &["new", "feat/one"]));
    ok(m.pando_in(&m.b, &["new", "feat/two"]));

    // Each project registered where its repository is, and a run that
    // finds the registry as it would write it writes nothing.
    let registry = m.project_dir(&m.a).join("project.json");
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&registry).unwrap()).unwrap();
    let a_root = std::fs::canonicalize(&m.a).unwrap();
    assert_eq!(written["root"], a_root.display().to_string().as_str());
    let bytes = std::fs::read(&registry).unwrap();
    let modified = std::fs::metadata(&registry).unwrap().modified().unwrap();
    ok(m.pando_in(&m.a, &["ls"]));
    assert_eq!(std::fs::read(&registry).unwrap(), bytes);
    assert_eq!(
        std::fs::metadata(&registry).unwrap().modified().unwrap(),
        modified
    );

    // One up: it comes first, whatever was used last.
    ok(m.pando_in(&m.b, &["start", "feat/two"]));
    ok(m.pando_in(&m.a, &["ls"]));
    let v = m.all();
    assert_eq!(v["version"], pando::cli::JSON_VERSION);
    let projects = v["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2, "{v}");
    assert_eq!(projects[0]["id"], m.id(&m.b).as_str(), "{v}");
    let counts = &projects[0]["counts"];
    assert_eq!(
        counts["up"].as_u64().unwrap() + counts["starting"].as_u64().unwrap(),
        1,
        "{v}"
    );
    let two = &projects[0]["worktrees"][0];
    assert_eq!(two["name"], "feat+two", "{v}");
    assert!(
        two["status"] == "starting" || two["status"] == "running",
        "{v}"
    );
    assert!(two["processes"]["dev"]["pid"].is_u64(), "{v}");
    assert_eq!(projects[1]["id"], m.id(&m.a).as_str(), "{v}");
    assert_eq!(projects[1]["moved"], false);

    // The text, from the same place, groups them the same way.
    let out = ok(m.pando_in(&m.nowhere, &["ls", "--all"]));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let b_at = text.find("feat+two").expect(&text);
    let a_name = a_root.file_name().unwrap().to_string_lossy().into_owned();
    assert!(text.rfind(&a_name).unwrap() > b_at, "{text}");

    // From inside a repository it is the same machine-wide answer.
    let inside = m.pando_in(&m.a, &["ls", "--all", "--json"]);
    let inside: serde_json::Value = serde_json::from_slice(&ok(inside).stdout).unwrap();
    assert_eq!(inside["projects"].as_array().unwrap().len(), 2);

    // Plain `ls` outside a repository still says there is none.
    let out = m.pando_in(&m.nowhere, &["ls"]);
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn a_project_without_a_registry_is_recovered_and_a_moved_one_said_so() {
    let m = Machine::new();
    ok(m.pando_in(&m.a, &["new", "feat/one"]));
    ok(m.pando_in(&m.b, &["new", "feat/two"]));

    // Registered before registries: recovered from what its state records,
    // and written down then.
    let registry = m.project_dir(&m.a).join("project.json");
    std::fs::remove_file(&registry).unwrap();
    let v = m.all();
    let a = v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == m.id(&m.a).as_str())
        .expect("recovered");
    let a_root = std::fs::canonicalize(&m.a).unwrap();
    assert_eq!(a["root"], a_root.display().to_string().as_str(), "{v}");
    assert_eq!(a["moved"], false, "{v}");
    assert!(registry.is_file(), "the recovered registry is written");

    // Moved aside: still listed, last, and said to be moved.
    let b_id = m.id(&m.b);
    std::fs::rename(&m.b, m.b.with_file_name("elsewhere")).unwrap();
    let v = m.all();
    let projects = v["projects"].as_array().unwrap();
    assert_eq!(projects.len(), 2, "{v}");
    assert_eq!(projects[1]["id"], b_id.as_str(), "{v}");
    assert_eq!(projects[1]["moved"], true, "{v}");
    assert_eq!(projects[0]["moved"], false, "{v}");
}
