//! How pando gets onto a machine, held to the one place each fact lives.
//!
//! The oldest Rust pando builds with is `rust-version` in `Cargo.toml`.
//! The README, the contributing guide and the website each state it,
//! and the CI job `msrv` builds with it; a number written in four places
//! drifts the first time one of them is edited alone, and a developer
//! with the stated toolchain then gets a build error pando promised them
//! they would not. So the documents are read here, and CI is checked to
//! run these tests on the documents' own changes.

use std::path::{Path, PathBuf};

/// Every document that tells a person how to install pando.
const INSTALL_DOCS: [&str; 4] = [
    "README.md",
    "CONTRIBUTING.md",
    "site/index.html",
    "site/docs.html",
];

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

fn read(relative: &str) -> String {
    let path = repo(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn rust_version() -> String {
    let manifest: toml::Table = read("Cargo.toml").parse().expect("Cargo.toml parses");
    manifest["package"]["rust-version"]
        .as_str()
        .expect("Cargo.toml states [package] rust-version")
        .to_string()
}

/// Every `Rust X.Y` in `text`, as `X.Y`.
fn stated_rust_versions(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, _) in text.match_indices("Rust ") {
        let rest = &text[at + "Rust ".len()..];
        let version: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let version = version.trim_end_matches('.');
        if version.contains('.') {
            out.push(version.to_string());
        }
    }
    out
}

#[test]
fn every_rust_version_the_docs_state_is_cargo_tomls() {
    let wanted = rust_version();
    for doc in INSTALL_DOCS {
        let stated = stated_rust_versions(&read(doc));
        assert!(
            !stated.is_empty(),
            "{doc} does not say which Rust pando needs (Rust {wanted} or newer)"
        );
        for version in stated {
            assert_eq!(
                version, wanted,
                "{doc} says Rust {version}; Cargo.toml's rust-version is {wanted}"
            );
        }
    }
}

/// Without `--locked`, `cargo install` resolves the newest dependency
/// versions, which may need a newer Rust than `rust-version` says; the
/// lockfile is the set CI tested.
#[test]
fn every_cargo_install_in_the_docs_is_locked() {
    for doc in INSTALL_DOCS {
        let text = read(doc);
        for (at, _) in text.match_indices("cargo install ") {
            let rest = &text[at + "cargo install ".len()..];
            assert!(
                rest.starts_with("--locked"),
                "{doc} has a `cargo install` without --locked: `cargo install {}`",
                rest.lines().next().unwrap_or_default()
            );
        }
    }
}

/// The patterns in one `paths-ignore:` list of `ci.yml`, the one under
/// `trigger:`.
fn ignored_paths(workflow: &str, trigger: &str) -> Vec<String> {
    let start = workflow
        .find(&format!("  {trigger}:\n"))
        .unwrap_or_else(|| panic!("ci.yml has no `{trigger}:` trigger"));
    let list = workflow[start..]
        .split_once("paths-ignore:\n")
        .unwrap_or_else(|| panic!("ci.yml's `{trigger}:` has no paths-ignore"))
        .1;
    list.lines()
        .map_while(|line| line.trim().strip_prefix("- "))
        .map(|pattern| pattern.trim_matches('"').to_string())
        .collect()
}

fn ignores(pattern: &str, path: &str) -> bool {
    match pattern.strip_suffix("/**") {
        Some(dir) => path.starts_with(&format!("{dir}/")),
        None => pattern == path,
    }
}

/// A change to a document these tests read must run them: were it on
/// `paths-ignore`, the one change most likely to break the fact, an edit
/// to that document alone, would never be checked.
#[test]
fn ci_runs_these_tests_on_every_document_they_read() {
    let workflow = read(".github/workflows/ci.yml");
    let push = ignored_paths(&workflow, "push");
    let pull_request = ignored_paths(&workflow, "pull_request");
    assert!(!push.is_empty(), "ci.yml's push paths-ignore did not parse");
    assert_eq!(
        push, pull_request,
        "ci.yml ignores different paths for pushes and for pull requests"
    );
    for doc in INSTALL_DOCS.iter().chain(&[
        "Cargo.toml",
        ".github/workflows/ci.yml",
        ".github/workflows/release-smoke.yml",
    ]) {
        for pattern in &push {
            assert!(
                !ignores(pattern, doc),
                "ci.yml's paths-ignore has {pattern}, so a change to {doc} alone skips the tests that read it"
            );
        }
    }
}

#[test]
fn ci_builds_with_the_rust_version_cargo_toml_states_and_writes_it_nowhere() {
    let workflow = read(".github/workflows/ci.yml");
    assert!(
        workflow.contains("  msrv:\n"),
        "ci.yml has no msrv job building with the oldest supported Rust"
    );
    assert!(
        workflow.contains(".rust_version"),
        "the msrv job must read rust-version from `cargo metadata`, not repeat it"
    );
    assert!(
        !workflow.contains(&format!("toolchain: {}", rust_version()))
            && !workflow.contains(&format!("toolchain: \"{}\"", rust_version())),
        "ci.yml writes the Rust version itself; read it from Cargo.toml"
    );
}

#[test]
fn a_stated_version_is_read_whole() {
    assert_eq!(
        stated_rust_versions("needs Rust 1.88 or newer, not Rust 1.9. Rust is nice."),
        ["1.88", "1.9"]
    );
    assert_eq!(stated_rust_versions("only without Rust 1.88+"), ["1.88"]);
}

/// The ways in the README and the website lead with, built from what the
/// release publishes: `[workspace.metadata.dist]` names the Homebrew tap
/// and formula, and dist names the install script after the package. A
/// renamed package, tap or formula must not leave the docs pointing at a
/// 404.
#[test]
fn the_docs_install_what_the_release_publishes() {
    let manifest: toml::Table = read("Cargo.toml").parse().expect("Cargo.toml parses");
    let package = &manifest["package"];
    let name = package["name"].as_str().unwrap();
    let repository = package["repository"].as_str().unwrap();
    let dist = &manifest["workspace"]["metadata"]["dist"];
    let tap = dist["tap"].as_str().expect("dist has a Homebrew tap");
    let formula = dist["formula"].as_str().expect("dist names the formula");

    // brew spells mertkaradayi/homebrew-tap as mertkaradayi/tap.
    let (owner, tap_repo) = tap.split_once('/').unwrap();
    let tap_name = tap_repo.strip_prefix("homebrew-").unwrap_or(tap_repo);
    let brew = format!("brew install {owner}/{tap_name}/{formula}");
    let script = format!("{repository}/releases/latest/download/{name}-installer.sh");
    let from_source = format!("cargo install --locked --git {repository} {name}");

    for doc in ["README.md", "site/index.html", "site/docs.html"] {
        let text = read(doc);
        for wanted in [&brew, &script, &from_source] {
            assert!(
                text.contains(wanted.as_str()),
                "{doc} does not say `{wanted}`"
            );
        }
    }
}

/// The formula reaches the tap through `publish-homebrew.yml`, not dist's
/// own job, and that workflow names the tap a second time: it must be
/// the one `[workspace.metadata.dist]` names, and dist must be told to
/// run it, or a release publishes no Homebrew install at all.
#[test]
fn the_homebrew_publish_job_pushes_to_the_tap_dist_names() {
    let manifest: toml::Table = read("Cargo.toml").parse().expect("Cargo.toml parses");
    let dist = &manifest["workspace"]["metadata"]["dist"];
    let tap = dist["tap"].as_str().expect("dist has a Homebrew tap");
    let jobs: Vec<&str> = dist["publish-jobs"]
        .as_array()
        .expect("dist has publish-jobs")
        .iter()
        .filter_map(|job| job.as_str())
        .collect();
    assert!(
        jobs.contains(&"./publish-homebrew"),
        "publish-jobs is {jobs:?}, so dist never runs publish-homebrew.yml"
    );

    let workflow = read(".github/workflows/publish-homebrew.yml");
    assert!(
        workflow.contains(&format!("repository: {tap}\n")),
        "publish-homebrew.yml does not check out {tap}, the tap dist names"
    );
}

/// The release smoke makes its fixture in a few lines of shell, so it
/// needs no Rust toolchain, and those lines carry the test suite's own
/// listener: a bare socket that listens the moment it binds. Written out
/// twice, the two drift, and the smoke is what proves a release installs.
/// `http.server`, which it ran before, resolves its own name between bind
/// and listen, and on the macOS runners that outlasted the readiness wait.
#[test]
fn the_release_smoke_starts_the_test_suites_own_listener() {
    let workflow = read(".github/workflows/release-smoke.yml");
    // Inside a double-quoted shell word there, so its quotes are escaped.
    let listener = crate::common::listener_on_port_template().replace('"', "\\\"");
    assert!(
        workflow.contains(&format!("cmd = '''{listener}'''")),
        "release-smoke.yml's fixture no longer starts listener_on_port_template():\n{listener}"
    );
}

/// `pando update` updates each install the way the README made it, so
/// the formula and the install script's URL it uses are the README's.
#[test]
fn pando_update_uses_the_formula_and_the_script_the_readme_installs_with() {
    let readme = read("README.md");
    let brew = format!("brew install {}", pando::actions::FORMULA);
    assert!(readme.contains(&brew), "the README installs with `{brew}`");
    let script = format!(
        "curl --proto '=https' --tlsv1.2 -LsSf {} | sh",
        pando::actions::installer_url()
    );
    assert!(
        readme.contains(&script),
        "the README installs with `{script}`"
    );
}
