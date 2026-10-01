//! Fixture repositories for the integration tests.
//!
//! Every mutating test runs against one of these, never against a real
//! repository. They are built under the test's own temp directory and torn
//! down with it.

#![allow(dead_code)]

pub mod docker;
pub mod postgres;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use pando::config::{
    Config, HookConfig, HookPoint, PortsSpec, ProcessConfig, ReadySpec, ServiceConfig,
};
use pando::paths::PandoPaths;
use pando::project::ProjectRef;

/// A fixture identity, so committing works on any machine and never depends
/// on (or picks up) the operator's git config. Fixture repositories only.
const FIXTURE_IDENTITY: [&str; 10] = [
    "-c",
    "user.name=t",
    "-c",
    "user.email=t@t",
    "-c",
    "commit.gpgsign=false",
    "-c",
    "tag.gpgSign=false",
    "-c",
    "init.defaultBranch=main",
];

pub fn git(cwd: &Path, args: &[&str]) {
    let out = git_raw(cwd, args);
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
}

pub fn git_raw(cwd: &Path, args: &[&str]) -> std::process::Output {
    hermetic_home();
    hermetic_path();
    no_auto_maintenance();
    Command::new("git")
        .args(FIXTURE_IDENTITY)
        .current_dir(cwd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|e| panic!("spawn git {args:?}: {e}"))
}

/// Gives this test binary an empty HOME of its own, once, before its first
/// fixture: every fixture is made through [`git_raw`], which calls this.
///
/// pando runs every command through `bash -lc`, and a login shell reads
/// `~/.bash_profile`. With nvm or conda in it that is most of a second per
/// shell, and whatever it puts on PATH becomes what the test tested: the
/// tests that ran a real `pnpm install` passed only on a machine with nvm
/// and the fixture's Node version. The library's own unit tests get the
/// same through `process::login_shell`.
pub fn hermetic_home() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let home = std::env::temp_dir().join("pando-test-home");
        std::fs::create_dir_all(&home).unwrap();
        // SAFETY: std's own environment lock orders this with every other
        // std::env read and with Command's spawn, and nothing in these
        // tests reads the environment through libc behind std's back.
        unsafe { std::env::set_var("HOME", &home) };
    });
}

/// Takes the Windows half out of this test binary's PATH under WSL, once,
/// before its first fixture: every fixture is made through [`git_raw`],
/// which calls this. Anywhere else there is no such half, and PATH is
/// left as it is.
///
/// WSL appends Windows' PATH to the Linux one, so what the developer
/// installed on Windows is on it: Node's installer leaves an `npm` shell
/// script, Docker Desktop a `docker` one. `doctor` reports a tool found on
/// a Windows drive, and its tests, which expect a report about the
/// fixture, got one about the laptop.
pub fn hermetic_path() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let Some(wsl) = pando::wsl::Wsl::at(Path::new("/")) else {
            return;
        };
        let Some(path) = std::env::var_os("PATH") else {
            return;
        };
        let linux: Vec<PathBuf> = std::env::split_paths(&path)
            .filter(|dir| wsl.drive_of(dir).is_none())
            .collect();
        let joined = std::env::join_paths(linux).unwrap();
        // SAFETY: as for HOME above.
        unsafe { std::env::set_var("PATH", joined) };
    });
}

/// Turns git's automatic maintenance off for every git this test binary
/// starts — the fixtures', pando's, and any a process pando runs starts —
/// once, before its first fixture: every fixture is made through
/// [`git_raw`], which calls this.
///
/// A commit, a merge, a rebase or a fetch ends by starting `git maintenance
/// run --auto`, as does the receive-pack a push starts in the repository
/// it pushes to, unless `maintenance.auto` is false. Since git 2.47 that
/// run detaches, and since 2.55 its detached half holds
/// `.git/objects/maintenance.lock` until it is done, so a repository goes
/// on changing after the command that changed it has returned. The two
/// `init --agent` tests in `cli.rs` that compare `.git` before and after
/// failed on it on CI: the lock of the fixture's own commit was there for
/// the first look and gone by the second. No test here is about git's
/// housekeeping.
///
/// In the environment rather than in a config file: git reads
/// `GIT_CONFIG_COUNT` pairs (2.31 and later) as it reads `-c`, over every
/// config file, so no system, global or repository setting turns it back
/// on, and every child inherits them. Added after any pairs the
/// environment already had. The one git they do not reach is the
/// `receive-pack` a push starts in a bare origin, because git clears them
/// for a command it runs in another repository; [`build_with_origin`] turns
/// maintenance off in the origin's own config.
pub fn no_auto_maintenance() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let n: usize = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .and_then(|count| count.parse().ok())
            .unwrap_or(0);
        // SAFETY: as for HOME above. The pair goes in before the count
        // that reaches it, so a git spawned in between never reads a count
        // past its pairs.
        unsafe {
            std::env::set_var(format!("GIT_CONFIG_KEY_{n}"), "maintenance.auto");
            std::env::set_var(format!("GIT_CONFIG_VALUE_{n}"), "false");
            std::env::set_var("GIT_CONFIG_COUNT", (n + 1).to_string());
        }
    });
}

/// Installs a stand-in `pnpm` at `<home>/bin/pnpm`, which pando puts in
/// front of PATH, so a fixture's install and scripts need neither Node nor
/// the network. The fixtures lock nothing, so `install` has nothing to do,
/// as the real one would find; any other word runs that `package.json`
/// script with `sh`, the way pnpm does. Every call is appended to
/// `<home>/bin/pnpm.calls`.
pub fn fake_pnpm(home: &Path) {
    fake_package_manager(home, "pnpm");
}

/// [`fake_pnpm`]'s stand-in as `npm`, for fixtures whose install step is
/// npm's. doctor asks for the program an install step runs, and whether
/// this machine has Node is not what those tests are about: they passed
/// only where npm was installed, and under WSL only because Windows' npm
/// was on PATH.
pub fn fake_npm(home: &Path) {
    fake_package_manager(home, "npm");
}

fn fake_package_manager(home: &Path, name: &str) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join(name);
    std::fs::write(
        &path,
        r#"#!/bin/sh
echo "$*" >> "$0.calls"
case "$1" in
  install|i|ci) exit 0 ;;
  run) shift ;;
esac
script=$1
shift
body=$(python3 -c 'import json, sys; print(json.load(open("package.json"))["scripts"][sys.argv[1]])' "$script" 2>/dev/null) || {
  echo "NAME: no script named $script in package.json" >&2
  exit 1
}
exec sh -c "$body \"\$@\"" NAME "$@"
"#
        .replace("NAME", name),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Installs a stand-in `node` at `<home>/bin/node` that answers `-v` with
/// `v<version>`, for fixtures that pin a Node version pando then checks.
pub fn fake_node(home: &Path, version: &str) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("node");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             case \"$1\" in -v|--version) echo v{version}; exit 0 ;; esac\n\
             echo \"node: a stand-in under test runs nothing\" >&2\n\
             exit 1\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Installs a stand-in at `<home>/bin` for every binary a built-in
/// recipe needs, so the machine probe finds every engine on every host.
///
/// Whether an engine is installed decides whether pando ticks it by
/// default, and so whether `init` asks which services to run. That is a
/// fact about the host, and a test that answers it only on a laptop with
/// Postgres and Redis installed fails on one without them. The stand-ins
/// only have to be found: nothing under these tests starts an engine.
pub fn fake_engines(home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (_, loaded) in pando::recipes::Recipes::built_in().entries() {
        for binary in &loaded.recipe.binaries {
            let path = bin.join(binary);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\necho \"{binary}: a stand-in under test runs nothing\" >&2\nexit 1\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
}

/// `git status --porcelain` output for a checkout. Empty means clean.
pub fn status_porcelain(cwd: &Path) -> String {
    let out = git_raw(cwd, &["status", "--porcelain"]);
    assert!(
        out.status.success(),
        "git status failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Which fixture project to build. Shapes match the fixture catalogue; only
/// the files matter in this phase, since nothing is started yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The smallest repo that satisfies the common fixture contract: a
    /// commit, a gitignore, and an untracked ignored `.env`.
    Plain,
    /// A Next-shaped app with pnpm, compose services, and prisma migrations.
    NextPnpmCompose,
    /// A Django app with uv, a positional port, and split database settings.
    DjangoUvPostgres,
    /// A Go service: no lockfile install, no compose, port by convention.
    GoService,
    /// A library with no dev server at all. Level zero.
    RustLib,
    /// A workspace with a web and an api process, one needing the other's
    /// port.
    MonoWebApi,
    /// The Next fixture with deliberate ambiguity in scripts, ports, and
    /// service names.
    NextMessy,

    // The hard shapes. Every one of them is a real pattern described
    // generically — a shape, never a project. They exist because the
    // corpus *is* pando's validation: real repositories are off limits,
    // and a corpus of tidy shapes validates nothing, since tidy is not
    // what a first run meets.
    /// Several apps under `apps/`, workspaces declared with
    /// `package.json`'s own `workspaces` key rather than a workspace
    /// file, and no lockfile at all.
    WorkspaceNoLock,
    /// Ports declared only in the env example — two the app serves on,
    /// two its services listen on — with a dev script that reads them
    /// from the environment rather than taking a flag.
    EnvPorts,
    /// A compose file that packages only the application: one service
    /// built from this repository, with a bind mount and a healthcheck,
    /// and every database entry commented out.
    ComposeAppOnly,
    /// A version file pinning a runtime no machine will have, and an
    /// `engines` range that disagrees with it.
    PinnedRuntime,
    /// A project that needs services with no manifest describing them:
    /// the env example names a database and a cache, and nothing in the
    /// repository says how to run either.
    ServicesNoManifest,
    /// A gitignored env file that never arrived, with the example beside
    /// it.
    EnvNeverArrived,
    /// The hybrid: a compose file that packages only the application,
    /// and an env example that addresses a database the compose file
    /// says nothing about. A compose file is not automatically a
    /// container option, so the database has to be proposed natively.
    ComposeAppAndDatabase,
}

impl Kind {
    pub fn parse(name: &str) -> Option<Kind> {
        Some(match name {
            "plain" => Kind::Plain,
            "next-pnpm-compose" => Kind::NextPnpmCompose,
            "django-uv-postgres" => Kind::DjangoUvPostgres,
            "go-service" => Kind::GoService,
            "rust-lib" => Kind::RustLib,
            "mono-web-api" => Kind::MonoWebApi,
            "next-messy" => Kind::NextMessy,
            "workspace-no-lock" => Kind::WorkspaceNoLock,
            "env-ports" => Kind::EnvPorts,
            "compose-app-only" => Kind::ComposeAppOnly,
            "pinned-runtime" => Kind::PinnedRuntime,
            "services-no-manifest" => Kind::ServicesNoManifest,
            "env-never-arrived" => Kind::EnvNeverArrived,
            "compose-app-and-database" => Kind::ComposeAppAndDatabase,
            _ => return None,
        })
    }

    pub fn dir_name(self) -> &'static str {
        match self {
            Kind::Plain => "plain",
            Kind::NextPnpmCompose => "next-pnpm-compose",
            Kind::DjangoUvPostgres => "django-uv-postgres",
            Kind::GoService => "go-service",
            Kind::RustLib => "rust-lib",
            Kind::MonoWebApi => "mono-web-api",
            Kind::NextMessy => "next-messy",
            Kind::WorkspaceNoLock => "workspace-no-lock",
            Kind::EnvPorts => "env-ports",
            Kind::ComposeAppOnly => "compose-app-only",
            Kind::PinnedRuntime => "pinned-runtime",
            Kind::ServicesNoManifest => "services-no-manifest",
            Kind::EnvNeverArrived => "env-never-arrived",
            Kind::ComposeAppAndDatabase => "compose-app-and-database",
        }
    }

    /// The config detection should arrive at for this fixture, for the
    /// slots this phase fills: install, runtime version files, the dev
    /// command, its port role, and provision.
    ///
    /// Written by hand from `fixtures.md` before detection existed, and
    /// compared structurally — never as TOML text, because key order and
    /// comments would make that fragile. Services, hooks and probes are
    /// later phases and are left empty here.
    pub fn expected_config(self) -> Config {
        let mut config = Config::default();
        match self {
            Kind::Plain => {
                config.project.provision = Some(strings(&[".env", ".env.local"]));
            }
            Kind::NextPnpmCompose | Kind::NextMessy => {
                config.project.install = Some("pnpm install --frozen-lockfile".to_string());
                config.project.provision = Some(strings(&[".env", ".env.local"]));
                config.runtime.version_files = strings(&[".nvmrc"]);
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        cmd: "pnpm dev".to_string(),
                        ports: Some(port_env("PORT")),
                        ..Default::default()
                    },
                );
                if self == Kind::NextPnpmCompose {
                    // `mailpit` is offered unticked: it is a mail catcher,
                    // and nothing in the env example names it.
                    config.services.push(compose_service(
                        &["postgres", "redis"],
                        &[("DATABASE_URL", "postgres"), ("REDIS_URL", "redis")],
                    ));
                    config.hooks.push(migrate_hook(
                        &["prisma/migrations/**"],
                        "pnpm prisma migrate deploy",
                    ));
                } else {
                    // `db` and `mail` resolve by rule; `cache` and `queue`
                    // have nothing pointing at them, so they are the
                    // question, and declining them is the answer taken here.
                    config.services.push(compose_service(
                        &["db", "mail"],
                        &[("DB_PORT", "db"), ("SMTP_PORT", "mail")],
                    ));
                }
            }
            Kind::DjangoUvPostgres => {
                config.project.install = Some("uv sync --frozen".to_string());
                config.project.provision = Some(strings(&[".env"]));
                config.runtime.version_files = strings(&[".python-version"]);
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        // Positional: Django takes the port on the command
                        // line, so there is no environment variable to pick.
                        cmd: "uv run python manage.py runserver 127.0.0.1:{port:web}".to_string(),
                        ports: Some(PortsSpec::List(strings(&["web"]))),
                        ..Default::default()
                    },
                );
                // `DB_PORT` is a plain value, so it becomes the bare port;
                // `REDIS_URL` is a URL, so its port is rewritten in place.
                config.services.push(compose_service(
                    &["db", "redis"],
                    &[("DB_PORT", "db"), ("REDIS_URL", "redis")],
                ));
                config.hooks.push(migrate_hook(
                    &["**/migrations/*.py"],
                    "uv run python manage.py migrate",
                ));
            }
            Kind::GoService => {
                // No install step: `go run` resolves its own modules.
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        cmd: "go run .".to_string(),
                        ports: Some(port_env("PORT")),
                        ..Default::default()
                    },
                );
            }
            // Level zero: a library has nothing to serve, so detection
            // proposes nothing and asks nothing.
            Kind::RustLib => {}
            // A workspace declared with `package.json`'s own
            // `workspaces` key, and no lockfile anywhere. Two facts:
            // `apps/*` is found and becomes a process per app — which is
            // a question, not a decision, so the shape is asked about —
            // and with no lockfile there is no frozen install to
            // propose, so the slot stays silent rather than guessing.
            Kind::WorkspaceNoLock => {
                // The root `.env` for each app too: neither has its own.
                config.project.provision =
                    Some(strings(&[".env", "apps/api/.env", "apps/web/.env"]));
                config.project.provision_from = BTreeMap::from([
                    ("apps/api/.env".to_string(), ".env".to_string()),
                    ("apps/web/.env".to_string(), ".env".to_string()),
                ]);
                for app in ["api", "web"] {
                    config.processes.insert(
                        app.to_string(),
                        ProcessConfig {
                            cmd: "npm run dev".to_string(),
                            ports: Some(PortsSpec::List(strings(&[app]))),
                            cwd: Some(format!("apps/{app}")),
                            // The env example's `<APP>_PORT` beside the
                            // Node convention: both carry the one port.
                            // Every app is also told the other's variable.
                            env: BTreeMap::from([
                                ("PORT".to_string(), format!("{{port:{app}}}")),
                                ("API_PORT".to_string(), "{port:api}".to_string()),
                                ("WEB_PORT".to_string(), "{port:web}".to_string()),
                            ]),
                            ready: Some(ReadySpec {
                                role: Some(app.to_string()),
                                timeout_s: None,
                            }),
                            page: None,
                        },
                    );
                }
            }
            // Ports declared only in the env example: two the app serves
            // on and two its services listen on. The env example beats
            // the framework guess, one project gives two app roles, and
            // the two service addresses become native recipes because
            // nothing in the repository says how to run them.
            Kind::EnvPorts => {
                config.project.install = Some("npm ci".to_string());
                config.project.provision = Some(strings(&[".env"]));
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        cmd: "npm run dev".to_string(),
                        ports: Some(PortsSpec::Map(BTreeMap::from([
                            ("ADMIN_PORT".to_string(), "admin".to_string()),
                            ("WEB_PORT".to_string(), "web".to_string()),
                        ]))),
                        ..Default::default()
                    },
                );
                config
                    .services
                    .push(native_service("postgres", "DATABASE_URL"));
                config.services.push(native_service("redis", "CACHE_URL"));
            }
            // A compose file that packages only the application. The
            // build-from-this-repository filter leaves nothing to offer,
            // and the recorded negative — an entry naming the file with
            // an empty `include` — is what stops the question coming
            // back on every isolated start.
            Kind::ComposeAppOnly => {
                config.project.install = Some("npm ci".to_string());
                config.project.provision = Some(strings(&[".env"]));
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        cmd: "npm run dev".to_string(),
                        ports: Some(port_env("PORT")),
                        ..Default::default()
                    },
                );
                config.services.push(ServiceConfig::Compose {
                    file: "docker-compose.yml".to_string(),
                    include: Vec::new(),
                    env: BTreeMap::new(),
                    ready_timeout_s: None,
                });
            }
            // A pinned runtime no machine resolves, and an `engines`
            // range that disagrees with it. Detection records which file
            // pins it; the disagreement is the runtime check's business,
            // and the refusal happens before the first spawn.
            Kind::PinnedRuntime => {
                config.project.install = Some("npm ci".to_string());
                config.project.provision = Some(strings(&[".env"]));
                config.runtime.version_files = strings(&[".nvmrc"]);
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        cmd: "npm run dev".to_string(),
                        ports: Some(port_env("PORT")),
                        ..Default::default()
                    },
                );
            }
            // The shape Phase 6 exists for: a database and a cache the
            // app plainly talks to, and not one file saying how either
            // is run. No compose file means no container option at all,
            // so the recipes are proposed without a preference being
            // needed to break any tie.
            Kind::ServicesNoManifest => {
                config.project.install = Some("npm ci".to_string());
                config.project.provision = Some(strings(&[".env"]));
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        cmd: "npm run dev".to_string(),
                        ports: Some(port_env("PORT")),
                        ..Default::default()
                    },
                );
                config
                    .services
                    .push(native_service("postgres", "DATABASE_URL"));
                config.services.push(native_service("redis", "CACHE_URL"));
            }
            // A gitignored env that never arrived. There is no manifest
            // and no script, so the dev command stays empty and is
            // asked about; what the shape is really for is the seed —
            // `.env` from `.env.example`, recorded as a copy.
            Kind::EnvNeverArrived => {
                config.project.provision = Some(strings(&[".env"]));
                config.project.provision_from =
                    BTreeMap::from([(".env".to_string(), ".env.example".to_string())]);
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        ports: Some(port_env("PORT")),
                        ..Default::default()
                    },
                );
            }
            // A compose file that packages the application, and a
            // database it says nothing about. The build-from-this-
            // repository filter leaves the compose file offering
            // nothing, so there is no container option to weigh at all —
            // and the env example's address is still a declaration, so
            // the database is proposed as a recipe. No compose entry is
            // written: the mechanism chosen was the other one, and the
            // native answer closes the slot on its own.
            Kind::ComposeAppAndDatabase => {
                config.project.install = Some("npm ci".to_string());
                config.project.provision = Some(strings(&[".env"]));
                config.processes.insert(
                    "dev".to_string(),
                    ProcessConfig {
                        cmd: "npm run dev".to_string(),
                        ports: Some(port_env("PORT")),
                        ..Default::default()
                    },
                );
                config
                    .services
                    .push(native_service("postgres", "DATABASE_URL"));
            }
            Kind::MonoWebApi => {
                config.project.install = Some("pnpm install --frozen-lockfile".to_string());
                // The root `.env` for each app as well: neither has an env
                // file of its own, and an app that loads `.env` from its
                // working directory would find none.
                config.project.provision =
                    Some(strings(&[".env", "apps/api/.env", "apps/web/.env"]));
                config.project.provision_from = std::collections::BTreeMap::from([
                    ("apps/api/.env".to_string(), ".env".to_string()),
                    ("apps/web/.env".to_string(), ".env".to_string()),
                ]);
                // Two processes, each in its own directory, with the web
                // one told the api's port. The root `dev` script is a
                // `pnpm -r` wrapper, which works but gives one log and one
                // readiness rule for two servers.
                config.processes.insert(
                    "web".to_string(),
                    ProcessConfig {
                        // Vite takes its port on the command line, so the
                        // flag is appended to the app's own dev script.
                        cmd: "pnpm dev --port {port:web}".to_string(),
                        cwd: Some("apps/web".to_string()),
                        ports: Some(PortsSpec::List(strings(&["web"]))),
                        // The reason `{port:<role>}` exists: the web app
                        // has to be told the port the api was given in
                        // this worktree.
                        // And `API_PORT`, the api's own variable in the
                        // env example: whatever reads it finds this
                        // worktree's api, not the default port.
                        env: std::collections::BTreeMap::from([
                            (
                                "VITE_API_URL".to_string(),
                                "http://localhost:{port:api}".to_string(),
                            ),
                            ("API_PORT".to_string(), "{port:api}".to_string()),
                        ]),
                        ready: Some(ReadySpec {
                            role: Some("web".to_string()),
                            timeout_s: None,
                        }),
                        page: None,
                    },
                );
                config.processes.insert(
                    "api".to_string(),
                    ProcessConfig {
                        cmd: "pnpm dev".to_string(),
                        cwd: Some("apps/api".to_string()),
                        ports: Some(PortsSpec::List(strings(&["api"]))),
                        // `API_PORT` from the env example, beside the
                        // Node convention the app itself reads.
                        // `WEB_PORT` is the web app's, told to the api
                        // the same way.
                        env: std::collections::BTreeMap::from([
                            ("PORT".to_string(), "{port:api}".to_string()),
                            ("API_PORT".to_string(), "{port:api}".to_string()),
                            ("WEB_PORT".to_string(), "{port:web}".to_string()),
                        ]),
                        ready: Some(ReadySpec {
                            role: Some("api".to_string()),
                            timeout_s: None,
                        }),
                        page: None,
                    },
                );
                config.services.push(compose_service(
                    &["postgres"],
                    &[("DATABASE_URL", "postgres")],
                ));
            }
        }
        config
    }

    pub const ALL: [Kind; 14] = [
        Kind::Plain,
        Kind::NextPnpmCompose,
        Kind::DjangoUvPostgres,
        Kind::GoService,
        Kind::RustLib,
        Kind::MonoWebApi,
        Kind::NextMessy,
        Kind::WorkspaceNoLock,
        Kind::EnvPorts,
        Kind::ComposeAppOnly,
        Kind::PinnedRuntime,
        Kind::ServicesNoManifest,
        Kind::EnvNeverArrived,
        Kind::ComposeAppAndDatabase,
    ];
}

pub struct Fixture {
    /// The main checkout.
    pub root: PathBuf,
    /// The bare repository acting as `origin`, when the fixture has one.
    pub remote: Option<PathBuf>,
}

/// The default fixture: a repo with one commit, a gitignore, and an
/// untracked ignored `.env`.
pub fn fixture_repo(parent: &Path) -> PathBuf {
    build(Kind::Plain, parent).root
}

/// Builds a fixture under `parent` and returns its paths. Every kind gets a
/// commit, a `.gitignore`, and any ignored files its config would provision.
pub fn build(kind: Kind, parent: &Path) -> Fixture {
    build_with(kind, parent, true)
}

/// The same fixture as a fresh clone leaves it: everything tracked, and
/// none of the gitignored local files, because those are gitignored and
/// never arrive with a clone. The state a project's own `.env.example` is
/// the only source of local settings in.
pub fn build_fresh_clone(kind: Kind, parent: &Path) -> Fixture {
    build_with(kind, parent, false)
}

fn build_with(kind: Kind, parent: &Path, local_files: bool) -> Fixture {
    let root = parent.join(kind.dir_name());
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet", "--initial-branch=main"]);
    for (rel, contents) in files_for(kind) {
        write_file(&root, rel, contents);
    }
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "initial commit"]);
    if local_files {
        for (rel, contents) in ignored_files_for(kind) {
            write_file(&root, rel, contents);
        }
    }
    Fixture { root, remote: None }
}

/// The same fixture plus a bare `origin` it was cloned from, so
/// remote-tracking refs exist.
pub fn build_with_origin(kind: Kind, parent: &Path) -> Fixture {
    let bare = parent.join(format!("{}-origin.git", kind.dir_name()));
    git(
        parent,
        &[
            "init",
            "--bare",
            "--quiet",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    // A push's receive-pack gets no config from the environment: see
    // `no_auto_maintenance`.
    git(&bare, &["config", "maintenance.auto", "false"]);
    let seed = build(kind, &parent.join("seed"));
    git(
        &seed.root,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git(&seed.root, &["push", "--quiet", "origin", "main"]);

    let root = parent.join(kind.dir_name());
    git(
        parent,
        &[
            "clone",
            "--quiet",
            bare.to_str().unwrap(),
            root.to_str().unwrap(),
        ],
    );
    for (rel, contents) in ignored_files_for(kind) {
        write_file(&root, rel, contents);
    }
    Fixture {
        root,
        remote: Some(bare),
    }
}

fn write_file(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&path, contents).unwrap();
}

/// Paths pointing at an injected pando home, so no test can reach the real
/// `~/.pando`.
pub fn paths_for(home: &Path, root: &Path) -> PandoPaths {
    PandoPaths::new(home, ProjectRef::from_root(root).unwrap())
}

/// The lockfile pnpm writes for a manifest with no dependencies, byte for
/// byte (pnpm 12.5.1). A stub is not good enough: pnpm rewrites one it
/// considers malformed even under `--frozen-lockfile`, and then the
/// fixture's own worktree is dirty for reasons that have nothing to do
/// with what is being tested.
const PNPM_LOCK: &str = "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  \
                         excludeLinksFromLockfile: false\n\nimporters:\n\n  .: {}\n";

/// The same for a workspace: pnpm lists every importer it resolved.
const PNPM_LOCK_WORKSPACE: &str = "lockfileVersion: '9.0'\n\nsettings:\n  autoInstallPeers: true\n  \
     excludeLinksFromLockfile: false\n\nimporters:\n\n  .: {}\n\n  apps/api: {}\n\n  \
     apps/web: {}\n";

/// Tracked files, written before the initial commit.
/// The `[[services]] kind = "native"` entry a detected address writes:
/// the recipe is implied by the name, and the env key points at it.
fn native_service(name: &str, env_key: &str) -> ServiceConfig {
    ServiceConfig::Native {
        name: name.to_string(),
        preset: None,
        port_env: None,
        init: None,
        cmd: None,
        ready: None,
        ready_timeout_s: None,
        env: BTreeMap::from([(env_key.to_string(), name.to_string())]),
    }
}

fn files_for(kind: Kind) -> Vec<(&'static str, &'static str)> {
    match kind {
        Kind::Plain => vec![
            (".gitignore", ".env\n.env.local\nnode_modules/\n"),
            ("README.md", "# plain fixture\n"),
        ],
        Kind::NextPnpmCompose => vec![
            (
                "package.json",
                r#"{
  "name": "next-pnpm-compose",
  "scripts": {
    "dev": "next dev",
    "build": "next build",
    "start": "next start",
    "lint": "next lint",
    "prisma": "echo prisma-stub"
  }
}
"#,
            ),
            ("pnpm-lock.yaml", PNPM_LOCK),
            (".nvmrc", "22\n"),
            (
                ".env.example",
                "PORT=3000\nDATABASE_URL=postgres://acme:acme@localhost:5432/acme\nREDIS_URL=redis://localhost:6379\n",
            ),
            (
                "docker-compose.yml",
                // The password is what makes this file one docker can
                // really bring up: the official postgres image refuses to
                // initialise without one, so a fixture that leaves it out
                // cannot be started by hand or by the demo.
                r#"services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_PASSWORD: acme
    ports: ["5432:5432"]
    volumes:
      - pgdata:/var/lib/postgresql/data
  redis:
    image: redis:7
    ports: ["6379:6379"]
    volumes:
      - redisdata:/data
  mailpit:
    image: axllent/mailpit
    ports: ["1025:1025"]
volumes:
  pgdata:
  redisdata:
"#,
            ),
            (
                "prisma/schema.prisma",
                "datasource db {\n  provider = \"postgresql\"\n  url = env(\"DATABASE_URL\")\n}\n",
            ),
            (
                "prisma/migrations/20260101000000_init/migration.sql",
                "CREATE TABLE item (id SERIAL PRIMARY KEY);\n",
            ),
            (".gitignore", ".env\n.env.local\nnode_modules/\n.next/\n"),
        ],
        Kind::DjangoUvPostgres => vec![
            (
                "pyproject.toml",
                "[project]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"django\"]\n",
            ),
            ("uv.lock", "version = 1\n"),
            (".python-version", "3.12\n"),
            (
                "manage.py",
                "#!/usr/bin/env python\nimport sys\n\nif __name__ == \"__main__\":\n    sys.exit(0)\n",
            ),
            ("app/__init__.py", ""),
            ("app/migrations/__init__.py", ""),
            ("app/migrations/0001_initial.py", "# initial migration\n"),
            (
                ".env.example",
                "DB_HOST=localhost\nDB_PORT=5432\nDB_NAME=app\nREDIS_URL=redis://localhost:6379/0\n",
            ),
            (
                "docker-compose.yml",
                r#"services:
  db:
    image: postgres:16
    ports: ["5432:5432"]
  redis:
    image: redis:7
    ports: ["6379:6379"]
"#,
            ),
            (".gitignore", ".env\n.venv/\n__pycache__/\n"),
        ],
        Kind::GoService => vec![
            ("go.mod", "module example.test/service\n\ngo 1.23\n"),
            ("go.sum", ""),
            (
                "main.go",
                r#"package main

import (
    "net/http"
    "os"
)

func main() {
    port := os.Getenv("PORT")
    http.ListenAndServe(":"+port, nil)
}
"#,
            ),
            ("Makefile", "run:\n\tgo run .\n"),
            (".gitignore", "bin/\n"),
        ],
        Kind::RustLib => vec![
            (
                "Cargo.toml",
                "[package]\nname = \"fixture-lib\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"src/lib.rs\"\n",
            ),
            ("Cargo.lock", "version = 3\n"),
            ("src/lib.rs", "pub fn answer() -> u32 {\n    42\n}\n"),
            (".gitignore", "target/\n"),
        ],
        Kind::MonoWebApi => vec![
            (
                "package.json",
                r#"{
  "name": "mono-web-api",
  "private": true,
  "workspaces": ["apps/*"],
  "scripts": { "dev": "pnpm -r --parallel dev" }
}
"#,
            ),
            ("pnpm-workspace.yaml", "packages:\n  - 'apps/*'\n"),
            ("pnpm-lock.yaml", PNPM_LOCK_WORKSPACE),
            (
                "apps/web/package.json",
                "{\n  \"name\": \"web\",\n  \"scripts\": { \"dev\": \"vite\" }\n}\n",
            ),
            (
                "apps/web/vite.config.ts",
                "export default { server: { port: Number(process.env.WEB_PORT) } }\n",
            ),
            (
                "apps/api/package.json",
                "{\n  \"name\": \"api\",\n  \"scripts\": { \"dev\": \"node --watch src/index.js\" }\n}\n",
            ),
            (
                "apps/api/src/index.js",
                "const port = process.env.PORT;\nconsole.log('api on', port);\n",
            ),
            (
                ".env.example",
                "WEB_PORT=5173\nAPI_PORT=4000\nVITE_API_URL=http://localhost:4000\nDATABASE_URL=postgres://app:app@localhost:5432/app\n",
            ),
            (
                "docker-compose.yml",
                "services:\n  postgres:\n    image: postgres:16\n    ports: [\"5432:5432\"]\n",
            ),
            (".gitignore", ".env\nnode_modules/\ndist/\n"),
        ],
        Kind::WorkspaceNoLock => vec![
            (".gitignore", ".env\nnode_modules/\n"),
            (
                "package.json",
                r#"{
  "name": "workspace-no-lock",
  "private": true,
  "workspaces": ["apps/*"],
  "scripts": {
    "dev": "npm run dev --workspaces"
  }
}
"#,
            ),
            (
                "apps/web/package.json",
                "{\n  \"name\": \"web\",\n  \"scripts\": { \"dev\": \"node server.js\" }\n}\n",
            ),
            (
                "apps/api/package.json",
                "{\n  \"name\": \"api\",\n  \"scripts\": { \"dev\": \"node server.js\" }\n}\n",
            ),
            ("apps/web/server.js", "// a server\n"),
            ("apps/api/server.js", "// a server\n"),
            (".env.example", "WEB_PORT=3000\nAPI_PORT=3001\n"),
        ],
        Kind::EnvPorts => vec![
            (".gitignore", ".env\nnode_modules/\n"),
            (
                "package.json",
                r#"{
  "name": "env-ports",
  "scripts": {
    "dev": "node server.js"
  }
}
"#,
            ),
            ("package-lock.json", "{\n  \"lockfileVersion\": 3\n}\n"),
            (
                "server.js",
                "// reads WEB_PORT and ADMIN_PORT from the environment\n",
            ),
            (
                ".env.example",
                "WEB_PORT=3000\nADMIN_PORT=3001\n\
                 DATABASE_URL=postgres://user:pass@localhost:5432/appdb\n\
                 CACHE_URL=redis://localhost:6379\n",
            ),
        ],
        Kind::ComposeAppOnly => vec![
            (".gitignore", ".env\nnode_modules/\n"),
            (
                "package.json",
                "{\n  \"name\": \"compose-app-only\",\n  \
                 \"scripts\": { \"dev\": \"node server.js\" }\n}\n",
            ),
            ("package-lock.json", "{\n  \"lockfileVersion\": 3\n}\n"),
            ("server.js", "// a server\n"),
            ("Dockerfile", "FROM scratch\n"),
            (
                "docker-compose.yml",
                r#"services:
  app:
    build: .
    volumes:
      - .:/srv
    ports:
      - "3000:3000"
    healthcheck:
      test: ["CMD", "true"]
      interval: 5s
  # db:
  #   image: postgres:16
  # cache:
  #   image: redis:7
"#,
            ),
            (".env.example", "PORT=3000\n"),
        ],
        Kind::PinnedRuntime => vec![
            (".gitignore", ".env\nnode_modules/\n"),
            // A version nothing will ever resolve, so the refusal path is
            // exercised without depending on what the host has installed.
            (".nvmrc", "99.0.0\n"),
            (
                "package.json",
                r#"{
  "name": "pinned-runtime",
  "engines": { "node": ">=18 <21" },
  "scripts": {
    "dev": "node server.js"
  }
}
"#,
            ),
            ("package-lock.json", "{\n  \"lockfileVersion\": 3\n}\n"),
            ("server.js", "// a server\n"),
            (".env.example", "PORT=3000\n"),
        ],
        Kind::ServicesNoManifest => vec![
            (".gitignore", ".env\nnode_modules/\n"),
            (
                "package.json",
                "{\n  \"name\": \"services-no-manifest\",\n  \
                 \"scripts\": { \"dev\": \"node server.js\" }\n}\n",
            ),
            ("package-lock.json", "{\n  \"lockfileVersion\": 3\n}\n"),
            ("server.js", "// a server\n"),
            // A database and a cache the app plainly talks to, and not one
            // file in the repository saying how either of them is run.
            (
                ".env.example",
                "PORT=3000\n\
                 DATABASE_URL=postgres://user:pass@localhost:5432/appdb\n\
                 CACHE_URL=redis://localhost:6379\n",
            ),
        ],
        Kind::EnvNeverArrived => vec![
            (".gitignore", ".env\n"),
            (
                "README.md",
                "# a repository whose .env never arrives with a clone\n",
            ),
            (".env.example", "PORT=3000\nSECRET=replace-me\n"),
        ],
        // A compose file that packages the application — and nothing
        // else, not even commented out — beside an env example that
        // plainly addresses a database. The two halves of the hybrid.
        Kind::ComposeAppAndDatabase => vec![
            (".gitignore", ".env\nnode_modules/\n"),
            (
                "package.json",
                "{\n  \"name\": \"compose-app-and-database\",\n  \
                 \"scripts\": { \"dev\": \"node server.js\" }\n}\n",
            ),
            ("package-lock.json", "{\n  \"lockfileVersion\": 3\n}\n"),
            ("server.js", "// a server\n"),
            ("Dockerfile", "FROM scratch\n"),
            (
                "docker-compose.yml",
                r#"services:
  app:
    build: .
    volumes:
      - .:/srv
    ports:
      - "3000:3000"
    healthcheck:
      test: ["CMD", "true"]
      interval: 5s
"#,
            ),
            (
                ".env.example",
                "PORT=3000\nDATABASE_URL=postgres://user:pass@localhost:5432/appdb\n",
            ),
        ],
        Kind::NextMessy => vec![
            (
                "package.json",
                r#"{
  "name": "next-messy",
  "scripts": {
    "dev": "concurrently \"npm:dev:*\"",
    "dev:web": "next dev",
    "dev:worker": "node worker.js",
    "dev:all": "./scripts/dev.sh",
    "start": "next start",
    "serve": "serve out",
    "preview": "next start -p 4000"
  }
}
"#,
            ),
            ("pnpm-lock.yaml", PNPM_LOCK),
            (".nvmrc", "22\n"),
            ("worker.js", "setInterval(() => {}, 1000);\n"),
            ("scripts/dev.sh", "#!/bin/sh\nexec npm run dev:web\n"),
            (
                ".env.example",
                "PORT=3000\nAPI_PORT=3001\nVITE_PORT=5173\nDB_PORT=5432\nSMTP_PORT=1025\n",
            ),
            (
                "docker-compose.yml",
                r#"services:
  db:
    image: postgres:16
    ports: ["5432:5432"]
  cache:
    image: redis:7
    ports: ["6379:6379"]
  queue:
    image: rabbitmq:3
    ports: ["5672:5672"]
  mail:
    image: axllent/mailpit
    ports: ["1025:1025"]
"#,
            ),
            (".gitignore", ".env\n.env.local\nnode_modules/\n.next/\n"),
        ],
    }
}

/// Untracked but gitignored files, written after the initial commit, so
/// provisioning has something to link and `git status` stays clean.
fn ignored_files_for(kind: Kind) -> Vec<(&'static str, &'static str)> {
    match kind {
        Kind::Plain => vec![(".env", "SECRET=1\n"), (".env.local", "LOCAL=1\n")],
        Kind::NextPnpmCompose | Kind::NextMessy => vec![
            (
                ".env",
                "PORT=3000\nDATABASE_URL=postgres://acme:acme@localhost:5432/acme\nREDIS_URL=redis://localhost:6379\n",
            ),
            (".env.local", "NEXT_PUBLIC_FLAG=1\n"),
        ],
        Kind::DjangoUvPostgres => vec![(
            ".env",
            "DB_HOST=localhost\nDB_PORT=5432\nDB_NAME=app\nREDIS_URL=redis://localhost:6379/0\n",
        )],
        Kind::MonoWebApi => vec![(
            ".env",
            "WEB_PORT=5173\nAPI_PORT=4000\nVITE_API_URL=http://localhost:4000\n",
        )],
        Kind::WorkspaceNoLock => vec![(".env", "WEB_PORT=3000\nAPI_PORT=3001\n")],
        Kind::EnvPorts => vec![(
            ".env",
            "WEB_PORT=3000\nADMIN_PORT=3001\n\
             DATABASE_URL=postgres://user:pass@localhost:5432/appdb\n\
             CACHE_URL=redis://localhost:6379\n",
        )],
        Kind::ComposeAppOnly | Kind::PinnedRuntime => vec![(".env", "PORT=3000\n")],
        Kind::ComposeAppAndDatabase => vec![(
            ".env",
            "PORT=3000\nDATABASE_URL=postgres://user:pass@localhost:5432/appdb\n",
        )],
        Kind::ServicesNoManifest => vec![(
            ".env",
            "PORT=3000\nDATABASE_URL=postgres://user:pass@localhost:5432/appdb\n\
             CACHE_URL=redis://localhost:6379\n",
        )],
        // The whole point of this one: the ignored file never arrived.
        Kind::EnvNeverArrived => vec![],
        Kind::GoService | Kind::RustLib => vec![],
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| v.to_string()).collect()
}

/// The `[[services]]` entry detection writes for a fixture's compose file.
fn compose_service(include: &[&str], env: &[(&str, &str)]) -> ServiceConfig {
    ServiceConfig::Compose {
        file: "docker-compose.yml".to_string(),
        include: strings(include),
        env: env
            .iter()
            .map(|(key, service)| (key.to_string(), service.to_string()))
            .collect(),
        ready_timeout_s: None,
    }
}

/// The `[[hooks]]` entry detection writes for a project's schema step.
fn migrate_hook(fingerprint: &[&str], cmd: &str) -> HookConfig {
    HookConfig {
        name: "migrate".to_string(),
        after: HookPoint::Services,
        fingerprint: strings(fingerprint),
        cmd: cmd.to_string(),
        cwd: None,
        fallback: None,
        on: Some(pando::config::HookScope::Isolated),
    }
}

/// `ports = { <VAR> = "web" }`: the sugar for one role reached through an
/// environment variable.
fn port_env(var: &str) -> PortsSpec {
    PortsSpec::Map(std::collections::BTreeMap::from([(
        var.to_string(),
        "web".to_string(),
    )]))
}

/// Whether `python3` is on PATH. Tests that need a process which really
/// binds a port skip with a message rather than failing on a machine
/// without it.
pub fn python3_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A dev "server" for tests and demos: it binds its port, says so, and
/// stays up. Real enough to drive readiness, observed ports, and the log
/// tail, and available on every machine that has python3.
///
/// Deliberately free of `{` and `}` except the placeholder itself: braces
/// are pando's template syntax, and a command full of them would need
/// escaping everywhere it appears.
pub fn listener_on_port_env() -> String {
    "python3 -u -c \"import os,socket,time;p=int(os.environ['PORT']);s=socket.socket();\
     s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('127.0.0.1',p));\
     s.listen(5);print('listening on',p);time.sleep(3600)\""
        .to_string()
}

/// The same listener with its port coming from a template rather than the
/// environment — the positional shape Django and Rails use.
pub fn listener_on_port_template() -> String {
    "python3 -u -c \"import socket,time;s=socket.socket();\
     s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);s.bind(('127.0.0.1',{port:web}));\
     s.listen(5);print('listening on {port:web}');time.sleep(3600)\""
        .to_string()
}

/// A listener that prints an environment variable before it binds, so what
/// one process was told about another is visible in its log.
///
/// Brace-free except the placeholder: `{` is pando's template syntax.
pub fn listener_printing(var: &str) -> String {
    format!(
        "python3 -u -c \"import os,socket,time;\
         print('{var}=' + os.environ['{var}']);s=socket.socket();\
         s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);\
         s.bind(('127.0.0.1',{{port:web}}));s.listen(5);\
         print('listening on {{port:web}}');time.sleep(3600)\""
    )
}

/// The pando-home config the `--listener` fixture writes for the workspace
/// fixture: two processes, each in its own directory, with the web one told
/// the api's port through a template — and printing it, so the
/// cross-process reference is visible in its own log.
pub fn workspace_listener_config() -> String {
    let mut out = String::new();
    out.push_str("# written by scripts/fixture-repo.sh --listener\n");
    out.push_str("[project]\nprovision = [\".env\"]\ninstall = \"true\"\n\n");
    out.push_str("[processes.web]\ncwd = \"apps/web\"\ncmd = '''");
    out.push_str(&listener_printing("VITE_API_URL"));
    out.push_str("'''\nports = [\"web\"]\n");
    out.push_str("env = { VITE_API_URL = \"http://localhost:{port:api}\" }\n");
    out.push_str("ready = { role = \"web\" }\n\n");
    out.push_str("[processes.api]\ncwd = \"apps/api\"\ncmd = '''");
    out.push_str(&listener_on_port_env());
    out.push_str("'''\nports = [\"api\"]\nenv = { PORT = \"{port:api}\" }\n");
    out.push_str("ready = { role = \"api\" }\n");
    out
}

/// The listener config for a fixture: the workspace gets two processes,
/// everything else the single one.
pub fn listener_config_for(kind: Kind) -> String {
    match kind {
        Kind::MonoWebApi => workspace_listener_config(),
        _ => listener_config(),
    }
}

/// The pando-home config the `--listener` fixture writes: a dev process
/// that needs no framework installed and no real server.
///
/// The command is a TOML literal string, so the quotes inside it survive
/// exactly as written.
pub fn listener_config() -> String {
    let mut out = String::new();
    out.push_str("# written by scripts/fixture-repo.sh --listener\n");
    out.push_str("[project]\nprovision = [\".env\"]\n");
    // A no-op stand-in for a real install: the demo needs a hook that runs,
    // logs, and records a fingerprint, not a package manager.
    out.push_str("install = \"true\"\n\n[dev]\ncmd = '''");
    out.push_str(&listener_on_port_env());
    out.push_str("'''\nports = { PORT = \"web\" }\n");
    out
}

/// The quick-tunnel URL the fake provider publishes.
pub const FAKE_TUNNEL_URL: &str = "https://fake-tunnel-for-tests.trycloudflare.com";

/// Installs a fake `cloudflared` at `<home>/bin/cloudflared` — the hook
/// `tunnel::cloudflared_program` looks at first, and the same one a
/// developer would use for a real shim. No test touches PATH.
///
/// It echoes its own arguments, publishes a URL in cloudflared's bordered
/// format, registers a connection with the edge, and then stays up as a
/// tunnel does.
pub fn fake_cloudflared(home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let path = bin.join("cloudflared");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n\
             echo \"ARGS: $*\"\n\
             echo 'INF Requesting new quick Tunnel on trycloudflare.com...'\n\
             echo 'INF +---------------------------------------------------+'\n\
             echo 'INF |  {FAKE_TUNNEL_URL}  |'\n\
             echo 'INF +---------------------------------------------------+'\n\
             echo 'INF Registered tunnel connection connIndex=0 location=tst01 protocol=quic'\n\
             exec sleep 300\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Installs stand-ins for every program `pando open` runs to open an app
/// on a device — `xcrun`, `adb`, `xcode-select` and macOS's `open` — at
/// `<home>/bin`, which pando puts first on PATH for them, so no test ever
/// reaches a real simulator or launches an app on the developer's Mac.
///
/// `xcrun simctl list devices booted` lists one booted simulator when
/// `booted` is true, and `adb devices` one emulator when `android` is.
/// Every other call succeeds and prints nothing, except `xcode-select`,
/// which names no Xcode. Each program appends its arguments, one call to
/// a line, to `<home>/bin/<program>.calls`.
pub fn fake_devices(home: &Path, booted: bool, android: bool) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let simulator = match booted {
        true => "echo '    iPhone 17 (5E1B7C0A) (Booted) '",
        false => "echo '== Devices =='",
    };
    let emulator = match android {
        true => "printf 'List of devices attached\\nemulator-5554\\tdevice\\n'",
        false => "echo 'List of devices attached'",
    };
    for (program, body) in [
        (
            "xcrun",
            format!("[ \"$*\" = 'simctl list devices booted' ] && {simulator}\nexit 0"),
        ),
        (
            "adb",
            format!("[ \"$*\" = 'devices' ] && {emulator}\nexit 0"),
        ),
        ("xcode-select", "exit 2".to_string()),
        ("open", "exit 1".to_string()),
    ] {
        let path = bin.join(program);
        std::fs::write(
            &path,
            format!("#!/bin/sh\necho \"$*\" >> \"$0.calls\"\n{body}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// What a stand-in [`fake_devices`] installed was called with, one call to
/// a line; empty when it never was.
pub fn device_calls(home: &Path, program: &str) -> Vec<String> {
    std::fs::read_to_string(home.join("bin").join(format!("{program}.calls")))
        .map(|calls| calls.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Writes the fixture's listener config into an injected pando home.
pub fn write_listener_config(kind: Kind, home: &Path, root: &Path) -> PathBuf {
    let project = ProjectRef::from_root(root).unwrap();
    let dir = home.join("projects").join(&project.id);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pando.toml");
    std::fs::write(&path, listener_config_for(kind)).unwrap();
    path
}

/// Polls until `ready` is true or the timeout passes, and says which.
///
/// A fixed sleep is either slower than it has to be or shorter than a
/// loaded machine needs; this is neither.
pub fn wait_until(timeout: std::time::Duration, ready: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if ready() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

/// A fake `mariadb` in a pando home's `bin`, which every namespace command
/// finds first on PATH: its databases are files under `dbs/` of the
/// directory returned, and `created` and `dropped` record what it did.
/// Enough of the client for a namespaced start and an `rm`, and no server
/// anywhere. A file `deny` beside `dbs/` refuses every CREATE the way a
/// login with no grant is refused.
pub fn fake_mariadb(home: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let state = home.join("fake-mariadb");
    std::fs::create_dir_all(state.join("dbs")).unwrap();
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        r#"#!/bin/sh
state='{state}'
printf '%s\n' "$*" >> "$state/argv"
case "$*" in
  *"SELECT 1"*) echo 1 ;;
  *"CURRENT_USER()"*) echo "app@localhost" ;;
  *"CREATE DATABASE"*)
    db=$(printf '%s' "$*" | sed -n 's/.*CREATE DATABASE `\([^`]*\)`.*/\1/p')
    if [ -f "$state/deny" ]; then
      echo "ERROR 1044 (42000) at line 1: Access denied for user 'app'@'localhost' to database '$db'" >&2; exit 1
    fi
    if [ -f "$state/dbs/$db" ]; then echo "ERROR 1007 (HY000): database exists" >&2; exit 1; fi
    touch "$state/dbs/$db"; echo "$db" >> "$state/created" ;;
  *"LIKE"*) ;;
  *"SCHEMATA"*)
    db=$(printf '%s' "$*" | sed -n "s/.*SCHEMA_NAME = '\([^']*\)'.*/\1/p")
    if [ -f "$state/dbs/$db" ]; then echo "$db"; fi ;;
  *"DROP DATABASE"*)
    db=$(printf '%s' "$*" | sed -n 's/.*DROP DATABASE IF EXISTS `\([^`]*\)`.*/\1/p')
    rm -f "$state/dbs/$db"; echo "$db" >> "$state/dropped" ;;
  *) echo "unexpected: $*" >&2; exit 9 ;;
esac
"#,
        state = state.display()
    );
    std::fs::write(bin.join("mariadb"), script).unwrap();
    std::fs::set_permissions(bin.join("mariadb"), std::fs::Permissions::from_mode(0o755)).unwrap();
    state
}
