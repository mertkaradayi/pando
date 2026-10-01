//! WSL: whether pando is running in Windows' Linux, and which of its
//! directories are Windows drives.
//!
//! Under WSL 2 pando is a Linux program and needs nothing of its own to
//! run. What changes is around it: there is no desktop to open a URL with,
//! Windows' PATH is appended to the Linux one, and a Windows drive
//! (`/mnt/c`) is a network filesystem, slow for git and silent to inotify.
//! It is read from files, under a root a test can point elsewhere, the way
//! [`crate::actions::Machine`]'s `system` is.

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

    /// This machine's, read once. Never under `cargo test`: a unit test
    /// that happens to run in WSL must not open a URL, or copy, the way
    /// the developer's machine would.
    pub fn here() -> Option<&'static Wsl> {
        #[cfg(test)]
        {
            None
        }
        #[cfg(not(test))]
        {
            static HERE: std::sync::OnceLock<Option<Wsl>> = std::sync::OnceLock::new();
            HERE.get_or_init(|| Wsl::at(Path::new("/"))).as_ref()
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{WSL_MOUNTS, WSL_RELEASE, wsl_system};

    fn system(release: &str, mounts: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        wsl_system(dir.path(), release, mounts);
        dir
    }

    #[test]
    fn wsl_is_read_from_the_kernel_release_and_its_drives_from_the_mounts() {
        let second = "D:\\134 /mnt/d 9p rw,noatime,aname=drvfs;path=D:\\;uid=1000 0 0\n";
        let wsl2 = system(WSL_RELEASE, &format!("{WSL_MOUNTS}{second}"));
        assert_eq!(
            Wsl::at(wsl2.path()).expect("a WSL 2 kernel").drives,
            vec![PathBuf::from("/mnt/c"), PathBuf::from("/mnt/d")],
            "the drivers mount is 9p but not a drive"
        );
        assert!(
            Wsl::at(system("4.4.0-19041-Microsoft", "").path()).is_some(),
            "WSL 1"
        );
        assert_eq!(Wsl::at(system("6.8.0-45-generic", WSL_MOUNTS).path()), None);
        let nothing = tempfile::tempdir().unwrap();
        assert_eq!(Wsl::at(nothing.path()), None, "macOS has no /proc");
    }

    #[test]
    fn a_path_is_on_the_drive_whose_mount_it_is_under() {
        let wsl = Wsl {
            drives: vec![PathBuf::from("/mnt/c"), PathBuf::from("/mnt/c/Data")],
        };
        let on = |path: &str| wsl.drive_of(Path::new(path)).map(Path::to_path_buf);
        assert_eq!(on("/mnt/c/Users/me/app"), Some(PathBuf::from("/mnt/c")));
        assert_eq!(
            on("/mnt/c/Data/app"),
            Some(PathBuf::from("/mnt/c/Data")),
            "the deepest mount"
        );
        assert_eq!(
            on("/mnt/cache/app"),
            None,
            "a prefix of the name is not the mount"
        );
        assert_eq!(on("/home/me/app"), None);
    }

    #[test]
    fn wsl_1_mounts_drvfs_itself_and_a_mount_point_may_be_escaped() {
        let mounts = "C: /mnt/c drvfs rw,noatime 0 0\nE: /mnt/my\\040drive drvfs rw 0 0\n";
        assert_eq!(
            drive_mounts(mounts),
            vec![PathBuf::from("/mnt/c"), PathBuf::from("/mnt/my drive")]
        );
        assert_eq!(unescape(r"/a\134b\011c"), "/a\\b\tc");
        assert_eq!(unescape(r"/trailing\04"), r"/trailing\04", "not an escape");
    }

    #[test]
    fn no_test_sees_the_machine_it_runs_on_as_wsl() {
        assert_eq!(Wsl::here(), None);
    }
}
