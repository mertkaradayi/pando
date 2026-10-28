//! Carrying out a [`Launch`]: the one place the TUI gives its terminal away.
//!
//! A tmux window and a GUI editor are handed off on a worker thread with
//! every stream redirected, like the browser opener. A suspend is the
//! exception to "never block on a child": the TUI leaves the alternate
//! screen, stops reading keys, and waits for the shell or editor on the UI
//! thread — there is nothing else for that thread to do while somebody
//! else owns the screen — then takes the terminal back.

use std::io::stdout;
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

use ratatui::crossterm::event::{DisableFocusChange, EnableFocusChange};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use super::app::{AppEvent, Launch, LaunchRequest, said_after};

/// Lets the event loop stop the input thread reading the terminal while a
/// suspended program owns it: crossterm's poll reads the tty, and a
/// keystroke it took would never reach the shell.
#[derive(Default)]
pub struct InputGate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(Default)]
struct GateState {
    paused: bool,
    parked: bool,
}

impl InputGate {
    /// Called by the input thread between polls: blocks while paused.
    pub fn wait_if_paused(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.paused {
            return;
        }
        state.parked = true;
        self.changed.notify_all();
        while state.paused {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state.parked = false;
    }

    /// Stops the input thread, and returns once it has stopped — or after
    /// a second, in case it has already gone and will never park.
    fn pause(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.paused = true;
        let _ = self
            .changed
            .wait_timeout_while(state, std::time::Duration::from_secs(1), |s| !s.parked);
    }

    fn resume(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.paused = false;
        self.changed.notify_all();
    }
}

/// Runs `request`. Returns what to say once it is done — nothing for a
/// hand-off, which the key already announced — or the error to show for a
/// suspend that could not run; a handed-off launch reports its own failure
/// as an event. `mouse` says whether the terminal reports the mouse now,
/// so a suspend gives it back as it found it.
pub fn carry_out(
    request: LaunchRequest,
    terminal: &mut ratatui::DefaultTerminal,
    gate: &Arc<InputGate>,
    tx: &Sender<AppEvent>,
    mouse: bool,
) -> Result<Option<String>, String> {
    let said = said_after(&request);
    match request.launch {
        Launch::Tmux { args, cwd } => {
            hand_off("tmux".to_string(), args, Some(cwd), tx.clone());
            Ok(said)
        }
        Launch::Detached { program, args, cwd } => {
            hand_off(program, args, Some(cwd), tx.clone());
            Ok(said)
        }
        Launch::Suspend { program, args, cwd } => {
            gate.pause();
            let _ = disable_raw_mode();
            // A shell given focus reports would print them as `^[[I`, and
            // one given the mouse every click as `^[[<0;12;5M`.
            if mouse {
                super::set_mouse(false);
            }
            let _ = execute!(stdout(), DisableFocusChange, LeaveAlternateScreen);
            // Every frame hides the cursor, and leaving the alternate
            // screen does not bring it back; a shell does not either, so
            // one opened by `!` would be typed into blind. The next frame
            // hides it again.
            let _ = terminal.show_cursor();
            let ran = Command::new(&program)
                .args(&args)
                .current_dir(&cwd)
                .status();
            let _ = enable_raw_mode();
            let _ = execute!(stdout(), EnterAlternateScreen, EnableFocusChange);
            if mouse {
                super::set_mouse(true);
            }
            let _ = terminal.clear();
            gate.resume();
            match ran {
                Ok(_) => Ok(said),
                Err(e) => Err(format!("could not run {program} in {}: {e}", cwd.display())),
            }
        }
    }
}

/// Starts `program` off the UI thread with its streams redirected, and
/// reports a failure — including a non-zero exit, which is how tmux says a
/// window could not be made — back as an event.
///
/// A `cwd` that is not there is said before anything runs: tmux given it
/// as `-c` opens the window in $HOME and exits 0, and any program run in
/// it fails with "No such file or directory", as if the program were
/// what is missing.
fn hand_off(
    program: String,
    args: Vec<String>,
    cwd: Option<std::path::PathBuf>,
    tx: Sender<AppEvent>,
) {
    thread::spawn(move || {
        if let Some(cwd) = &cwd
            && !cwd.is_dir()
        {
            let _ = tx.send(AppEvent::LaunchFailed(format!(
                "could not run {program}: {} is missing — git worktree prune removes the entry",
                cwd.display()
            )));
            return;
        }
        let mut command = Command::new(&program);
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(cwd) = &cwd {
            command.current_dir(cwd);
        }
        let failure = match command.output() {
            Ok(out) if out.status.success() => None,
            Ok(out) => Some(format!(
                "{program} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(e) => Some(format!("could not run {program}: {e}")),
        };
        if let Some(message) = failure {
            let _ = tx.send(AppEvent::LaunchFailed(message));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    // The input thread has to be out of crossterm's poll for the whole of
    // a suspend, or it steals the shell's keystrokes.
    #[test]
    fn a_paused_gate_holds_the_input_thread_until_resumed() {
        let gate = Arc::new(InputGate::default());
        let polls = Arc::new(AtomicUsize::new(0));
        let (g, p) = (Arc::clone(&gate), Arc::clone(&polls));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = Arc::clone(&stop);
        let reader = thread::spawn(move || {
            while !s.load(Ordering::SeqCst) {
                g.wait_if_paused();
                p.fetch_add(1, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(5));
            }
        });
        gate.pause();
        let held = polls.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(60));
        assert_eq!(polls.load(Ordering::SeqCst), held, "it read while paused");
        gate.resume();
        thread::sleep(Duration::from_millis(60));
        assert!(polls.load(Ordering::SeqCst) > held, "it never came back");
        stop.store(true, Ordering::SeqCst);
        reader.join().unwrap();
    }

    // Inside tmux, `!` on a worktree whose directory had gone opened a
    // shell in $HOME: tmux exits 0 for a `-c` it cannot enter, so nothing
    // took back the "opened a shell in …" the key had said.
    #[test]
    fn a_hand_off_into_a_missing_directory_says_so_and_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("feat+gone");
        let ran = dir.path().join("ran");
        let (tx, rx) = std::sync::mpsc::channel();
        hand_off(
            "/usr/bin/touch".to_string(),
            vec![ran.display().to_string()],
            Some(gone.clone()),
            tx,
        );
        let Ok(AppEvent::LaunchFailed(message)) = rx.recv_timeout(Duration::from_secs(10)) else {
            panic!("no failure came back");
        };
        assert!(
            message.contains(&format!("{} is missing", gone.display())),
            "{message}"
        );
        assert!(message.contains("git worktree prune"), "{message}");
        assert!(!ran.exists(), "it ran anyway");
    }

    #[test]
    fn pausing_a_gate_nobody_reads_does_not_hang() {
        let gate = InputGate::default();
        gate.pause();
        gate.resume();
    }
}
