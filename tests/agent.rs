//! The agent layer's packaging: the Claude Code plugin and the Codex
//! skills.
//!
//! Both hosts get wrappers, and the whole design rests on those wrappers
//! staying *thin*. The reasoning lives once, in `agent/brief.md`; two
//! copies of it drift within a month and nobody notices, because a
//! procedure for a language model still reads perfectly when it is wrong.
//! So the tests here are mostly about what the wrappers must **not**
//! become.
//!
//! What a document *says* is held to what the binary takes by
//! `cli::tests::assert_every_documented_command_is_real`, which reads
//! these same files.

use std::path::{Path, PathBuf};

/// How much glue a host wrapper is allowed. The phase plan says "under
/// fifty lines of glue each"; this counts the body, since the frontmatter
/// is the host's own metadata rather than anything a reader follows.
const MAX_WRAPPER_BODY_LINES: usize = 50;

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    let path = repo(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Every host wrapper, by path. One list, so a host added later cannot
/// escape any test here.
const WRAPPERS: [&str; 4] = [
    "agent/skills/pando-setup/SKILL.md",
    "agent/skills/pando-operate/SKILL.md",
    "agent/codex/pando-setup/SKILL.md",
    "agent/codex/pando-operate/SKILL.md",
];

/// The two skills, under the names both hosts use for them.
const SKILLS: [&str; 2] = ["pando-setup", "pando-operate"];

/// The frontmatter block and the body, split at the closing `---`.
fn frontmatter_and_body(text: &str) -> (String, String) {
    let mut lines = text.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "a skill file starts with its frontmatter"
    );
    let mut front = String::new();
    for line in lines.by_ref() {
        if line == "---" {
            return (front, lines.collect::<Vec<_>>().join("\n"));
        }
        front.push_str(line);
        front.push('\n');
    }
    panic!("frontmatter was never closed");
}

#[test]
fn every_wrapper_declares_a_name_and_a_description() {
    for file in WRAPPERS {
        let (front, _) = frontmatter_and_body(&read(file));
        // The name a host lists it under, and the sentence a host matches
        // against what the developer asked for. A skill with no
        // description is a skill nothing ever invokes.
        assert!(front.contains("name: "), "{file} declares no name");
        let description = front
            .lines()
            .find_map(|line| line.strip_prefix("description: "))
            .unwrap_or_else(|| panic!("{file} declares no description"));
        assert!(
            description.len() > 60,
            "{file}'s description is too short to match anything: {description:?}"
        );
        assert!(
            description.to_lowercase().contains("pando"),
            "{file}'s description never says what tool it is about"
        );
    }
}

/// The load-bearing test of this phase.
///
/// If a wrapper grows past glue, it has started to contain reasoning —
/// and the moment there are two copies of the reasoning, the one nobody
/// is reading goes stale silently. The brief is the only place it lives.
#[test]
fn no_wrapper_is_thick_enough_to_hold_reasoning() {
    for file in WRAPPERS {
        let (_, body) = frontmatter_and_body(&read(file));
        let lines = body.lines().filter(|l| !l.trim().is_empty()).count();
        assert!(
            lines <= MAX_WRAPPER_BODY_LINES,
            "{file} is {lines} lines of glue — over {MAX_WRAPPER_BODY_LINES}, it has started \
             to contain reasoning, and the brief is where that belongs"
        );
    }
}

#[test]
fn every_wrapper_sends_its_reader_to_the_brief() {
    for file in WRAPPERS {
        let text = read(file);
        assert!(
            text.contains("brief.md"),
            "{file} never points at the brief, so it is either useless or a second copy of it"
        );
    }
}

/// A path a wrapper names has to resolve from where that wrapper is
/// installed, which is not where it sits in this repository.
///
/// The Claude Code plugin root is `agent/` — which is why the brief lives
/// there and not one directory up. A plugin is installed by a sparse
/// checkout of the paths it declares, so `${CLAUDE_PLUGIN_ROOT}/../` is
/// not a place anything can be relied on to exist.
#[test]
fn the_brief_is_reachable_from_every_wrapper_as_it_names_it() {
    for skill in SKILLS {
        let text = read(&format!("agent/skills/{skill}/SKILL.md"));
        assert!(
            text.contains("${CLAUDE_PLUGIN_ROOT}/brief.md"),
            "agent/skills/{skill} must reach the brief through the plugin root, which is agent/"
        );
    }
    // Which is only true because the plugin root is the directory the
    // brief is in.
    assert!(repo("agent/.claude-plugin/plugin.json").is_file());
    assert!(repo("agent/brief.md").is_file());
    assert!(repo("agent/json.md").is_file());

    // Codex has no such variable, so its installer puts the brief beside
    // the wrapper that names it. Run for real, into a temporary home —
    // the claim "beside this file" is worth nothing unless something
    // checks that it lands there.
    let home = tempfile::tempdir().unwrap();
    let out = std::process::Command::new("bash")
        .arg(repo("agent/codex/install.sh"))
        .env("CODEX_HOME", home.path())
        .output()
        .expect("run the installer");
    assert!(
        out.status.success(),
        "the installer failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for skill in SKILLS {
        let installed = home.path().join("skills").join(skill);
        for file in ["SKILL.md", "brief.md", "json.md"] {
            assert!(
                installed.join(file).is_file(),
                "{} is not there after an install",
                installed.join(file).display()
            );
        }
        // The same brief, not a second one.
        assert_eq!(
            std::fs::read_to_string(installed.join("brief.md")).unwrap(),
            read("agent/brief.md"),
            "the installed brief is not the one in the repository"
        );
        assert_eq!(
            std::fs::read_to_string(installed.join("SKILL.md")).unwrap(),
            read(&format!("agent/codex/{skill}/SKILL.md"))
        );
    }
    // And it wrote nowhere else: a script that installs into somebody's
    // real home when asked for a temporary one is a script nothing can
    // test.
    let mut top: Vec<String> = std::fs::read_dir(home.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    top.sort();
    assert_eq!(top, vec!["skills"]);
}

/// Both hosts, the same two skills, under the same two names.
///
/// A developer who moves between them should not have to learn a second
/// vocabulary, and a bug report naming a skill should be findable in
/// either packaging.
#[test]
fn both_hosts_ship_the_same_two_skills_under_the_same_names() {
    for host in ["agent/skills", "agent/codex"] {
        for skill in SKILLS {
            let file = format!("{host}/{skill}/SKILL.md");
            let (front, _) = frontmatter_and_body(&read(&file));
            assert!(
                front.contains(&format!("name: {skill}")),
                "{file} calls itself something else"
            );
        }
    }
    // And they describe themselves identically, because they are the same
    // skill: a host that matched one and not the other would send a
    // developer down two different paths for the same request.
    for skill in SKILLS {
        let described = |host: &str| {
            frontmatter_and_body(&read(&format!("{host}/{skill}/SKILL.md")))
                .0
                .lines()
                .find_map(|l| l.strip_prefix("description: ").map(str::to_string))
                .expect("a description")
        };
        assert_eq!(
            described("agent/skills"),
            described("agent/codex"),
            "{skill} means two different things to the two hosts"
        );
    }
}

#[test]
fn the_plugin_manifest_is_valid_and_the_marketplace_points_at_it() {
    let plugin: serde_json::Value =
        serde_json::from_str(&read("agent/.claude-plugin/plugin.json")).expect("valid JSON");
    assert_eq!(plugin["name"], "pando");
    assert!(plugin["description"].as_str().is_some_and(|d| d.len() > 40));
    assert!(plugin["version"].as_str().is_some());

    let market: serde_json::Value =
        serde_json::from_str(&read(".claude-plugin/marketplace.json")).expect("valid JSON");
    let entries = market["plugins"].as_array().expect("a list of plugins");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], plugin["name"]);
    let source = entries[0]["source"].as_str().expect("a source");
    assert_eq!(
        source, "./agent",
        "the marketplace must point at the directory that holds the brief"
    );
    assert!(
        repo(source.trim_start_matches("./"))
            .join(".claude-plugin/plugin.json")
            .is_file(),
        "the marketplace's source has no plugin manifest in it"
    );
}

// ---- a recorded answers file, end to end -----------------------------------
//
// No network and no model: a fixed answers file — what a program sends
// after reading `pando signals` — driven through `pando init --answers`
// against each hard fixture, with the resulting config and doctor's
// verdict asserted. The corpus *is* pando's validation, so the promise
// "an agent can configure this correctly on the first try" is only worth
// something if the shapes are the ones a first run really meets.

use crate::common;

use common::{Kind, build, paths_for, status_porcelain};
use pando::config::{Config, PortsSpec, ServiceConfig};
use tempfile::TempDir;

/// One hard shape, the answers a program would record for it, and what
/// has to be true afterwards.
struct Recorded {
    kind: Kind,
    /// The answers file, verbatim.
    answers: &'static str,
    /// The questions the rules cannot settle here, in the order they are
    /// asked — which is what the answers file is for and what the
    /// decisions log must hold afterwards. `None` where the answer
    /// depends on what this machine has installed.
    asks: Option<&'static [&'static str]>,
    /// Whether `doctor` exits 0 afterwards.
    healthy: bool,
    /// What config must say. Asserted against the loaded config rather
    /// than the file's text: a service entry's provenance comment
    /// depends on whether this machine has the engine, and the *answer*
    /// does not.
    expect: fn(&Config),
}

const RECORDED: &[Recorded] = &[
    // Several apps and no workspace file. Which apps to run is the
    // developer's call — and it is the only question, because taking the
    // per-app form settles the dev command and the ports with it.
    Recorded {
        kind: Kind::WorkspaceNoLock,
        answers: r#"{"processes": "api: npm run dev in apps/api; web: npm run dev in apps/web"}"#,
        asks: Some(&["processes"]),
        healthy: true,
        expect: |c| {
            assert_eq!(c.processes.keys().collect::<Vec<_>>(), ["api", "web"]);
            assert_eq!(c.processes["web"].cwd.as_deref(), Some("apps/web"));
            assert_eq!(c.processes["api"].cwd.as_deref(), Some("apps/api"));
            assert_eq!(
                c.project.install, None,
                "no lockfile means no frozen install exists, and pando never proposes a loose one"
            );
        },
    },
    // Two app ports in the env example and a dev script that takes
    // neither on the command line: which of them the one process serves
    // on is a question, and the pre-ticked answer is both.
    Recorded {
        kind: Kind::EnvPorts,
        answers: r#"{"port_env": "WEB_PORT, ADMIN_PORT"}"#,
        asks: Some(&["port_env"]),
        healthy: true,
        expect: |c| {
            assert_eq!(c.project.install.as_deref(), Some("npm ci"));
            let PortsSpec::Map(ports) = c.processes["dev"].ports.as_ref().expect("ports") else {
                panic!("the env example names its ports by role");
            };
            assert_eq!(ports["WEB_PORT"], "web");
            assert_eq!(ports["ADMIN_PORT"], "admin");
        },
    },
    // The shape this phase exists to get right: one compose service,
    // built from this repository. It packages the application, so there
    // is no container option, nothing to ask, and a recorded negative so
    // the question never comes back.
    Recorded {
        kind: Kind::ComposeAppOnly,
        answers: "{}",
        asks: Some(&[]),
        healthy: true,
        expect: |c| {
            assert_eq!(c.services.len(), 1);
            let ServiceConfig::Compose { file, include, .. } = &c.services[0] else {
                panic!("a compose file that packages the app is still a compose entry");
            };
            assert_eq!(file, "docker-compose.yml");
            assert!(
                include.is_empty(),
                "the negative is recorded, not left unanswered: {include:?}"
            );
        },
    },
    // The hybrid of the shape above and the services one below, and the
    // only fixture that exercises both halves of the brief's §5 in a
    // single project: the compose file packages the application, so it
    // is not a container option, and the database the env example
    // addresses has to be proposed as a recipe instead. Nothing is
    // written about the compose file at all — the mechanism chosen was
    // the other one.
    Recorded {
        kind: Kind::ComposeAppAndDatabase,
        answers: r#"{"services": ["postgres"]}"#,
        asks: None,
        healthy: true,
        expect: |c| {
            assert_eq!(c.project.install.as_deref(), Some("npm ci"));
            assert_eq!(c.services.len(), 1, "{:?}", c.services);
            let ServiceConfig::Native { name, env, .. } = &c.services[0] else {
                panic!("a compose file that packages the app runs no private copy of anything");
            };
            assert_eq!(name, "postgres");
            assert_eq!(
                env.keys().collect::<Vec<_>>(),
                ["DATABASE_URL"],
                "the app is told where its own private copy is"
            );
        },
    },
    // A pin no machine resolves. Nothing is asked, everything is
    // configured, and doctor says the one true thing about it.
    Recorded {
        kind: Kind::PinnedRuntime,
        answers: "{}",
        asks: Some(&[]),
        healthy: false,
        expect: |c| {
            assert_eq!(c.runtime.version_files, [".nvmrc"]);
            assert_eq!(c.project.install.as_deref(), Some("npm ci"));
        },
    },
    // Services with nothing in the repository describing them. Whether
    // the engines are installed here decides whether this is a question
    // or a decision; it does not change the answer.
    Recorded {
        kind: Kind::ServicesNoManifest,
        answers: r#"{"services": ["postgres", "redis"]}"#,
        asks: None,
        healthy: true,
        expect: |c| {
            let named: Vec<(&str, Vec<&str>)> = c
                .services
                .iter()
                .map(|s| match s {
                    ServiceConfig::Native { name, env, .. } => {
                        (name.as_str(), env.keys().map(String::as_str).collect())
                    }
                    ServiceConfig::Compose { .. } => {
                        panic!("there is no compose file here to run containers from")
                    }
                })
                .collect();
            assert_eq!(
                named,
                vec![
                    ("postgres", vec!["DATABASE_URL"]),
                    ("redis", vec!["CACHE_URL"])
                ],
                "the engine comes from the URL scheme, and the app is told where it is"
            );
        },
    },
    // A gitignored env that never arrived. Copying a file out of a
    // tracked example is a write nobody has authorised, so `--yes`
    // declines it — and an answers file naming it is a developer
    // authorising it. With no manifest and no script, nothing would run
    // until the dev command is answered too, and `init` asks it.
    Recorded {
        kind: Kind::EnvNeverArrived,
        answers: r#"{"dev_cmd": "./serve.sh", "provision": ".env"}"#,
        asks: Some(&["dev_cmd", "provision"]),
        healthy: true,
        expect: |c| {
            assert_eq!(
                c.project.provision.as_deref(),
                Some([".env".to_string()].as_slice())
            );
            assert_eq!(c.project.provision_from[".env"], ".env.example");
        },
    },
];

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    home: PathBuf,
    /// Stand-ins for the tools this machine may lack, first on PATH.
    tools: PathBuf,
}

fn fixture(kind: Kind) -> Fixture {
    let dir = TempDir::new().unwrap();
    let parent = std::fs::canonicalize(dir.path()).unwrap();
    let root = build(kind, &parent).root;
    let home = parent.join("pando-home");
    let paths = paths_for(&home, &root);
    paths.ensure_home().unwrap();
    // The one question that is about this machine rather than the
    // fixture: pando spawns with `bash -lc`, and whether that shell
    // resolves what a project pins is a fact about the host. Answering
    // it here is what keeps every assertion below the same on every
    // machine — including the one fixture that pins a version nothing
    // resolves, whose whole point is the refusal.
    std::fs::write(home.join("config.toml"), "[runtime]\nprelude = \"\"\n").unwrap();
    // The other: whether this machine has the engines the fixture's env
    // example names, which decides whether the services are ticked.
    common::fake_engines(&home);
    // And whether it has npm, which the npm fixtures install with and
    // doctor asks for: a stand-in, first on the PATH every run gets.
    let tools = parent.join("tools");
    common::fake_npm(&tools);
    Fixture {
        root,
        home,
        tools: tools.join("bin"),
        _dir: dir,
    }
}

impl Fixture {
    fn pando(&self, args: &[&str], stdin: Option<&str>) -> std::process::Output {
        self.pando_in(args, stdin, None)
    }

    /// The same, with `HOME` — the developer's home, where version
    /// managers live — set to `user`.
    fn pando_with_home(&self, args: &[&str], user: &Path) -> std::process::Output {
        self.pando_in(args, None, Some(user))
    }

    fn pando_in(
        &self,
        args: &[&str],
        stdin: Option<&str>,
        user: Option<&Path>,
    ) -> std::process::Output {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_pando"));
        if let Some(user) = user {
            command.env("HOME", user);
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(self.tools.clone()).chain(std::env::split_paths(&path)),
        )
        .unwrap();
        let mut child = command
            .env("PATH", path)
            .env("PANDO_HOME", &self.home)
            .current_dir(&self.root)
            .args(args)
            .stdin(match stdin {
                Some(_) => std::process::Stdio::piped(),
                None => std::process::Stdio::null(),
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("run pando");
        if let Some(text) = stdin {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("a piped stdin")
                .write_all(text.as_bytes())
                .expect("write the answers");
        }
        child.wait_with_output().expect("wait for pando")
    }

    fn config(&self) -> Config {
        let paths = paths_for(&self.home, &self.root);
        let loaded = pando::config::load(&paths).expect("the config pando just wrote");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        loaded.config
    }

    fn decisions(&self) -> Vec<serde_json::Value> {
        let paths = paths_for(&self.home, &self.root);
        let Ok(text) = std::fs::read_to_string(paths.decisions_file()) else {
            return Vec::new();
        };
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("one object per line"))
            .collect()
    }
}

#[test]
fn a_recorded_answers_file_configures_every_hard_shape() {
    for case in RECORDED {
        let name = case.kind.dir_name();
        let fx = fixture(case.kind);

        // The preview first, because the brief tells an agent to show it
        // before writing — and a preview that is not the real renderer
        // is a preview of something else.
        let preview = fx.pando(&["init", "--answers", "-", "--dry-run"], Some(case.answers));
        assert_eq!(
            preview.status.code(),
            Some(0),
            "{name}: dry run failed: {}",
            String::from_utf8_lossy(&preview.stderr)
        );
        let previewed = String::from_utf8_lossy(&preview.stdout).into_owned();

        let out = fx.pando(&["init", "--answers", "-"], Some(case.answers));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{name}: init failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let paths = paths_for(&fx.home, &fx.root);
        let written = std::fs::read_to_string(paths.config_file()).expect("a config file");
        assert!(
            previewed.contains(&written),
            "{name}: the preview was not what landed"
        );
        (case.expect)(&fx.config());

        // Every answer a program gave says so, and every answer a rule
        // gave says that instead.
        if case.asks.is_some_and(|asks| !asks.is_empty()) {
            assert!(
                written.contains("# answered: a program,"),
                "{name}: a key a program answered does not say so: {written}"
            );
        }

        // What the rules could not settle is exactly what the decisions
        // log holds — the corpus the rules get better from.
        if let Some(asks) = case.asks {
            let slots: Vec<String> = fx
                .decisions()
                .iter()
                .map(|d| d["slot"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(
                slots, asks,
                "{name}: the decisions log holds the wrong slots"
            );
        }

        // And then the proof, which is the point of the whole pass. doctor
        // asks the Docker daemon whether it is up, and whether this
        // laptop's is running is not what the corpus is about: the shim
        // pando runs in place of docker answers `info` itself and hands
        // everything else to the real one.
        let shim = fx.home.join("bin").join("docker");
        std::fs::create_dir_all(shim.parent().unwrap()).unwrap();
        std::fs::write(
            &shim,
            "#!/bin/sh\n[ \"$1\" = info ] && { echo 27.0.0; exit 0; }\nexec docker \"$@\"\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let doctor = fx.pando(&["doctor", "--json"], None);
        let report: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&doctor.stdout))
                .unwrap_or_else(|e| panic!("{name}: doctor --json did not parse: {e}"));
        assert_eq!(
            report["ok"], case.healthy,
            "{name}: doctor said {:#?}",
            report["findings"]
        );
        assert_eq!(
            doctor.status.code(),
            Some(if case.healthy { 0 } else { 1 }),
            "{name}: the exit code and `ok` disagree"
        );

        // Not one byte in the repository, on any of those paths.
        assert_eq!(status_porcelain(&fx.root), "", "{name}");
    }
}

/// The one shape that cannot be green, and why that is the fixture
/// working rather than the setup failing.
///
/// It pins a runtime version no machine will ever have. pando configures
/// it completely and then refuses to start it, naming the pin and the
/// line that would fix it — which is the refusal-before-spawn path, and
/// the thing an agent must report rather than work around.
#[test]
fn a_pinned_runtime_nothing_resolves_is_configured_and_then_refused() {
    let fx = fixture(Kind::PinnedRuntime);
    assert_eq!(
        fx.pando(&["init", "--answers", "-"], Some("{}"))
            .status
            .code(),
        Some(0),
        "the project is configurable; it is this machine that cannot run it"
    );

    // Whether a version manager is installed decides what the fix says,
    // and that is a fact about the host: give this one a home with nvm in
    // it, so the fix is the prelude on every machine.
    let user = fx.home.join("user-home");
    std::fs::create_dir_all(user.join(".nvm")).unwrap();
    std::fs::write(user.join(".nvm/nvm.sh"), "# a stand-in nvm\n").unwrap();
    let doctor = fx.pando_with_home(&["doctor", "--json"], &user);
    let report: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&doctor.stdout)).expect("doctor --json");
    let problems: Vec<&serde_json::Value> = report["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .filter(|f| f["severity"] == "problem")
        .collect();
    assert_eq!(problems.len(), 1, "{problems:#?}");
    let message = problems[0]["message"].as_str().unwrap();
    assert!(message.contains("99.0.0"), "{message}");
    assert!(message.contains("node"), "{message}");
    assert!(
        problems[0]["fix"]
            .as_str()
            .is_some_and(|fix| fix.contains("prelude")),
        "and the fix names the one thing that would change it: {problems:#?}"
    );
}
