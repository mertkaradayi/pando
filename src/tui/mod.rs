//! The ratatui front end: terminal setup, the event loop, and the watchers
//! that feed it. Like `cli`, a thin wrapper — behaviour lives in `actions`.

pub mod app;
mod handoff;
mod modal;
mod render;

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_full::{DebouncedEvent, Debouncer, RecommendedCache, new_debouncer};
use ratatui::crossterm::event::{self, DisableFocusChange, EnableFocusChange};
use ratatui::crossterm::execute;
use std::io::stdout;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use crate::config::Config;
use crate::paths::PandoPaths;
use app::AppEvent;
use handoff::InputGate;

const TICK_INTERVAL: Duration = Duration::from_millis(250);

/// How often the theme watcher looks again at the file `[ui] theme_from`
/// names and at whether the system is dark or light. Each look runs one
/// small program, so not every tick; often enough that a terminal switched
/// to another theme takes pando with it before anyone waits for it.
const THEME_POLL: Duration = Duration::from_secs(2);
const FS_DEBOUNCE: Duration = Duration::from_millis(500);

/// How long the input thread waits for a key before it checks whether a
/// suspended program wants the terminal. Short enough that a suspend
/// starts at once.
const INPUT_POLL: Duration = Duration::from_millis(50);

/// What asks the terminal for the mouse: presses, releases and the wheel
/// (1000), drags (1002), in SGR's coordinates (1006), which a column past
/// 223 does not break. A drag is how text was selected before pando took
/// the mouse, and its first step says how to select it now: worth the
/// write each step costs while a button is held.
/// Not every move of the pointer (1003), which crossterm's
/// `EnableMouseCapture` asks for as well: nothing here answers a hover,
/// and over SSH or through tmux each move is a write.
const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";

/// What gives the mouse back, in the reverse order.
const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

pub fn run(paths: PandoPaths, config: Config) -> Result<()> {
    // The terminal is the TUI's from here on: a hook that prompted on it
    // would draw over the frame and wait on keys pando reads. A hook
    // detached from it is hung up on when pando is.
    crate::hooks::detach_from_terminal();
    // Before anything creates a directory under it: the watcher and the
    // enrichment cache both `create_dir_all` their way down, and a home made
    // by one of those would carry the umask instead of 0700.
    paths.ensure_home()?;
    let worktrees_dir = config.worktrees_dir(&paths);
    // Before the first frame, so the screen never flashes another theme.
    let themes_dir = paths.themes_dir();
    let resolved = crate::theme::resolve(&config.ui.theme_settings(), Some(&themes_dir));
    let mut app = app::App::new(paths, config)?;
    let watched = (resolved.name.clone(), resolved.appearance);
    app.adopt_theme(resolved);
    spawn_theme_watcher(
        app.event_tx.clone(),
        Arc::clone(&app.theme.settings),
        themes_dir,
        watched,
    );
    // A process with no terminal to take is told so rather than panicking.
    // Raw mode is switched on before the screen, so a failure between the
    // two still gives it back.
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(e) => {
            let _ = ratatui::try_restore();
            return Err(anyhow::Error::new(e).context("could not open the terminal for the TUI"));
        }
    };
    // ratatui's panic hook gives back raw mode and the screen, and not
    // what is asked for below: after a panic the shell would print every
    // click and every change of focus. This one runs first, then ratatui's.
    let restore = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        set_mouse(false);
        let _ = execute!(stdout(), DisableFocusChange);
        restore(info);
    }));
    // Focus reports bring a refresh when the pane is come back to: a pull
    // request merged in a browser or another pane shows then. A terminal
    // that sends none still has the periodic one.
    let _ = execute!(stdout(), EnableFocusChange);
    let result = main_loop(&mut terminal, &mut app, &worktrees_dir);
    // Whatever the loop last told the terminal: one that ended in an
    // error never said.
    set_mouse(false);
    let _ = execute!(stdout(), DisableFocusChange);
    ratatui::restore();
    // `q` twice abandons what is in flight, and nothing waits for a hook
    // once the TUI has gone.
    crate::hooks::hang_up_detached();
    result
}

fn main_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut app::App,
    worktrees_dir: &Path,
) -> Result<()> {
    let event_rx = app
        .event_rx
        .take()
        .expect("event_rx is present until the loop takes it");

    let gate = Arc::new(InputGate::default());
    spawn_input_thread(app.event_tx.clone(), Arc::clone(&gate));
    let tick_waiting = Arc::new(AtomicBool::new(false));
    spawn_tick_thread(app.event_tx.clone(), Arc::clone(&tick_waiting));
    // The watcher is an optimisation for pando's own worktrees; the slow
    // tick's porcelain refresh is the source of truth, and it sees adopted
    // worktrees anywhere on disk.
    let _watcher = spawn_fs_watcher(worktrees_dir, app.event_tx.clone());

    terminal.draw(|f| render::render(f, app))?;
    let mut mouse = follow_mouse(false, app.wants_mouse());
    loop {
        let Ok(ev) = event_rx.recv() else {
            return Ok(());
        };
        let mut redraw = deliver(app, ev, &tick_waiting);
        // Coalesce whatever piled up while drawing, so a burst (typing, a
        // resize, an enrichment flood) costs one redraw.
        loop {
            match event_rx.try_recv() {
                Ok(more) => redraw |= deliver(app, more, &tick_waiting),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return Ok(()),
            }
        }
        // A shell or editor `!` or `e` asked for. Run here because this is
        // where the terminal is.
        if let Some(request) = app.launch.take() {
            match handoff::carry_out(request, terminal, &gate, &app.event_tx, mouse) {
                Ok(Some(done)) => app.set_success(done),
                Ok(None) => {}
                Err(e) => app.set_error(e),
            }
            // Whatever changed while it ran — a commit, a server stopped
            // from the shell, a file edited, a pull request merged with
            // `gh` — shows at once rather than on the slow tick.
            app.came_back();
            redraw = true;
        }
        if redraw {
            terminal.draw(|f| render::render(f, app))?;
        }
        // The setup screen opening or closing is a frame like any other,
        // so this is the one place the mouse is taken or given back.
        mouse = follow_mouse(mouse, app.wants_mouse());
        if app.should_quit {
            return Ok(());
        }
    }
}

/// Tells the terminal to report the mouse, or to stop, when what the app
/// wants is not what the terminal was last told. Returns what it is told
/// now.
fn follow_mouse(on: bool, wanted: bool) -> bool {
    if on != wanted {
        set_mouse(wanted);
    }
    wanted
}

/// Asks the terminal to report the mouse to pando, or to stop. Written as
/// it is rather than as a crossterm command, the way the OSC 52 copy is:
/// one write to the terminal pando owns, with nothing in it that depends
/// on the OS.
fn set_mouse(on: bool) {
    use std::io::Write as _;
    let mut out = stdout();
    let _ = out.write_all(if on { MOUSE_ON } else { MOUSE_OFF }.as_bytes());
    let _ = out.flush();
}

/// Follows the theme while pando runs: a theme switcher writing a new
/// name to the file `[ui] theme_from` names, a choice saved from the
/// picker, the system going dark or light. Only a change is sent.
fn spawn_theme_watcher(
    tx: Sender<AppEvent>,
    settings: Arc<std::sync::Mutex<crate::theme::Settings>>,
    themes_dir: std::path::PathBuf,
    mut last: (String, crate::theme::Appearance),
) {
    thread::spawn(move || {
        loop {
            thread::sleep(THEME_POLL);
            let current = match settings.lock() {
                Ok(settings) => settings.clone(),
                Err(_) => return,
            };
            let resolved = crate::theme::resolve(&current, Some(&themes_dir));
            let now = (resolved.name.clone(), resolved.appearance);
            if now == last {
                continue;
            }
            last = now;
            if tx.send(AppEvent::Theme(Box::new(resolved))).is_err() {
                return;
            }
        }
    });
}

fn spawn_input_thread(tx: Sender<AppEvent>, gate: Arc<InputGate>) {
    thread::spawn(move || {
        loop {
            // Polled rather than a bare `read`, so the thread comes back
            // here often enough to step aside for a suspended program.
            gate.wait_if_paused();
            match event::poll(INPUT_POLL) {
                Ok(false) => continue,
                Ok(true) => {}
                Err(_) => return,
            }
            match event::read() {
                Ok(ev) => {
                    if tx.send(AppEvent::Input(ev)).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        }
    });
}

/// Hands an event to the app. A tick taken lets the tick thread send the
/// next one.
fn deliver(app: &mut app::App, ev: AppEvent, tick_waiting: &AtomicBool) -> bool {
    if matches!(ev, AppEvent::Tick) {
        tick_waiting.store(false, Ordering::Release);
    }
    app.handle_event(ev)
}

/// A tick every `TICK_INTERVAL`, and never a second one while the last is
/// still waiting in the channel. A tick is time passing, not work to do:
/// while a shell or editor has the terminal the loop takes nothing, and
/// every tick queued meanwhile would be replayed on the way back, with a
/// discovery for every twentieth of them.
fn spawn_tick_thread(tx: Sender<AppEvent>, waiting: Arc<AtomicBool>) {
    thread::spawn(move || {
        loop {
            thread::sleep(TICK_INTERVAL);
            if waiting.swap(true, Ordering::AcqRel) {
                continue;
            }
            if tx.send(AppEvent::Tick).is_err() {
                return;
            }
        }
    });
}

/// Watches the worktrees directory non-recursively. The returned debouncer
/// owns the watcher; dropping it stops the thread. `None` when the watcher
/// cannot start — the slow tick still refreshes.
fn spawn_fs_watcher(
    dir: &Path,
    app_tx: Sender<AppEvent>,
) -> Option<Debouncer<RecommendedWatcher, RecommendedCache>> {
    // notify fails on a path that does not exist, and this one does not
    // until the first `new`.
    std::fs::create_dir_all(dir).ok()?;
    let (raw_tx, raw_rx) = mpsc::channel();
    let mut debouncer = new_debouncer(FS_DEBOUNCE, None, raw_tx).ok()?;
    debouncer.watch(dir, RecursiveMode::NonRecursive).ok()?;
    // FSEvents reports resolved paths, so relevance checks compare against
    // the canonicalized watched directory.
    let watched = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    thread::spawn(move || {
        while let Ok(batch) = raw_rx.recv() {
            let relevant = match &batch {
                Ok(events) => fs_batch_is_relevant(&watched, events),
                Err(_) => true,
            };
            if relevant && app_tx.send(AppEvent::FsChange).is_err() {
                return;
            }
        }
    });
    Some(debouncer)
}

/// True when a debounced batch touches something discovery would list: a
/// path whose first component under the watched directory is not
/// dot-prefixed. Fails open on anything ambiguous.
fn fs_batch_is_relevant(watched: &Path, events: &[DebouncedEvent]) -> bool {
    if events.is_empty() {
        return true;
    }
    events.iter().any(|ev| {
        if ev.paths.is_empty() {
            return true;
        }
        ev.paths.iter().any(|path| {
            let Ok(relative) = path.strip_prefix(watched) else {
                return true;
            };
            let Some(first) = relative.components().next() else {
                return true;
            };
            !first.as_os_str().to_string_lossy().starts_with('.')
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::Event;
    use notify::event::{CreateKind, EventKind};
    use std::path::PathBuf;
    use std::time::Instant;

    fn event_with_paths(paths: &[PathBuf]) -> DebouncedEvent {
        let mut event = Event::new(EventKind::Create(CreateKind::Any));
        for path in paths {
            event = event.add_path(path.clone());
        }
        DebouncedEvent::new(event, Instant::now())
    }

    fn watched() -> PathBuf {
        PathBuf::from("/home/.pando/projects/acme-3f9a2c1d/worktrees")
    }

    #[test]
    fn a_new_worktree_directory_is_relevant() {
        let batch = [event_with_paths(&[watched().join("feat+one")])];
        assert!(fs_batch_is_relevant(&watched(), &batch));
    }

    #[test]
    fn dot_prefixed_artifacts_are_not_relevant() {
        let batch = [
            event_with_paths(&[watched().join(".scratch")]),
            event_with_paths(&[watched().join(".hidden/thing.log")]),
        ];
        assert!(!fs_batch_is_relevant(&watched(), &batch));
    }

    #[test]
    fn a_mixed_batch_is_relevant() {
        let batch = [
            event_with_paths(&[watched().join(".scratch")]),
            event_with_paths(&[watched().join("feat+one")]),
        ];
        assert!(fs_batch_is_relevant(&watched(), &batch));
    }

    #[test]
    fn anything_ambiguous_fails_open() {
        assert!(fs_batch_is_relevant(&watched(), &[]));
        assert!(fs_batch_is_relevant(&watched(), &[event_with_paths(&[])]));
        assert!(fs_batch_is_relevant(
            &watched(),
            &[event_with_paths(&[watched()])]
        ));
        assert!(fs_batch_is_relevant(
            &watched(),
            &[event_with_paths(&[PathBuf::from("/somewhere/else")])]
        ));
    }

    // A tick is time, not work: a suspended shell's half hour of them was
    // replayed on the way back, and every twentieth started a discovery.
    #[test]
    fn a_tick_not_yet_taken_is_not_joined_by_another() {
        let (tx, rx) = mpsc::channel();
        let waiting = Arc::new(AtomicBool::new(false));
        spawn_tick_thread(tx, Arc::clone(&waiting));
        let first = rx.recv_timeout(Duration::from_secs(10));
        assert!(matches!(first, Ok(AppEvent::Tick)), "no first tick");
        thread::sleep(TICK_INTERVAL * 4);
        assert!(rx.try_recv().is_err(), "a second tick queued behind it");
        waiting.store(false, Ordering::Release);
        let next = rx.recv_timeout(Duration::from_secs(10));
        assert!(
            matches!(next, Ok(AppEvent::Tick)),
            "and none once it was taken"
        );
        // Closed, so the thread's next send fails and it ends.
        drop(rx);
        waiting.store(false, Ordering::Release);
    }

    // Not every move of the pointer: nothing answers a hover, and each is a
    // write over SSH. What is asked for is given back, every mode of it.
    #[test]
    fn the_mouse_is_asked_for_without_its_moves_and_given_back_whole() {
        let modes = |sequence: &str, end: char| -> Vec<String> {
            let mut modes: Vec<String> = sequence
                .split("\x1b[?")
                .filter(|mode| !mode.is_empty())
                .map(|mode| {
                    mode.strip_suffix(end)
                        .unwrap_or_else(|| panic!("{sequence:?}"))
                        .to_string()
                })
                .collect();
            modes.sort();
            modes
        };
        let on = modes(MOUSE_ON, 'h');
        assert!(!on.contains(&"1003".to_string()), "{MOUSE_ON:?}");
        assert!(
            on.contains(&"1006".to_string()),
            "SGR, or a click past column 223 is garbage"
        );
        assert_eq!(on, modes(MOUSE_OFF, 'l'));
    }

    #[test]
    fn the_watcher_creates_the_directory_it_watches() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("projects").join("p").join("worktrees");
        let (tx, _rx) = mpsc::channel();
        let watcher = spawn_fs_watcher(&target, tx);
        assert!(target.is_dir(), "notify cannot watch a missing directory");
        drop(watcher);
    }
}
