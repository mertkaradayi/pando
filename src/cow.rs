//! Copy-on-write clones: a file or a directory tree that shares its
//! source's blocks until either side writes, on a filesystem that can make
//! one (APFS on macOS; btrfs, XFS and bcachefs on Linux).
//!
//! Never a plain copy in disguise. A filesystem that cannot clone is
//! reported as [`is_unsupported`], so a caller can let git or the install
//! write the file instead: a full copy would cost the disk and the time
//! cloning exists to save, and would hide that nothing was shared.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Clones `src` to `dst`, which must not exist. Neither path is followed if
/// it is a symlink: `src` must be a regular file.
pub fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
    imp::clone_file(src, dst)
}

/// Clones the directory `src` to `dst`, which must not exist, with every
/// file, directory and symlink in it. Symlinks are recreated as links, not
/// followed; anything else (a socket, a fifo) is left out.
///
/// On failure nothing this call made is left at `dst`: a half tree is
/// worse than none, because whatever runs next would treat it as complete.
/// Something already at `dst` is never touched: the call fails with
/// `AlreadyExists` before it makes anything.
pub fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
    if dst.symlink_metadata().is_ok() {
        return Err(io::Error::from(io::ErrorKind::AlreadyExists));
    }
    imp::clone_tree(src, dst)
}

/// Removes a tree a clone made, read-only directories included: a clone
/// keeps its source's modes, and `remove_dir_all` cannot empty a `0555`
/// directory. Never follows a symlink, so a link in the tree cannot take
/// its target with it.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = path.symlink_metadata()?;
    if !meta.is_dir() {
        return std::fs::remove_file(path);
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700))?;
    }
    for entry in std::fs::read_dir(path)? {
        remove_tree(&entry?.path())?;
    }
    std::fs::remove_dir(path)
}

/// Whether an error from [`clone_file`] or [`clone_tree`] means "this
/// filesystem, or this pair of paths, cannot clone", as opposed to a
/// failure of one file, such as a missing source or a full disk.
///
/// A different volume (`EXDEV`) counts: a clone cannot cross one, and
/// pando's home may well be on another disk than the repository. The
/// values differ by platform: on macOS `EINVAL` is a bad argument, on
/// Linux it is what some filesystems answer `FICLONE` with.
pub fn is_unsupported(e: &io::Error) -> bool {
    e.raw_os_error()
        .is_some_and(|code| imp::UNSUPPORTED.contains(&code))
}

/// Makes a cloned file look the way `git checkout` writes one: git's mode
/// for the blob (`0777` for an executable, `0666` otherwise, less the
/// umask) and no quarantine flag.
///
/// A clone carries its source's permission bits and extended attributes,
/// and git compares only the executable bit, so a main checkout's `0600`
/// file or a quarantined download would otherwise pass into the worktree
/// unseen.
pub fn as_git_writes(path: &Path, executable: bool, umask: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match executable {
        true => 0o777,
        false => 0o666,
    } & !umask;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    imp::drop_quarantine(path)
}

/// Whether a file is one a clone must not be made of:
///
/// - locked (`uchg`) or append-only on macOS, flags a clone copies: `git
///   reset` could not rewrite such a clone, and `git worktree remove
///   --force` could not remove it;
/// - dataless on macOS, its contents evicted to iCloud: cloning it would
///   download it into the main checkout, spending the disk a clone exists
///   to save, where git's own checkout never reads the main checkout.
pub fn must_not_clone(meta: &std::fs::Metadata) -> bool {
    imp::must_not_clone(meta)
}

static UMASK: std::sync::OnceLock<u32> = std::sync::OnceLock::new();

/// The process's umask, which git applies to every file it writes.
///
/// Reading it means setting it, for a moment, process-wide: a file another
/// thread makes in that moment would get the wrong mode. So it is read
/// once, and `main` reads it first, before any thread starts.
pub fn umask() -> u32 {
    *UMASK.get_or_init(|| {
        // SAFETY: umask only swaps an integer in the process; it is read
        // and put back at once, the only way the call offers to read it.
        let old = unsafe { libc::umask(0o022) };
        unsafe { libc::umask(old) };
        // `mode_t` is a `u16` on macOS and already a `u32` on Linux.
        #[allow(clippy::useless_conversion)]
        u32::from(old)
    })
}

/// The first symlink under `tree` that would make a worktree use files that
/// are not its own: an absolute target inside the main checkout (under any
/// spelling of its path), or a relative one that climbs out of `worktree`,
/// which resolves somewhere else from a worktree than from the checkout it
/// was cloned from.
///
/// A cloned dependency tree with such a link would run the main checkout's
/// files from inside a worktree — the very sharing a worktree exists to
/// avoid. Relative links that stay inside the worktree, as npm's workspace
/// links do, resolve to the worktree's own files and are fine. Absolute
/// links elsewhere, such as a system interpreter, mean the same thing from
/// anywhere.
///
/// The link itself is read, never followed.
pub fn link_out_of(tree: &Path, worktree: &Path, main: &Path) -> io::Result<Option<PathBuf>> {
    let worktree = normalise(worktree);
    let mains: Vec<PathBuf> = [Some(normalise(main)), main.canonicalize().ok()]
        .into_iter()
        .flatten()
        .collect();
    let mut pending = vec![tree.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&path)?;
                let out = match target.is_absolute() {
                    true => {
                        let spellings = [normalise(&target), canonical_prefix(&target)];
                        spellings
                            .iter()
                            .any(|t| mains.iter().any(|m| t.starts_with(m)))
                    }
                    false => !normalise(&dir.join(&target)).starts_with(&worktree),
                };
                if out {
                    return Ok(Some(path));
                }
            }
        }
    }
    Ok(None)
}

/// `path` with its deepest existing ancestor resolved by the filesystem and
/// the rest appended: `/var/x/missing` becomes `/private/var/x/missing` on
/// macOS, where `/var` is a link.
fn canonical_prefix(path: &Path) -> PathBuf {
    let path = normalise(path);
    for ancestor in path.ancestors() {
        if let Ok(real) = ancestor.canonicalize() {
            let rest = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return real.join(rest);
        }
    }
    path
}

/// `path` with `.` dropped and each `..` taking the component before it,
/// without asking the filesystem.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether the filesystem under `dir` can clone at all: the tests that
/// prove sharing run only where it can, and every other test holds on any
/// filesystem.
#[cfg(test)]
pub(crate) fn can_clone(dir: &Path) -> bool {
    let src = dir.join(".probe-src");
    let dst = dir.join(".probe-dst");
    std::fs::write(&src, b"probe").unwrap();
    let ok = clone_file(&src, &dst).is_ok();
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dst);
    ok
}

#[cfg(target_os = "macos")]
mod imp {
    use std::io;
    use std::path::Path;

    fn cstring(path: &Path) -> io::Result<std::ffi::CString> {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))
    }

    /// `<sys/clonefile.h>`; the libc crate declares `clonefile` but not its
    /// flags. `CLONE_NOFOLLOW`: a symlink at `src` is cloned as a link, never
    /// as its target. `CLONE_NOOWNERCOPY`: the clone belongs to whoever runs
    /// pando, as a file git wrote would.
    const CLONE_NOFOLLOW: u32 = 0x0001;
    const CLONE_NOOWNERCOPY: u32 = 0x0002;

    /// `clonefile(2)`: `ENOTSUP` where the volume cannot clone, `EXDEV`
    /// across volumes.
    pub(super) const UNSUPPORTED: [i32; 2] = [libc::ENOTSUP, libc::EXDEV];

    /// `<sys/stat.h>`: a file whose contents the system evicted.
    const SF_DATALESS: u32 = 0x4000_0000;

    /// `<sys/resource.h>`: whether this thread may download a dataless file
    /// on access. Off, such a file fails with `EDEADLK` instead.
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: i32 = 3;
    const IOPOL_SCOPE_THREAD: i32 = 1;
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: i32 = 1;

    unsafe extern "C" {
        fn getiopolicy_np(iotype: i32, scope: i32) -> i32;
        fn setiopolicy_np(iotype: i32, scope: i32, policy: i32) -> i32;
    }

    /// One call clones a file or, recursively, a whole directory, keeping
    /// symlinks as links, modes and times.
    ///
    /// With this thread's downloads of dataless files off for the call: a
    /// tree with an evicted file in it fails instead of filling the main
    /// checkout's disk, and the caller leaves it to git or the install.
    fn clonefile(src: &Path, dst: &Path) -> io::Result<()> {
        let (src, dst) = (cstring(src)?, cstring(dst)?);
        let flags = CLONE_NOFOLLOW | CLONE_NOOWNERCOPY;
        // SAFETY: the policy calls take and return integers for this thread
        // alone; the clone's pointers are NUL-terminated strings that
        // outlive the call, which reads them only during it.
        unsafe {
            let before = getiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
            );
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_THREAD,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            );
            let done = libc::clonefile(src.as_ptr(), dst.as_ptr(), flags);
            let result = match done {
                0 => Ok(()),
                _ => Err(io::Error::last_os_error()),
            };
            if before >= 0 {
                setiopolicy_np(
                    IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                    IOPOL_SCOPE_THREAD,
                    before,
                );
            }
            result
        }
    }

    pub(super) fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
        if !src.symlink_metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        clonefile(src, dst)
    }

    pub(super) fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
        if !src.symlink_metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory",
            ));
        }
        clonefile(src, dst)
    }

    pub(super) fn drop_quarantine(path: &Path) -> io::Result<()> {
        let path = cstring(path)?;
        let name = c"com.apple.quarantine";
        // SAFETY: both are NUL-terminated strings that outlive the call.
        let done = unsafe { libc::removexattr(path.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) };
        match done {
            0 => Ok(()),
            _ => {
                let e = io::Error::last_os_error();
                match e.raw_os_error() == Some(libc::ENOATTR) {
                    true => Ok(()),
                    false => Err(e),
                }
            }
        }
    }

    pub(super) fn must_not_clone(meta: &std::fs::Metadata) -> bool {
        use std::os::macos::fs::MetadataExt;
        let flags = libc::UF_IMMUTABLE
            | libc::UF_APPEND
            | libc::SF_IMMUTABLE
            | libc::SF_APPEND
            | SF_DATALESS;
        meta.st_flags() & flags != 0
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::{self, File, OpenOptions};
    use std::io;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::Path;

    /// `ioctl_ficlone(2)`: `EOPNOTSUPP` where the filesystem cannot reflink,
    /// `EXDEV` across filesystems (and across mounts before 5.18), and
    /// `EINVAL`, `EBADF` or `ENOSYS` from filesystems and kernels that do
    /// not take the call at all.
    pub(super) const UNSUPPORTED: [i32; 5] = [
        libc::EOPNOTSUPP,
        libc::EXDEV,
        libc::EINVAL,
        libc::EBADF,
        libc::ENOSYS,
    ];

    /// Linux has no quarantine attribute.
    pub(super) fn drop_quarantine(_: &Path) -> io::Result<()> {
        Ok(())
    }

    /// `FICLONE` makes a new inode, which carries none of the source's
    /// inode flags, and Linux has no dataless files.
    pub(super) fn must_not_clone(_: &std::fs::Metadata) -> bool {
        false
    }

    pub(super) fn clone_file(src: &Path, dst: &Path) -> io::Result<()> {
        let meta = src.symlink_metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a regular file",
            ));
        }
        let from = File::open(src)?;
        let to = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(meta.permissions().mode())
            .open(dst)?;
        // SAFETY: both descriptors are open for the duration of the call;
        // FICLONE reads the source descriptor as its argument.
        let done = unsafe { libc::ioctl(to.as_raw_fd(), libc::FICLONE, from.as_raw_fd()) };
        if done != 0 {
            let e = io::Error::last_os_error();
            drop(to);
            let _ = fs::remove_file(dst);
            return Err(e);
        }
        // The mode asked for at open is masked by the umask; the clone
        // should carry the source's exactly, as macOS's does.
        to.set_permissions(meta.permissions())?;
        let times = fs::FileTimes::new()
            .set_accessed(meta.accessed()?)
            .set_modified(meta.modified()?);
        to.set_times(times)
    }

    /// No call clones a directory on Linux, so the tree is walked: each
    /// directory made, each file cloned, each link recreated. A walk that
    /// fails part way removes what it made: only after its own
    /// `create_dir` succeeded, so nothing that was there before is touched.
    pub(super) fn clone_tree(src: &Path, dst: &Path) -> io::Result<()> {
        let meta = src.symlink_metadata()?;
        if !meta.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a directory",
            ));
        }
        fs::create_dir(dst)?;
        let walked = walk(src, dst, &meta);
        if walked.is_err() {
            let _ = super::remove_tree(dst);
        }
        walked
    }

    fn walk(src: &Path, dst: &Path, meta: &fs::Metadata) -> io::Result<()> {
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            let to = dst.join(entry.file_name());
            if kind.is_dir() {
                fs::create_dir(&to)?;
                walk(&entry.path(), &to, &entry.path().symlink_metadata()?)?;
            } else if kind.is_file() {
                clone_file(&entry.path(), &to)?;
            } else if kind.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(entry.path())?, &to)?;
            }
        }
        // Last, so a read-only directory can still be filled first, and
        // its time is not moved by what was made inside it: macOS's
        // clonefile keeps a directory's time, and a build tool that reads
        // it should see the same tree on both.
        let times = fs::FileTimes::new()
            .set_accessed(meta.accessed()?)
            .set_modified(meta.modified()?);
        File::open(dst)?.set_times(times)?;
        fs::set_permissions(dst, meta.permissions())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn a_cloned_file_has_the_contents_and_mode_of_its_source() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("run.sh");
        std::fs::write(&src, b"#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o755)).unwrap();
        let dst = dir.path().join("copy.sh");
        match clone_file(&src, &dst) {
            Ok(()) => {
                assert_eq!(std::fs::read(&dst).unwrap(), b"#!/bin/sh\necho hi\n");
                let mode = dst.metadata().unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o755);
            }
            Err(e) => {
                assert!(is_unsupported(&e), "a real failure: {e}");
                assert!(!dst.exists(), "an unsupported clone left a file behind");
            }
        }
    }

    #[test]
    fn a_tree_clone_over_an_existing_directory_leaves_it_as_it_was() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a"), b"a").unwrap();
        let dst = dir.path().join("dst");
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(dst.join("keep"), b"mine").unwrap();
        let err = clone_tree(&src, &dst).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(dst.join("keep")).unwrap(), b"mine");
    }

    #[test]
    fn remove_tree_removes_read_only_directories_and_never_follows_a_link() {
        let dir = tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("precious"), b"x").unwrap();
        let tree = dir.path().join("tree");
        std::fs::create_dir_all(tree.join("ro")).unwrap();
        std::fs::write(tree.join("ro/f"), b"f").unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("ro/link")).unwrap();
        std::fs::set_permissions(tree.join("ro"), std::fs::Permissions::from_mode(0o555)).unwrap();
        remove_tree(&tree).unwrap();
        assert!(tree.symlink_metadata().is_err());
        assert!(outside.join("precious").exists(), "removal followed a link");
    }

    #[test]
    fn a_clone_never_replaces_what_is_there() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("a");
        let dst = dir.path().join("b");
        std::fs::write(&src, b"new").unwrap();
        std::fs::write(&dst, b"mine").unwrap();
        assert!(clone_file(&src, &dst).is_err());
        assert_eq!(std::fs::read(&dst).unwrap(), b"mine");
    }

    #[test]
    fn a_symlink_is_not_a_file_to_clone() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("real"), b"x").unwrap();
        std::os::unix::fs::symlink("real", dir.path().join("link")).unwrap();
        let err = clone_file(&dir.path().join("link"), &dir.path().join("out")).unwrap_err();
        assert!(!is_unsupported(&err), "{err}");
        assert!(dir.path().join("out").symlink_metadata().is_err());
    }

    #[test]
    fn a_cloned_tree_keeps_its_files_directories_and_links() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("node_modules");
        std::fs::create_dir_all(src.join("pkg/lib")).unwrap();
        std::fs::write(src.join("pkg/lib/index.js"), b"module.exports = 1\n").unwrap();
        std::fs::create_dir_all(src.join(".bin")).unwrap();
        std::os::unix::fs::symlink("../pkg/lib/index.js", src.join(".bin/pkg")).unwrap();
        let dst = dir.path().join("clone");
        match clone_tree(&src, &dst) {
            Ok(()) => {
                assert_eq!(
                    std::fs::read(dst.join("pkg/lib/index.js")).unwrap(),
                    b"module.exports = 1\n"
                );
                assert_eq!(
                    std::fs::read_link(dst.join(".bin/pkg")).unwrap(),
                    Path::new("../pkg/lib/index.js")
                );
            }
            Err(e) => {
                assert!(is_unsupported(&e), "a real failure: {e}");
                assert!(dst.symlink_metadata().is_err(), "a half tree was left");
            }
        }
    }

    #[test]
    fn writing_to_a_clone_leaves_the_source_as_it_was() {
        let dir = tempdir().unwrap();
        if !can_clone(dir.path()) {
            return;
        }
        let src = dir.path().join("a");
        let dst = dir.path().join("b");
        std::fs::write(&src, b"original").unwrap();
        clone_file(&src, &dst).unwrap();
        std::fs::write(&dst, b"changed in the worktree").unwrap();
        assert_eq!(std::fs::read(&src).unwrap(), b"original");
    }

    #[test]
    fn unsupported_is_only_the_cannot_clone_errors() {
        assert!(is_unsupported(&io::Error::from_raw_os_error(libc::EXDEV)));
        #[cfg(target_os = "macos")]
        assert!(is_unsupported(&io::Error::from_raw_os_error(libc::ENOTSUP)));
        #[cfg(target_os = "linux")]
        assert!(is_unsupported(&io::Error::from_raw_os_error(
            libc::EOPNOTSUPP
        )));
        assert!(!is_unsupported(&io::Error::from_raw_os_error(libc::ENOENT)));
        assert!(!is_unsupported(&io::Error::from_raw_os_error(libc::ENOSPC)));
        assert!(!is_unsupported(&io::Error::from_raw_os_error(libc::EEXIST)));
    }

    #[test]
    fn a_link_into_the_main_checkout_is_found_and_one_inside_the_worktree_is_not() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("main");
        let wt = dir.path().join("wt");
        let tree = wt.join("node_modules");
        std::fs::create_dir_all(main.join("packages/sdk")).unwrap();
        std::fs::create_dir_all(tree.join("pkg")).unwrap();
        std::os::unix::fs::symlink("pkg", tree.join("alias")).unwrap();
        // npm's workspace link: relative, resolved inside the worktree.
        std::os::unix::fs::symlink("../packages/sdk", tree.join("sdk")).unwrap();
        // A system interpreter means the same thing from anywhere.
        std::os::unix::fs::symlink("/usr/bin/env", tree.join("env")).unwrap();
        assert_eq!(link_out_of(&tree, &wt, &main).unwrap(), None);

        std::os::unix::fs::symlink(main.join("packages/sdk"), tree.join("pkg/abs")).unwrap();
        assert_eq!(
            link_out_of(&tree, &wt, &main).unwrap(),
            Some(tree.join("pkg/abs"))
        );
    }

    // On macOS `/var` is a link to `/private/var`, and the temp directory is
    // under it: a link written with the other spelling is the same place.
    #[test]
    fn a_link_into_the_main_checkout_under_another_spelling_is_found() {
        let dir = tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        let main = real.join("main");
        let wt = real.join("wt");
        let tree = wt.join("node_modules");
        std::fs::create_dir_all(main.join("lib")).unwrap();
        std::fs::create_dir_all(&tree).unwrap();
        // A spelling through a link of our own, so the test holds anywhere.
        std::os::unix::fs::symlink(&real, real.join("alias")).unwrap();
        std::os::unix::fs::symlink(real.join("alias/main/lib"), tree.join("lib")).unwrap();
        assert_eq!(
            link_out_of(&tree, &wt, &main).unwrap(),
            Some(tree.join("lib"))
        );
    }

    #[test]
    fn a_relative_link_that_climbs_out_of_the_worktree_is_found() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("main");
        let wt = dir.path().join("wt");
        let tree = wt.join("node_modules");
        std::fs::create_dir_all(&tree).unwrap();
        // `file:../sibling` in the main checkout means a sibling of the
        // main checkout; from a worktree under pando's home, something else.
        std::os::unix::fs::symlink("../../sibling", tree.join("climb")).unwrap();
        assert_eq!(
            link_out_of(&tree, &wt, &main).unwrap(),
            Some(tree.join("climb"))
        );
    }

    #[test]
    fn a_clone_made_to_look_git_written_has_gits_mode() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("secret");
        std::fs::write(&file, b"x").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        as_git_writes(&file, false, 0o022).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o644);
        as_git_writes(&file, true, 0o002).unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o775);
    }
}
