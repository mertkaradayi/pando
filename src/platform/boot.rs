//! Which boot this is: an id that changes whenever pids start again, a
//! pid namespace restarting under a kernel that keeps running among them.

/// A boot of the machine, as something written down during it records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Boot<'a> {
    /// The system's own id of this boot: the same for every process until
    /// the machine restarts, and different after.
    pub id: &'a str,
    /// When the pids now in use began to be handed out, where that can be
    /// later than the boot: on Linux, init's start time.
    ///
    /// The kernel's id alone does not see every restart that hands pids
    /// out again. A WSL 2 distro restarts on a kernel that keeps running
    /// (the VM stays up for Docker Desktop, or for another distro), and so
    /// does a container: the id is the same, while every pid starts again
    /// from the bottom. Init's start time changes with each such restart
    /// and with nothing else.
    ///
    /// Apart from `id`, never folded into it: a pando that knows only the
    /// id compares it whole, and would take every boot this one records
    /// for another.
    pub pids_since: Option<u64>,
}

/// This boot, read once. `None` where the system does not say which boot
/// this is.
pub fn now() -> Option<Boot<'static>> {
    static NOW: std::sync::OnceLock<Option<(String, Option<u64>)>> = std::sync::OnceLock::new();
    NOW.get_or_init(|| Some((imp::id()?, imp::pids_since())))
        .as_ref()
        .map(|(id, pids_since)| Boot {
            id,
            pids_since: *pids_since,
        })
}

/// Whether something written down during boot `written` was written
/// during this one, `now`.
///
/// Both knowing when their pids began: the same id and the same start.
/// One not knowing — written by a pando that did not record it, or read
/// where `/proc/1` could not be — is judged on the id alone, which is all
/// it can be judged on: not knowing is not evidence of a restart.
pub fn same(written: Boot<'_>, now: Boot<'_>) -> bool {
    written.id == now.id
        && match (written.pids_since, now.pids_since) {
            (Some(written), Some(now)) => written == now,
            _ => true,
        }
}

/// The start time in a `/proc/<pid>/stat` line, in clock ticks since the
/// kernel booted: field 22, counted after the `)` that ends the command
/// name, since a name may hold spaces and parentheses of its own.
///
/// Compiled everywhere so its tests run everywhere; only Linux reads it.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(super) fn parse_start_time(stat: &str) -> Option<u64> {
    let rest = &stat[stat.rfind(')')? + 1..];
    // After comm: state is field 3, so field 22 is the 20th.
    rest.split_whitespace().nth(19)?.parse().ok()
}

#[cfg(target_os = "macos")]
mod imp {
    /// `kern.bootsessionuuid`: a new one at every boot.
    pub(super) fn id() -> Option<String> {
        let name = c"kern.bootsessionuuid";
        let mut len: libc::size_t = 0;
        // SAFETY: a null buffer asks for the length only.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len];
        // SAFETY: `buf` is `len` bytes long, which is what the kernel is told.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                buf.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        buf.truncate(len);
        let id = String::from_utf8_lossy(&buf)
            .trim_end_matches('\0')
            .trim()
            .to_string();
        (!id.is_empty()).then_some(id)
    }

    /// Pids start again only when the machine does.
    pub(super) fn pids_since() -> Option<u64> {
        None
    }
}

#[cfg(target_os = "linux")]
mod imp {
    /// The kernel's boot id.
    pub(super) fn id() -> Option<String> {
        let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        let id = id.trim().to_string();
        (!id.is_empty()).then_some(id)
    }

    /// When this pid namespace's init started, in clock ticks since the
    /// kernel booted; `None` where `/proc/1` cannot be read.
    pub(super) fn pids_since() -> Option<u64> {
        let stat = std::fs::read_to_string("/proc/1/stat").ok()?;
        super::parse_start_time(&stat)
    }
}

/// Not read yet: the time the system booted is the way to one. Without
/// one, nothing recorded is taken for another boot's.
#[cfg(windows)]
mod imp {
    pub(super) fn id() -> Option<String> {
        None
    }

    pub(super) fn pids_since() -> Option<u64> {
        None
    }
}
