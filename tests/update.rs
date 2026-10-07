//! `pando update`, run as a person runs it, against a stand-in `curl`
//! that answers for GitHub: nothing here reaches the network.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

const RUNNING: &str = env!("CARGO_PKG_VERSION");

/// A directory with a `curl` that says `latest` is the latest release
/// and serves an install script that puts a pando of that version where
/// `PANDO_CLI_INSTALL_DIR` says, as dist's does: written beside it and
/// moved over it, never written into a binary that may be running. Every
/// call is logged to `<dir>/log`.
fn fake_curl(latest: Option<&str>) -> TempDir {
    fake_curl_installing(latest, true, 0)
}

/// [`fake_curl`], whose install script puts the new pando in place only
/// when `installs`, then exits with `exit`.
fn fake_curl_installing(latest: Option<&str>, installs: bool, exit: i32) -> TempDir {
    let dir = TempDir::new().unwrap();
    let log = dir.path().join("log");
    let lookup = match latest {
        Some(latest) => {
            format!("printf '%s' 'https://github.com/mertkaradayi/pando/releases/tag/v{latest}'")
        }
        None => "echo 'curl: (6) Could not resolve host: github.com' >&2; exit 6".to_string(),
    };
    let script = format!(
        r#"#!/bin/sh
echo "curl $*" >> '{log}'
case "$*" in
  *redirect_url*) {lookup} ;;
  *)
    while [ $# -gt 0 ]; do [ "$1" = -o ] && out="$2"; shift; done
    cat > "$out" <<'SCRIPT'
#!/bin/sh
echo "installer dir=$PANDO_CLI_INSTALL_DIR no_modify_path=$PANDO_CLI_NO_MODIFY_PATH" >> '{log}'
{install}exit {exit}
SCRIPT
    ;;
esac
"#,
        log = log.display(),
        install = match installs {
            true => format!(
                "printf '#!/bin/sh\\necho \"pando {latest}\"\\n' > \"$PANDO_CLI_INSTALL_DIR/pando.new\"
chmod +x \"$PANDO_CLI_INSTALL_DIR/pando.new\"
mv \"$PANDO_CLI_INSTALL_DIR/pando.new\" \"$PANDO_CLI_INSTALL_DIR/pando\"
",
                latest = latest.unwrap_or("0.0.0")
            ),
            false => String::new(),
        },
    );
    let curl = dir.path().join("curl");
    std::fs::write(&curl, script).unwrap();
    std::fs::set_permissions(&curl, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap_or(-1)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn log(curl: &TempDir) -> String {
    std::fs::read_to_string(curl.path().join("log")).unwrap_or_default()
}

/// A copy of pando outside any checkout: what the install script leaves.
fn installed_copy() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let pando = bin.join("pando");
    std::fs::copy(env!("CARGO_BIN_EXE_pando"), &pando).unwrap();
    (dir, pando)
}

fn update(pando: &Path, curl: &TempDir, args: &[&str]) -> Output {
    let path = format!(
        "{}:{}",
        curl.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Command::new(pando)
        .arg("update")
        .args(args)
        .current_dir(curl.path())
        .env("PATH", path)
        .output()
        .unwrap()
}

#[test]
fn an_installed_pando_is_replaced_by_the_latest_release_in_its_own_directory() {
    let (_dir, pando) = installed_copy();
    let curl = fake_curl(Some("99.0.0"));

    let check = update(&pando, &curl, &["--check"]);
    assert_eq!(code(&check), 0, "{}", stderr(&check));
    let text = stdout(&check);
    assert!(
        text.contains(&format!("pando {RUNNING}, installed in ")),
        "{text}"
    );
    assert!(text.contains("99.0.0 is out"), "{text}");
    assert!(
        text.contains("pando-cli-installer.sh | PANDO_CLI_INSTALL_DIR="),
        "{text}"
    );
    assert!(!log(&curl).contains("installer"), "--check ran nothing");

    let out = update(&pando, &curl, &[]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("pando {RUNNING} → 99.0.0\n"));
    let log = log(&curl);
    assert!(
        log.contains("releases/latest/download/pando-cli-installer.sh"),
        "{log}"
    );
    let bin = std::fs::canonicalize(pando.parent().unwrap()).unwrap();
    assert!(
        log.contains(&format!("installer dir={} no_modify_path=1", bin.display())),
        "{log}"
    );
    let now = Command::new(&pando).arg("--version").output().unwrap();
    assert_eq!(stdout(&now), "pando 99.0.0\n");
}

// Homebrew installed 0.10.0, then its periodic `brew cleanup` refused a
// cask from another, untrusted tap and exited 1: `pando update` said the
// update failed, with the new pando already in place.
#[test]
fn an_update_that_installed_the_release_and_then_failed_says_it_is_updated() {
    let (_dir, pando) = installed_copy();
    let curl = fake_curl_installing(Some("99.0.0"), true, 1);
    let out = update(&pando, &curl, &[]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(stdout(&out), format!("pando {RUNNING} → 99.0.0\n"));
    let err = stderr(&out);
    assert!(err.contains("the install script failed"), "{err}");
    assert!(
        err.contains("after it installed 99.0.0: pando is updated"),
        "{err}"
    );
}

#[test]
fn an_update_that_failed_before_installing_anything_fails() {
    let (_dir, pando) = installed_copy();
    let curl = fake_curl_installing(Some("99.0.0"), false, 1);
    let out = update(&pando, &curl, &[]);
    assert_eq!(code(&out), 1, "{}", stdout(&out));
    assert!(
        stderr(&out).contains("the install script failed"),
        "{}",
        stderr(&out)
    );
    let now = Command::new(&pando).arg("--version").output().unwrap();
    assert_eq!(stdout(&now), format!("pando {RUNNING}\n"));
}

#[test]
fn the_latest_release_already_installed_runs_nothing() {
    let (_dir, pando) = installed_copy();
    let curl = fake_curl(Some(RUNNING));
    let out = update(&pando, &curl, &[]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("pando {RUNNING} is the latest release\n")
    );
    assert!(!log(&curl).contains("installer"), "{}", log(&curl));
}

#[test]
fn check_without_the_network_says_so_and_fails() {
    let (_dir, pando) = installed_copy();
    let curl = fake_curl(None);
    let out = update(&pando, &curl, &["--check"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).contains("could not read the latest release")
            && stderr(&out).contains("Could not resolve host"),
        "{}",
        stderr(&out)
    );
}

// The test binary is cargo's, in this checkout's target directory: pando
// says how to update it there and replaces nothing.
#[test]
fn a_build_in_a_checkout_is_not_replaced() {
    let curl = fake_curl(Some("99.0.0"));
    let out = update(Path::new(env!("CARGO_BIN_EXE_pando")), &curl, &[]);
    assert_eq!(code(&out), 1);
    let why = stderr(&out);
    match option_env!("PANDO_BUILD_LABEL").filter(|l| !l.trim().is_empty()) {
        Some(_) => assert!(why.contains("development build"), "{why}"),
        None => assert!(why.contains("update it there: `git pull`"), "{why}"),
    }
    assert!(!log(&curl).contains("installer"), "{}", log(&curl));
}
