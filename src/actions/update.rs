//! `pando update`: how the running pando was installed, the latest
//! release, and the one command that install method updates itself with.
//! pando picks the command and runs it; Homebrew, cargo and the install
//! script do the updating, as they would by hand.

use std::cmp::Ordering;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

use crate::catalog::tools;
use crate::process::shell_word;

/// Where releases come from: `repository` in `Cargo.toml`.
pub const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The package `cargo install` names, and the app name the install
/// script is published under: `pando-cli`.
const CRATE: &str = env!("CARGO_PKG_NAME");

/// The Homebrew formula, by its tap, as the README installs it.
pub const FORMULA: &str = "mertkaradayi/tap/pando";

/// What the install script reads to put the binary in a directory of
/// the caller's choosing, and to leave shell profiles alone. dist names
/// them after the app.
const INSTALL_DIR_ENV: &str = "PANDO_CLI_INSTALL_DIR";
const NO_MODIFY_PATH_ENV: &str = "PANDO_CLI_NO_MODIFY_PATH";

/// How long the lookup of the latest release may take, in seconds.
const LOOKUP_SECONDS: &str = "15";

/// The install script every release publishes, always the latest one's.
pub fn installer_url() -> String {
    format!("{REPOSITORY}/releases/latest/download/{CRATE}-installer.sh")
}

/// `0.8.1`, or `0.9.0-rc.1`: enough of semver to order pando's own tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    numbers: [u64; 3],
    pre: Option<String>,
}

impl Version {
    /// `0.8.1`, `v0.8.1` or `0.9.0-rc.1`; anything after a space — a
    /// development build's `(branch@sha)` — is not part of it.
    pub fn parse(text: &str) -> Option<Version> {
        let text = text.split_whitespace().next()?;
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre.to_string())),
            None => (text, None),
        };
        let mut parts = core.split('.').map(|n| n.parse::<u64>().ok());
        let numbers = [parts.next()??, parts.next()??, parts.next()??];
        if parts.next().is_some() || pre.as_deref() == Some("") {
            return None;
        }
        Some(Version { numbers, pre })
    }

    /// The version this binary is.
    pub fn running() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml's version is semver")
    }

    /// `v0.8.1`: the tag a release is published under.
    pub fn tag(&self) -> String {
        format!("v{self}")
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.numbers.cmp(&other.numbers).then_with(|| {
            // A pre-release comes before the release it leads up to.
            match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(b),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [major, minor, patch] = self.numbers;
        write!(f, "{major}.{minor}.{patch}")?;
        match &self.pre {
            Some(pre) => write!(f, "-{pre}"),
            None => Ok(()),
        }
    }
}

/// How the running pando got where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// A branch's build, labelled by `scripts/dev`.
    Development { label: String },
    /// Built by cargo inside a checkout of pando, at `root`.
    Checkout { root: PathBuf },
    /// Homebrew's, below `<prefix>/Cellar/pando/`.
    Homebrew { prefix: PathBuf },
    /// `cargo install --git`, into `<root>/bin`. `default_root` when
    /// that is cargo's own home, so the command needs no `--root`.
    CargoGit { root: PathBuf, default_root: bool },
    /// `cargo install --path`, from a checkout at `source`.
    CargoPath { source: String },
    /// The install script's, or a binary put in `dir` by hand: either
    /// way, the install script replaces it there.
    Standalone { dir: PathBuf },
}

impl Install {
    /// How the binary at `exe` was installed. `exe` has its symlinks
    /// resolved: Homebrew's `bin/pando` is a link into its Cellar.
    /// `label` is [`crate::version::label`], `cargo_home` where cargo
    /// installs when given no `--root`.
    pub fn of(exe: &Path, label: Option<&str>, cargo_home: &Path) -> Install {
        if let Some(label) = label {
            return Install::Development {
                label: label.to_string(),
            };
        }
        if let Some(root) = exe
            .ancestors()
            .find(|dir| dir.file_name().is_some_and(|name| name == "target"))
            .and_then(Path::parent)
            .filter(|root| root.join("Cargo.toml").is_file())
        {
            return Install::Checkout {
                root: root.to_path_buf(),
            };
        }
        let components: Vec<_> = exe.components().collect();
        if let Some(at) = components
            .windows(2)
            .position(|pair| pair[0].as_os_str() == "Cellar" && pair[1].as_os_str() == "pando")
        {
            return Install::Homebrew {
                prefix: components[..at].iter().collect(),
            };
        }
        let dir = exe.parent().unwrap_or(Path::new("/")).to_path_buf();
        if dir.file_name().is_some_and(|name| name == "bin")
            && let Some(root) = dir.parent()
            && let Some(source) = cargo_source(root)
        {
            return match source.strip_prefix("path+") {
                Some(path) => Install::CargoPath {
                    source: path.strip_prefix("file://").unwrap_or(path).to_string(),
                },
                None => Install::CargoGit {
                    root: root.to_path_buf(),
                    default_root: same_dir(root, cargo_home),
                },
            };
        }
        Install::Standalone { dir }
    }

    /// How it reads after "installed": `with Homebrew`.
    pub fn describe(&self) -> String {
        match self {
            Install::Development { label } => format!("as a development build of {label}"),
            Install::Checkout { root } => {
                format!("by cargo, in the checkout at {}", root.display())
            }
            Install::Homebrew { .. } => "with Homebrew".to_string(),
            Install::CargoGit { .. } => "with cargo install".to_string(),
            Install::CargoPath { source } => format!("with cargo install, from {source}"),
            Install::Standalone { dir } => format!("in {}", dir.display()),
        }
    }

    /// Where the updated binary will be, to ask it its version.
    fn binary(&self) -> Option<PathBuf> {
        match self {
            Install::Homebrew { prefix } => Some(prefix.join("bin").join("pando")),
            Install::CargoGit { root, .. } => Some(root.join("bin").join("pando")),
            Install::Standalone { dir } => Some(dir.join("pando")),
            Install::Development { .. } | Install::Checkout { .. } | Install::CargoPath { .. } => {
                None
            }
        }
    }

    /// What updates it to `latest`, or why pando will not. `latest` is
    /// `None` when it could not be read; cargo needs it, for the tag.
    pub fn updater(&self, latest: Option<&Version>) -> Result<Updater, Refusal> {
        match self {
            Install::Development { label } => Err(Refusal(format!(
                "this pando is a development build of {label} — update that branch and build it \
                 again, or go back to the released pando"
            ))),
            Install::Checkout { root } => Err(Refusal(format!(
                "this pando was built in the checkout at {} — update it there: `git pull`, then \
                 `cargo build --release`",
                root.display()
            ))),
            Install::CargoPath { source } => Err(Refusal(format!(
                "this pando was installed from the checkout at {source} — update it there: `git \
                 pull`, then `cargo install --locked --path {}`",
                shell_word(source)
            ))),
            Install::Homebrew { prefix } => Ok(Updater::Run {
                program: prefix.join("bin").join("brew"),
                args: vec!["upgrade".into(), FORMULA.into()],
                env: Vec::new(),
            }),
            Install::CargoGit { root, default_root } => {
                let Some(latest) = latest else {
                    return Err(Refusal(
                        "cargo installs a release by its tag, and the latest release could not \
                         be read"
                            .to_string(),
                    ));
                };
                let mut args: Vec<String> = ["install", "--locked", "--git", REPOSITORY, "--tag"]
                    .map(String::from)
                    .into();
                args.push(latest.tag());
                if !default_root {
                    args.push("--root".into());
                    args.push(root.display().to_string());
                }
                args.push(CRATE.into());
                Ok(Updater::Run {
                    program: "cargo".into(),
                    args,
                    env: Vec::new(),
                })
            }
            Install::Standalone { dir } => Ok(Updater::Script {
                url: installer_url(),
                env: vec![
                    (INSTALL_DIR_ENV.to_string(), dir.display().to_string()),
                    (NO_MODIFY_PATH_ENV.to_string(), "1".to_string()),
                ],
            }),
        }
    }
}

/// Why pando will not update this install itself, with what will.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal(pub String);

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refusal {}

/// The command that updates an install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Updater {
    /// A program, run as it is.
    Run {
        program: PathBuf,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    /// The install script, downloaded and run with `env`.
    Script {
        url: String,
        env: Vec<(String, String)>,
    },
}

impl Updater {
    /// The command as a person would type it.
    pub fn command_line(&self) -> String {
        let assignments = |env: &[(String, String)]| {
            env.iter()
                .map(|(key, value)| format!("{key}={} ", shell_word(value)))
                .collect::<String>()
        };
        match self {
            Updater::Run { program, args, env } => {
                let words: Vec<String> = std::iter::once(program.display().to_string())
                    .chain(args.iter().cloned())
                    .map(|word| shell_word(&word))
                    .collect();
                format!("{}{}", assignments(env), words.join(" "))
            }
            Updater::Script { url, env } => format!(
                "curl --proto '=https' --tlsv1.2 -LsSf {url} | {}sh",
                assignments(env)
            ),
        }
    }

    /// Runs it, its output on the terminal.
    fn run(&self) -> Result<()> {
        match self {
            Updater::Run { program, args, env } => {
                let status = Command::new(program)
                    .args(args)
                    .envs(env.iter().cloned())
                    .status()
                    .with_context(|| format!("could not run {}", program.display()))?;
                if !status.success() {
                    bail!("`{}` failed ({status})", self.command_line());
                }
            }
            // Downloaded first and run second, so a failed download is a
            // failure and not an empty script that "succeeded".
            Updater::Script { url, env } => {
                let script = private_script_file()?;
                let download = curl()
                    .args(["--proto", "=https", "--tlsv1.2", "-LsSf", "-o"])
                    .arg(&script)
                    .arg(url)
                    .status()
                    .map_err(curl_missing);
                let ran = download.and_then(|status| {
                    if !status.success() {
                        bail!("could not download {url} ({status})");
                    }
                    let status = crate::platform::shell::posix()
                        .context("could not run sh")?
                        .arg(&script)
                        .envs(env.iter().cloned())
                        .status()
                        .context("could not run sh")?;
                    if !status.success() {
                        bail!("the install script failed ({status})");
                    }
                    Ok(())
                });
                let _ = std::fs::remove_file(&script);
                ran?;
            }
        }
        Ok(())
    }
}

/// A new, empty file only this user can write, for the install script.
///
/// Made here, exclusively, before curl writes into it: the temporary
/// directory may be shared, and a name another user could guess and
/// create first — as a file they can rewrite, or a link — would hand them
/// the script `sh` runs next.
pub(super) fn private_script_file() -> Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or_default();
    let path = std::env::temp_dir().join(format!(
        "pando-installer-{}-{nanos:09}.sh",
        std::process::id()
    ));
    crate::platform::files::create_new(&path, 0o600)
        .with_context(|| format!("could not make {}", path.display()))?;
    Ok(path)
}

/// What `pando update` found, before it does anything.
#[derive(Debug)]
pub struct Survey {
    pub running: Version,
    pub install: Install,
    /// The latest release, or why it could not be read.
    pub latest: Result<Version, String>,
}

impl Survey {
    /// This binary, how it was installed, and the latest release.
    pub fn take() -> Result<Survey> {
        let exe = std::env::current_exe()
            .and_then(std::fs::canonicalize)
            .context("cannot tell where this pando is")?;
        let install = Install::of(&exe, crate::version::label(), &cargo_home());
        Ok(Survey {
            running: Version::running(),
            install,
            latest: latest_release().map_err(|e| format!("{e:#}")),
        })
    }

    /// Whether the latest release is newer than this binary. `None` when
    /// it could not be read.
    pub fn behind(&self) -> Option<bool> {
        self.latest
            .as_ref()
            .ok()
            .map(|latest| *latest > self.running)
    }
}

/// How an update ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Updated {
    /// Already the latest release, or newer: nothing ran.
    Current,
    /// The command ran, and the binary is now `to`.
    To(Version),
    /// The command ran, and the binary is still the version it was:
    /// Homebrew's formula is pushed a few minutes after a release.
    Unchanged,
}

/// Updates the running pando the way it was installed: `survey` says
/// how, and `progress` hears the command before it runs.
pub fn update(survey: &Survey, progress: &dyn Fn(&str)) -> Result<Updated> {
    if survey.behind() == Some(false) {
        return Ok(Updated::Current);
    }
    let latest = survey.latest.as_ref().ok();
    let updater = survey.install.updater(latest)?;
    if let Err(why) = &survey.latest {
        progress(&format!(
            "could not read the latest release ({why}) — updating anyway"
        ));
    }
    progress(&format!("running {}", updater.command_line()));
    updater.run()?;
    let now = survey
        .install
        .binary()
        .and_then(|binary| version_of(&binary));
    Ok(match now {
        Some(now) if now != survey.running => Updated::To(now),
        _ => Updated::Unchanged,
    })
}

/// The latest release, from where GitHub's `releases/latest` redirects:
/// no API, so no token and no rate limit.
pub fn latest_release() -> Result<Version> {
    let url = format!("{REPOSITORY}/releases/latest");
    let output = curl()
        .args(["-fsS", "--max-time", LOOKUP_SECONDS, "-o", "/dev/null"])
        .args(["-w", "%{redirect_url}"])
        .arg(&url)
        .stdin(Stdio::null())
        .output()
        .map_err(curl_missing)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{url}: {}",
            stderr.trim().trim_start_matches("curl: ").trim()
        );
    }
    release_of(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| anyhow!("{url} named no release"))
}

/// The version a `releases/tag/v0.8.1` URL names.
pub(super) fn release_of(redirect: &str) -> Option<Version> {
    let (_, tag) = redirect.trim().rsplit_once("/releases/tag/")?;
    Version::parse(tag)
}

fn curl() -> Command {
    Command::new("curl")
}

fn curl_missing(e: std::io::Error) -> anyhow::Error {
    match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!(
            "curl is not installed — install it with: {}",
            tools::how_to_get("curl").unwrap_or("your package manager")
        ),
        _ => anyhow!("could not run curl: {e}"),
    }
}

/// What the binary at `path` says its version is.
fn version_of(path: &Path) -> Option<Version> {
    let output = Command::new(path).arg("--version").output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    Version::parse(text.trim().strip_prefix("pando ")?)
}

/// Where cargo installs with no `--root`: `$CARGO_HOME`, or `~/.cargo`.
fn cargo_home() -> PathBuf {
    match std::env::var_os("CARGO_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => super::user_home().join(".cargo"),
    }
}

/// The source cargo recorded for `pando-cli` in the install root `root`:
/// `git+https://…#<sha>` or `path+file:///…`. `None` when cargo installed
/// no pando there.
fn cargo_source(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".crates2.json")).ok()?;
    let installs: serde_json::Value = serde_json::from_str(&text).ok()?;
    installs["installs"]
        .as_object()?
        .keys()
        .find_map(|key| cargo_key_source(key))
}

/// `pando-cli 0.8.1 (git+https://…#sha)` → `git+https://…#sha`.
pub(super) fn cargo_key_source(key: &str) -> Option<String> {
    let rest = key.strip_prefix(CRATE)?.strip_prefix(' ')?;
    let (_, source) = rest.split_once(" (")?;
    Some(source.strip_suffix(')')?.to_string())
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let canonical = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canonical(a) == canonical(b)
}
