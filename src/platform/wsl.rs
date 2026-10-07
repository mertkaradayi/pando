//! WSL: whether the Linux pando runs on is Windows' own, and which of its
//! directories are Windows drives.
//!
//! Read from files under a `system` root, never from the build: a WSL
//! distro runs the ordinary Linux binary, and a test describes one on any
//! OS. [`super::Host`] carries what is read here.

use std::path::{Path, PathBuf};

/// The WSL pando is running in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wsl {
    /// Where each Windows drive is mounted: `/mnt/c` unless `wsl.conf`
    /// says otherwise.
    drives: Vec<PathBuf>,
}

impl Wsl {
    /// The WSL whose files are under `system` (`/` on a real machine), or
    /// `None` when this is not one.
    pub fn at(system: &Path) -> Option<Wsl> {
        let release = std::fs::read_to_string(system.join("proc/sys/kernel/osrelease")).ok()?;
        if !is_wsl_release(&release) {
            return None;
        }
        let mounts = std::fs::read_to_string(system.join("proc/mounts")).unwrap_or_default();
        Some(Wsl {
            drives: drive_mounts(&mounts),
        })
    }

    /// Where the Windows drive `path` is on is mounted, when it is on one.
    pub fn drive_of(&self, path: &Path) -> Option<&Path> {
        self.drives
            .iter()
            .filter(|drive| path.starts_with(drive))
            .max_by_key(|drive| drive.as_os_str().len())
            .map(PathBuf::as_path)
    }
}

/// WSL's kernel says whose it is: `…-microsoft-standard-WSL2` under WSL 2,
/// `…-Microsoft` under WSL 1.
fn is_wsl_release(release: &str) -> bool {
    release.to_ascii_lowercase().contains("microsoft")
}

/// The mount points of Windows drives in a `/proc/mounts`: `9p` mounts of
/// `drvfs` under WSL 2 (`C:\134 /mnt/c 9p rw,…,aname=drvfs;path=C:\;…`),
/// `drvfs` itself under WSL 1. WSL's other `9p` mounts, its GPU drivers
/// among them, are not drives.
fn drive_mounts(mounts: &str) -> Vec<PathBuf> {
    mounts
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _device = fields.next()?;
            let point = fields.next()?;
            let kind = fields.next()?;
            let options = fields.next().unwrap_or_default();
            let drvfs = options
                .split([',', ';'])
                .any(|option| option == "aname=drvfs");
            (kind == "drvfs" || (kind == "9p" && drvfs)).then(|| PathBuf::from(unescape(point)))
        })
        .collect()
}

/// A mount point as `/proc/mounts` writes it, where a space, a tab, a
/// newline or a backslash is a three-digit octal escape (`\040`).
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let escape = bytes.get(i + 1..i + 4).and_then(|digits| {
            let digits = std::str::from_utf8(digits).ok()?;
            u8::from_str_radix(digits, 8).ok()
        });
        match (bytes[i], escape) {
            (b'\\', Some(byte)) => {
                out.push(byte);
                i += 4;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
