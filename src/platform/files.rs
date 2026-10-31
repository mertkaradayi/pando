//! Locks, permission bits, executability, links, file identity and the
//! bytes of a path.

use std::borrow::Cow;
use std::fs::{File, Metadata};
use std::io;
use std::path::Path;

/// Waits for an exclusive lock on `file`, held until the file is closed or
/// the process ends. Advisory: it keeps out whoever asks for it, nothing
/// else.
pub fn lock_exclusive(file: &File) -> io::Result<()> {
    imp::lock(file, true).map(|_| ())
}

/// Takes the lock [`lock_exclusive`] waits for, if nobody holds it:
/// `Ok(false)` when somebody does.
pub fn try_lock_exclusive(file: &File) -> io::Result<bool> {
    imp::lock(file, false)
}

/// The `rwx` bits of the file `meta` describes, where the OS has them.
pub fn permission_bits(meta: &Metadata) -> Option<u32> {
    imp::permission_bits(meta)
}

/// Sets the `rwx` bits of `path`; nothing to do where the OS has none.
pub fn set_permission_bits(path: &Path, bits: u32) -> io::Result<()> {
    imp::set_permission_bits(path, bits)
}

/// A new file at `path` with exactly `bits`, whatever the umask: there is
/// never a moment it is open to more. Fails when `path` exists.
pub fn create_new(path: &Path, bits: u32) -> io::Result<File> {
    imp::create_new(path, bits)
}

/// Whether the user pando runs as owns the file `meta` describes.
pub fn owned_by_current_user(meta: &Metadata) -> bool {
    imp::owned_by_current_user(meta)
}

/// Whether `path` is a file this user could run, following links.
pub fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && imp::runnable(path, &meta))
}

/// A symbolic link at `link` to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    imp::symlink(target, link)
}

/// `target` as the link at `link` will resolve it: a relative target from
/// the link's own directory, not from pando's.
///
/// Compiled everywhere so its tests run everywhere; only Windows, where a
/// link to a directory is another kind, asks.
#[cfg_attr(not(windows), allow(dead_code))]
pub(super) fn as_seen_from(link: &Path, target: &Path) -> std::path::PathBuf {
    match target.is_relative() {
        true => link.parent().unwrap_or(Path::new("")).join(target),
        false => target.to_path_buf(),
    }
}

/// Which file a path names right now: a file put in another's place behind
/// the same path has another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileId {
    device: u64,
    file: u64,
}

impl FileId {
    /// The file `meta` describes; `None` where the OS cannot say from its
    /// metadata.
    pub fn of(meta: &Metadata) -> Option<FileId> {
        imp::file_id(meta)
    }
}

/// The bytes of `path` as the OS stores them, for a digest that must stay
/// the same across versions: a project's id is one.
pub fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
    imp::path_bytes(path)
}

/// The bytes the file `meta` describes takes on disk: its blocks, where the
/// OS counts them, which a directory of small files outgrows its length by.
pub fn allocated_bytes(meta: &Metadata) -> u64 {
    imp::allocated_bytes(meta)
}

/// The bytes free to this user on the filesystem holding `path`; `None`
/// where the OS is not asked.
pub fn free_bytes(path: &Path) -> Option<u64> {
    imp::free_bytes(path)
}

#[cfg(unix)]
mod imp {
    use super::FileId;
    use std::borrow::Cow;
    use std::fs::{File, Metadata};
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    /// `flock(2)`; `Ok(false)` when not waiting and somebody holds it.
    pub(super) fn lock(file: &File, wait: bool) -> io::Result<bool> {
        let operation = match wait {
            true => libc::LOCK_EX,
            false => libc::LOCK_EX | libc::LOCK_NB,
        };
        // SAFETY: a descriptor `file` owns, open for the whole call.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(true);
        }
        let err = io::Error::last_os_error();
        // EWOULDBLOCK and EAGAIN are the same value on every Unix pando
        // builds for.
        match err.raw_os_error() {
            Some(libc::EWOULDBLOCK) if !wait => Ok(false),
            _ => Err(err),
        }
    }

    pub(super) fn permission_bits(meta: &Metadata) -> Option<u32> {
        Some(meta.permissions().mode() & 0o777)
    }

    pub(super) fn set_permission_bits(path: &Path, bits: u32) -> io::Result<()> {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits))
    }

    pub(super) fn create_new(path: &Path, bits: u32) -> io::Result<File> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(bits)
            .open(path)?;
        // Exactly `bits` whatever the umask made of the mode above.
        file.set_permissions(std::fs::Permissions::from_mode(bits))?;
        Ok(file)
    }

    pub(super) fn owned_by_current_user(meta: &Metadata) -> bool {
        // SAFETY: geteuid takes nothing and cannot fail.
        meta.uid() == unsafe { libc::geteuid() }
    }

    /// Any execute bit: whose it is, the OS decides when the file is run.
    pub(super) fn runnable(_path: &Path, meta: &Metadata) -> bool {
        meta.permissions().mode() & 0o111 != 0
    }

    pub(super) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    pub(super) fn file_id(meta: &Metadata) -> Option<FileId> {
        Some(FileId {
            device: meta.dev(),
            file: meta.ino(),
        })
    }

    pub(super) fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
        Cow::Borrowed(path.as_os_str().as_bytes())
    }

    /// `st_blocks` counts 512-byte units on every Unix pando builds for.
    pub(super) fn allocated_bytes(meta: &Metadata) -> u64 {
        meta.blocks().saturating_mul(512)
    }

    /// `statvfs(3)`: the blocks free to an unprivileged user, in the
    /// filesystem's fragment size.
    pub(super) fn free_bytes(path: &Path) -> Option<u64> {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: a NUL-terminated path, and a buffer statvfs fills on
        // success, read only then.
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: statvfs returned 0, so it filled the buffer.
        let stat = unsafe { stat.assume_init() };
        #[allow(clippy::unnecessary_cast)] // the field widths differ by OS
        Some((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
}

#[cfg(windows)]
mod imp {
    use super::FileId;
    use std::borrow::Cow;
    use std::fs::{File, Metadata};
    use std::io;
    use std::path::Path;

    /// Not taken yet: `File::lock` arrives in a later Rust than the oldest
    /// pando builds with.
    pub(super) fn lock(_: &File, _: bool) -> io::Result<bool> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a native Windows build cannot lock a file yet",
        ))
    }

    /// No `rwx` bits: who may use a file is its access list's to say.
    pub(super) fn permission_bits(_: &Metadata) -> Option<u32> {
        None
    }

    pub(super) fn set_permission_bits(_: &Path, _: u32) -> io::Result<()> {
        Ok(())
    }

    pub(super) fn create_new(path: &Path, _: u32) -> io::Result<File> {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }

    /// Not asked yet: a profile's files are its user's.
    pub(super) fn owned_by_current_user(_: &Metadata) -> bool {
        true
    }

    /// What `PATHEXT` names, the extensions Windows runs a file by.
    pub(super) fn runnable(path: &Path, _: &Metadata) -> bool {
        let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let known = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        known.split(';').any(|known| {
            known
                .trim_start_matches('.')
                .eq_ignore_ascii_case(extension)
        })
    }

    /// A link to a directory and a link to a file are two kinds here, told
    /// apart by what the target is as the link will see it.
    pub(super) fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        match super::as_seen_from(link, target).is_dir() {
            true => std::os::windows::fs::symlink_dir(target, link),
            false => std::os::windows::fs::symlink_file(target, link),
        }
    }

    /// Not read yet: the file index needs the file open.
    pub(super) fn file_id(_: &Metadata) -> Option<FileId> {
        None
    }

    /// The path as UTF-8: Windows stores UTF-16, which has no bytes of its
    /// own to hash.
    pub(super) fn path_bytes(path: &Path) -> Cow<'_, [u8]> {
        Cow::Owned(path.to_string_lossy().into_owned().into_bytes())
    }

    /// Its length: the allocation needs the file open.
    pub(super) fn allocated_bytes(meta: &Metadata) -> u64 {
        meta.len()
    }

    /// Not asked yet: `GetDiskFreeSpaceExW` is a dependency pando does not
    /// take for one number.
    pub(super) fn free_bytes(_: &Path) -> Option<u64> {
        None
    }
}
