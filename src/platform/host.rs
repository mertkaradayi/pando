//! What pando runs on: the OS it was built for, and what it finds at run
//! time.

use std::path::Path;

use super::Wsl;

/// The operating system pando was built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    /// The OS this build is for.
    #[cfg(target_os = "macos")]
    pub const HERE: Os = Os::MacOs;
    /// The OS this build is for.
    #[cfg(target_os = "linux")]
    pub const HERE: Os = Os::Linux;
    /// The OS this build is for.
    #[cfg(windows)]
    pub const HERE: Os = Os::Windows;
}

/// The machine pando runs on, as far as anything above this layer may
/// know it: the OS of the build, and what pando reads about the machine
/// at run time, each a field of its own.
///
/// A field rather than a `cfg`, so a decision that depends on the OS is
/// made from a value a test can choose, and both of its branches run on
/// every CI runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    pub os: Os,
    /// The WSL this Linux is, when it is one: read at run time, since a
    /// distro runs the ordinary Linux build.
    pub wsl: Option<Wsl>,
}

impl Host {
    /// This machine, read once.
    ///
    /// Under `cargo test`, the OS of the build and nothing read from the
    /// machine: a test run inside WSL never takes it for WSL, and opens and
    /// copies as the build's own desktop does. A test that wants WSL
    /// describes it with [`Host::at`].
    ///
    /// Read only where pando meets the outside: [`crate::actions::Machine`],
    /// the TUI's launch environment, `pando open`, and the theme. Everything
    /// below them is handed a `&Host`, which `tests.rs` holds them to.
    pub fn here() -> &'static Host {
        static HERE: std::sync::OnceLock<Host> = std::sync::OnceLock::new();
        HERE.get_or_init(|| {
            #[cfg(test)]
            {
                Host::default()
            }
            #[cfg(not(test))]
            {
                Host::at(Path::new("/"))
            }
        })
    }

    /// A machine whose run-time facts are read from files under `system`:
    /// `/` on a real one, a directory of a test's own in a test.
    pub fn at(system: &Path) -> Host {
        Host {
            os: Os::HERE,
            wsl: Wsl::at(system),
        }
    }
}

impl Default for Host {
    /// This build's OS, with nothing read at run time.
    fn default() -> Host {
        Host {
            os: Os::HERE,
            wsl: None,
        }
    }
}
