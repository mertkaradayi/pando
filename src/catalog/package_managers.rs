//! Package managers: the lockfile each one writes and what pando does
//! about it.
//!
//! One row per manager. A field is `None` where pando deliberately does
//! nothing, and the row's comment says why, because an empty field that
//! looks like an oversight is the first thing somebody "fixes".

/// The language a manager's commands belong to. It decides which runner a
/// proposal uses: a `package.json` script is not run with `uv run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    JavaScript,
    Python,
    Ruby,
    Php,
    Elixir,
    Go,
    Rust,
}

/// The frozen install pando proposes for a lockfile.
#[derive(Debug, Clone, Copy)]
pub struct FrozenInstall {
    pub cmd: &'static str,
    /// The evidence written next to the proposal, `# detected: <why>`.
    /// `None` means the lockfile's own name.
    pub why: Option<&'static str>,
}

/// An install a developer might write by hand, and what makes it frozen.
///
/// Only the shapes pando itself proposes. A project whose install step is
/// `make setup` has said something pando has no opinion about, and guessing
/// at it would make doctor's check noise.
#[derive(Debug, Clone, Copy)]
pub struct InstallShape {
    /// The subcommands that install: `npm install` and `npm i`.
    pub verbs: &'static [&'static str],
    /// Any one of these in the step means it cannot rewrite the lockfile.
    /// Empty when no flag does, which is npm's case: `npm ci` is a
    /// different verb.
    pub frozen_markers: &'static [&'static str],
    /// The spelling doctor suggests instead.
    pub suggest: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct PackageManager {
    /// The binary on PATH, which is what doctor looks for.
    pub program: &'static str,
    /// In detection order. Every lockfile of every row is also part of the
    /// install hook's fingerprint: a lockfile changing is what means the
    /// dependencies changed.
    pub lockfiles: &'static [&'static str],
    pub ecosystem: Ecosystem,
    /// How this manager runs a project command, as a prefix: a
    /// `package.json` script for JavaScript, any command for Python.
    pub run_prefix: Option<&'static str>,
    /// What goes between a command run with `run_prefix` and the arguments
    /// handed on to it. npm needs `-- `, or it reads a flag as its own.
    /// Empty for every other manager: pnpm hands a `--` on to the script
    /// as a literal argument, and a CLI that meets one stops reading its
    /// options there, so `vite -- --port 1234` never sees the port.
    pub script_args: &'static str,
    /// How it runs a binary from the project's dependencies. `pnpm foo` and
    /// `bunx foo` run it; `npm foo` does not, which is what `npx` is for.
    pub exec: Option<&'static str>,
    /// Always the frozen variant: an install that can rewrite a lockfile
    /// would be pando writing into the repository, which Invariant 1
    /// forbids.
    pub install: Option<FrozenInstall>,
    pub install_shape: Option<InstallShape>,
    /// The install for a project that keeps no lockfile in git: proposed
    /// only when this manager's lockfiles are gitignored — the ones present,
    /// or every one when none is — because then the lockfile it writes
    /// cannot change the repository, and a frozen install has nothing in a
    /// new worktree to be frozen against.
    /// `None` where no such form is proposed: outside JavaScript, a
    /// project without a lockfile configures its own install.
    pub unlocked_install: Option<&'static str>,
    /// The install that writes no lockfile at all: proposed instead of the
    /// plain one where the lockfile present is gitignored and another name
    /// this manager writes is not. A new worktree is checked out without
    /// the one present, and the plain install there writes whichever name
    /// this version of the manager writes — `bun.lock` from bun 1.2 on.
    /// `None` for a manager with one lockfile name, where the plain
    /// install writes the one that is ignored.
    pub unsaved_install: Option<&'static str>,
    /// How a developer gets this manager, as doctor prints it after
    /// "install it with:". pando never runs it.
    pub get: &'static str,
}

/// Every manager pando knows, in detection order. Order is a contract: it
/// is the order `signals` lists lockfiles in, and the first lockfile present
/// decides which runner a proposal uses, so the JavaScript managers come
/// first.
pub const PACKAGE_MANAGERS: [PackageManager; 12] = [
    PackageManager {
        program: "pnpm",
        lockfiles: &["pnpm-lock.yaml"],
        ecosystem: Ecosystem::JavaScript,
        run_prefix: Some("pnpm "),
        script_args: "",
        exec: Some("pnpm"),
        install: Some(FrozenInstall {
            cmd: "pnpm install --frozen-lockfile",
            why: None,
        }),
        install_shape: Some(InstallShape {
            verbs: &["install"],
            frozen_markers: &["--frozen-lockfile"],
            suggest: "pnpm install --frozen-lockfile",
        }),
        unlocked_install: Some("pnpm install"),
        unsaved_install: None,
        get: "corepack enable pnpm   (or npm install -g pnpm)",
    },
    PackageManager {
        program: "npm",
        lockfiles: &["package-lock.json"],
        ecosystem: Ecosystem::JavaScript,
        run_prefix: Some("npm run "),
        script_args: "-- ",
        exec: Some("npx"),
        install: Some(FrozenInstall {
            cmd: "npm ci",
            why: None,
        }),
        install_shape: Some(InstallShape {
            verbs: &["install", "i"],
            frozen_markers: &[],
            suggest: "npm ci",
        }),
        unlocked_install: Some("npm install"),
        unsaved_install: None,
        get: "brew install node   (npm comes with Node.js)",
    },
    PackageManager {
        program: "yarn",
        lockfiles: &["yarn.lock"],
        ecosystem: Ecosystem::JavaScript,
        run_prefix: Some("yarn "),
        script_args: "",
        exec: Some("yarn"),
        // `--frozen-lockfile`, not `--immutable`: Yarn 1 does not know
        // `--immutable`, ignores it, and rewrites `yarn.lock`. Yarn 2 and
        // later still honour `--frozen-lockfile` as `--immutable`, so it is
        // the one spelling that is frozen on every Yarn.
        install: Some(FrozenInstall {
            cmd: "yarn install --frozen-lockfile",
            why: None,
        }),
        install_shape: Some(InstallShape {
            verbs: &["install"],
            frozen_markers: &["--immutable", "--frozen-lockfile"],
            suggest: "yarn install --frozen-lockfile",
        }),
        unlocked_install: Some("yarn install"),
        unsaved_install: None,
        get: "corepack enable yarn   (or npm install -g yarn)",
    },
    PackageManager {
        program: "bun",
        lockfiles: &["bun.lockb", "bun.lock"],
        ecosystem: Ecosystem::JavaScript,
        run_prefix: Some("bun run "),
        script_args: "",
        exec: Some("bunx"),
        install: Some(FrozenInstall {
            cmd: "bun install --frozen-lockfile",
            why: Some("a bun lockfile"),
        }),
        install_shape: Some(InstallShape {
            verbs: &["install"],
            frozen_markers: &["--frozen-lockfile"],
            suggest: "bun install --frozen-lockfile",
        }),
        unlocked_install: Some("bun install"),
        unsaved_install: Some("bun install --no-save"),
        get: "brew install oven-sh/bun/bun   (or the install script at bun.sh)",
    },
    PackageManager {
        program: "uv",
        lockfiles: &["uv.lock"],
        ecosystem: Ecosystem::Python,
        run_prefix: Some("uv run "),
        script_args: "",
        exec: None,
        install: Some(FrozenInstall {
            cmd: "uv sync --frozen",
            why: None,
        }),
        install_shape: Some(InstallShape {
            verbs: &["sync"],
            frozen_markers: &["--frozen", "--locked"],
            suggest: "uv sync --frozen",
        }),
        unlocked_install: None,
        unsaved_install: None,
        get: "brew install uv   (or the install script at astral.sh/uv)",
    },
    PackageManager {
        program: "poetry",
        lockfiles: &["poetry.lock"],
        ecosystem: Ecosystem::Python,
        run_prefix: Some("poetry run "),
        script_args: "",
        exec: None,
        // Poetry has no `--frozen`, and needs none: `install` refuses a
        // lockfile that no longer matches `pyproject.toml` rather than
        // regenerating it. So there is no unfrozen shape to warn about.
        install: Some(FrozenInstall {
            cmd: "poetry install --sync",
            why: None,
        }),
        install_shape: None,
        unlocked_install: None,
        unsaved_install: None,
        get: "pipx install poetry",
    },
    PackageManager {
        program: "pipenv",
        lockfiles: &["Pipfile.lock"],
        ecosystem: Ecosystem::Python,
        run_prefix: Some("pipenv run "),
        script_args: "",
        exec: None,
        // `sync` installs exactly what the lockfile says and never writes
        // it; `install` resolves again and can rewrite it.
        install: Some(FrozenInstall {
            cmd: "pipenv sync",
            why: None,
        }),
        install_shape: Some(InstallShape {
            verbs: &["install", "lock", "update"],
            frozen_markers: &["--deploy", "--ignore-pipfile"],
            suggest: "pipenv sync",
        }),
        unlocked_install: None,
        unsaved_install: None,
        get: "pipx install pipenv",
    },
    PackageManager {
        program: "bundle",
        lockfiles: &["Gemfile.lock"],
        ecosystem: Ecosystem::Ruby,
        run_prefix: None,
        script_args: "",
        exec: None,
        // The environment variable rather than `bundle config`, which would
        // write `.bundle/config` into the repository.
        install: Some(FrozenInstall {
            cmd: "BUNDLE_FROZEN=true bundle install",
            why: None,
        }),
        install_shape: Some(InstallShape {
            verbs: &["install"],
            frozen_markers: &["BUNDLE_FROZEN", "--deployment", "--frozen"],
            suggest: "BUNDLE_FROZEN=true bundle install",
        }),
        unlocked_install: None,
        unsaved_install: None,
        get: "gem install bundler",
    },
    PackageManager {
        program: "composer",
        lockfiles: &["composer.lock"],
        ecosystem: Ecosystem::Php,
        run_prefix: None,
        script_args: "",
        exec: None,
        // With a lockfile present `install` installs exactly what it pins
        // and never rewrites it; `update` is the verb that resolves again.
        install: Some(FrozenInstall {
            cmd: "composer install",
            why: None,
        }),
        install_shape: Some(InstallShape {
            verbs: &["update", "require"],
            frozen_markers: &[],
            suggest: "composer install",
        }),
        unlocked_install: None,
        unsaved_install: None,
        get: "brew install composer   (or the installer at getcomposer.org)",
    },
    PackageManager {
        program: "mix",
        lockfiles: &["mix.lock"],
        ecosystem: Ecosystem::Elixir,
        run_prefix: None,
        script_args: "",
        exec: None,
        // Deliberately nothing: `mix deps.get` writes the lockfile for a
        // dependency that is not in it yet, and an install that can rewrite
        // a lockfile is an Invariant 1 break. An Elixir project configures
        // its own install command.
        install: None,
        install_shape: None,
        unlocked_install: None,
        unsaved_install: None,
        get: "brew install elixir   (mix comes with Elixir)",
    },
    PackageManager {
        program: "go",
        lockfiles: &["go.sum"],
        ecosystem: Ecosystem::Go,
        run_prefix: None,
        script_args: "",
        exec: None,
        // `go run` resolves its own modules, and proposing a warm-up step
        // for it is noise.
        install: None,
        install_shape: None,
        unlocked_install: None,
        unsaved_install: None,
        get: "brew install go   (or your distribution's golang package)",
    },
    PackageManager {
        program: "cargo",
        lockfiles: &["Cargo.lock"],
        ecosystem: Ecosystem::Rust,
        run_prefix: None,
        script_args: "",
        exec: None,
        // `cargo run` resolves its own crates, so nothing is proposed; but a
        // developer who does write a fetch step is held to `--locked`.
        install: None,
        install_shape: Some(InstallShape {
            verbs: &["fetch"],
            frozen_markers: &["--locked"],
            suggest: "cargo fetch --locked",
        }),
        unlocked_install: None,
        unsaved_install: None,
        get: "rustup, from https://rustup.rs",
    },
];

/// Every lockfile pando recognises, in detection order.
pub fn lockfiles() -> Vec<&'static str> {
    PACKAGE_MANAGERS
        .iter()
        .flat_map(|manager| manager.lockfiles.iter().copied())
        .collect()
}

/// The manager that writes this lockfile.
pub fn for_lockfile(lockfile: &str) -> Option<&'static PackageManager> {
    PACKAGE_MANAGERS
        .iter()
        .find(|manager| manager.lockfiles.contains(&lockfile))
}

/// The manager whose binary this is.
pub fn for_program(program: &str) -> Option<&'static PackageManager> {
    PACKAGE_MANAGERS
        .iter()
        .find(|manager| manager.program == program)
}

/// Whether `cmd` is an install pando proposes for a project that keeps no
/// lockfile in git: some manager's plain install, or its install that
/// writes no lockfile.
pub fn is_unlocked_install(cmd: &str) -> bool {
    PACKAGE_MANAGERS.iter().any(|manager| {
        manager.unlocked_install == Some(cmd) || manager.unsaved_install == Some(cmd)
    })
}

/// Installs that delete a dependency directory before they install, by
/// the words that run them, and the directory: `npm ci` removes
/// `node_modules` first, under each of the names npm gives the command.
/// Cloning that directory into a worktree before such an install is work
/// thrown away.
pub const CLEARING_INSTALLS: [(&[&str], &str); 4] = [
    (&["npm", "ci"], "node_modules"),
    (&["npm", "clean-install"], "node_modules"),
    (&["npm", "ic"], "node_modules"),
    (&["npm", "install-clean"], "node_modules"),
];

/// The directory `install` deletes before installing, when it runs one of
/// [`CLEARING_INSTALLS`] anywhere in it: `cd web && npm ci` counts.
pub fn cleared_by(install: &str) -> Option<&'static str> {
    let words: Vec<&str> = install
        .split(|c: char| c.is_whitespace() || ";&|()".contains(c))
        .filter(|w| !w.is_empty())
        .collect();
    CLEARING_INSTALLS
        .iter()
        .find(|(cmd, _)| words.windows(cmd.len()).any(|w| w == *cmd))
        .map(|(_, dir)| *dir)
}

/// The frozen install to propose for a lockfile, with the evidence to
/// write beside it.
pub fn install_for(lockfile: &str) -> Option<(&'static str, &'static str)> {
    let manager = for_lockfile(lockfile)?;
    let install = manager.install?;
    // The lockfile string is borrowed, so the evidence is the row's own
    // spelling of it rather than the caller's.
    let own = manager.lockfiles.iter().find(|l| **l == lockfile)?;
    Some((install.cmd, install.why.unwrap_or(own)))
}

/// The manager a JavaScript project names in `package.json`'s
/// `packageManager` field (`"pnpm@9.1.0"`), else npm's, which is what
/// runs a `package.json` that names none.
pub fn declared_javascript(manifest: &str) -> &'static PackageManager {
    let declared = serde_json::from_str::<serde_json::Value>(manifest)
        .ok()
        .and_then(|json| json.get("packageManager")?.as_str().map(str::to_string))
        .and_then(|spec| {
            let program = spec.split('@').next()?.trim().to_string();
            for_program(&program).filter(|m| m.ecosystem == Ecosystem::JavaScript)
        });
    declared.unwrap_or_else(|| for_program("npm").expect("npm is a row"))
}

/// The manager of the first present lockfile that belongs to `ecosystem`
/// and runs project commands: the one whose run prefix a proposal uses.
fn runner<'a>(
    lockfiles: impl IntoIterator<Item = &'a str>,
    ecosystem: Ecosystem,
) -> Option<&'static PackageManager> {
    lockfiles
        .into_iter()
        .filter_map(for_lockfile)
        .filter(|manager| manager.ecosystem == ecosystem)
        .find(|manager| manager.run_prefix.is_some())
}

/// The run prefix of the first present lockfile whose manager belongs to
/// `ecosystem`.
pub fn run_prefix<'a>(
    lockfiles: impl IntoIterator<Item = &'a str>,
    ecosystem: Ecosystem,
) -> Option<&'static str> {
    runner(lockfiles, ecosystem)?.run_prefix
}

/// What goes between a command and the arguments handed on to it, for the
/// manager whose [`run_prefix`] runs it.
pub fn script_args<'a>(
    lockfiles: impl IntoIterator<Item = &'a str>,
    ecosystem: Ecosystem,
) -> Option<&'static str> {
    Some(runner(lockfiles, ecosystem)?.script_args)
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_install_that_clears_node_modules_is_recognised_wherever_it_runs() {
        use super::cleared_by;
        assert_eq!(cleared_by("npm ci"), Some("node_modules"));
        assert_eq!(
            cleared_by("cd web && npm ci --no-audit"),
            Some("node_modules")
        );
        assert_eq!(cleared_by("npm clean-install"), Some("node_modules"));
        assert_eq!(cleared_by("npm install"), None);
        assert_eq!(cleared_by("pnpm install --frozen-lockfile"), None);
        assert_eq!(cleared_by("echo npm-ci"), None);
    }

    use super::*;

    #[test]
    fn a_lockfile_belongs_to_one_manager() {
        let all = lockfiles();
        for lock in &all {
            let owners = PACKAGE_MANAGERS
                .iter()
                .filter(|m| m.lockfiles.contains(lock))
                .count();
            assert_eq!(owners, 1, "{lock} is claimed by {owners} rows");
        }
    }

    #[test]
    fn the_javascript_managers_come_first() {
        // The first lockfile decides the package runner, so a Python or
        // Rust lockfile must never be listed before a JavaScript one.
        let first_other = PACKAGE_MANAGERS
            .iter()
            .position(|m| m.ecosystem != Ecosystem::JavaScript)
            .unwrap();
        assert!(
            PACKAGE_MANAGERS[first_other..]
                .iter()
                .all(|m| m.ecosystem != Ecosystem::JavaScript)
        );
    }

    #[test]
    fn a_suggested_install_is_one_its_own_shape_accepts() {
        for manager in PACKAGE_MANAGERS {
            let Some(shape) = manager.install_shape else {
                continue;
            };
            let frozen_verb = shape
                .suggest
                .split_whitespace()
                .skip_while(|w| w.contains('='))
                .nth(1)
                .unwrap_or("");
            let accepted = !shape.verbs.contains(&frozen_verb)
                || shape
                    .frozen_markers
                    .iter()
                    .any(|m| shape.suggest.contains(m));
            assert!(
                accepted,
                "{} suggests {:?}, which its own check would flag",
                manager.program, shape.suggest
            );
        }
    }

    #[test]
    fn a_proposed_install_is_the_one_doctor_suggests() {
        for manager in PACKAGE_MANAGERS {
            if let (Some(install), Some(shape)) = (manager.install, manager.install_shape) {
                assert_eq!(install.cmd, shape.suggest, "{}", manager.program);
            }
        }
    }

    #[test]
    fn both_bun_lockfiles_propose_the_same_install() {
        assert_eq!(install_for("bun.lockb"), install_for("bun.lock"));
        assert_eq!(
            install_for("bun.lock"),
            Some(("bun install --frozen-lockfile", "a bun lockfile"))
        );
        assert_eq!(
            install_for("pnpm-lock.yaml"),
            Some(("pnpm install --frozen-lockfile", "pnpm-lock.yaml"))
        );
    }
}
