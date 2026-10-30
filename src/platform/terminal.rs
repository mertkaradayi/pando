//! Reading a line with the terminal's echo off, and whether pando is what
//! the terminal is running in front.

/// A line from the terminal with nothing it types shown: a password is not
/// for the scrollback, or for whoever is looking at the screen.
///
/// The terminal's echo is switched off for the one read and back on after
/// it, newline included, so the next line of output starts where it
/// should. A stdin that is not a terminal has no echo to switch.
pub fn read_line_unechoed(line: &mut String) -> std::io::Result<usize> {
    imp::read_line_unechoed(line)
}

/// Whether pando's process group is the foreground group of its
/// controlling terminal: where what pando runs in its own group can
/// prompt, and gets the terminal's Ctrl-C as pando does. `false` with no
/// controlling terminal at all — an agent's pando, CI's, a service's.
pub fn in_foreground() -> bool {
    imp::in_foreground()
}

#[cfg(unix)]
mod imp {
    use std::os::unix::io::AsRawFd;

    pub(super) fn in_foreground() -> bool {
        // Opening it reads nothing, so a background pando is not stopped
        // for asking; with no controlling terminal the open fails.
        let Ok(tty) = std::fs::File::open("/dev/tty") else {
            return false;
        };
        // SAFETY: `tty` is open for as long as the call, and `getpgrp`
        // cannot fail.
        let front = unsafe { libc::tcgetpgrp(tty.as_raw_fd()) };
        front > 0 && front == unsafe { libc::getpgrp() }
    }

    pub(super) fn read_line_unechoed(line: &mut String) -> std::io::Result<usize> {
        let fd = std::io::stdin().as_raw_fd();
        // SAFETY: a zeroed termios is a valid value to be overwritten by
        // `tcgetattr`, which is all it is used for when that call fails.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is this process's stdin and `saved` is a termios.
        let is_terminal = unsafe { libc::tcgetattr(fd, &mut saved) } == 0;
        if is_terminal {
            let mut quiet = saved;
            quiet.c_lflag &= !libc::ECHO;
            quiet.c_lflag |= libc::ECHONL;
            // SAFETY: as above; `quiet` is `saved` with two flags changed.
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
        }
        let read = std::io::stdin().read_line(line);
        if is_terminal {
            // SAFETY: puts back exactly what `tcgetattr` returned.
            unsafe { libc::tcsetattr(fd, libc::TCSANOW, &saved) };
        }
        read
    }
}

/// Not done yet: the console's echo mode is the way to it. A console's
/// process is taken to be in front of it, so what pando runs keeps the
/// console as it always has.
#[cfg(windows)]
mod imp {
    pub(super) fn in_foreground() -> bool {
        true
    }

    pub(super) fn read_line_unechoed(_: &mut String) -> std::io::Result<usize> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "a native Windows build cannot read a line unechoed yet",
        ))
    }
}
