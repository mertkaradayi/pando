//! The one layer that talks to the operating system.
//!
//! Everything pando asks of the OS goes through here: starting process
//! groups and stopping them, locks and permission bits, which boot this
//! is, the shell a command string runs in, what a desktop opens a URL
//! with. Nothing above this layer names an OS, and `tests.rs` holds every
//! other module to that.
//!
//! One file per concern. Each is a facade whose functions carry the
//! contract every backend keeps, and call `imp`: an inline
//! `#[cfg(…)] mod imp` when the backend is short, a file per OS when it is
//! long. A difference between macOS and Linux inside a Unix backend stays
//! a small `#[cfg(target_os)]` item, and a pure parser is compiled on every
//! OS its backend builds for, so its tests run on every runner that has
//! tests: `ss`'s on macOS, `lsof`'s on Linux. Tests run on macOS and Linux
//! alone until a native port, so a parser a Windows backend asks for sits
//! in the facade, where they run too (`files::as_seen_from`).
//!
//! What the OS is, and what pando finds at run time, is [`Host`]: a value
//! handed down from where pando meets the outside, read once for this
//! machine or from files under a root a test chooses. Whether a Linux is
//! Windows' own, [`Wsl`], is one of those findings.

pub mod boot;
pub mod cow;
pub mod desktop;
pub mod dirs;
pub mod files;
pub mod host;
pub mod process;
pub mod shell;
pub mod signals;
pub mod terminal;
pub mod wsl;

pub use host::{Host, Os};
pub use wsl::Wsl;

/// What pando does first, before any thread starts: reads the umask, which
/// means setting it process-wide for a moment, and makes a fork safe on a
/// system where another thread could make it unsafe.
pub fn init() {
    let _ = cow::umask();
    process::settle_before_fork();
}

/// Why pando cannot run on this OS yet, or `None` where it can.
///
/// A native Windows build compiles, so the layer's Windows backends are
/// held to compiling by CI, but most of them only say "not yet": it
/// refuses here, after `--help` and `--version`, rather than failing
/// halfway through a command. `completions` only prints, so `main` lets
/// it through.
pub fn unsupported() -> Option<&'static str> {
    match Os::HERE {
        Os::Windows => Some(
            "a native Windows build runs nothing yet — run pando inside WSL 2, with the \
             repository in WSL's own filesystem: https://github.com/mertkaradayi/pando#install",
        ),
        Os::MacOs | Os::Linux => None,
    }
}

#[cfg(test)]
mod tests;
