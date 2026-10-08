use super::*;
use super::{dialogs::*, log_view::*, operations::*, pending::*, tails::*};
use crate::actions;
use crate::config::Config;
use crate::log_tail::{LogLevel, LogTail};
use crate::paths::PandoPaths;
use crate::process::Group;
use crate::project::ProjectRef;
use crate::state::State;
use crate::state::{Phase, ServiceMode};
use crate::worktree::BranchSource;
use crate::worktree::{BranchEntry, Worktree};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub fn wt(name: &str) -> Worktree {
    Worktree {
        name: name.to_string(),
        path: PathBuf::from("/trees").join(name),
        head: Some("abc123".into()),
        branch: Some(name.replace('+', "/")),
        detached: false,
        prunable: false,
        prunable_reason: None,
        locked: false,
        lock_reason: None,
        bare: false,
        created_at: None,
        head_sha: Some("abc1234".into()),
        head_subject: Some("do the thing".into()),
        head_age: Some("2 hours ago".into()),
        dirty: Some(false),
        ahead_behind: Some((1, 0)),
        in_progress: None,
    }
}

pub fn test_app(names: &[&str]) -> App {
    // A path that cannot exist: these tests drive the app's own logic,
    // and a worker thread that wandered into a real repository would be
    // exactly the thing the testing policy forbids.
    let paths = PandoPaths::new(
        "/pando-test-does-not-exist/home",
        ProjectRef {
            id: "acme-shop-3f9a2c1d".into(),
            root: PathBuf::from("/pando-test-does-not-exist/acme-shop"),
            display_name: "acme-shop".into(),
        },
    );
    let worktrees: Vec<Worktree> = names.iter().map(|n| wt(n)).collect();
    let mut app = App::new_for_test(paths, Config::default(), worktrees);
    app.created_by_pando = names.iter().map(|n| (n.to_string(), true)).collect();
    app
}

use crate::state::{ProcessRecord, WorktreeRecord};
use chrono::Utc;

/// A quick refresh that read `state` and had nothing else to say.
pub fn refreshed(state: State) -> AppEvent {
    refreshed_with(state, None, Vec::new())
}

pub fn refreshed_with(state: State, warning: Option<&str>, notices: Vec<String>) -> AppEvent {
    AppEvent::Refreshed(Box::new(background::QuickRefresh {
        refreshed: actions::Refreshed {
            state,
            warning: warning.map(str::to_string),
            notices,
            ..Default::default()
        },
        ran: true,
    }))
}

/// A quick refresh that found nothing could change, and only read `state`.
pub fn reread(state: State) -> AppEvent {
    AppEvent::Refreshed(Box::new(background::QuickRefresh {
        refreshed: actions::Refreshed {
            state,
            ..Default::default()
        },
        ran: false,
    }))
}

/// Gives a worktree a process in `phase`, as a refresh would have.
pub fn with_process(app: &mut App, name: &str, phase: Phase) {
    let mut record = WorktreeRecord::new(format!("/trees/{name}"), true);
    record.ports.insert("web".to_string(), 17_342);
    record
        .roles
        .insert("dev".to_string(), vec!["web".to_string()]);
    record.processes.insert(
        "dev".to_string(),
        ProcessRecord {
            pid: 4242,
            pgid: Group::from_raw(4242),
            started_at: Utc::now(),
            log_path: PathBuf::from("/does/not/exist/dev.log"),
            ready_port: Some(17_342),
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase,
        },
    );
    app.state.worktrees.insert(name.to_string(), record);
}

/// Gives a worktree a second process, as a workspace start would have.
pub fn with_second_process(app: &mut App, name: &str, process: &str, phase: Phase) {
    let record = app
        .state
        .worktrees
        .get_mut(name)
        .expect("the worktree has a record");
    let port = 17_343 + record.processes.len() as u16;
    record.ports.insert(process.to_string(), port);
    record
        .roles
        .insert(process.to_string(), vec![process.to_string()]);
    record.processes.insert(
        process.to_string(),
        ProcessRecord {
            pid: 4343,
            pgid: Group::from_raw(4343),
            started_at: Utc::now(),
            log_path: PathBuf::from(format!("/does/not/exist/{process}.log")),
            ready_port: Some(port),
            ready_timeout_s: None,
            observed_ports: Vec::new(),
            swept: false,
            phase,
        },
    );
}

/// Gives a worktree a live share, as a `share` would have.
pub fn with_share(app: &mut App, name: &str, proxy_port: Option<u16>) {
    let record = app
        .state
        .worktrees
        .get_mut(name)
        .expect("the worktree has a record");
    record.share_port = proxy_port;
    record.share = Some(crate::state::ShareRecord {
        tunnel_pid: 5151,
        tunnel_pgid: Group::from_raw(5151),
        public_url: "https://fake-host.trycloudflare.com".to_string(),
        local_port: 17_342,
        started_at: Utc::now(),
        log_path: PathBuf::from("/does/not/exist/tunnel.log"),
        proxy_pid: proxy_port.map(|_| 5252),
        proxy_pgid: proxy_port.map(|_| Group::from_raw(5252)),
        proxy_port,
    });
}

pub fn running_phase() -> Phase {
    Phase::Running { since: Utc::now() }
}

fn a_question() -> actions::Question {
    actions::Question {
        slot: crate::detect::Slot::DevCmd,
        prompt: "Which command starts the local development server?".to_string(),
        options: vec![
            (
                "pnpm dev".to_string(),
                "package.json scripts.dev".to_string(),
            ),
            (
                "pnpm dev:web".to_string(),
                "package.json scripts.dev:web".to_string(),
            ),
        ],
        preselect: Some(0),
        allow_custom: true,
        allow_none: false,
        multi: false,
        checked: Vec::new(),
        details: Vec::new(),
        answer_file: None,
        snippet: String::new(),
    }
}

/// Opens the question modal the way a worker would, and hands back the
/// end of the channel that worker would be blocked on.
fn open_question(
    app: &mut App,
    question: actions::Question,
) -> Receiver<Result<actions::Answer, String>> {
    let (tx, rx) = mpsc::channel();
    app.handle_event(AppEvent::AskQuestion(Box::new((question, tx))));
    rx
}

fn press(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
}

/// The services question, as detection on fixture 6 would raise it.
fn a_services_question() -> actions::Question {
    actions::Question {
        slot: crate::detect::Slot::Services,
        prompt: "Run private copies of these services for each worktree?".to_string(),
        options: ["cache", "db", "mail", "queue"]
            .iter()
            .map(|v| (v.to_string(), "docker-compose.yml".to_string()))
            .collect(),
        preselect: Some(0),
        allow_custom: false,
        allow_none: true,
        multi: true,
        checked: vec![1, 2],
        details: Vec::new(),
        answer_file: None,
        snippet: String::new(),
    }
}

/// A single-choice question, the dev command's, with a first choice or
/// without one.
fn a_dev_question(preselect: Option<usize>) -> actions::Question {
    actions::Question {
        slot: crate::detect::Slot::DevCmd,
        prompt: "Which command starts the local development server?".to_string(),
        options: vec![
            (
                "npm run dev".to_string(),
                "package.json scripts.dev".to_string(),
            ),
            (
                "npm run start".to_string(),
                "package.json scripts.start".to_string(),
            ),
        ],
        preselect,
        allow_custom: true,
        allow_none: false,
        multi: false,
        checked: Vec::new(),
        details: Vec::new(),
        answer_file: Some(PathBuf::from("/home/.pando/projects/p/pando.toml")),
        snippet: String::new(),
    }
}

// A first `start` runs rather than interviews: a question the rules have a
// first choice for takes it, and says so where `m` keeps it.
#[test]
fn the_tui_takes_the_rules_first_choice_instead_of_asking() {
    let (tx, rx) = mpsc::channel();
    let answer = super::background::ask_through_ui(&tx, &a_dev_question(Some(0))).unwrap();
    assert_eq!(answer, actions::Answer::Choice(0));
    match rx.try_recv() {
        Ok(AppEvent::Notice(line)) => {
            assert!(
                line.contains("\"npm run dev\" (package.json scripts.dev), pando's first choice"),
                "{line}"
            );
            assert!(line.contains("over 1 other option"), "{line}");
            assert!(
                line.contains("/home/.pando/projects/p/pando.toml"),
                "{line}"
            );
        }
        _ => panic!("no notice was sent"),
    }
}

// With nothing to take, and for a set of services, the question is put.
#[test]
fn a_question_with_no_options_or_a_set_is_still_asked() {
    let mut empty = a_dev_question(None);
    empty.options.clear();
    assert!(actions::recommended(&empty).is_none());
    assert!(actions::recommended(&a_services_question()).is_none());
}

// A local file seeded from the project's example is one `--yes` may not
// take; somebody at the TUI is told which file it came from instead.
#[test]
fn an_option_only_a_person_may_take_is_taken_when_a_person_is_there() {
    let mut seeded = a_dev_question(None);
    seeded.slot = crate::detect::Slot::Provision;
    seeded.options = vec![(
        ".env".to_string(),
        ".env copied from .env.example — this clone has none of its own".to_string(),
    )];
    let (answer, line) = actions::recommended(&seeded).unwrap();
    assert_eq!(answer, actions::Answer::Choice(0));
    assert!(line.contains(".env copied from .env.example"), "{line}");
}

// ---- the multi-select modal ------------------------------------------

#[test]
fn a_set_question_opens_with_the_rules_answer_ticked() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_services_question());
    assert_eq!(app.question_checked, vec![1, 2]);
    assert!(
        matches!(app.modal, Some(Modal::Question { custom: None, .. })),
        "a set question has nothing to type"
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        rx.try_recv().unwrap(),
        Ok(actions::Answer::Many(vec![1, 2]))
    );
}

#[test]
fn space_ticks_the_row_under_the_cursor_and_enter_takes_the_set() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_services_question());
    // The cursor starts on `cache`; tick it, then move to `db` and
    // untick that.
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char(' '));
    assert_eq!(app.question_checked, vec![0, 2]);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        rx.try_recv().unwrap(),
        Ok(actions::Answer::Many(vec![0, 2]))
    );
}

#[test]
fn an_empty_set_is_the_answer_none() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_services_question());
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Enter);
    assert_eq!(rx.try_recv().unwrap(), Ok(actions::Answer::None));
}

#[test]
fn escape_on_a_set_question_still_answers_the_worker() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_services_question());
    press(&mut app, KeyCode::Esc);
    assert!(
        rx.try_recv().unwrap().is_err(),
        "a modal that closes without sending leaves a worker waiting forever"
    );
    assert!(app.modal.is_none());
}

#[test]
fn the_cursor_of_a_set_question_clamps_at_both_ends() {
    let mut app = test_app(&["feat+one"]);
    let _rx = open_question(&mut app, a_services_question());
    for _ in 0..8 {
        press(&mut app, KeyCode::Char('j'));
    }
    press(&mut app, KeyCode::Char(' '));
    assert!(
        app.question_checked.contains(&3),
        "the last row, not past it"
    );
    for _ in 0..8 {
        press(&mut app, KeyCode::Char('k'));
    }
    press(&mut app, KeyCode::Char(' '));
    assert!(app.question_checked.contains(&0));
}

// ---- services in the app --------------------------------------------

#[test]
fn the_isolated_key_starts_the_selected_worktree() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('i'));
    let pending = app.pending.as_ref().expect("the key started something");
    assert_eq!(pending.kind, PendingKind::Start);
    assert_eq!(pending.name, "feat+one");
}

#[test]
fn service_health_reaches_the_app_off_the_ui_thread() {
    let mut app = test_app(&["feat+one"]);
    assert!(app.services_of("feat+one").is_empty());
    let health = ServiceHealth {
        shared: vec![actions::ServiceStatus {
            name: "postgres".into(),
            port: Some(5432),
            up: false,
            logging: false,
            env_file: None,
        }],
        worktrees: BTreeMap::from([(
            "feat+one".to_string(),
            vec![actions::ServiceStatus {
                name: "postgres".into(),
                port: Some(17_004),
                up: true,
                logging: false,
                env_file: None,
            }],
        )]),
    };
    assert!(
        app.handle_event(AppEvent::ServiceHealth(Box::new(health.clone()))),
        "a change in health is a reason to repaint"
    );
    assert_eq!(app.services_of("feat+one")[0].port, Some(17_004));
    assert!(!app.service_health.shared[0].up);
    assert!(
        !app.handle_event(AppEvent::ServiceHealth(Box::new(health))),
        "and the same answer twice is not"
    );
}

/// A worktree record carrying one private service with no port and no
/// pid, so a probe of it answers without a connect or a signal.
fn with_portless_service(state: &mut State, name: &str) {
    let mut record = WorktreeRecord::new(format!("/trees/{name}"), true);
    record.services.push(crate::state::ServiceRecord {
        name: "postgres".into(),
        kind: crate::state::ServiceKind::Compose,
        port: None,
        pid: None,
        pgid: None,
        compose_project: Some(format!("pando-{name}")),
    });
    state.worktrees.insert(name.to_string(), record);
}

// Every refresh probed every worktree's private services, a connect per
// service a second, though only the selected worktree's are ever drawn.
#[test]
fn a_refresh_probes_only_the_selected_worktrees_services() {
    let (_dir, mut app) = app_with_logs(&["feat+a", "feat+b"]);
    let rx = app.event_rx.take().expect("the app owns its receiver");
    let mut state = State::new();
    with_portless_service(&mut state, "feat+a");
    with_portless_service(&mut state, "feat+b");
    std::fs::create_dir_all(app.paths.state_file().parent().unwrap()).unwrap();
    crate::state::save(&app.paths.state_file(), &state).unwrap();
    app.handle_event(refreshed(state));
    app.select_index(1);

    app.spawn_refresh();
    let deadline = Instant::now() + Duration::from_secs(10);
    let health = loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .expect("the probe lands");
        if let Ok(AppEvent::ServiceHealth(health)) = rx.recv_timeout(left) {
            break health;
        }
    };
    assert_eq!(
        health.worktrees.keys().collect::<Vec<_>>(),
        ["feat+b"],
        "the selected worktree's, and no other"
    );
}

// With only the selected worktree probed, a move onto another left its
// service rows out until the next refresh came round.
#[test]
fn a_move_onto_a_worktree_with_services_probes_it_on_the_next_tick() {
    let mut app = test_app(&["feat+a", "feat+b", "feat+c"]);
    with_portless_service(&mut app.state, "feat+a");
    with_portless_service(&mut app.state, "feat+b");
    app.service_health
        .worktrees
        .insert("feat+a".to_string(), Vec::new());
    // Ticks that are no refresh's own.
    app.tick = 0;
    app.handle_event(AppEvent::Tick);
    assert!(!app.refreshing, "the selected worktree was probed");

    app.select_index(1);
    app.handle_event(AppEvent::Tick);
    assert!(app.refreshing, "the one moved onto was not");

    app.refreshing = false;
    app.select_index(2);
    app.handle_event(AppEvent::Tick);
    assert!(!app.refreshing, "one with no services has nothing to probe");
}

// Every request for a discovery started one, so a backlog of ticks — or a
// state lock held a long time — stacked up workers queued on the lock. One
// asked for while one runs is still run, once, after it.
#[test]
fn a_discovery_asked_for_while_one_runs_runs_once_after_it() {
    let mut app = test_app(&["feat+one"]);
    let rx = app.event_rx.take().expect("the app owns its receiver");
    let next_discovery = |within: Duration| {
        let deadline = Instant::now() + within;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match rx.recv_timeout(left) {
                Ok(event @ AppEvent::Discovered(_)) => return Some(event),
                Ok(_) => {}
                Err(_) => return None,
            }
        }
        None
    };
    for _ in 0..3 {
        app.spawn_discovery();
    }
    assert!(app.discovering && app.discover_again);
    let landed = next_discovery(Duration::from_secs(10)).expect("the first lands");
    app.handle_event(landed);
    assert!(
        app.discovering && !app.discover_again,
        "the one asked for meanwhile runs now"
    );
    let landed = next_discovery(Duration::from_secs(10)).expect("and lands");
    app.handle_event(landed);
    assert!(!app.discovering);
    assert!(
        next_discovery(Duration::from_millis(500)).is_none(),
        "two workers for three requests, not three"
    );
}

// The slow git tick's discovery resolves the base branch again. Put off
// behind one already running, it ran once the tick had moved on and
// reused the old base for another thirty seconds.
#[test]
fn a_discovery_put_off_on_the_slow_git_tick_still_resolves_the_base() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        ProjectRef::from_root(&root).unwrap(),
    );
    let mut app = App::new_for_test(paths, Config::default(), Vec::new());
    let rx = app.event_rx.take().expect("the app owns its receiver");
    app.default_base = Some("stale".into());
    app.discovering = true;
    app.tick = GIT_ALL_EVERY;
    app.spawn_discovery();
    assert!(app.discover_again, "put off behind the one in flight");

    app.tick = GIT_ALL_EVERY + 1;
    app.handle_event(AppEvent::Discovered(Box::new(Ok(Snapshot {
        default_base: Some("stale".into()),
        ..listing(&app, &[])
    }))));
    let deadline = Instant::now() + Duration::from_secs(20);
    let snapshot = loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .expect("the discovery put off lands");
        if let Ok(AppEvent::Discovered(result)) = rx.recv_timeout(left) {
            break result.expect("the fixture lists");
        }
    };
    assert_eq!(snapshot.default_base.as_deref(), Some("main"));
}

fn type_str(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

#[test]
fn the_cursor_moves_and_clamps_at_both_ends() {
    let mut app = test_app(&["a", "b", "c"]);
    assert_eq!(app.list_state.selected(), Some(0));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.list_state.selected(), Some(2));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.list_state.selected(), Some(2), "clamped at the end");
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.list_state.selected(), Some(0));
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.list_state.selected(), Some(0), "clamped at the start");
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(app.list_state.selected(), Some(2));
}

#[test]
fn an_empty_list_has_no_selection_and_ignores_movement() {
    let mut app = test_app(&[]);
    assert_eq!(app.list_state.selected(), None);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.list_state.selected(), None);
}

#[test]
fn filtering_narrows_the_list_and_escape_restores_it() {
    let mut app = test_app(&["feat+one", "feat+two", "fix+three"]);
    press(&mut app, KeyCode::Char('/'));
    assert_eq!(app.mode, Mode::Filter);
    type_str(&mut app, "fix");
    assert_eq!(app.filtered_indices.len(), 1);
    assert_eq!(app.selected_worktree().unwrap().name, "fix+three");

    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.filtered_indices.len(), 1, "\"fi\" still matches one");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.filter.is_empty());
    assert_eq!(app.filtered_indices.len(), 3);
}

#[test]
fn filtering_matches_the_branch_as_well_as_the_name() {
    let mut app = test_app(&["odd+name"]);
    app.worktrees[0].branch = Some("release/1.2".into());
    press(&mut app, KeyCode::Char('/'));
    type_str(&mut app, "release");
    assert_eq!(app.filtered_indices.len(), 1);
}

#[test]
fn enter_leaves_filter_mode_but_keeps_the_filter() {
    let mut app = test_app(&["feat+one", "fix+two"]);
    press(&mut app, KeyCode::Char('/'));
    type_str(&mut app, "fix");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.filter, "fix");
    assert_eq!(app.filtered_indices.len(), 1);
}

// The kept filter's line says `esc clears`; quitting instead would lose
// the session to a key pressed to undo a search.
#[test]
fn escape_clears_a_kept_filter_before_it_quits() {
    let mut app = test_app(&["feat+one", "fix+two"]);
    press(&mut app, KeyCode::Char('/'));
    type_str(&mut app, "fix");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Esc);
    assert!(!app.should_quit);
    assert!(app.filter.is_empty());
    assert_eq!(app.filtered_indices.len(), 2);
    press(&mut app, KeyCode::Esc);
    assert!(app.should_quit, "and then esc quits as before");
}

#[test]
fn q_quits_and_ctrl_c_quits_from_anywhere() {
    let mut app = test_app(&["a"]);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit);

    let mut app = test_app(&["a"]);
    press(&mut app, KeyCode::Char('?'));
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.should_quit, "ctrl-c quits even with a modal open");
}

// Quitting ends the worker wherever it is, and an esc meant to dismiss
// something ended a start halfway through its switch.
#[test]
fn q_with_an_action_in_flight_asks_first_and_esc_does_not_quit() {
    let mut app = test_app(&["feat+one"]);
    let (_hold, held) = mpsc::channel::<()>();
    app.spawn_pending("feat+one".into(), PendingKind::Start, move || {
        let _ = held.recv();
        Err("let go".to_string())
    });

    press(&mut app, KeyCode::Esc);
    assert!(
        !app.should_quit,
        "esc cancels, and there is nothing to cancel"
    );
    let (message, _) = app.active_status().unwrap();
    assert_eq!(
        message,
        "starting feat/one is in flight — q twice abandons it and quits"
    );

    press(&mut app, KeyCode::Char('q'));
    assert!(!app.should_quit);
    let (message, _) = app.active_status().unwrap();
    assert_eq!(
        message,
        "abandon starting feat/one and quit? q again to confirm · esc cancels"
    );
    press(&mut app, KeyCode::Esc);
    assert!(!app.should_quit);
    assert_eq!(app.active_status(), Some(("cancelled", false)));

    press(&mut app, KeyCode::Char('q'));
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit);

    // ctrl-c is the hard way out: a worker that never returns must not
    // trap anybody.
    let mut app = test_app(&["feat+one"]);
    let (_hold, held) = mpsc::channel::<()>();
    app.spawn_pending("feat+one".into(), PendingKind::Start, move || {
        let _ = held.recv();
        Err("let go".to_string())
    });
    app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.should_quit);
}

#[test]
fn the_help_modal_opens_scrolls_and_closes() {
    let mut app = test_app(&["a"]);
    press(&mut app, KeyCode::Char('?'));
    assert!(matches!(app.modal, Some(Modal::Help)));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.help_scroll, 1);
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.help_scroll, 0);
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert_eq!(app.help_scroll, 0, "scroll resets for the next open");
}

#[test]
fn the_create_modal_opens_accepts_typing_and_closes_on_escape() {
    let mut app = test_app(&["a"]);
    press(&mut app, KeyCode::Char('n'));
    assert!(matches!(app.modal, Some(Modal::Create { .. })));
    type_str(&mut app, "feat/new");
    match &app.modal {
        Some(Modal::Create { input, .. }) => assert_eq!(input, "feat/new"),
        other => panic!("expected the create modal, got {other:?}"),
    }
    press(&mut app, KeyCode::Backspace);
    match &app.modal {
        Some(Modal::Create { input, .. }) => assert_eq!(input, "feat/ne"),
        other => panic!("expected the create modal, got {other:?}"),
    }
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
}

// Validation happens in the modal, before any worker starts, so a
// colliding name is not an error: somebody typing the name of a branch
// that already has a worktree wants that worktree, and gets it.
#[test]
fn the_create_modal_goes_to_a_worktree_that_already_exists() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    assert_eq!(app.selected_worktree().unwrap().name, "feat+one");
    press(&mut app, KeyCode::Char('n'));
    type_str(&mut app, "feat/two");
    press(&mut app, KeyCode::Enter);

    assert!(app.modal.is_none(), "the modal closes");
    assert!(app.pending.is_none(), "no worker should have started");
    assert_eq!(app.selected_worktree().unwrap().name, "feat+two");
    let (message, is_error) = app.active_status().unwrap();
    assert!(!is_error);
    assert!(message.contains("already has a worktree"), "{message}");
}

fn a_pr(number: u32, branch: &str, state: crate::worktree::PrState) -> crate::worktree::PrInfo {
    crate::worktree::PrInfo {
        number,
        title: format!("title of {number}"),
        branch: branch.into(),
        author: "someone".into(),
        draft: false,
        state,
        url: format!("https://example.test/pull/{number}"),
        cross_repository: false,
    }
}

fn app_with_prs(names: &[&str]) -> App {
    use crate::worktree::PrState::{Merged, Open};
    let mut app = test_app(names);
    app.pr_list = vec![
        a_pr(12, "feat/new", Open),
        a_pr(11, "feat/one", Open),
        a_pr(10, "feat/old", Merged),
    ];
    app
}

fn picker_rows(app: &App) -> Vec<u32> {
    match &app.modal {
        Some(Modal::PullRequests { input, .. }) => pr_rows(&app.pr_list, input)
            .iter()
            .map(|pr| pr.number)
            .collect(),
        _ => panic!("the pull request picker is not open"),
    }
}

#[test]
fn p_lists_the_open_pull_requests_and_typing_narrows_them() {
    let mut app = app_with_prs(&["feat+one"]);
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(
        picker_rows(&app),
        vec![12, 11],
        "merged ones are not listed"
    );
    type_str(&mut app, "#11");
    assert_eq!(picker_rows(&app), vec![11]);
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
}

#[test]
fn enter_on_a_pull_request_makes_a_worktree_for_it() {
    let mut app = app_with_prs(&["feat+one"]);
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Enter);
    assert!(
        app.modal.is_none(),
        "the picker closes once the work starts"
    );
    let pending = app.pending.as_ref().expect("a worker started");
    assert_eq!(pending.kind, PendingKind::Create);
    assert_eq!(pending.name, "feat+new");
    assert_eq!(pending.label, "#12 feat/new");
}

#[test]
fn enter_on_a_pull_request_with_a_worktree_selects_it() {
    let mut app = app_with_prs(&["feat+two", "feat+one"]);
    assert_eq!(app.selected_worktree().unwrap().name, "feat+two");
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    assert!(app.pending.is_none(), "no worker should have started");
    assert_eq!(app.selected_worktree().unwrap().name, "feat+one");
    let (message, is_error) = app.active_status().unwrap();
    assert!(
        !is_error && message.contains("#11 already has a worktree"),
        "{message}"
    );
}

// A fetch that lands while the picker is open can shorten the list; enter
// takes the row the paint showed selected, not nothing.
#[test]
fn enter_after_the_list_shrank_takes_the_last_row() {
    let mut app = app_with_prs(&["feat+two"]);
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Down);
    app.handle_event(AppEvent::PrsReady(Ok(vec![a_pr(
        12,
        "feat/new",
        crate::worktree::PrState::Open,
    )])));
    press(&mut app, KeyCode::Enter);
    let pending = app.pending.as_ref().expect("a worker started");
    assert_eq!(pending.label, "#12 feat/new");
}

// The fetch `p` starts lands a second or so after it. A pull request
// opened since the last fetch, or one merged since, moves the rows below
// it, and the cursor was a row: enter made a worktree for whichever pull
// request that row held by then.
#[test]
fn a_fetch_that_lands_in_the_picker_keeps_the_cursor_on_its_pull_request() {
    use crate::worktree::PrState::{Merged, Open};
    for fetched in [
        vec![
            a_pr(13, "feat/newer", Open),
            a_pr(12, "feat/new", Open),
            a_pr(11, "feat/one", Open),
        ],
        vec![
            a_pr(12, "feat/new", Merged),
            a_pr(11, "feat/one", Open),
            a_pr(9, "feat/older", Open),
        ],
    ] {
        let mut app = app_with_prs(&["feat+two"]);
        press(&mut app, KeyCode::Char('p'));
        press(&mut app, KeyCode::Down);
        app.handle_event(AppEvent::PrsReady(Ok(fetched)));
        press(&mut app, KeyCode::Enter);
        let pending = app.pending.as_ref().expect("a worker started");
        assert_eq!(pending.label, "#11 feat/one");
    }
}

#[test]
fn a_forks_pull_request_is_not_the_chip_of_the_main_branch() {
    let mut app = test_app(&["feat+one"]);
    let fork = crate::worktree::PrInfo {
        cross_repository: true,
        ..a_pr(5, "main", crate::worktree::PrState::Open)
    };
    app.handle_event(AppEvent::PrsReady(Ok(vec![fork])));
    assert!(!app.prs.contains_key("main"));
    assert_eq!(app.prs["pr-5/main"].number, 5);
}

#[test]
fn the_picker_says_why_it_has_no_pull_requests() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('p'));
    app.handle_event(AppEvent::PrsReady(Err("gh pr list failed: no auth".into())));
    assert_eq!(app.pr_error.as_deref(), Some("gh pr list failed: no auth"));
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(app.modal, Some(Modal::PullRequests { .. })),
        "enter on nothing keeps the picker open"
    );
    assert!(app.pending.is_none());
}

// And one `n` did create takes the cursor as soon as it is listed.
#[test]
fn a_created_worktree_is_selected_when_it_arrives() {
    let mut app = test_app(&["feat+one"]);
    app.select_on_arrival = Some("feat+new".to_string());
    let listed = vec![wt("feat+one"), wt("feat+new")];
    app.apply_snapshot(Snapshot {
        main: wt("acme-shop"),
        worktrees: listed,
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: None,
        notices: Vec::new(),
        default_base: None,
    });
    assert_eq!(app.selected_worktree().unwrap().name, "feat+new");
    assert!(app.select_on_arrival.is_none(), "and it is done with");
}

// Not in the middle of a filter being typed: the arrival switched the list
// out of filter mode, and the rest of the filter ran as list keys — `d` ⏎
// removed the worktree just made. It waits for the filter to be done with.
#[test]
fn a_created_worktree_waits_for_a_filter_being_typed() {
    for end in [KeyCode::Enter, KeyCode::Esc] {
        let mut app = test_app(&["feat+one", "fix+two"]);
        app.select_on_arrival = Some("feat+new".to_string());
        press(&mut app, KeyCode::Char('/'));
        type_str(&mut app, "fi");
        app.apply_snapshot(Snapshot {
            main: wt("acme-shop"),
            worktrees: vec![wt("feat+one"), wt("fix+two"), wt("feat+new")],
            created_by_pando: BTreeMap::new(),
            state: State::new(),
            warning: None,
            notices: Vec::new(),
            default_base: None,
        });
        assert_eq!(app.mode, Mode::Filter, "{end:?}: still typing");
        type_str(&mut app, "xd");
        assert_eq!(app.filter, "fixd", "{end:?}: the keys stay the filter's");
        assert!(app.modal.is_none(), "{end:?}: d opened nothing");
        assert!(app.select_on_arrival.is_some(), "{end:?}: it waits");

        press(&mut app, end);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(
            app.selected_worktree().map(|w| w.name.as_str()),
            Some("feat+new"),
            "{end:?}: and takes the cursor once the filter is done with"
        );
        assert!(app.select_on_arrival.is_none(), "{end:?}");
    }
}

// But enter on a row the filter found is a choice. The arrival waiting
// for it cleared the filter and took the cursor, and `s` next started the
// worktree `n` made instead of the row just picked.
#[test]
fn enter_on_a_filtered_row_keeps_it_over_a_created_worktree() {
    let mut app = test_app(&["feat+one", "fix+two"]);
    app.select_on_arrival = Some("feat+new".to_string());
    press(&mut app, KeyCode::Char('/'));
    type_str(&mut app, "fi");
    app.apply_snapshot(listing(&app, &["feat+one", "fix+two", "feat+new"]));
    type_str(&mut app, "x");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.filter, "fix", "the filter is kept");
    assert_eq!(
        app.selected_worktree().map(|w| w.name.as_str()),
        Some("fix+two")
    );
    assert!(
        app.select_on_arrival.is_none(),
        "and the arrival is done with"
    );

    // Nor does the next discovery hand it the cursor.
    app.apply_snapshot(listing(&app, &["feat+one", "fix+two", "feat+new"]));
    press(&mut app, KeyCode::Char('s'));
    let pending = app.pending.as_ref().expect("s started something");
    assert_eq!(
        (pending.name.as_str(), pending.kind),
        ("fix+two", PendingKind::Start)
    );
}

// Nor when the discovery that lists it comes back only after the enter:
// the arrival outlived the choice, and that discovery cleared the filter
// and took the cursor from the row just picked.
#[test]
fn enter_on_a_filtered_row_keeps_it_over_a_created_worktree_not_yet_listed() {
    let mut app = test_app(&["feat+one", "fix+two"]);
    app.select_on_arrival = Some("feat+new".to_string());
    press(&mut app, KeyCode::Char('/'));
    type_str(&mut app, "fix");
    press(&mut app, KeyCode::Enter);
    assert!(app.select_on_arrival.is_none(), "the arrival is done with");

    app.apply_snapshot(listing(&app, &["feat+one", "fix+two", "feat+new"]));
    assert_eq!(app.filter, "fix", "the filter is kept");
    assert_eq!(
        app.selected_worktree().map(|w| w.name.as_str()),
        Some("fix+two")
    );
    press(&mut app, KeyCode::Char('s'));
    let pending = app.pending.as_ref().expect("s started something");
    assert_eq!(
        (pending.name.as_str(), pending.kind),
        ("fix+two", PendingKind::Start)
    );
}

// A worktree removed while the cursor is on it — by `d`, or from the CLI —
// leaves the cursor on its neighbour, not back at the top of a long list.
#[test]
fn a_selected_worktree_that_goes_away_leaves_the_cursor_on_its_neighbour() {
    let names = ["feat+a", "feat+b", "feat+c", "feat+d"];
    let mut app = test_app(&names);
    let snapshot = |listed: &[&str]| Snapshot {
        main: wt("acme-shop"),
        worktrees: listed.iter().map(|n| wt(n)).collect(),
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: None,
        notices: Vec::new(),
        default_base: None,
    };
    // The main checkout's row first, as a discovery lists it; the cursor
    // stays on feat+a, under it.
    app.apply_snapshot(snapshot(&names));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected_worktree().unwrap().name, "feat+c");
    app.tail_index = 1;
    app.apply_snapshot(snapshot(&["feat+a", "feat+b", "feat+d"]));
    assert_eq!(app.selected_worktree().unwrap().name, "feat+d");
    assert_eq!(app.tail_index, 0, "a different worktree's tail starts over");
    // The last row going away leaves the cursor on the new last row.
    app.apply_snapshot(snapshot(&["feat+a", "feat+b"]));
    assert_eq!(app.selected_worktree().unwrap().name, "feat+b");
    // And a worktree that stays keeps the cursor wherever it moved to.
    app.apply_snapshot(snapshot(&["feat+new", "feat+a", "feat+b"]));
    assert_eq!(app.selected_worktree().unwrap().name, "feat+b");
}

#[test]
fn enter_on_an_empty_create_modal_asks_for_a_name() {
    let mut app = test_app(&[]);
    press(&mut app, KeyCode::Char('n'));
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Create { .. })));
    assert!(app.active_status().unwrap().0.contains("branch name"));
}

#[test]
fn branches_arriving_populate_an_open_create_modal() {
    let mut app = test_app(&[]);
    press(&mut app, KeyCode::Char('n'));
    let branches = vec![BranchEntry {
        name: "main".into(),
        source: BranchSource::Local,
    }];
    app.handle_event(AppEvent::BranchesReady(branches));
    match &app.modal {
        Some(Modal::Create { branches, .. }) => {
            assert!(!branches.is_loading());
            assert_eq!(branches.as_slice().len(), 1);
        }
        other => panic!("expected the create modal, got {other:?}"),
    }
}

#[test]
fn the_remove_modal_names_its_target_and_closes_on_escape() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('d'));
    match &app.modal {
        Some(Modal::Remove { name, .. }) => {
            assert_eq!(name, "feat+one");
            assert!(
                app.remove_blockers(name).is_empty(),
                "a clean, stopped pando worktree has nothing to warn about"
            );
        }
        other => panic!("expected the remove modal, got {other:?}"),
    }
    press(&mut app, KeyCode::Char('n'));
    assert!(app.modal.is_none());
}

#[test]
fn the_remove_modal_states_why_a_removal_would_be_refused() {
    let mut app = test_app(&["locked+one", "dirty+one", "adopted+one"]);
    app.worktrees[0].locked = true;
    app.worktrees[0].lock_reason = Some("benchmarking".into());
    app.worktrees[1].dirty = Some(true);
    app.created_by_pando.insert("adopted+one".into(), false);

    let locked = app.remove_blockers("locked+one");
    assert!(
        locked[0].is_fatal(),
        "a locked worktree can never be removed"
    );
    assert!(locked[0].line().contains("benchmarking"));

    assert_eq!(app.remove_blockers("dirty+one"), vec![RemoveBlocker::Dirty]);
    assert!(!RemoveBlocker::Dirty.is_fatal());
    assert_eq!(
        app.remove_blockers("adopted+one"),
        vec![RemoveBlocker::NotOurs]
    );
}

// All of it at once: a dirty, running worktree pando did not create says
// all three, not just the first one found.
#[test]
fn the_remove_modal_states_dirty_running_and_adopted_together() {
    let mut app = test_app(&["feat+tui"]);
    app.worktrees[0].dirty = Some(true);
    with_process(&mut app, "feat+tui", running_phase());
    app.created_by_pando.insert("feat+tui".into(), false);
    assert_eq!(
        app.remove_blockers("feat+tui"),
        vec![
            RemoveBlocker::Dirty,
            RemoveBlocker::Running,
            RemoveBlocker::NotOurs
        ]
    );
    // Git not read yet is said as such, not as clean.
    app.worktrees[0].dirty = None;
    assert!(
        app.remove_blockers("feat+tui")
            .contains(&RemoveBlocker::DirtyUnknown)
    );
}

// `y` on a worktree known to be dirty would only be refused by git: the
// dialog stays and names the key that works.
#[test]
fn y_on_a_dirty_worktree_keeps_the_dialog_and_points_at_f() {
    let mut app = test_app(&["feat+tui"]);
    app.worktrees[0].dirty = Some(true);
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.pending.is_none(), "nothing git would refuse is started");
    assert!(
        matches!(app.modal, Some(Modal::Remove { .. })),
        "the dialog stays open"
    );
    let (message, is_error) = app.active_status().unwrap();
    assert!(is_error);
    assert!(message.contains("F removes it anyway"), "{message}");
}

// Removed from the CLI while the dialog was open: its blockers read as
// "nothing", and `y` sent a worker after a worktree that is not there.
#[test]
fn confirming_the_removal_of_a_worktree_already_gone_starts_nothing() {
    let mut app = test_app(&["feat+a", "feat+b"]);
    press(&mut app, KeyCode::Char('d'));
    app.apply_snapshot(Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![wt("feat+b")],
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: None,
        notices: Vec::new(),
        default_base: None,
    });
    press(&mut app, KeyCode::Char('y'));
    assert!(
        app.pending.is_none(),
        "no worker for a worktree that is gone"
    );
    assert!(app.modal.is_none());
    let (message, is_error) = app.active_status().unwrap();
    assert!(!is_error && message.contains("already gone"), "{message}");
}

#[test]
fn capital_f_in_the_remove_dialog_removes_with_force() {
    let mut app = test_app(&["feat+tui"]);
    app.worktrees[0].dirty = Some(true);
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('F'));
    assert!(app.modal.is_none(), "the dialog closes");
    let pending = app.pending.as_ref().expect("a removal is under way");
    assert_eq!(pending.kind, PendingKind::Remove);
    assert_eq!(pending.name, "feat+tui");
}

// A clean worktree removes on `y`, without force.
#[test]
fn y_on_a_clean_worktree_removes_it() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.modal.is_none());
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Remove)
    );
}

// Git's refusal of a dirty removal names the key, not the CLI flag.
#[test]
fn a_dirty_removal_that_git_refused_says_press_f_not_force() {
    let mut app = test_app(&["feat+tui"]);
    app.spawn_pending("feat+tui".into(), PendingKind::Remove, || {
        Err(
            "feat+tui contains modified or untracked files (M package.json) — commit or \
             remove them, or pass --force to let git discard them"
                .to_string(),
        )
    });
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().unwrap();
    assert!(is_error);
    assert!(!message.contains("--force"), "{message}");
    assert!(
        message.contains("press F in the remove dialog to remove it anyway"),
        "{message}"
    );
}

// Confirming a locked worktree must not start a worker that git would
// refuse anyway.
#[test]
fn confirming_a_locked_removal_refuses_without_starting_work() {
    let mut app = test_app(&["locked+one"]);
    app.worktrees[0].locked = true;
    app.worktrees[0].lock_reason = Some("benchmarking".into());
    press(&mut app, KeyCode::Char('d'));
    press(&mut app, KeyCode::Char('y'));

    assert!(app.pending.is_none(), "no worker for a refusal");
    assert!(app.modal.is_none());
    let (message, is_error) = app.active_status().unwrap();
    assert!(is_error);
    assert!(message.contains("locked"), "{message}");
}

#[test]
fn d_with_nothing_selected_says_so() {
    let mut app = test_app(&[]);
    press(&mut app, KeyCode::Char('d'));
    assert!(app.modal.is_none());
    assert!(app.active_status().unwrap().1, "should be an error");
}

#[test]
fn y_copies_the_selected_worktree_path() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.clipboard.as_deref(), Some("/trees/feat+one"));
    assert!(app.active_status().unwrap().0.contains("/trees/feat+one"));
}

#[test]
fn create_rows_offers_the_typed_name_then_matching_branches() {
    let branches = vec![
        BranchEntry {
            name: "main".into(),
            source: BranchSource::Local,
        },
        BranchEntry {
            name: "feat/one".into(),
            source: BranchSource::Remote,
        },
    ];
    let rows = create_rows("", &branches);
    assert_eq!(rows.len(), 2, "empty input offers no new-branch row");
    assert!(matches!(rows[0], CreateRow::Existing(_)));

    let rows = create_rows("feat", &branches);
    assert_eq!(rows[0], CreateRow::NewBranch("feat".into()));
    assert!(matches!(&rows[1], CreateRow::Existing(b) if b.name == "feat/one"));

    let rows = create_rows("feat/one", &branches);
    assert_eq!(
        rows.len(),
        1,
        "an exact match suppresses the new-branch row: {rows:?}"
    );
    assert!(matches!(&rows[0], CreateRow::Existing(b) if b.name == "feat/one"));

    let rows = create_rows("nothing-matches", &branches);
    assert_eq!(rows, vec![CreateRow::NewBranch("nothing-matches".into())]);

    assert!(create_rows("", &[]).is_empty());
}

#[test]
fn create_rows_matching_is_case_insensitive_and_input_is_trimmed() {
    let branches = vec![BranchEntry {
        name: "Feat/One".into(),
        source: BranchSource::Local,
    }];
    let rows = create_rows("  feat  ", &branches);
    assert_eq!(rows[0], CreateRow::NewBranch("feat".into()));
    assert_eq!(rows.len(), 2);
}

#[test]
fn a_snapshot_keeps_enrichment_for_unchanged_worktrees() {
    let mut app = test_app(&["feat+one"]);
    let mut refreshed = wt("feat+one");
    refreshed.head_sha = None;
    refreshed.dirty = None;
    let fresh = app.apply_snapshot(Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![refreshed, wt("feat+two")],
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: None,
        notices: Vec::new(),
        default_base: Some("main".into()),
    });

    assert_eq!(
        fresh,
        vec!["acme-shop", "feat+two"],
        "only new entries need enriching — the main checkout's row is new here"
    );
    let kept = app.worktrees.iter().find(|w| w.name == "feat+one").unwrap();
    assert_eq!(
        kept.head_sha.as_deref(),
        Some("abc1234"),
        "enrichment already collected must survive a refresh"
    );
}

#[test]
fn a_snapshot_re_enriches_a_worktree_whose_head_moved() {
    let mut app = test_app(&["feat+one"]);
    let mut moved = wt("feat+one");
    moved.head = Some("def456".into());
    let fresh = app.apply_snapshot(Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![moved],
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: None,
        notices: Vec::new(),
        default_base: None,
    });
    assert_eq!(fresh, vec!["acme-shop", "feat+one"]);
}

// The ownership map is what the Remove modal's "pando did not create
// this" warning reads, so a state file the refresh could not use has to
// reach the user rather than turning every row silently adopted.
#[test]
fn a_state_warning_from_a_refresh_reaches_the_status_line() {
    let mut app = test_app(&["feat+one"]);
    let snapshot = |warning: Option<&str>| Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![wt("feat+one")],
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: warning.map(str::to_string),
        notices: Vec::new(),
        default_base: None,
    };

    app.apply_snapshot(snapshot(Some("state file /s is version 3")));
    let (message, is_error) = app.active_status().unwrap();
    assert!(message.contains("version 3"), "{message}");
    assert!(is_error, "a state file pando cannot use is an error");

    // A standing warning is not re-announced on every refresh.
    app.status = None;
    app.apply_snapshot(snapshot(Some("state file /s is version 3")));
    assert!(app.active_status().is_none());

    app.apply_snapshot(snapshot(None));
    assert_eq!(app.state_warning, None);
}

// Finding 10. A share that dies is announced once, because the record
// is gone by the next tick — so when two die together, showing only
// the first means the second worktree's URL closed in silence.
#[test]
fn every_refresh_notice_reaches_the_status_line_not_only_the_first() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    app.apply_snapshot(Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![wt("feat+one"), wt("feat+two")],
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: None,
        notices: vec![
            "feat+one: the share's tunnel exited, so the public URL is closed".to_string(),
            "feat+two: the share's proxy exited, so the public URL is closed".to_string(),
        ],
        default_base: None,
    });

    let (message, _) = app.active_status().expect("a notice");
    assert!(message.contains("feat+one"), "{message}");
    assert!(
        message.contains("feat+two"),
        "the second share closed in silence: {message}"
    );
}

// The cursor follows the worktree, not the row it happened to be on:
// a refresh that reorders the list (a new worktree is newest-first)
// must not move the selection to a different one.
#[test]
fn a_refresh_keeps_the_cursor_on_the_same_worktree_when_the_order_changes() {
    let mut app = test_app(&["feat+one", "feat+two", "fix+three"]);
    app.select_index(2);
    assert_eq!(app.selected_worktree().unwrap().name, "fix+three");

    app.apply_snapshot(Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![wt("fix+three"), wt("feat+one"), wt("feat+two")],
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: None,
        notices: Vec::new(),
        default_base: None,
    });
    assert_eq!(
        app.selected_worktree().unwrap().name,
        "fix+three",
        "the cursor must stay on the worktree it was on"
    );
    // Under the main checkout's row, which is always first.
    assert_eq!(app.list_state.selected(), Some(1));
}

#[test]
fn the_status_message_expires() {
    let mut app = test_app(&[]);
    app.set_status("hello");
    assert!(app.active_status().is_some());
    app.status.as_mut().unwrap().at = Instant::now() - STATUS_TTL - Duration::from_secs(1);
    assert!(app.active_status().is_none());
    assert!(app.expire_status());
    assert!(app.status.is_none());
}

#[test]
fn base64_matches_the_reference_encoding() {
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"foob"), "Zm9vYg==");
    assert_eq!(base64(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    assert_eq!(base64("/trees/feat+one".as_bytes()), "L3RyZWVzL2ZlYXQrb25l");
}
// ---- processes -------------------------------------------------------

#[test]
fn the_process_keys_each_start_their_own_work() {
    for (key, kind) in [
        (KeyCode::Char('s'), PendingKind::Start),
        (KeyCode::Char('x'), PendingKind::Stop),
        (KeyCode::Char('r'), PendingKind::Restart),
    ] {
        let mut app = test_app(&["feat+one"]);
        with_process(&mut app, "feat+one", running_phase());
        press(&mut app, key);
        // Stop and restart interrupt something that runs, so they are
        // pressed twice; start on a running worktree interrupts nothing.
        if kind != PendingKind::Start {
            assert!(app.pending.is_none(), "{key:?}: the first press only asks");
            press(&mut app, key);
        }
        let pending = app.pending.as_ref().expect("the key started something");
        assert_eq!(pending.kind, kind, "{key:?}");
        assert_eq!(pending.name, "feat+one");
        // And the work is on a worker thread, not this one: the frame
        // is still answering keys.
        assert!(!app.should_quit);
    }
}

// The answers a session writes have to be in that session's own copy of
// the config. They were written to `pando.toml` and nowhere else, so
// the next `s` re-ran detection against a config that still had
// nothing and asked the same question again — with the rule's first
// candidate preselected rather than the answer just given.
#[test]
fn what_a_start_resolves_is_applied_to_this_session_in_memory() {
    // A real repository, because detection reads one. Nothing is
    // started: the worktree in the list does not exist, so the worker
    // resolves, sends the config, and then fails to find it — which is
    // exactly the case the config must survive.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    std::fs::write(
        root.join("package.json"),
        "{\n  \"name\": \"x\",\n  \"scripts\": { \"dev\": \"next dev\" }\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    std::fs::write(root.join(".env.example"), "PORT=3000\n").unwrap();
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        crate::project::ProjectRef::from_root(&root).unwrap(),
    );
    let mut app = App::new_for_test(paths, Config::default(), vec![wt("feat+one")]);
    assert!(app.config.processes.is_empty(), "nothing is known yet");

    press(&mut app, KeyCode::Char('s'));
    let rx = app.event_rx.take().expect("the app owns its receiver");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut applied = false;
    while Instant::now() < deadline && !applied {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                let is_config = matches!(event, AppEvent::ConfigResolved(_));
                app.handle_event(event);
                applied = is_config;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    app.event_rx = Some(rx);

    assert!(applied, "the worker never sent what it resolved");
    assert_eq!(
        app.config.processes["dev"].cmd, "pnpm dev",
        "the session knows what it just answered, so the next `s` asks nothing"
    );
    assert_eq!(app.config.processes["dev"].roles(), vec!["web"]);
}

// The session's copy is from when the TUI opened. Something else — an
// agent's `init --answers`, a hand edit — may have written `pando.toml`
// since, and a start that resolved the old copy saw the dev command
// still open, took the rule's first choice and wrote it over the answer
// already in the file.
#[test]
fn a_start_resolves_the_config_on_disk_not_the_one_the_tui_opened_with() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    std::fs::write(
        root.join("package.json"),
        "{\n  \"name\": \"x\",\n  \"scripts\": { \"dev\": \"next dev\", \"dev:web\": \"next dev\" }\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    std::fs::write(root.join(".env.example"), "PORT=3000\n").unwrap();
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        crate::project::ProjectRef::from_root(&root).unwrap(),
    );
    let mut app = App::new_for_test(paths.clone(), Config::default(), vec![wt("feat+one")]);

    // Written by another pando after this session opened.
    let file = paths.config_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        "[dev]\ncmd = \"pnpm dev:web\"\nports = { PORT = \"web\" }\n",
    )
    .unwrap();

    press(&mut app, KeyCode::Char('s'));
    let rx = app.event_rx.take().expect("the app owns its receiver");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut applied = false;
    while Instant::now() < deadline && !applied {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                let is_config = matches!(event, AppEvent::ConfigResolved(_));
                app.handle_event(event);
                applied = is_config;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    app.event_rx = Some(rx);

    assert!(applied, "the worker never sent what it resolved");
    assert_eq!(app.config.processes["dev"].cmd, "pnpm dev:web");
    let written = std::fs::read_to_string(&file).unwrap();
    assert!(
        written.contains("cmd = \"pnpm dev:web\"") && !written.contains("cmd = \"pnpm dev\""),
        "the answer already in the file stays: {written}"
    );
}

// `main` guards the config it loads, once; the workers read the file
// again, and `config::load` knows only the repository root. A
// `worktrees_dir` moved into a linked worktree after the TUI opened is
// where `n` made the next worktree: inside another checkout.
#[test]
fn n_refuses_a_worktrees_dir_moved_into_a_linked_worktree_since_the_tui_opened() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    let linked = dir.path().join("linked");
    crate::testutil::git(
        &root,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "linked",
            linked.to_str().unwrap(),
        ],
    );
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        crate::project::ProjectRef::from_root(&root).unwrap(),
    );
    let mut app = App::new_for_test(paths.clone(), Config::default(), Vec::new());

    // Written after this session opened.
    let nested = linked.join("nested");
    let file = paths.config_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(
        &file,
        format!("[project]\nworktrees_dir = \"{}\"\n", nested.display()),
    )
    .unwrap();

    assert!(app.spawn_create("feat/x".into(), None));
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().expect("an error");
    assert!(is_error, "{message}");
    assert!(message.contains("inside the worktree"), "{message}");
    assert!(!nested.exists(), "nothing is made inside another checkout");
}

#[test]
fn enter_twice_starts_the_selected_worktree() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Start)
    );
}

#[test]
fn a_process_key_with_nothing_selected_says_so() {
    let mut app = test_app(&[]);
    press(&mut app, KeyCode::Char('s'));
    assert!(app.pending.is_none());
    assert_eq!(
        app.active_status().map(|(m, _)| m),
        Some("nothing selected")
    );
}

#[test]
fn open_needs_a_port_before_it_has_a_url() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened, None);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(message, "feat/one is not running — s starts it");
    assert!(is_error);

    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened.as_deref(), Some("http://localhost:17342"));
}

// A worktree whose only process serves no page, Expo's Metro, has no URL:
// `o` hands no browser Metro's root, and opens its app instead, on a
// worker, saying so; the worker's end is a success or an error. A test
// reaches no simulator: the app it would open is recorded.
#[test]
fn o_on_a_worktree_that_serves_no_page_opens_its_app() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    app.config =
        toml::from_str("[processes.dev]\ncmd = \"npx expo start\"\nports = [\"web\"]\n").unwrap();
    let record = app.state.worktrees.get_mut("feat+one").unwrap();
    record.pageless.insert("dev".to_string());
    assert!(app.url_of("feat+one").is_none());

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened, None, "no browser");
    assert_eq!(app.opened_apps, ["exp://127.0.0.1:17342"]);
    assert_eq!(
        app.active_status().map(|(m, _)| m),
        Some("opening feat/one's app")
    );
    // A second `o` while it opens starts no second opening.
    app.opened_apps.clear();
    press(&mut app, KeyCode::Char('o'));
    assert!(app.opened_apps.is_empty());
    assert!(
        app.active_status()
            .unwrap()
            .0
            .contains("being opened already"),
        "{:?}",
        app.active_status()
    );
    app.handle_event(AppEvent::AppOpening(
        "starting DeviceHub and waiting up to 120s for a simulator to boot".into(),
    ));
    assert_eq!(
        app.active_status().map(|(m, _)| m),
        Some("starting DeviceHub and waiting up to 120s for a simulator to boot")
    );
    app.handle_event(AppEvent::AppOpened(Ok(
        "opened feat/one's dev in Expo Go on the booted iOS simulator".into(),
    )));
    let (message, is_error) = app.active_status().unwrap();
    assert!(message.starts_with("opened feat/one's dev"), "{message}");
    assert!(!is_error);
    app.handle_event(AppEvent::AppOpened(Err("feat/one: no simulator".into())));
    assert!(app.active_status().unwrap().1, "an error");
    // Once it ended, `o` opens it again.
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened_apps, ["exp://127.0.0.1:17342"]);
    app.handle_event(AppEvent::AppOpened(Ok("opened".into())));

    // A process that only says `page = false` has no app to open.
    app.opened_apps.clear();
    app.config =
        toml::from_str("[processes.dev]\ncmd = \"npm run worker\"\nports = [\"web\"]\n").unwrap();
    press(&mut app, KeyCode::Char('o'));
    assert!(app.opened_apps.is_empty());
    let (message, is_error) = app.active_status().unwrap();
    assert!(message.contains("serves no page"), "{message}");
    assert!(message.contains("runs no app"), "{message}");
    assert!(is_error);
}

// A worktree whose processes all run with `ports = []`, a worker or a
// watcher, is running and has no URL. `o` and `c` said to start it first,
// beside a row that said it was running.
#[test]
fn o_and_c_on_a_running_worktree_that_holds_no_port_say_so() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let record = app.state.worktrees.get_mut("feat+one").unwrap();
    record.ports.clear();
    record.roles.insert("dev".to_string(), Vec::new());
    record.processes.get_mut("dev").unwrap().ready_port = None;
    assert!(app.url_of("feat+one").is_none());

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened, None);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(
        message,
        "feat/one is running and holds no port, so it has no URL to open"
    );
    assert!(is_error);

    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.clipboard, None);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(
        message,
        "feat/one is running and holds no port, so it has no URL to copy"
    );
    assert!(is_error);

    // A failed one is let past the phase check, since its URL may still
    // answer; with no port, `c` said it was running beside a failed row.
    app.state
        .worktrees
        .get_mut("feat+one")
        .unwrap()
        .processes
        .get_mut("dev")
        .unwrap()
        .phase = Phase::Failed {
        reason: "exit 1".into(),
        at: Utc::now(),
    };
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.clipboard, None);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(
        message,
        "feat/one has failed and holds no port, so it has no URL to copy"
    );
    assert!(is_error);
}

// A stop keeps the port assignment, so `o` and `c` handed out a URL that
// nothing served, and said so with a tick.
#[test]
fn o_and_c_refuse_a_stopped_worktrees_leftover_url() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    app.state
        .worktrees
        .get_mut("feat+one")
        .unwrap()
        .processes
        .clear();
    assert!(app.url_of("feat+one").is_some(), "the ports outlive a stop");

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened, None);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(message, "feat/one is not running — s starts it");
    assert!(is_error);

    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.clipboard, None);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(message, "feat/one is not running — s starts it");
    assert!(is_error);
}

// `pando open` refuses a failed worktree; its URL may still answer from
// another process, so `c` still copies it.
#[test]
fn o_refuses_a_failed_worktree_and_c_still_copies_its_url() {
    let mut app = test_app(&["feat+one"]);
    let failed = Phase::Failed {
        reason: "exit 1".into(),
        at: Utc::now(),
    };
    with_process(&mut app, "feat+one", failed);

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened, None);
    let (message, is_error) = app.active_status().unwrap();
    assert!(message.starts_with("feat/one has failed"), "{message}");
    assert!(is_error);

    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.clipboard.as_deref(), Some("http://localhost:17342"));
}

// The URL is one process's port. With that one stopped on its own and a
// sibling still up, `o` opened a refused connection and `c` copied it,
// each with a tick, while `t` refused the same state.
#[test]
fn o_and_c_refuse_a_url_whose_own_process_is_stopped_while_a_sibling_runs() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    app.state
        .worktrees
        .get_mut("feat+one")
        .unwrap()
        .processes
        .remove("dev");
    let refusal = "feat/one is not running dev, the process its URL points at — s starts it";

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened, None);
    assert_eq!(app.active_status(), Some((refusal, true)));

    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.clipboard, None);
    assert_eq!(app.active_status(), Some((refusal, true)));
}

// ---- share -----------------------------------------------------------

// Sharing puts the dev server on the internet, and a link once given
// out cannot be taken back, so it asks as unsharing does.
#[test]
fn t_on_a_worktree_that_is_not_shared_asks_and_then_shares() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());

    press(&mut app, KeyCode::Char('t'));
    assert!(
        matches!(&app.modal, Some(Modal::Share { name }) if name == "feat+one"),
        "{:?}",
        app.modal
    );
    assert!(
        app.pending.is_none(),
        "nothing is published until confirmed"
    );
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert!(app.pending.is_none(), "escape shares nothing");

    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Enter);
    let pending = app.pending.as_ref().expect("a share is in flight");
    assert_eq!(pending.kind, PendingKind::Share);
    assert_eq!(pending.name, "feat+one");
    assert!(app.modal.is_none());
}

// The session's copy is from when the TUI opened, and the share worker
// took `[share]` from it: an `auth_cmd` deleted since still minted a
// session and put it on the public URL, where `pando share` read the
// file and did not.
#[test]
fn a_share_reads_the_config_on_disk_not_the_one_the_tui_opened_with() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        crate::project::ProjectRef::from_root(&root).unwrap(),
    );
    let mut app = App::new_for_test(paths.clone(), Config::default(), vec![wt("feat+one")]);

    // Written after this session opened. A provider pando does not speak
    // is refused before anything is spawned or looked up, so the refusal
    // says which config the worker read.
    let file = paths.config_file();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "[share]\nprovider = \"ngrok\"\n").unwrap();

    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('y'));
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.pending.is_some() {
        assert!(Instant::now() < deadline, "the worker never reported");
        app.poll_pending();
        std::thread::sleep(Duration::from_millis(5));
    }
    let (message, is_error) = app.active_status().expect("a status");
    assert!(is_error, "{message}");
    assert!(
        message.contains("[share].provider is \"ngrok\""),
        "the share used the session's copy: {message}"
    );
}

#[test]
fn t_on_a_shared_worktree_asks_before_taking_the_url_away() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_share(&mut app, "feat+one", None);

    press(&mut app, KeyCode::Char('t'));
    match &app.modal {
        Some(Modal::Unshare { name, url }) => {
            assert_eq!(name, "feat+one");
            assert_eq!(url, "https://fake-host.trycloudflare.com");
        }
        other => panic!("expected the unshare confirmation, got {other:?}"),
    }
    assert!(
        app.pending.is_none(),
        "nothing happens until it is confirmed"
    );
}

#[test]
fn the_unshare_confirmation_takes_the_url_down_on_y_and_keeps_it_otherwise() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_share(&mut app, "feat+one", None);

    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert!(app.pending.is_none(), "escape keeps the URL up");

    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('n'));
    assert!(app.pending.is_none(), "so does n");

    press(&mut app, KeyCode::Char('t'));
    press(&mut app, KeyCode::Char('y'));
    let pending = app.pending.as_ref().expect("an unshare is in flight");
    assert_eq!(pending.kind, PendingKind::Unshare);
    assert!(app.modal.is_none());
}

#[test]
fn shift_o_opens_the_public_url_and_says_so_when_there_is_none() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());

    press(&mut app, KeyCode::Char('O'));
    assert_eq!(app.opened, None);
    let (message, is_error) = app.active_status().unwrap();
    assert!(message.contains("not shared"), "{message}");
    assert!(is_error);

    with_share(&mut app, "feat+one", None);
    press(&mut app, KeyCode::Char('O'));
    assert_eq!(
        app.opened.as_deref(),
        Some("https://fake-host.trycloudflare.com")
    );
}

// `o` is the local one and `O` is the public one: two keys, two URLs,
// and neither may quietly become the other.
#[test]
fn the_two_open_keys_hand_out_different_urls() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_share(&mut app, "feat+one", None);

    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened.as_deref(), Some("http://localhost:17342"));
    press(&mut app, KeyCode::Char('O'));
    assert_eq!(
        app.opened.as_deref(),
        Some("https://fake-host.trycloudflare.com")
    );
}

#[test]
fn the_tunnel_and_proxy_logs_come_last_in_tab_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app(&["feat+one"]);
    app.paths = PandoPaths::new(dir.path(), app.paths.project.clone());
    app.config
        .processes
        .insert("dev".to_string(), crate::config::ProcessConfig::default());
    let logs = app.paths.logs_dir("feat+one");
    std::fs::create_dir_all(&logs).unwrap();
    for source in ["proxy", "tunnel", "dev", "install"] {
        std::fs::write(logs.join(format!("{source}.log")), "x\n").unwrap();
    }

    assert_eq!(
        app.log_sources("feat+one"),
        vec!["dev", "install", "tunnel", "proxy"],
        "a share's own logs sort last, and the tunnel before the proxy"
    );
}

// Phase 2b review, finding 6. `url_of` was a third implementation of
// the URL rule: the alphabetically first *role* rather than the first
// role of the first process, and it never looked at what the process
// was really listening on. `o` opened a different address from the one
// `pando status` had just printed.
#[test]
fn open_uses_the_same_url_rule_as_status() {
    let mut app = test_app(&["feat+url2"]);
    let mut state = State::new();
    let mut record = WorktreeRecord::new("/trees/feat+url2", true);
    record.ports.insert("srv".to_string(), 19_056);
    record.ports.insert("admin".to_string(), 19_057);
    record
        .roles
        .insert("alpha".to_string(), vec!["srv".to_string()]);
    record
        .roles
        .insert("beta".to_string(), vec!["admin".to_string()]);
    // And `alpha` ignored the port it was given, which the TUI never
    // noticed at all.
    record.processes.insert(
        "alpha".to_string(),
        ProcessRecord {
            pid: 4242,
            pgid: Group::from_raw(4242),
            started_at: Utc::now(),
            log_path: PathBuf::from("/does/not/exist/alpha.log"),
            ready_port: Some(19_056),
            ready_timeout_s: None,
            observed_ports: vec![3000],
            swept: false,
            phase: running_phase(),
        },
    );
    record.observed_ports = vec![3000];
    state.worktrees.insert("feat+url2".to_string(), record);
    app.handle_event(refreshed(state.clone()));

    assert_eq!(
        app.url_of("feat+url2"),
        crate::actions::worktree_url(&state.worktrees["feat+url2"]),
        "one rule, wherever it is asked"
    );
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(app.opened.as_deref(), Some("http://localhost:3000"));
}

#[test]
fn a_refresh_replaces_the_process_state() {
    let mut app = test_app(&["feat+one"]);
    let mut state = State::new();
    let mut record = WorktreeRecord::new("/trees/feat+one", true);
    record.ports.insert("web".to_string(), 17_342);
    state.worktrees.insert("feat+one".to_string(), record);
    assert!(app.handle_event(refreshed(state)));
    assert_eq!(
        app.url_of("feat+one").as_deref(),
        Some("http://localhost:17342")
    );
    assert!(!app.refreshing, "the single-flight slot is free again");
}

#[test]
fn a_refresh_that_failed_reaches_the_status_line() {
    let mut app = test_app(&["feat+one"]);
    app.handle_event(refreshed_with(
        State::new(),
        Some("state is v3"),
        Vec::new(),
    ));
    let (message, is_error) = app.active_status().unwrap();
    assert!(message.contains("state is v3"), "{message}");
    assert!(is_error);
}

// Four quick refreshes in five seconds, each finding the same state file
// it cannot read: said on every one, `m` held nothing else within a
// minute.
#[test]
fn a_standing_warning_from_the_quick_refresh_is_said_once() {
    let mut app = test_app(&["feat+one"]);
    let warning = Some("state file /s is version 3");
    app.handle_event(refreshed_with(State::new(), warning, Vec::new()));
    app.handle_event(refreshed_with(State::new(), warning, Vec::new()));
    // Nor again by the discovery that reads the same file.
    app.apply_snapshot(Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![wt("feat+one")],
        created_by_pando: BTreeMap::new(),
        state: State::new(),
        warning: warning.map(str::to_string),
        notices: Vec::new(),
        default_base: None,
    });
    let said = app
        .messages
        .iter()
        .filter(|m| m.message.contains("version 3"))
        .count();
    assert_eq!(said, 1, "{:?}", app.messages);

    // Read again after a good refresh, it is news again.
    app.handle_event(refreshed(State::new()));
    app.handle_event(refreshed_with(State::new(), warning, Vec::new()));
    let said = app
        .messages
        .iter()
        .filter(|m| m.message.contains("version 3"))
        .count();
    assert_eq!(said, 2, "{:?}", app.messages);
}

// A save that keeps failing — a home made read-only while a running
// server has a port the file lacks — is said by each discovery's full
// refresh. The quick refreshes between them only read the file, saved
// nothing, and cleared it all the same, so every discovery said it again.
#[test]
fn a_standing_warning_is_not_cleared_by_a_refresh_that_only_read() {
    let mut app = test_app(&["feat+one"]);
    let discovery = |app: &App| Snapshot {
        warning: Some("could not save /s: read-only file system".into()),
        ..listing(app, &["feat+one"])
    };
    app.apply_snapshot(discovery(&app));
    app.handle_event(reread(State::new()));
    app.handle_event(reread(State::new()));
    app.apply_snapshot(discovery(&app));
    let said = app
        .messages
        .iter()
        .filter(|m| m.message.contains("could not save"))
        .count();
    assert_eq!(said, 1, "{:?}", app.messages);
}

// The quick refresh is four refreshes in five, and the one that sweeps a
// dead share saves it away: a notice it dropped, no later refresh had.
#[test]
fn a_notice_from_the_quick_refresh_reaches_the_status_line() {
    let mut app = test_app(&["feat+one"]);
    app.handle_event(refreshed_with(
        State::new(),
        None,
        vec!["feat+one: the share's tunnel exited, so the public URL is closed".into()],
    ));
    let (message, is_error) = app.active_status().expect("a notice");
    assert!(message.contains("public URL is closed"), "{message}");
    assert!(is_error);
}

// A save that failed has the phases right in memory; the quick refresh
// dropped them and kept the older read.
#[test]
fn a_quick_refresh_with_a_warning_still_adopts_its_state() {
    let mut app = test_app(&["feat+one"]);
    let mut state = State::new();
    let mut record = WorktreeRecord::new("/trees/feat+one", true);
    record.ports.insert("web".to_string(), 17_342);
    state.worktrees.insert("feat+one".to_string(), record);
    app.handle_event(refreshed_with(state, Some("could not save"), Vec::new()));
    assert!(app.record_for("feat+one").is_some());
}

// ---- the question modal ----------------------------------------------

#[test]
fn a_question_opens_a_modal_with_the_recommendation_preselected() {
    let mut app = test_app(&["feat+one"]);
    let _rx = open_question(&mut app, a_question());
    match app.modal.as_ref() {
        Some(Modal::Question {
            question,
            selected,
            custom,
            ..
        }) => {
            assert_eq!(*selected, 0, "the rules' own pick is preselected");
            assert!(custom.is_none());
            assert_eq!(question.options.len(), 2);
        }
        other => panic!("expected a question modal, got {:?}", other.is_some()),
    }
}

#[test]
fn choosing_an_option_answers_the_worker_and_closes_the_modal() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_question());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(rx.try_recv().unwrap(), Ok(actions::Answer::Choice(1)));
    assert!(app.modal.is_none(), "the modal closes once it is answered");
}

// Every slot accepts a shell command, so there is never a dead end.
#[test]
fn a_typed_command_is_sent_as_written() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_question());
    press(&mut app, KeyCode::Char('c'));
    type_str(&mut app, "./serve.sh");
    press(&mut app, KeyCode::Backspace);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        rx.try_recv().unwrap(),
        Ok(actions::Answer::Custom("./serve.s".to_string()))
    );
    assert!(app.modal.is_none());
}

// The worker is blocked on the reply channel. A modal that closed
// without sending would leave it there for the life of the process.
#[test]
fn cancelling_a_question_always_tells_the_worker() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_question());
    press(&mut app, KeyCode::Esc);
    assert_eq!(rx.try_recv().unwrap(), Err("cancelled".to_string()));
    assert!(app.modal.is_none());
}

#[test]
fn escape_from_the_typing_line_goes_back_to_the_options() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_question());
    press(&mut app, KeyCode::Char('c'));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(
        app.modal.as_ref(),
        Some(Modal::Question { custom: None, .. })
    ));
    assert!(rx.try_recv().is_err(), "nothing was answered yet");
    press(&mut app, KeyCode::Esc);
    assert_eq!(rx.try_recv().unwrap(), Err("cancelled".to_string()));
}

#[test]
fn a_question_with_no_options_opens_straight_into_typing() {
    let mut app = test_app(&["feat+one"]);
    let mut question = a_question();
    question.options.clear();
    question.preselect = None;
    let rx = open_question(&mut app, question);
    assert!(matches!(
        app.modal.as_ref(),
        Some(Modal::Question {
            custom: Some(_),
            ..
        })
    ));
    type_str(&mut app, "node server.js");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        rx.try_recv().unwrap(),
        Ok(actions::Answer::Custom("node server.js".to_string()))
    );
}

#[test]
fn an_empty_typed_answer_is_refused_rather_than_sent() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_question());
    press(&mut app, KeyCode::Char('c'));
    press(&mut app, KeyCode::Enter);
    assert!(rx.try_recv().is_err());
    assert!(app.modal.is_some());
    assert!(app.active_status().unwrap().0.contains("type the command"));
}

// ---- several processes -----------------------------------------------

#[test]
fn tab_switches_the_tail_between_processes() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());

    let (_, first, _) = app.tail_target().expect("a process to tail");
    assert_eq!(first, "api", "config order, which is the row order");

    press(&mut app, KeyCode::Tab);
    let (key_of, second, path) = app.tail_target().expect("a process to tail");
    assert_eq!(second, "dev");
    assert_eq!(
        key_of, "feat+one/dev",
        "one tail per process, not per worktree"
    );
    assert!(path.ends_with("dev.log"));

    // And round it goes.
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.tail_target().unwrap().1, "api");
}

#[test]
fn tab_on_a_worktree_with_one_process_says_so_rather_than_cycling() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.tail_target().unwrap().1, "dev");
    let (message, is_error) = app.active_status().expect("something was said");
    assert!(message.contains("one process"), "{message}");
    assert!(is_error);
}

#[test]
fn moving_to_another_worktree_starts_at_its_first_process() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    with_process(&mut app, "feat+two", running_phase());

    press(&mut app, KeyCode::Tab);
    assert_eq!(app.tail_target().unwrap().1, "dev");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.tail_target().unwrap().0, "feat+two/dev");
    assert_eq!(app.tail_index, 0, "the index belongs to the row");
}

// ---- the log tail ----------------------------------------------------

#[test]
fn the_tail_lru_keeps_the_newest_and_drops_the_coldest() {
    let mut tails = LogTails::default();
    for i in 0..MAX_LOG_TAILS + 2 {
        tails.touch(&format!("w{i}"), PathBuf::from("/does/not/exist.log"));
    }
    assert_eq!(tails.len(), MAX_LOG_TAILS);
    assert!(tails.get("w0").is_none(), "the coldest went");
    assert!(tails.get(&format!("w{}", MAX_LOG_TAILS + 1)).is_some());
}

#[test]
fn the_selected_tail_never_evicts_itself() {
    let mut tails = LogTails::default();
    tails.touch("keep", PathBuf::from("/does/not/exist.log"));
    for i in 0..MAX_LOG_TAILS + 4 {
        tails.touch("keep", PathBuf::from("/does/not/exist.log"));
        tails.touch(&format!("w{i}"), PathBuf::from("/does/not/exist.log"));
    }
    assert!(tails.get("keep").is_some());
}

// `l` used to focus the inline tail so j/k scrolled it; it opens the
// full viewer now, and the page keys scroll the tail in place — which
// is the origin tool's own binding, and leaves j/k on the list.
#[test]
fn the_page_keys_scroll_the_inline_tail_without_moving_the_list() {
    let (_dir, mut app) = app_with_logs(&["feat+one", "feat+two"]);
    let lines: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+one", "dev", &lines);
    tail_the_log(&mut app, "feat+one", "dev");
    app.tail_rows = 10;
    app.handle_event(AppEvent::Tick);

    let row = app.list_state.selected();
    press(&mut app, KeyCode::PageUp);
    assert_eq!(app.tail_scroll, 10, "a page back through the tail");
    assert_eq!(app.list_state.selected(), row, "the list cursor stays put");
    press(&mut app, KeyCode::PageDown);
    assert_eq!(app.tail_scroll, 0, "and a page forward returns to the end");
}

#[test]
fn moving_the_cursor_returns_the_tail_to_the_end() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    app.tail_scroll = 12;
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.tail_scroll, 0);
}

// The list moves while a start runs, and the start's return sent the tail
// on screen to its end even when that tail was another worktree's, which
// the start had not touched.
#[test]
fn a_start_finishing_keeps_the_scroll_of_another_worktrees_tail() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    let start_one = |app: &mut App| {
        app.spawn_pending("feat+one".into(), PendingKind::Start, || {
            Ok(PendingOutcome::Started(
                "feat+one".into(),
                None,
                vec![("dev".into(), 4242)],
            ))
        });
    };
    start_one(&mut app);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected_worktree().unwrap().name, "feat+two");
    app.tail_scroll = 3;
    wait_for_pending(&mut app);
    assert_eq!(app.tail_scroll, 3, "feat/two's tail is where it was read");

    press(&mut app, KeyCode::Char('k'));
    app.tail_scroll = 3;
    start_one(&mut app);
    wait_for_pending(&mut app);
    assert_eq!(app.tail_scroll, 0, "feat/one's own log is new");
}

// ---- the log viewer --------------------------------------------------

/// An app whose home is a real (temporary) directory, so the log files
/// the viewer reads are real files.
pub fn app_with_logs(names: &[&str]) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let paths = PandoPaths::new(
        dir.path().join("home"),
        ProjectRef {
            id: "acme-shop-3f9a2c1d".into(),
            root: dir.path().join("acme-shop"),
            display_name: "acme-shop".into(),
        },
    );
    let worktrees: Vec<Worktree> = names.iter().map(|n| wt(n)).collect();
    // Every project that has a dev log has a dev process configured;
    // tab order is read off config, so the fixture carries one.
    let mut config = Config::default();
    config
        .processes
        .insert("dev".to_string(), crate::config::ProcessConfig::default());
    let mut app = App::new_for_test(paths, config, worktrees);
    app.created_by_pando = names.iter().map(|n| (n.to_string(), true)).collect();
    (dir, app)
}

pub fn write_log<S: AsRef<str>>(app: &App, worktree: &str, source: &str, lines: &[S]) {
    let path = app.paths.log_file(worktree, source);
    std::fs::create_dir_all(path.parent().expect("a log has a directory")).unwrap();
    let body: String = lines
        .iter()
        .map(|line| format!("{}\n", line.as_ref()))
        .collect();
    std::fs::write(path, body).unwrap();
}

/// Points the detail pane's tail at a log that really exists.
fn tail_the_log(app: &mut App, worktree: &str, source: &str) {
    let path = app.paths.log_file(worktree, source);
    if !app.state.worktrees.contains_key(worktree) {
        with_process(app, worktree, running_phase());
    }
    let record = app
        .state
        .worktrees
        .get_mut(worktree)
        .expect("the worktree has a record");
    let process = record
        .processes
        .remove("dev")
        .expect("with_process left a dev record");
    record.processes.insert(
        source.to_string(),
        ProcessRecord {
            log_path: path,
            ..process
        },
    );
}

/// Opens the viewer the way a key press would, then paints once so the
/// tab list and the viewport are what the first frame decided.
fn open_viewer(app: &mut App, width: u16, height: u16) {
    press(app, KeyCode::Char('l'));
    paint(app, width, height);
}

fn paint(app: &mut App, width: u16, height: u16) {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| crate::tui::render::render(f, app))
        .unwrap();
}

fn viewer(app: &App) -> &LogView {
    app.log_view().expect("the viewer is open")
}

#[test]
fn l_opens_the_viewer_on_the_selected_worktree_and_q_returns_to_the_list() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["listening on 17342"]);
    open_viewer(&mut app, 80, 20);
    assert_eq!(viewer(&app).name, "feat+one");
    assert_eq!(viewer(&app).source, "dev");
    press(&mut app, KeyCode::Char('q'));
    assert!(app.log_view().is_none(), "q goes back to the list");
    assert!(!app.should_quit, "and does not quit pando");
}

#[test]
fn a_worktree_with_two_logs_gets_two_tabs_and_one_with_one_gets_one() {
    let (_dir, mut app) = app_with_logs(&["feat+one", "feat+two"]);
    write_log(&app, "feat+one", "dev", &["up"]);
    write_log(&app, "feat+one", "install", &["installed"]);
    write_log(&app, "feat+two", "dev", &["up"]);

    open_viewer(&mut app, 80, 20);
    assert_eq!(viewer(&app).available, vec!["dev", "install"]);

    press(&mut app, KeyCode::Char('q'));
    press(&mut app, KeyCode::Char('j'));
    open_viewer(&mut app, 80, 20);
    assert_eq!(viewer(&app).available, vec!["dev"]);
}

#[test]
fn a_source_that_appears_later_gets_a_tab_on_the_next_draw() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["up"]);
    open_viewer(&mut app, 80, 20);
    assert_eq!(viewer(&app).available, vec!["dev"]);

    write_log(&app, "feat+one", "migrate", &["done"]);
    paint(&mut app, 80, 20);
    assert_eq!(
        viewer(&app).available,
        vec!["dev", "migrate"],
        "the tab bar is rebuilt from the files that are there"
    );
}

#[test]
fn tabs_run_processes_first_then_hooks_then_whatever_is_left() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    app.config
        .processes
        .insert("web".to_string(), crate::config::ProcessConfig::default());
    app.config
        .processes
        .insert("api".to_string(), crate::config::ProcessConfig::default());
    app.config.hooks.push(crate::config::HookConfig {
        name: "migrate".to_string(),
        after: crate::config::HookPoint::Services,
        fingerprint: Vec::new(),
        cmd: "true".to_string(),
        cwd: None,
        fallback: None,
        on: None,
    });
    for source in ["tunnel", "migrate", "install", "web", "api"] {
        write_log(&app, "feat+one", source, &["x"]);
    }
    open_viewer(&mut app, 80, 20);
    assert_eq!(
        viewer(&app).available,
        vec!["all", "api", "web", "install", "migrate", "tunnel"],
        "the merged processes, then each process, then hooks, then the rest"
    );
}

#[test]
fn the_viewer_opens_on_the_source_the_detail_tail_was_showing() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "api", &["api up"]);
    write_log(&app, "feat+one", "dev", &["dev up"]);
    tail_the_log(&mut app, "feat+one", "api");
    open_viewer(&mut app, 80, 20);
    assert_eq!(
        viewer(&app).source,
        "api",
        "pressing l while reading the api's tail must not land on dev"
    );
}

#[test]
fn tab_cycles_the_source_and_shift_tab_goes_back() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["dev up"]);
    write_log(&app, "feat+one", "install", &["installed"]);
    open_viewer(&mut app, 80, 20);
    assert_eq!(viewer(&app).source, "dev");
    press(&mut app, KeyCode::Tab);
    assert_eq!(viewer(&app).source, "install");
    press(&mut app, KeyCode::Tab);
    assert_eq!(viewer(&app).source, "dev", "and it wraps");
    press(&mut app, KeyCode::BackTab);
    assert_eq!(viewer(&app).source, "install");
}

#[test]
fn tab_does_nothing_when_there_is_only_one_source() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["dev up"]);
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Tab);
    assert_eq!(viewer(&app).source, "dev");
}

#[test]
fn a_worktree_with_no_log_yet_opens_a_viewer_that_says_so() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    open_viewer(&mut app, 80, 20);
    let view = viewer(&app);
    assert!(view.missing);
    assert_eq!(
        view.available,
        vec!["dev".to_string()],
        "the source on screen is always in the tab list, file or no file, \
         so `tab` can leave it"
    );
    assert!(!view.follow, "nothing to follow");
}

// The other half of finding 4: the title and the tab list are decided
// by the paint, and a tail whose file has been deleted never grows —
// so without this the frame goes on naming a file that is not there
// until something else happens to repaint it.
#[test]
fn a_log_deleted_under_the_viewer_asks_for_the_repaint_that_says_so() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["one"]);
    open_viewer(&mut app, 80, 12);
    assert!(!viewer(&app).gone);

    std::fs::remove_file(app.paths.log_file("feat+one", "dev")).unwrap();
    assert!(
        app.handle_event(AppEvent::Tick),
        "the tick has to ask for a repaint; nothing else will"
    );
    paint(&mut app, 80, 12);
    assert!(viewer(&app).gone, "and the paint records what it decided");

    assert!(
        !app.handle_event(AppEvent::Tick),
        "once the frame agrees with the file, the viewer settles"
    );
}

// Phase 2c review, finding 5. `poll_viewer` returned early while
// `missing`, so a viewer opened before `start` had written anything —
// the common case — stayed on `no log file for this source yet` for as
// long as it was open.
#[test]
fn a_log_that_appears_after_the_viewer_opened_is_picked_up_on_the_next_tick() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    open_viewer(&mut app, 80, 20);
    assert!(viewer(&app).missing);

    write_log(&app, "feat+one", "dev", &["listening on 17342"]);
    app.handle_event(AppEvent::Tick);

    let view = viewer(&app);
    assert!(!view.missing, "the file is there now");
    assert!(view.follow, "and a viewer that has nothing yet follows it");
    assert_eq!(view.tail.lines().len(), 1, "read on the same tick");
}

#[test]
fn the_cursor_moves_and_never_leaves_the_log() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+one", "dev", &lines);
    open_viewer(&mut app, 80, 12);
    assert!(viewer(&app).follow, "the viewer opens on the live tail");

    press(&mut app, KeyCode::Char('k'));
    assert!(!viewer(&app).follow, "k breaks follow");
    assert_eq!(viewer(&app).cursor, 18);

    for _ in 0..30 {
        press(&mut app, KeyCode::Char('j'));
    }
    assert_eq!(viewer(&app).cursor, 19, "clamped at the last line");
    for _ in 0..30 {
        press(&mut app, KeyCode::Char('k'));
    }
    assert_eq!(viewer(&app).cursor, 0, "and at the first");
}

#[test]
fn g_goes_to_the_top_and_capital_g_follows() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+one", "dev", &lines);
    open_viewer(&mut app, 80, 12);

    press(&mut app, KeyCode::Char('g'));
    assert_eq!(viewer(&app).cursor, 0);
    assert!(!viewer(&app).follow);

    press(&mut app, KeyCode::Char('G'));
    assert!(viewer(&app).follow, "G returns to the live tail");
}

// The tabs are numbered, so the numbers are keys: `2` is what a reader
// presses to see the second tab. A digit past the last tab does nothing.
#[test]
fn a_digit_switches_to_the_tab_it_numbers() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["dev line"]);
    write_log(&app, "feat+one", "install", &["install line"]);
    open_viewer(&mut app, 80, 12);
    let first = viewer(&app).source.clone();
    let second = viewer(&app).available[1].clone();

    press(&mut app, KeyCode::Char('2'));
    assert_eq!(viewer(&app).source, second);
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(viewer(&app).source, first);
    press(&mut app, KeyCode::Char('9'));
    assert_eq!(viewer(&app).source, first, "there is no ninth tab");
}

#[test]
fn the_half_page_keys_move_by_half_the_viewer() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let lines: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+one", "dev", &lines);
    // 20 rows minus the border and the footer leaves 17 body rows.
    open_viewer(&mut app, 80, 20);
    let half = app.viewer_height / 2;
    assert!(half > 1, "the body has room for a half page: {half}");

    app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
    assert_eq!(viewer(&app).cursor, 59 - half);
    app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
    assert_eq!(viewer(&app).cursor, 59);
}

#[test]
fn motions_on_an_empty_and_a_one_line_log_stay_in_range() {
    for lines in [vec![], vec!["only".to_string()]] {
        let (_dir, mut app) = app_with_logs(&["feat+one"]);
        write_log(&app, "feat+one", "dev", &lines);
        open_viewer(&mut app, 80, 8);
        for key in ['j', 'k', 'g', 'G', 'w'] {
            type_str(&mut app, "9");
            press(&mut app, KeyCode::Char(key));
            paint(&mut app, 80, 8);
        }
        app.handle_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        paint(&mut app, 80, 8);
        assert!(viewer(&app).cursor <= lines.len().saturating_sub(1));
    }
}

#[test]
fn w_toggles_wrap() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["x".repeat(400)]);
    open_viewer(&mut app, 80, 12);
    assert!(viewer(&app).wrap, "lines wrap by default");
    press(&mut app, KeyCode::Char('w'));
    assert!(!viewer(&app).wrap);
    press(&mut app, KeyCode::Char('w'));
    assert!(viewer(&app).wrap);
}

#[test]
fn the_viewer_polls_its_own_log_on_the_tick() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["one"]);
    open_viewer(&mut app, 80, 12);
    assert_eq!(viewer(&app).tail.lines().len(), 1);
    write_log(&app, "feat+one", "dev", &["one", "two", "three"]);
    app.handle_event(AppEvent::Tick);
    assert_eq!(viewer(&app).tail.lines().len(), 3);
}

// ---- search and the level filter -------------------------------------

fn search_for(app: &mut App, query: &str) {
    press(app, KeyCode::Char('/'));
    for c in query.chars() {
        press(app, KeyCode::Char(c));
    }
    press(app, KeyCode::Enter);
}

#[test]
fn search_is_case_insensitive_and_lands_on_the_first_match() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["plain", "Compiled in 30ms", "plain", "compiled again"],
    );
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "COMPILED");
    assert_eq!(viewer(&app).search.matches, vec![1, 3]);
    assert_eq!(viewer(&app).cursor, 1, "the viewer jumps to the first one");
    assert!(!viewer(&app).follow);
    assert_eq!(viewer(&app).search_mode, SearchMode::Active);
}

#[test]
fn n_and_capital_n_step_the_matches_and_wrap() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["hit a", "miss", "hit b", "miss", "hit c"],
    );
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "hit");
    assert_eq!(viewer(&app).search.cursor, 0);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(viewer(&app).cursor, 2);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(viewer(&app).cursor, 4);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(viewer(&app).cursor, 0, "and it wraps");
    press(&mut app, KeyCode::Char('N'));
    assert_eq!(viewer(&app).cursor, 4, "backwards too");
}

#[test]
fn ctrl_n_and_ctrl_p_step_the_matches_as_well() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["hit a", "miss", "hit b"]);
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "hit");
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
    assert_eq!(viewer(&app).cursor, 2);
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
    assert_eq!(viewer(&app).cursor, 0);
}

#[test]
fn escape_while_typing_clears_the_search_and_keeps_the_viewer_open() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["hit"]);
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('/'));
    type_str(&mut app, "hi");
    assert_eq!(viewer(&app).search.query, "hi");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(viewer(&app).search.query, "h");
    press(&mut app, KeyCode::Esc);
    assert!(
        app.log_view().is_some(),
        "esc clears the query, not the view"
    );
    assert_eq!(viewer(&app).search_mode, SearchMode::Inactive);
    assert!(viewer(&app).search.query.is_empty());
}

// The search bar promises "esc clear", so esc has to clear rather than
// throw away the whole viewer and the reading position with it. One
// layer at a time; a second esc leaves.
#[test]
fn escape_on_a_live_search_clears_it_and_a_second_one_leaves() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["hit a", "plain", "hit b"]);
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "hit");
    assert_eq!(viewer(&app).search_mode, SearchMode::Active);

    press(&mut app, KeyCode::Esc);
    assert!(app.log_view().is_some(), "the viewer stays open");
    assert_eq!(viewer(&app).search_mode, SearchMode::Inactive);
    assert!(viewer(&app).search.matches.is_empty());

    press(&mut app, KeyCode::Esc);
    assert!(app.log_view().is_none(), "and the next one leaves");
}

#[test]
fn q_leaves_the_viewer_even_with_a_search_running() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["hit"]);
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "hit");
    press(&mut app, KeyCode::Char('q'));
    assert!(app.log_view().is_none());
    assert!(!app.should_quit);
}

#[test]
fn a_query_with_no_matches_leaves_the_cursor_where_it_was() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["one", "two", "three"]);
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('g'));
    search_for(&mut app, "nothing here");
    assert!(viewer(&app).search.matches.is_empty());
    assert_eq!(viewer(&app).cursor, 0);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(viewer(&app).cursor, 0, "stepping nothing moves nothing");
}

#[test]
fn f_cycles_the_level_filter_and_the_viewer_shows_only_what_passes() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["just info", "WARN slow", "ERROR boom", "more info"],
    );
    open_viewer(&mut app, 80, 12);
    assert_eq!(viewer(&app).visible_len(), 4);

    press(&mut app, KeyCode::Char('f'));
    assert_eq!(viewer(&app).log_filter, LogFilter::WarnPlus);
    assert_eq!(viewer(&app).visible_len(), 2);

    press(&mut app, KeyCode::Char('f'));
    assert_eq!(viewer(&app).log_filter, LogFilter::ErrorOnly);
    assert_eq!(viewer(&app).visible_len(), 1);

    press(&mut app, KeyCode::Char('f'));
    assert_eq!(viewer(&app).log_filter, LogFilter::All);
    assert!(viewer(&app).follow, "a filter change returns to the tail");
}

// dwt's `search_with_filter_skips_hidden_matches_and_maps_scroll_to_
// filtered_position`: the scroll and cursor are positions in the
// *filtered* list, so a match's absolute index has to be translated.
#[test]
fn search_under_a_filter_skips_hidden_matches_and_maps_the_position() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "target in an info line",
            "just info",
            "WARN target in a warn line",
            "just info",
            "ERROR target in an error line",
        ],
    );
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('f')); // warn+
    search_for(&mut app, "target");
    assert_eq!(
        viewer(&app).search.matches,
        vec![2, 4],
        "the info line matches the query but not the filter"
    );
    assert_eq!(
        viewer(&app).cursor,
        0,
        "line 2 is the first of the two lines the filter shows"
    );
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(viewer(&app).cursor, 1, "and line 4 is the second");
}

#[test]
fn changing_the_filter_drops_matches_it_now_hides() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["target info", "WARN target", "plain"],
    );
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "target");
    assert_eq!(viewer(&app).search.matches, vec![0, 1]);
    press(&mut app, KeyCode::Char('f'));
    assert_eq!(
        viewer(&app).search.matches,
        vec![1],
        "the cursor can never land on a row that is not painted"
    );
}

#[test]
fn ampersand_collapses_the_view_to_the_matches_and_expands_again() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["hit a", "miss", "hit b", "miss", "hit c"],
    );
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "hit");
    press(&mut app, KeyCode::Char('&'));
    assert!(viewer(&app).collapsed());
    assert_eq!(viewer(&app).visible_len(), 3, "only the matches");
    assert_eq!(viewer(&app).cursor, 0, "and it lands at the top");
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(
        viewer(&app).cursor,
        1,
        "stepping stays in the collapsed list"
    );
    press(&mut app, KeyCode::Char('&'));
    assert!(!viewer(&app).collapsed());
    assert_eq!(viewer(&app).visible_len(), 5);
}

#[test]
fn ampersand_does_nothing_without_an_active_search() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["a", "b"]);
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('&'));
    assert!(!viewer(&app).filter_to_matches);
    assert_eq!(viewer(&app).visible_len(), 2);
}

#[test]
fn matches_are_realigned_when_the_ring_buffer_evicts() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["hit a", "filler", "hit b"]);
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "hit");
    assert_eq!(viewer(&app).search.matches, vec![0, 2]);

    // A tail with room for three lines, so two more evict two.
    let path = app.paths.log_file("feat+one", "dev");
    let view = app.log_view_mut().expect("the viewer is open");
    view.tail = LogTail::new(path.clone(), 3).into();
    view.tail.poll().unwrap();
    view.search.matches = vec![0, 2];
    view.follow = false;
    write_log(
        &app,
        "feat+one",
        "dev",
        &["hit a", "filler", "hit b", "more", "and more"],
    );
    app.handle_event(AppEvent::Tick);

    let view = viewer(&app);
    assert_eq!(view.tail.lines().len(), 3);
    assert_eq!(
        view.search.matches,
        vec![0],
        "the match at index 0 fell out; index 2 moved down to 0"
    );
    assert!(view.search.cursor < view.search.matches.len().max(1));
}

#[test]
fn a_new_matching_line_joins_the_search_without_moving_the_reader() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["hit a", "plain", "hit b"]);
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "hit");
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(viewer(&app).search.cursor, 1);
    let was = viewer(&app).cursor;

    write_log(
        &app,
        "feat+one",
        "dev",
        &["hit a", "plain", "hit b", "hit c"],
    );
    app.handle_event(AppEvent::Tick);
    assert_eq!(viewer(&app).search.matches, vec![0, 2, 3]);
    assert_eq!(
        viewer(&app).search.cursor,
        1,
        "the reader stays on the match they were on"
    );
    assert_eq!(viewer(&app).cursor, was);
}

// ---- JSON blocks and the inspect overlay -----------------------------

/// A pretty-printed JSON block, as a framework prints one, with
/// ordinary lines on either side.
fn json_block_log() -> Vec<String> {
    [
        "starting up",
        "{",
        "  \"level\": \"error\",",
        "  \"msg\": \"boom\",",
        "  \"count\": 3",
        "}",
        "carrying on",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[test]
fn the_lines_of_a_json_block_share_one_id_and_one_severity() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &json_block_log());
    open_viewer(&mut app, 80, 20);
    let lines: Vec<_> = viewer(&app).tail.lines().iter().cloned().collect();
    let ids: Vec<Option<u64>> = lines.iter().map(|l| l.block_id).collect();
    assert_eq!(ids[0], None, "the line before the block is its own");
    assert!(ids[1].is_some());
    assert_eq!(
        ids[1..6]
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        1,
        "every line of the block carries the same id: {ids:?}"
    );
    assert_eq!(ids[6], None, "and the line after it is its own again");
    for line in &lines[1..6] {
        assert_eq!(
            line.level,
            LogLevel::Error,
            "the whole block takes the block's severity: {}",
            line.plain
        );
    }
}

#[test]
fn a_level_filter_keeps_or_drops_a_block_as_one_unit() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &json_block_log());
    open_viewer(&mut app, 80, 20);
    assert_eq!(viewer(&app).visible_len(), 7);
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Char('f')); // errors only
    assert_eq!(
        viewer(&app).visible_len(),
        5,
        "all five lines of the block, and neither of the plain ones"
    );
}

#[test]
fn capital_j_inspects_the_whole_block_and_j_scrolls_it() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &json_block_log());
    open_viewer(&mut app, 80, 20);
    // Onto a line in the middle of the block.
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('J'));

    let inspect = app.inspect.as_ref().expect("the overlay is open");
    assert!(
        inspect.text.contains("\"msg\": \"boom\""),
        "{}",
        inspect.text
    );
    assert!(inspect.text.contains("\"count\": 3"), "{}", inspect.text);
    assert!(
        !inspect.text.contains("starting up"),
        "the block, and only the block: {}",
        inspect.text
    );
    assert_eq!(inspect.scroll, 0);

    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.inspect.as_ref().unwrap().scroll, 1);
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.inspect.as_ref().unwrap().scroll, 0);
    press(&mut app, KeyCode::Char('q'));
    assert!(app.inspect.is_none(), "q closes the overlay");
    assert!(app.log_view().is_some(), "and leaves the viewer open");
}

#[test]
fn enter_inspects_the_cursor_line_and_pretty_prints_its_json() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["12:00:01 request {\"path\":\"/x\",\"ms\":30}"],
    );
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Enter);
    let inspect = app.inspect.as_ref().expect("the overlay is open");
    assert!(
        inspect.text.starts_with("12:00:01 request"),
        "the prefix is kept above the JSON: {}",
        inspect.text
    );
    assert!(
        inspect.text.contains("\n  \"path\": \"/x\""),
        "and the JSON is expanded: {}",
        inspect.text
    );
}

#[test]
fn a_line_with_no_json_inspects_as_itself() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["just a plain line"]);
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('J'));
    assert_eq!(app.inspect.as_ref().unwrap().text, "just a plain line");
    assert_eq!(app.inspect.as_ref().unwrap().lines.len(), 1);
}

#[test]
fn a_block_with_raw_output_interleaved_inspects_verbatim() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "{",
            "  \"level\": \"warn\",",
            "SELECT * FROM users;",
            "  \"msg\": \"slow\"",
            "}",
        ],
    );
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('J'));
    let inspect = app.inspect.as_ref().expect("the overlay is open");
    assert!(
        inspect.text.contains("SELECT * FROM users;"),
        "an unparseable block is shown as it arrived: {}",
        inspect.text
    );
    assert_eq!(inspect.lines.len(), 5, "one styled line per raw line");
}

#[test]
fn the_overlay_swallows_the_viewers_keys_while_it_is_open() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+one", "dev", &lines);
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('g'));
    let cursor = viewer(&app).cursor;
    press(&mut app, KeyCode::Char('J'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('G'));
    assert_eq!(
        viewer(&app).cursor,
        cursor,
        "the line underneath does not move"
    );
    press(&mut app, KeyCode::Esc);
    assert!(app.inspect.is_none());
    assert!(
        app.log_view().is_some(),
        "esc closes the overlay, not the viewer"
    );
}

#[test]
fn leaving_the_viewer_closes_the_overlay_with_it() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["a line"]);
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('J'));
    assert!(app.inspect.is_some());
    app.close_log_viewer();
    assert!(app.inspect.is_none());
}

// ---- error jumps -----------------------------------------------------

#[test]
fn e_lands_on_the_first_line_of_each_error_block_and_capital_e_goes_back() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "starting up", // 0
            "{",           // 1  block A, error
            "  \"level\": \"error\",",
            "  \"msg\": \"a\"",
            "}",          // 4
            "still fine", // 5
            "{",          // 6  block B, error
            "  \"level\": \"error\",",
            "  \"msg\": \"b\"",
            "}",    // 9
            "done", // 10
        ],
    );
    open_viewer(&mut app, 80, 24);
    press(&mut app, KeyCode::Char('g'));

    press(&mut app, KeyCode::Char('e'));
    assert_eq!(viewer(&app).cursor, 1, "the first line of the first block");
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        viewer(&app).cursor,
        6,
        "not every line of it — the next block"
    );
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(viewer(&app).cursor, 1, "and it wraps");
    press(&mut app, KeyCode::Char('E'));
    assert_eq!(viewer(&app).cursor, 6, "backwards too");
}

#[test]
fn two_adjacent_error_blocks_are_two_jump_targets() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "{",
            "  \"level\": \"error\",",
            "}",
            "{",
            "  \"level\": \"error\",",
            "}",
        ],
    );
    open_viewer(&mut app, 80, 24);
    assert_eq!(
        error_ranks(viewer(&app)),
        vec![0, 3],
        "back-to-back blocks must not merge into one run"
    );
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        viewer(&app).cursor,
        3,
        "the cursor was already on the first"
    );
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(viewer(&app).cursor, 0, "and it wraps back to it");
}

#[test]
fn a_run_of_standalone_error_lines_is_one_target() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "ok",
            "ERROR one",
            "ERROR two",
            "ERROR three",
            "ok again",
            "ERROR four",
        ],
    );
    open_viewer(&mut app, 80, 24);
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(viewer(&app).cursor, 1);
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(viewer(&app).cursor, 5, "the run counts once");
}

#[test]
fn an_error_jump_with_no_errors_moves_nothing() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["all", "quite", "fine"]);
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(viewer(&app).cursor, 0);
    assert!(!viewer(&app).follow);
}

#[test]
fn an_error_jump_respects_the_level_filter() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["info", "WARN slow", "info", "ERROR boom"],
    );
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('f')); // warn+, so two lines are shown
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        viewer(&app).cursor,
        1,
        "the error is the second of the two visible lines"
    );
}

// ---- yank ------------------------------------------------------------

#[test]
fn y_copies_exactly_the_cursor_line_and_says_so() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["first", "second", "third"]);
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.clipboard.as_deref(), Some("second"));
    let (message, is_error) = app.active_status().expect("a confirmation");
    assert!(message.contains("copied line"), "{message}");
    assert!(!is_error);
}

#[test]
fn y_inside_a_block_copies_the_one_line_not_the_block() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &json_block_log());
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(
        app.clipboard.as_deref(),
        Some("  \"level\": \"error\","),
        "y is one line; J is the block"
    );
}

#[test]
fn y_while_following_copies_the_newest_line() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["older", "newest"]);
    open_viewer(&mut app, 80, 12);
    assert!(viewer(&app).follow);
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.clipboard.as_deref(), Some("newest"));
}

#[test]
fn y_on_an_empty_log_copies_nothing_and_says_nothing() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log::<&str>(&app, "feat+one", "dev", &[]);
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('y'));
    assert!(app.clipboard.is_none());
    assert!(app.active_status().is_none());
}

#[test]
fn capital_y_copies_the_url_on_the_line_or_says_there_is_none() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["no url here", "ready on http://localhost:17342/ in 1.2s"],
    );
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('Y'));
    assert_eq!(app.clipboard.as_deref(), Some("http://localhost:17342/"));
    assert!(app.active_status().unwrap().0.contains("copied http"));

    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('Y'));
    assert_eq!(
        app.clipboard.as_deref(),
        Some("http://localhost:17342/"),
        "the clipboard is left as it was"
    );
    assert!(app.active_status().unwrap().0.contains("no URL"));
}

#[test]
fn first_url_stops_at_whitespace_and_closing_delimiters() {
    assert_eq!(
        first_url("ready on http://localhost:17342 now"),
        Some("http://localhost:17342".to_string())
    );
    assert_eq!(
        first_url("see <https://example.com/a>"),
        Some("https://example.com/a".to_string())
    );
    assert_eq!(
        first_url("both http://a.test and https://b.test"),
        Some("http://a.test".to_string()),
        "the first one"
    );
    assert_eq!(first_url("nothing here"), None);
}

#[test]
fn y_in_the_overlay_copies_the_whole_block_and_counts_its_lines() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &json_block_log());
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('J'));
    press(&mut app, KeyCode::Char('y'));
    let copied = app.clipboard.as_deref().expect("the block was copied");
    assert!(copied.contains("\"msg\": \"boom\""), "{copied}");
    assert!(copied.lines().count() > 1);
    let (message, _) = app.active_status().expect("a confirmation");
    assert!(message.contains("lines"), "{message}");
}

#[test]
fn a_single_line_confirmation_is_not_plural() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["a plain line"]);
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('J'));
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.active_status().unwrap().0, "copied 1 line");
}

#[test]
fn the_yank_confirmation_expires() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["a line"]);
    open_viewer(&mut app, 80, 12);
    press(&mut app, KeyCode::Char('y'));
    assert!(app.active_status().is_some());
    // Rather than sleeping for the whole window, age the status.
    app.status.as_mut().unwrap().at = Instant::now() - STATUS_TTL;
    assert!(
        app.active_status().is_none(),
        "a confirmation is a confirmation, not a fixture"
    );
}

// ---- eviction --------------------------------------------------------

/// Replaces the open viewer's tail with one that holds `capacity`
/// lines, so a test can make the ring buffer evict without writing ten
/// thousand lines.
fn shrink_viewer_tail(app: &mut App, capacity: usize) {
    let view = app.log_view_mut().expect("the viewer is open");
    let path = view.tail.path().expect("one source").to_path_buf();
    view.tail = LogTail::new(path, capacity).into();
    view.tail.poll().ok();
}

#[test]
fn scrolling_tracks_only_the_evictions_the_filter_was_showing() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["ERROR one", "info a", "info b", "ERROR two"],
    );
    open_viewer(&mut app, 80, 12);
    shrink_viewer_tail(&mut app, 4);
    press(&mut app, KeyCode::Char('f'));
    press(&mut app, KeyCode::Char('f')); // errors only: two lines visible
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(viewer(&app).cursor, 1, "on the second of the two errors");

    // Three more lines, of which only one is an error, push the first
    // three out of a four-line buffer.
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "ERROR one",
            "info a",
            "info b",
            "ERROR two",
            "info c",
            "info d",
            "ERROR three",
        ],
    );
    app.handle_event(AppEvent::Tick);
    assert_eq!(viewer(&app).tail.lines().len(), 4);
    assert_eq!(
        viewer(&app).cursor,
        0,
        "one visible line was evicted, so the cursor moved by one — \
         not by the three lines that actually fell out"
    );
    assert_eq!(viewer(&app).visible_len(), 2);
}

// Collapsed to the matches (`&`), the visible list is the match list: an
// evicted line that did not match was never on screen, and moving the
// cursor for it put the reader on a different match than the one they
// were reading. Nor is a new line that does not match "new below".
#[test]
fn a_collapsed_search_tracks_only_the_evictions_that_matched() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let first = ["miss a", "miss b", "hit one", "hit two", "hit three"];
    write_log(&app, "feat+one", "dev", &first);
    open_viewer(&mut app, 80, 12);
    shrink_viewer_tail(&mut app, 5);
    press(&mut app, KeyCode::Char('/'));
    for c in "hit".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('&'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(viewer(&app).cursor, 2, "on hit three");

    let mut more = first.to_vec();
    more.extend(["miss c", "miss d"]);
    write_log(&app, "feat+one", "dev", &more);
    app.handle_event(AppEvent::Tick);
    let view = viewer(&app);
    assert_eq!(view.visible_len(), 3, "every match is still there");
    assert_eq!(view.cursor, 2, "still on hit three");
    assert_eq!(view.new_below, 0, "neither new line is shown");
}

#[test]
fn motions_on_a_log_that_evicts_while_it_is_open_stay_in_range() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let first: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+one", "dev", &first);
    open_viewer(&mut app, 80, 12);
    shrink_viewer_tail(&mut app, 4);
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));

    for round in 0..5 {
        let lines: Vec<String> = (0..10 + round * 3).map(|i| format!("line {i}")).collect();
        write_log(&app, "feat+one", "dev", &lines);
        app.handle_event(AppEvent::Tick);
        paint(&mut app, 80, 12);
        let view = viewer(&app);
        assert!(
            view.cursor < view.visible_len().max(1),
            "round {round}: cursor {} of {}",
            view.cursor,
            view.visible_len()
        );
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('k'));
    }
    // And a truncation, which resets the tail to the start of the file.
    write_log(&app, "feat+one", "dev", &["one short line"]);
    app.handle_event(AppEvent::Tick);
    paint(&mut app, 80, 12);
    assert!(viewer(&app).cursor < viewer(&app).visible_len().max(1));
}

// ---- the subprocess and state rules ----------------------------------

/// Every key the viewer binds, in one list, so the guards below can
/// drive the lot.
fn every_viewer_key() -> Vec<KeyEvent> {
    let mut keys = Vec::new();
    for c in [
        'j', 'k', 'g', 'G', 'w', 'f', 'E', 'e', 'y', 'Y', 'J', 'n', 'N', '&', '/', '3', '0', 'z',
    ] {
        keys.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    for c in ['d', 'u', 'n', 'p'] {
        keys.push(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL));
    }
    for code in [
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Backspace,
        KeyCode::Esc,
    ] {
        keys.push(KeyEvent::new(code, KeyModifiers::NONE));
    }
    keys
}

// The origin tool's hard-won rule: a key handler that reads state takes
// the flock and forks a socket scan, and the frame freezes until it is
// done. The viewer reads its own log file on the tick and nothing else.
#[test]
fn no_key_the_viewer_binds_reads_or_writes_state() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &a_busy_log());
    open_viewer(&mut app, 80, 20);
    let before = app.state.clone();
    for key in every_viewer_key() {
        app.handle_key(key);
        paint(&mut app, 80, 20);
    }
    assert_eq!(app.state, before, "the viewer never touches state");
    assert!(
        !app.paths.state_file().exists(),
        "and never writes one either"
    );
    assert!(
        !app.paths.lock_file().exists(),
        "so it never takes the lock a mutation holds"
    );
}

/// A log with something for every key to act on.
fn a_busy_log() -> Vec<String> {
    let mut lines = json_block_log();
    lines.push("ERROR later on".to_string());
    lines.push("ready on http://localhost:17342/".to_string());
    lines
}

// The other half of the rule: a child that inherits the terminal paints
// over the alternate screen. The commands the TUI is allowed to run are
// the browser opener and `pbcopy`, both on a worker thread with every
// stream redirected, the hand-offs, and `pando check`, detached into a
// session of its own with its streams on a log.
#[test]
fn the_tui_spawns_nothing_that_could_paint_over_the_screen() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // Every file under `src/tui/`, found rather than listed, so a file
    // added later is held to the rule without anyone remembering to add
    // it here. Only these may spawn at all.
    let allowed_spawns = |file: &str| match file {
        "src/tui/app/operations.rs" => 2,
        // `c` and `e`: one hand-off (tmux, a GUI editor) on a worker
        // thread, and one suspend.
        "src/tui/handoff.rs" => 2,
        // `v` on the setup screen: `pando check`, detached.
        "src/tui/app/setup.rs" => 1,
        _ => 0,
    };
    // The suspend is the one child the UI thread waits on, on purpose:
    // the TUI has left the screen and stopped reading keys first, and
    // there is nothing else for it to do until the shell exits. The
    // detached check is reaped by a thread of its own.
    let allowed_waits = |file: &str| match file {
        "src/tui/handoff.rs" => 1,
        "src/tui/app/setup.rs" => 1,
        _ => 0,
    };
    let mut files: Vec<String> = Vec::new();
    let mut dirs = vec![root.join("src/tui")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path.strip_prefix(root).unwrap();
                files.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    files.sort();
    assert!(
        files.iter().any(|f| f == "src/tui/app/operations.rs"),
        "the file allowed to spawn has moved: {files:?}"
    );
    // Built rather than written, so this test does not match itself.
    // `wait` as well as `status`: waiting on a child pando spawned
    // blocks whatever thread asks, and the rule is about the thread,
    // not about which call is used to wait.
    let blocking = [format!(".{}()", "status"), format!(".{}()", "wait")];
    let spawn = format!("Command::{}", "new");
    for file in &files {
        let allowed = allowed_spawns(file);
        let whole = std::fs::read_to_string(root.join(file)).unwrap();
        // Comments explain the rule; code has to keep it.
        let source: String = whole
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let waits: usize = blocking
            .iter()
            .map(|call| source.matches(call.as_str()).count())
            .sum();
        assert_eq!(
            waits,
            allowed_waits(file),
            "{file}: a blocking wait on a child belongs on a worker thread \
             and never in a key handler"
        );
        assert_eq!(
            source.matches(&spawn).count(),
            allowed,
            "{file}: every subprocess the TUI runs has to be one of the \
             documented ones, off the UI thread with its streams redirected"
        );
    }
}

#[test]
fn the_viewer_keeps_ten_thousand_lines() {
    assert_eq!(LOG_VIEWER_CAPACITY, 10_000);
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["one"]);
    open_viewer(&mut app, 80, 12);
    assert_eq!(viewer(&app).tail.capacity(), LOG_VIEWER_CAPACITY);
}

// ---- keys and help ---------------------------------------------------

/// Every key a person might press bare: printable ASCII, and the keys
/// with names.
fn candidate_keys() -> Vec<KeyCode> {
    let mut keys: Vec<KeyCode> = (' '..='~').map(KeyCode::Char).collect();
    keys.extend([
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Esc,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Insert,
        KeyCode::F(1),
    ]);
    keys
}

/// Three worktrees, the cursor on the middle one, which runs two
/// processes, has a log to scroll and a tail scrolled back one line — so
/// every list key has something to act on.
fn a_list_every_key_can_act_on() -> (tempfile::TempDir, App) {
    let (dir, mut app) = app_with_logs(&["feat+one", "feat+two", "feat+three"]);
    with_process(&mut app, "feat+one", running_phase());
    let lines: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+two", "dev", &lines);
    tail_the_log(&mut app, "feat+two", "dev");
    with_second_process(&mut app, "feat+two", "api", running_phase());
    app.refilter();
    app.select_index(1);
    assert_eq!(app.selected_worktree().unwrap().name, "feat+two");
    app.poll_logs();
    app.tail_scroll = 1;
    (dir, app)
}

/// What a key press can visibly change on the list.
fn list_fingerprint(app: &App) -> String {
    format!(
        "{:?}|{:?}|{:?}|{:?}|{}|{}|{:?}|{:?}|{:?}|{}|{}|{}|{:?}|{}",
        app.modal.as_ref().map(std::mem::discriminant),
        app.mode,
        app.status.as_ref().map(|s| s.message.clone()),
        app.pending.as_ref().map(|p| p.kind),
        matches!(app.view, View::Log(_)),
        app.should_quit,
        app.list_state.selected(),
        app.clipboard,
        app.opened,
        app.tail_index,
        app.tail_scroll,
        app.filter,
        app.launch,
        app.leader,
    )
}

// Help is the list of keys a developer learns pando from. A key the
// handler answers that help leaves out is a feature nobody finds; a key
// help lists that nothing answers is a lie. Every bare key is pressed on a
// list with something for each of them to do, and the ones that did
// something have to be exactly the ones help lists.
#[test]
fn help_lists_exactly_the_keys_the_list_answers() {
    let documented: Vec<KeyCode> = LIST_KEYS
        .iter()
        .flat_map(|k| k.codes.iter().copied())
        .collect();
    let mut answered = Vec::new();
    for code in candidate_keys() {
        let (_dir, mut app) = a_list_every_key_can_act_on();
        let before = list_fingerprint(&app);
        press(&mut app, code);
        if list_fingerprint(&app) != before {
            answered.push(code);
        }
    }
    for code in &answered {
        assert!(
            documented.contains(code),
            "{code:?} does something on the list, and help does not say so"
        );
    }
    for code in &documented {
        assert!(
            answered.contains(code),
            "help lists {code:?}, and pressing it on the list does nothing"
        );
    }
}

/// A viewer with a live search on two lines that carry URLs, errors either
/// side of the cursor, and three sources.
fn a_viewer_every_key_can_act_on() -> (tempfile::TempDir, App) {
    let (dir, mut app) = app_with_logs(&["feat+one"]);
    let mut lines: Vec<String> = vec!["start".into(), "ERROR first".into()];
    lines.extend((0..4).map(|i| format!("line {i}")));
    lines.push("see http://localhost:1/a".into());
    lines.push("line".into());
    lines.push("see http://localhost:2/b".into());
    lines.push("ERROR second".into());
    lines.extend((0..10).map(|i| format!("more {i}")));
    write_log(&app, "feat+one", "dev", &lines);
    write_log(&app, "feat+one", "install", &["installed"]);
    write_log(&app, "feat+one", "extra", &["extra"]);
    open_viewer(&mut app, 80, 12);
    search_for(&mut app, "http");
    paint(&mut app, 80, 12);
    assert_eq!(viewer(&app).cursor, 6, "on the first match");
    (dir, app)
}

fn viewer_fingerprint(app: &App) -> String {
    let view = app.log_view();
    format!(
        "{:?}|{:?}|{}|{}|{:?}|{:?}|{:?}",
        view.map(|v| (
            v.source.clone(),
            v.cursor,
            v.follow,
            v.wrap,
            v.log_filter,
            v.search_mode,
            v.search.query.clone(),
            v.search.cursor,
            v.filter_to_matches,
        )),
        app.modal.as_ref().map(std::mem::discriminant),
        app.inspect.is_some(),
        app.should_quit,
        app.clipboard,
        app.status.as_ref().map(|s| s.message.clone()),
        app.pending.as_ref().map(|p| p.kind),
    )
}

// The same contract for the log viewer. Digits are left to their own
// test: which of them does something depends on which tab is showing.
#[test]
fn help_lists_exactly_the_keys_the_viewer_answers() {
    let documented: Vec<KeyCode> = LOG_KEYS
        .iter()
        .flat_map(|k| k.codes.iter().copied())
        .filter(|code| !matches!(code, KeyCode::Char('1'..='9')))
        .collect();
    let mut answered = Vec::new();
    for code in candidate_keys() {
        if matches!(code, KeyCode::Char('1'..='9')) {
            continue;
        }
        let (_dir, mut app) = a_viewer_every_key_can_act_on();
        let before = viewer_fingerprint(&app);
        press(&mut app, code);
        if viewer_fingerprint(&app) != before {
            answered.push(code);
        }
    }
    for code in &answered {
        assert!(
            documented.contains(code),
            "{code:?} does something in the viewer, and help does not say so"
        );
    }
    for code in &documented {
        assert!(
            answered.contains(code),
            "help lists {code:?}, and pressing it in the viewer does nothing"
        );
    }
}

// ---- enter, and the modes -------------------------------------------

/// The row the mode chooser has under its cursor, when it is open.
fn chooser(app: &App) -> Option<ServiceMode> {
    match &app.modal {
        Some(Modal::Mode { selected, .. }) => ServiceMode::ALL.get(*selected).copied(),
        _ => None,
    }
}

// Decision 6: ⏎ is the mode chooser on every worktree. A stopped one has
// the mode it last ran in under the cursor — shared for one never
// started — so ⏎ ⏎ is still one quick start in it.
#[test]
fn enter_opens_the_mode_chooser_on_what_a_stopped_worktree_last_ran_in() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        chooser(&app),
        Some(ServiceMode::Shared),
        "never started: shared"
    );
    assert!(app.pending.is_none(), "nothing starts on the first ⏎");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Start)
    );

    for last in ServiceMode::ALL {
        let mut app = test_app(&["feat+one"]);
        app.state
            .worktrees
            .entry("feat+one".into())
            .or_insert_with(|| crate::state::WorktreeRecord::new("/abs/feat+one", true))
            .mode = Some(last);
        press(&mut app, KeyCode::Enter);
        assert_eq!(chooser(&app), Some(last), "last used: {last:?}");
    }

    // Moved and left: nothing happens.
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(chooser(&app), Some(ServiceMode::Namespaced));
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none() && app.pending.is_none());
}

// On a running worktree the chooser is the asking: another mode restarts
// it there with no second dialog, and the mode it runs in changes nothing
// and says so. ⏎ never opens the logs; `l` does.
#[test]
fn enter_on_a_running_worktree_switches_it_or_says_it_already_runs_so() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Enter);
    assert_eq!(chooser(&app), Some(ServiceMode::Shared));
    assert!(app.log_view().is_none(), "the logs are l's");
    press(&mut app, KeyCode::Enter);
    assert!(app.pending.is_none() && app.modal.is_none());
    let said = app.status.as_ref().map(|s| s.message.clone());
    assert!(
        said.as_deref()
            .is_some_and(|s| s.contains("already runs shared") && s.contains("r restarts")),
        "{said:?}"
    );

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(chooser(&app), Some(ServiceMode::Isolated));
    press(&mut app, KeyCode::Enter);
    assert!(
        !matches!(app.modal, Some(Modal::SwitchMode { .. })),
        "the chooser was the asking"
    );
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Restart)
    );

    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Char('l'));
    assert!(app.log_view().is_some(), "l opens the logs");
}

// The TUI's way back from `i`, as `start --shared` is the CLI's.
#[test]
fn capital_s_starts_the_selected_worktree_shared() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('S'));
    let pending = app.pending.as_ref().expect("the key started something");
    assert_eq!(pending.kind, PendingKind::Start);
}

// A start of a worktree that is already up leaves its processes on the
// services they were started against, so a mode key restarts instead —
// on the second press, when it is the mode it already runs in.
#[test]
fn a_mode_key_on_a_running_worktree_restarts_it_in_that_mode() {
    for (key, mode) in [('i', ServiceMode::Isolated), ('S', ServiceMode::Shared)] {
        let mut app = test_app(&["feat+one"]);
        with_process(&mut app, "feat+one", running_phase());
        app.state.worktrees.get_mut("feat+one").unwrap().mode = Some(mode);
        press(&mut app, KeyCode::Char(key));
        assert!(app.pending.is_none(), "{key}: the first press only asks");
        assert!(app.modal.is_none(), "{key}: no dialog for the same mode");
        press(&mut app, KeyCode::Char(key));
        assert_eq!(
            app.pending.as_ref().map(|p| p.kind),
            Some(PendingKind::Restart),
            "{key}"
        );
    }
}

// The accident this exists for: `i` on a worktree running shared swapped
// every service under it on one keystroke.
#[test]
fn a_mode_key_that_switches_a_running_worktree_asks_in_a_dialog() {
    for (key, from, target) in [
        ('i', ServiceMode::Shared, ServiceMode::Isolated),
        ('i', ServiceMode::Namespaced, ServiceMode::Isolated),
        ('S', ServiceMode::Isolated, ServiceMode::Shared),
        ('S', ServiceMode::Namespaced, ServiceMode::Shared),
    ] {
        let mut app = test_app(&["feat+one"]);
        with_process(&mut app, "feat+one", running_phase());
        app.state.worktrees.get_mut("feat+one").unwrap().mode = Some(from);
        press(&mut app, KeyCode::Char(key));
        assert!(
            matches!(
                &app.modal,
                Some(Modal::SwitchMode { name, to })
                    if name == "feat+one" && *to == target
            ),
            "{key}: {:?}",
            app.modal
        );
        assert!(app.pending.is_none(), "{key}: nothing restarts yet");
        press(&mut app, KeyCode::Char(key));
        assert!(app.pending.is_none(), "{key}: only y or enter confirms");
        assert!(app.modal.is_some(), "{key}: and the dialog stays");
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none() && app.pending.is_none(), "{key}");

        press(&mut app, KeyCode::Char(key));
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(
            app.pending.as_ref().map(|p| p.kind),
            Some(PendingKind::Restart),
            "{key}"
        );
    }
}

/// A discovery listing `names`, with the app's own state.
fn listing(app: &App, names: &[&str]) -> Snapshot {
    Snapshot {
        main: wt("acme-shop"),
        worktrees: names.iter().map(|n| wt(n)).collect(),
        created_by_pando: BTreeMap::new(),
        state: app.state.clone(),
        warning: None,
        notices: Vec::new(),
        default_base: None,
    }
}

// A discovery moves the cursor while a dialog is open — to the worktree
// `n` just made, or off one that went away. The chooser and the switch
// dialog started or restarted whichever row it had landed on.
#[test]
fn the_mode_chooser_and_the_switch_dialog_act_on_the_worktree_they_name() {
    let mut app = test_app(&["feat+a", "feat+b"]);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    app.select_on_arrival = Some("feat+new".to_string());
    app.apply_snapshot(listing(&app, &["feat+a", "feat+b", "feat+new"]));
    assert_eq!(app.selected_worktree().unwrap().name, "feat+new");
    press(&mut app, KeyCode::Enter);
    let pending = app.pending.as_ref().expect("the chooser started something");
    assert_eq!(
        (pending.name.as_str(), pending.kind),
        ("feat+b", PendingKind::Start)
    );

    let mut app = test_app(&["feat+a", "feat+b"]);
    with_process(&mut app, "feat+b", running_phase());
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('i'));
    assert!(matches!(app.modal, Some(Modal::SwitchMode { .. })));
    app.select_on_arrival = Some("feat+new".to_string());
    app.apply_snapshot(listing(&app, &["feat+a", "feat+b", "feat+new"]));
    assert_eq!(app.selected_worktree().unwrap().name, "feat+new");
    press(&mut app, KeyCode::Char('y'));
    let pending = app
        .pending
        .as_ref()
        .expect("the dialog restarted something");
    assert_eq!(
        (pending.name.as_str(), pending.kind),
        ("feat+b", PendingKind::Restart)
    );
}

// And a worktree removed from under either dialog is not replaced by its
// neighbour: there is nothing left to start.
#[test]
fn a_mode_dialog_whose_worktree_went_away_starts_nothing() {
    let mut app = test_app(&["feat+a", "feat+b"]);
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    app.apply_snapshot(listing(&app, &["feat+a"]));
    press(&mut app, KeyCode::Enter);
    assert!(app.pending.is_none());
    assert_eq!(app.active_status(), Some(("feat+b is already gone", false)));

    let mut app = test_app(&["feat+a", "feat+b"]);
    with_process(&mut app, "feat+b", running_phase());
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('i'));
    app.state.worktrees.remove("feat+b");
    app.apply_snapshot(listing(&app, &["feat+a"]));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.pending.is_none());
}

#[test]
fn c_copies_the_local_url_even_when_shared_and_capital_c_the_public_one() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.clipboard, None, "nothing runs, so there is no URL");
    assert!(app.active_status().unwrap().1, "and it says so as an error");

    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Char('C'));
    assert_eq!(app.clipboard, None, "not shared, so no public URL");
    let (message, error) = app.active_status().unwrap();
    assert!(error && message.contains("t shares it"), "{message}");

    press(&mut app, KeyCode::Char('c'));
    assert_eq!(app.clipboard.as_deref(), Some("http://localhost:17342"));

    with_share(&mut app, "feat+one", None);
    press(&mut app, KeyCode::Char('c'));
    assert_eq!(
        app.clipboard.as_deref(),
        Some("http://localhost:17342"),
        "c never stands in for the public one"
    );
    press(&mut app, KeyCode::Char('C'));
    assert_eq!(
        app.clipboard.as_deref(),
        Some("https://fake-host.trycloudflare.com")
    );
}

// ---- keys pressed twice ----------------------------------------------

#[test]
fn a_first_press_says_what_the_second_would_do() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Char('r'));
    let (message, error) = app.active_status().expect("a prompt");
    assert!(!error);
    assert_eq!(
        message,
        "restart feat/one? r again to confirm · esc cancels"
    );
    assert!(
        app.messages.iter().all(|m| !m.message.contains("again")),
        "a prompt is not kept in the history"
    );
}

#[test]
fn esc_takes_a_first_press_back_without_quitting() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Esc);
    assert!(!app.should_quit, "esc cancels rather than quits");
    assert_eq!(app.active_status(), Some(("cancelled", false)));
    press(&mut app, KeyCode::Char('x'));
    assert!(app.pending.is_none(), "after esc, x is a first press again");
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Esc);
    assert!(app.should_quit, "with nothing armed esc quits as before");
}

#[test]
fn any_other_key_between_the_presses_disarms() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    for name in ["feat+one", "feat+two"] {
        with_process(&mut app, name, running_phase());
    }
    // Another key between them.
    press(&mut app, KeyCode::Char('r'));
    press(&mut app, KeyCode::Char('y'));
    press(&mut app, KeyCode::Char('r'));
    assert!(app.pending.is_none(), "r y r is a first press again");
    // A different interrupting key is its own first press.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.pending.is_none(), "r x does not stop it");
    // Moving to another worktree: the second press is not on the same one.
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('x'));
    assert!(app.pending.is_none(), "x j x stops neither");
}

#[test]
fn a_first_press_that_has_waited_too_long_is_a_first_press_again() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Char('r'));
    app.armed.as_mut().unwrap().at -= super::ARM_TTL;
    press(&mut app, KeyCode::Char('r'));
    assert!(app.pending.is_none(), "too late to be the second press");
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Restart)
    );
}

// Only something up can be interrupted: a worktree whose every process
// has died stops and restarts on one press.
#[test]
fn a_worktree_with_nothing_up_is_stopped_on_one_press() {
    let mut app = test_app(&["feat+one"]);
    with_process(
        &mut app,
        "feat+one",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited".into(),
        },
    );
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Stop)
    );
}

// ---- the list's order ------------------------------------------------

#[test]
fn a_worktree_that_starts_moves_to_the_top_and_the_cursor_with_it() {
    let mut app = test_app(&["feat+one", "feat+two", "feat+three"]);
    app.select_index(2);
    assert_eq!(app.selected_worktree().unwrap().name, "feat+three");
    let mut state = app.state.clone();
    let mut probe = test_app(&["feat+three"]);
    with_process(&mut probe, "feat+three", running_phase());
    state.worktrees.extend(probe.state.worktrees);
    app.handle_event(refreshed(state));

    let order: Vec<&str> = app
        .filtered_indices
        .iter()
        .map(|&i| app.worktrees[i].name.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["feat+three", "feat+one", "feat+two"],
        "what runs first, the rest in the order `pando ls` prints"
    );
    assert_eq!(
        app.selected_worktree().unwrap().name,
        "feat+three",
        "the cursor stays on its worktree"
    );
    assert_eq!(app.list_state.selected(), Some(0), "on its new row");
}

// ---- messages --------------------------------------------------------

#[test]
fn an_error_is_marked_as_one_and_stays_longer_than_a_confirmation() {
    let mut app = test_app(&[]);
    app.set_success("started feat+one");
    let success = app.flash().unwrap();
    assert_eq!(success.kind, StatusKind::Success);
    let success_ttl = success.ttl;

    app.set_error("could not start feat+one: docker is not running");
    let error = app.flash().unwrap();
    assert!(error.is_error());
    assert!(
        error.ttl > success_ttl,
        "an error has to be read to its end"
    );

    // Past a confirmation's life, an error is still up.
    app.status.as_mut().unwrap().at = Instant::now() - success_ttl - Duration::from_secs(1);
    assert!(app.flash().is_some());
}

#[test]
fn m_shows_everything_said_even_after_it_has_expired() {
    let mut app = test_app(&["feat+one"]);
    app.set_error("could not start feat+one: a long reason that ends in what to do");
    app.set_success("copied /trees/feat+one");
    app.status = None;
    press(&mut app, KeyCode::Char('m'));
    assert!(matches!(app.modal, Some(Modal::Messages)));
    let said: Vec<&str> = app.messages.iter().map(|s| s.message.as_str()).collect();
    assert_eq!(
        said,
        vec![
            "could not start feat+one: a long reason that ends in what to do",
            "copied /trees/feat+one"
        ]
    );
    press(&mut app, KeyCode::Char('x'));
    assert!(app.modal.is_none(), "any other key closes it");
}

#[test]
fn a_spinner_is_not_kept_in_the_history() {
    let mut app = test_app(&["feat+one"]);
    app.set_progress("⠋ starting feat/one…");
    assert!(app.flash().is_some());
    assert!(app.messages.is_empty());
}

// A stop-all has no worktree's name, so the refusal named nothing; a
// create's name is the directory, not the branch that was typed.
#[test]
fn a_second_action_is_refused_with_what_the_first_is_called() {
    // Nothing is polled, so the first action is still in flight however
    // soon its worker gives up on paths that do not exist.
    let mut app = test_app(&["feat+one"]);
    app.stop_everything(Vec::new());
    press(&mut app, KeyCode::Char('s'));
    let (message, _) = app.active_status().unwrap();
    assert_eq!(message, "already busy stopping everything");

    let mut app = test_app(&["feat+one"]);
    assert!(app.spawn_create("feat/pay".into(), None));
    press(&mut app, KeyCode::Char('s'));
    let (message, _) = app.active_status().unwrap();
    assert_eq!(message, "already busy creating feat/pay");
}

// A start blocked on a question is not starting anything: the status line
// and the row say it is waiting, rather than counting seconds.
#[test]
fn a_start_waiting_on_a_question_says_so() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('s'));
    let _rx = open_question(&mut app, a_question());
    app.poll_pending();
    let (message, _) = app.active_status().unwrap();
    assert!(message.contains("waiting for your answer"), "{message}");
    assert!(app.awaiting_answer());
}

// The spinner rewrote the header on every tick, so what was said while an
// action ran — a crash, the refusal of a second action — was gone within a
// quarter of a second. It waits until that has had its time.
#[test]
fn a_message_said_while_an_action_runs_is_not_spun_over() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    let (hold, held) = mpsc::channel::<()>();
    app.spawn_pending("feat+one".into(), PendingKind::Start, move || {
        let _ = held.recv();
        Err("let go".to_string())
    });
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('s'));
    for _ in 0..3 {
        app.poll_pending();
    }
    assert_eq!(
        app.active_status(),
        Some(("already busy starting feat/one", true))
    );

    app.status.as_mut().unwrap().at = Instant::now() - ERROR_TTL - Duration::from_secs(1);
    app.poll_pending();
    let status = app.flash().expect("the spinner is back");
    assert_eq!(status.kind, StatusKind::Progress, "{}", status.message);
    drop(hold);
}

// ---- new -------------------------------------------------------------

// `n` settles what `pando new` settles before it creates anything. It did
// not: the TUI called `new` with the raw config, so a worktree made there
// had nothing installed and its first start failed.
#[test]
fn n_resolves_what_new_needs_before_creating() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    std::fs::write(
        root.join("package.json"),
        "{\n  \"name\": \"x\",\n  \"scripts\": { \"dev\": \"next dev\" }\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        crate::project::ProjectRef::from_root(&root).unwrap(),
    );
    let mut app = App::new_for_test(paths, Config::default(), Vec::new());
    assert!(app.config.project.install.is_none(), "nothing is known yet");

    press(&mut app, KeyCode::Char('n'));
    type_str(&mut app, "feat/new");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Create)
    );
    let rx = app.event_rx.take().expect("the app owns its receiver");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut applied = false;
    while Instant::now() < deadline && !applied {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => {
                let is_config = matches!(event, AppEvent::ConfigResolved(_));
                app.handle_event(event);
                applied = is_config;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    app.event_rx = Some(rx);
    assert!(applied, "the create worker never resolved the new slots");
    assert_eq!(
        app.config.project.install.as_deref(),
        Some("pnpm install --frozen-lockfile"),
        "the install step `pando new` would have run"
    );
}

// ---- leaving pando: a shell, an editor --------------------------------

fn env(tmux: bool, shell: Option<&str>, visual: Option<&str>, editor: Option<&str>) -> LaunchEnv {
    LaunchEnv {
        tmux,
        shell: shell.map(str::to_string),
        visual: visual.map(str::to_string),
        editor: editor.map(str::to_string),
        browser: None,
        host: crate::platform::Host::default(),
    }
}

#[test]
fn bang_outside_tmux_suspends_for_a_shell_in_the_worktree() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(false, Some("/bin/zsh"), None, None);
    press(&mut app, KeyCode::Char('!'));
    let request = app.launch.clone().expect("! asks for a shell");
    assert_eq!(
        request.launch,
        Launch::Suspend {
            program: "/bin/zsh".into(),
            args: Vec::new(),
            cwd: PathBuf::from("/trees/feat+one"),
        }
    );
    assert_eq!(request.done, "back from the shell in feat/one");
    // The message comes on the way back, not before the screen goes.
    assert!(app.status.is_none());
    assert!(!app.should_quit, "the TUI keeps running");
}

#[test]
fn bang_without_a_shell_falls_back_to_sh() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(false, None, None, None);
    press(&mut app, KeyCode::Char('!'));
    assert!(matches!(
        app.launch.map(|r| r.launch),
        Some(Launch::Suspend { program, .. }) if program == "/bin/sh"
    ));
}

#[test]
fn bang_inside_tmux_opens_a_window_named_for_the_branch() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(true, Some("/bin/zsh"), None, None);
    press(&mut app, KeyCode::Char('!'));
    assert_eq!(
        app.launch.clone().map(|r| r.launch),
        Some(Launch::Tmux {
            args: vec![
                "new-window".into(),
                "-c".into(),
                "/trees/feat+one".into(),
                "-n".into(),
                "feat/one".into(),
            ],
            // Run in the worktree, so that one whose directory has gone
            // is said to be gone rather than opened in $HOME.
            cwd: PathBuf::from("/trees/feat+one"),
        })
    );
    let (message, error) = app.active_status().expect("it says what it did");
    assert!(!error);
    assert!(message.contains("tmux window feat/one"), "{message}");
    // Said once: the key says it, and the event loop that hands the
    // window off to tmux has nothing more to add.
    assert_eq!(
        app.messages
            .iter()
            .filter(|m| m.message.contains("opened a shell"))
            .count(),
        1
    );
    let request = app.launch.clone().expect("a launch waits for the loop");
    assert_eq!(super::launch::said_after(&request), None);
}

// A suspend is the other way round: nothing is said until the shell
// returns, and then once.
#[test]
fn a_suspended_shell_says_so_only_on_the_way_back() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(false, Some("/bin/zsh"), None, None);
    press(&mut app, KeyCode::Char('!'));
    assert!(
        app.messages.is_empty(),
        "nothing yet: the shell has not run"
    );
    let request = app.launch.clone().expect("a launch waits for the loop");
    assert_eq!(
        super::launch::said_after(&request).as_deref(),
        Some("back from the shell in feat/one")
    );
}

#[test]
fn e_with_no_editor_set_says_so_rather_than_guessing() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(false, Some("/bin/zsh"), None, None);
    press(&mut app, KeyCode::Char('e'));
    assert!(app.launch.is_none());
    let (message, error) = app.active_status().expect("an error");
    assert!(error);
    assert!(
        message.contains("$VISUAL") && message.contains("$EDITOR"),
        "{message}"
    );
}

#[test]
fn e_prefers_visual_and_suspends_for_a_terminal_editor() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(false, None, Some("nvim"), Some("code -w"));
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.launch.clone().map(|r| r.launch),
        Some(Launch::Suspend {
            program: "nvim".into(),
            args: vec!["/trees/feat+one".into()],
            cwd: PathBuf::from("/trees/feat+one"),
        })
    );
}

#[test]
fn e_inside_tmux_puts_a_terminal_editor_in_its_own_window() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(true, None, None, Some("/usr/local/bin/hx"));
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.launch.clone().map(|r| r.launch),
        Some(Launch::Tmux {
            args: vec![
                "new-window".into(),
                "-c".into(),
                "/trees/feat+one".into(),
                "-n".into(),
                "feat/one".into(),
                "/usr/local/bin/hx".into(),
                "/trees/feat+one".into(),
            ],
            cwd: PathBuf::from("/trees/feat+one"),
        })
    );
}

// tmux reads `-c` and `-n` as formats. A branch `fix/#Hx` opened its
// shell in $HOME, in a window named for the host, while pando said it
// opened in the worktree; a fork's branch holding `#(…)` had tmux run
// what was in it.
#[test]
fn a_tmux_window_takes_a_hash_in_the_branch_as_it_is() {
    for key in ['!', 'e'] {
        let mut app = test_app(&["pr-12+x#(touch)#H"]);
        app.launch_env = env(true, Some("/bin/zsh"), None, Some("vim"));
        press(&mut app, KeyCode::Char(key));
        let Some(Launch::Tmux { args, .. }) = app.launch.clone().map(|r| r.launch) else {
            panic!("{key} inside tmux opens a window: {:?}", app.launch);
        };
        assert_eq!(
            args[..5],
            [
                "new-window",
                "-c",
                "/trees/pr-12+x##(touch)##H",
                "-n",
                "pr-12/x##(touch)##H"
            ],
            "{key}"
        );
        if key == 'e' {
            // The editor's argument is not a format: tmux passes it on.
            assert_eq!(args[5..], ["vim", "/trees/pr-12+x#(touch)#H"]);
        }
        let (message, _) = app.active_status().expect("it says what it did");
        assert!(
            message.contains("tmux window pr-12/x#(touch)#H"),
            "{message}"
        );
    }
}

#[test]
fn e_hands_a_gui_editor_off_with_its_arguments() {
    for tmux in [false, true] {
        let mut app = test_app(&["feat+one"]);
        app.launch_env = env(tmux, None, None, Some("code -w"));
        press(&mut app, KeyCode::Char('e'));
        assert_eq!(
            app.launch.clone().map(|r| r.launch),
            Some(Launch::Detached {
                program: "code".into(),
                args: vec!["-w".into(), "/trees/feat+one".into()],
                cwd: PathBuf::from("/trees/feat+one"),
            }),
            "tmux: {tmux}"
        );
        let (message, _) = app.active_status().expect("it says what it did");
        assert!(message.contains("opened feat/one in code"), "{message}");
    }
}

// `EDITOR='"/Applications/My Editor.app/…/bin/edit" -w'` was split on
// whitespace into a program called `"/Applications/My`.
#[test]
fn e_reads_a_quoted_editor_path_as_one_program() {
    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(
        false,
        None,
        None,
        Some(r#""/Applications/My Editor.app/bin/code" -w"#),
    );
    press(&mut app, KeyCode::Char('e'));
    assert_eq!(
        app.launch.clone().map(|r| r.launch),
        Some(Launch::Detached {
            program: "/Applications/My Editor.app/bin/code".into(),
            args: vec!["-w".into(), "/trees/feat+one".into()],
            cwd: PathBuf::from("/trees/feat+one"),
        })
    );

    let mut app = test_app(&["feat+one"]);
    app.launch_env = env(false, None, None, Some(r#""/opt/my editor -w"#));
    press(&mut app, KeyCode::Char('e'));
    assert!(app.launch.is_none());
    let (message, error) = app.active_status().expect("an error");
    assert!(error && message.contains("never closes"), "{message}");
}

#[test]
fn terminal_editors_are_told_from_gui_ones() {
    for editor in [
        "vim",
        "nvim",
        "/opt/homebrew/bin/hx",
        "nano",
        "emacs -nw",
        "micro",
    ] {
        assert!(is_terminal_editor(editor), "{editor}");
    }
    for editor in ["code", "code -w", "/usr/local/bin/subl -w", "zed", "cursor"] {
        assert!(!is_terminal_editor(editor), "{editor}");
    }
}

#[test]
fn a_tmux_window_is_named_for_the_whole_branch() {
    // `m` alone could be `feat/m` or `fix/m`; tmux takes the `/`.
    assert_eq!(super::launch::window_name("feat/m"), "feat/m");
    assert_eq!(super::launch::window_name("main"), "main");
    let long = super::launch::window_name("feature/a-very-long-branch-name-that-goes-on");
    assert_eq!(long.chars().count(), 30);
    assert!(long.starts_with("feature/a-very"), "{long}");
    assert!(long.ends_with('…'), "{long}");
}

#[test]
fn a_hand_off_that_failed_says_why() {
    let mut app = test_app(&["feat+one"]);
    app.handle_event(AppEvent::LaunchFailed("tmux failed: no server".into()));
    assert_eq!(app.active_status(), Some(("tmux failed: no server", true)));
}

// ---- restart one process, stop everything -----------------------------

#[test]
fn r_on_a_stopped_worktree_starts_it() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Start)
    );
}

#[test]
fn shift_p_restarts_only_the_process_the_detail_pane_marks() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    // Tab moves the `▸`; `P` follows it.
    press(&mut app, KeyCode::Tab);
    let (_, marked, _) = app.tail_target().expect("a process is marked");
    press(&mut app, KeyCode::Char('P'));
    let (message, _) = app.active_status().expect("a prompt");
    assert!(
        message.starts_with(&format!("restart {marked} of feat/one?")),
        "{message}"
    );
    press(&mut app, KeyCode::Char('P'));
    let pending = app.pending.as_ref().expect("P started something");
    assert_eq!(pending.kind, PendingKind::Restart);
    assert_eq!(pending.name, "feat+one");
    assert_eq!(pending.label, format!("{marked} of feat/one"));
}

#[test]
fn shift_p_on_one_process_restarts_the_worktree_and_on_none_says_so() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    press(&mut app, KeyCode::Char('P'));
    press(&mut app, KeyCode::Char('P'));
    let pending = app.pending.as_ref().expect("P started something");
    assert_eq!(pending.kind, PendingKind::Restart);
    assert_eq!(pending.label, "feat/one");

    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('P'));
    assert!(app.pending.is_none());
    let (message, error) = app.active_status().expect("an error");
    assert!(error && message.contains("running nothing"), "{message}");
}

#[test]
fn x_with_nothing_running_says_so() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('X'));
    assert!(app.modal.is_none());
    assert_eq!(app.active_status(), Some(("nothing is running", false)));
}

#[test]
fn x_asks_first_and_esc_keeps_everything_up() {
    let mut app = test_app(&["feat+one", "feat+two", "feat+three"]);
    with_process(&mut app, "feat+one", running_phase());
    // A failed process beside a running one: something is still up.
    with_process(&mut app, "feat+three", running_phase());
    with_second_process(
        &mut app,
        "feat+three",
        "worker",
        Phase::Failed {
            reason: "exit 1".into(),
            at: Utc::now(),
        },
    );
    press(&mut app, KeyCode::Char('X'));
    match &app.modal {
        Some(Modal::StopAll { names }) => {
            assert_eq!(
                names,
                &vec!["feat+one".to_string(), "feat+three".to_string()]
            )
        }
        _ => panic!("X opens the confirmation"),
    }
    press(&mut app, KeyCode::Char('j'));
    assert!(
        matches!(app.modal, Some(Modal::StopAll { .. })),
        "other keys keep it"
    );
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert!(app.pending.is_none());
}

#[test]
fn x_confirmed_stops_everything_and_every_running_row_says_so() {
    let mut app = test_app(&["feat+one", "feat+two", "feat+three"]);
    with_process(&mut app, "feat+one", running_phase());
    with_process(&mut app, "feat+three", running_phase());
    press(&mut app, KeyCode::Char('X'));
    press(&mut app, KeyCode::Char('y'));
    assert!(app.modal.is_none());
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::StopAll)
    );
    assert!(app.pending_on("feat+one").is_some());
    assert!(app.pending_on("feat+three").is_some());
    assert!(
        app.pending_on("feat+two").is_none(),
        "a stopped row is not stopping"
    );
}

// ---- CLI remedies said as keys ----------------------------------------

#[test]
fn a_dirty_removal_points_at_the_remove_dialog_not_a_flag() {
    let message = "could not remove feat/tui: feat+tui contains modified or untracked \
                   files (M package.json) — commit or remove them, or pass --force to \
                   let git discard them";
    let said = remedies::as_tui_remedy(message);
    assert!(!said.contains("--force"), "{said}");
    assert!(said.contains("press F in the remove dialog"), "{said}");
    assert!(said.contains("M package.json"), "the facts survive: {said}");
}

#[test]
fn an_isolation_remedy_names_the_shared_start_key() {
    let said = remedies::as_tui_remedy(
        "docker is not running — isolated mode needs Docker; install it, or start without --isolated",
    );
    assert!(said.ends_with("press S to start it shared"), "{said}");
}

#[test]
fn a_message_without_a_flag_is_left_alone() {
    let message = "feat+x is locked (benchmarking) — unlock it with `git worktree unlock` first";
    assert_eq!(remedies::as_tui_remedy(message), message);
    // Already neutral at the source: nothing to rewrite, nothing broken.
    let neutral = "feat+x has uncommitted changes — commit or remove them first";
    assert_eq!(remedies::as_tui_remedy(neutral), neutral);
}

fn type_key(app: &mut App, code: KeyCode) {
    app.handle_event(AppEvent::Input(ratatui::crossterm::event::Event::Key(
        KeyEvent::new(code, KeyModifiers::NONE),
    )));
}

// A start's question landing while a branch name is being typed took the
// keyboard mid-word: the next `n` answered "none", the next enter took the
// highlighted option, and the create modal and its text were thrown away.
#[test]
fn a_question_waits_until_the_create_modal_is_done_with_the_keyboard() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('n'));
    type_key(&mut app, KeyCode::Char('f'));
    let rx = open_question(&mut app, a_question());
    assert!(
        matches!(app.modal, Some(Modal::Create { .. })),
        "the modal being typed in stays"
    );
    type_key(&mut app, KeyCode::Char('i'));
    type_key(&mut app, KeyCode::Char('x'));
    match &app.modal {
        Some(Modal::Create { input, .. }) => assert_eq!(input, "fix"),
        other => panic!("expected the create modal, got {other:?}"),
    }
    assert!(rx.try_recv().is_err(), "nothing answered it");
    // Done typing: the question takes its turn.
    type_key(&mut app, KeyCode::Esc);
    assert!(matches!(app.modal, Some(Modal::Question { .. })));
    type_key(&mut app, KeyCode::Enter);
    assert!(matches!(rx.try_recv(), Ok(Ok(actions::Answer::Choice(0)))));
}

// A start's question landing while the chooser was open for another
// worktree replaced it, and the `⏎` meant for the chooser took the
// question's preselected answer unread.
#[test]
fn a_question_waits_until_the_mode_chooser_is_closed() {
    let mut app = test_app(&["feat+one"]);
    type_key(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Mode { .. })));
    let rx = open_question(&mut app, a_question());
    assert!(
        matches!(app.modal, Some(Modal::Mode { .. })),
        "the chooser stays"
    );
    assert!(app.queued_question.is_some());
    type_key(&mut app, KeyCode::Esc);
    assert!(matches!(app.modal, Some(Modal::Question { .. })));
    assert!(rx.try_recv().is_err(), "nothing answered it");
}

#[test]
fn a_question_waits_until_help_or_messages_is_dismissed() {
    for (key, overlay) in [('?', Modal::Help), ('m', Modal::Messages)] {
        let mut app = test_app(&["feat+one"]);
        type_key(&mut app, KeyCode::Char(key));
        let rx = open_question(&mut app, a_question());
        assert_eq!(
            std::mem::discriminant(app.modal.as_ref().unwrap()),
            std::mem::discriminant(&overlay)
        );
        // The key that closes the overlay closes only the overlay.
        type_key(&mut app, KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Question { .. })));
        assert!(rx.try_recv().is_err(), "nothing answered it");
    }
}

#[test]
fn a_question_waits_for_a_filter_being_typed() {
    let mut app = test_app(&["feat+one"]);
    type_key(&mut app, KeyCode::Char('/'));
    let rx = open_question(&mut app, a_question());
    type_key(&mut app, KeyCode::Char('c'));
    assert_eq!(app.filter, "c", "the key went to the filter");
    assert!(app.modal.is_none());
    type_key(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Some(Modal::Question { .. })));
    assert!(rx.try_recv().is_err());
}

// A command in backticks is one to type in a shell, and a key spliced
// into it is a command that does not exist.
#[test]
fn a_quoted_command_keeps_its_flags() {
    let message = "could not create feat/x — the partial worktree at /t/feat+x could not be \
                   removed; remove it with `git worktree remove --force` and delete the \
                   branch if it is new";
    assert_eq!(remedies::as_tui_remedy(message), message);
    let doctor = "`start --isolated` cannot run it, though a plain `start` still can";
    assert_eq!(remedies::as_tui_remedy(doctor), doctor);
    // Outside the quotes the rewrite still happens.
    assert_eq!(
        remedies::as_tui_remedy("run `pando rm x`, or retry with --force"),
        "run `pando rm x`, or retry with F in the remove dialog"
    );
}

// A phrase matches whole, never as the tail of a longer word or flag.
#[test]
fn a_phrase_inside_a_longer_word_is_left_alone() {
    let message = "git push --force-with-lease is not this";
    assert_eq!(remedies::as_tui_remedy(message), message);
    // `start --shared` is not inside `restart --shared`: the word stays a
    // word, and only the flag is said as its key.
    let said = remedies::as_tui_remedy("restart --shared puts it back");
    assert_eq!(said, "restart S puts it back");
}

#[test]
fn a_bare_flag_is_still_translated() {
    assert_eq!(
        remedies::as_tui_remedy("retry with --force"),
        "retry with F in the remove dialog"
    );
}

/// Polls the worker until it has reported, or fails the test.
fn wait_for_pending(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.pending.is_some() {
        assert!(Instant::now() < deadline, "the worker never reported");
        app.poll_pending();
        std::thread::sleep(Duration::from_millis(5));
    }
}

// `x` skips its confirmation on a row with nothing up, and the stop that
// found nothing to stop said `✓ stopped` all the same.
#[test]
fn x_on_a_worktree_that_is_not_running_says_so() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    press(&mut app, KeyCode::Char('x'));
    wait_for_pending(&mut app);
    let status = app.flash().expect("a status");
    assert_eq!(status.message, "feat/one was not running");
    assert_eq!(status.kind, StatusKind::Info);
}

// A stop closes the worktree's share, and said so only as a line on its
// way, which the "stopped" after it replaced: the row lost its public
// URL and the header never said which URL had gone, or how to get
// another.
#[test]
fn x_on_a_shared_worktree_says_which_public_url_it_closed() {
    let (dir, mut app) = app_with_logs(&["feat+one"]);
    let tunnel = crate::testutil::spawn_guarded(
        "exec sleep 300",
        dir.path(),
        &dir.path().join("tunnel.log"),
    );
    let mut record = WorktreeRecord::new("/trees/feat+one", true);
    record.share = Some(crate::state::ShareRecord {
        tunnel_pid: tunnel.pid,
        tunnel_pgid: tunnel.pgid,
        public_url: "https://x.trycloudflare.com".into(),
        local_port: 17_342,
        started_at: Utc::now(),
        log_path: dir.path().join("tunnel.log"),
        proxy_pid: None,
        proxy_pgid: None,
        proxy_port: None,
    });
    let mut store = State::new();
    store.worktrees.insert("feat+one".into(), record);
    crate::state::save(&app.paths.state_file(), &store).unwrap();
    app.state = store;

    press(&mut app, KeyCode::Char('x'));
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().expect("a status");
    assert!(
        message.contains("https://x.trycloudflare.com is closed"),
        "{message}"
    );
    assert!(message.contains("`pando share feat+one`"), "{message}");
    assert!(is_error, "said as a closed share is everywhere else");
    assert!(
        !crate::process::is_alive(tunnel.pid),
        "and the tunnel is down"
    );
}

// The checkout worked and the install after it did not: the worktree is
// kept, and "could not create feat/x: feat/x was created, but …" said
// both at once.
#[test]
fn a_create_whose_install_failed_says_it_was_created_and_goes_to_it() {
    let mut app = test_app(&["feat+one"]);
    let error = format!(
        "feat/x {}: the install hook failed: exited 1",
        actions::CREATED_BUT_INSTALL_FAILED
    );
    let reported = error.clone();
    app.spawn_pending("feat+x".into(), PendingKind::Create, move || Err(reported));
    app.pending.as_mut().unwrap().label = "feat/x".into();
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(message, error);
    assert!(is_error, "the install still failed");
    assert_eq!(app.select_on_arrival.as_deref(), Some("feat+x"));

    // Any other failure is still one to create it.
    let mut app = test_app(&["feat+one"]);
    app.spawn_pending("feat+y".into(), PendingKind::Create, || {
        Err("branch feat/y already exists".into())
    });
    wait_for_pending(&mut app);
    let (message, _) = app.active_status().unwrap();
    assert!(message.starts_with("could not create "), "{message}");
    assert_eq!(app.select_on_arrival, None);
}

// Kept over a record another command wrote while git checked it out:
// "could not create feat/x: … the worktree and that record are kept"
// said both at once, and the cursor stayed where it was.
#[test]
fn a_create_kept_over_a_raced_record_says_so_and_goes_to_it() {
    let mut app = test_app(&["feat+one"]);
    let error = format!(
        "feat/x was checked out at /trees/feat+x, but another pando command recorded \
         \"feat+x\" while git was checking it out — {}, but it did not get its install step",
        actions::KEPT_OVER_RACED_RECORD
    );
    let reported = error.clone();
    app.spawn_pending("feat+x".into(), PendingKind::Create, move || Err(reported));
    app.pending.as_mut().unwrap().label = "feat/x".into();
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(message, error);
    assert!(is_error, "it still lacks what new gives a worktree");
    assert_eq!(app.select_on_arrival.as_deref(), Some("feat+x"));
}

/// Starts `kind` on `name` with a worker that says `lines` on its way and
/// then reports `outcome` at once, the way a quick `rm` does.
fn pending_that_says(
    app: &mut App,
    name: &str,
    kind: PendingKind,
    lines: &[&str],
    outcome: Result<PendingOutcome, String>,
) {
    let (ptx, prx) = mpsc::channel::<String>();
    let lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    app.spawn_pending(name.into(), kind, move || {
        for line in lines {
            let _ = ptx.send(line);
        }
        outcome
    });
    app.pending.as_mut().unwrap().progress_rx = Some(prx);
}

// `F` with Docker down: the line naming the command that removes the
// volumes came in with the outcome, was never painted, and the record
// that named the compose project was gone. `m` had no trace of it.
#[test]
fn what_an_action_said_on_its_way_is_kept_and_a_warning_is_shown() {
    let mut app = test_app(&["feat+one"]);
    let volumes = "Docker is not running, so the services of pando-x could not be removed — \
                   their data volumes survive; once Docker is up, `docker compose -p pando-x \
                   down -v` removes them";
    pending_that_says(
        &mut app,
        "feat+one",
        PendingKind::Remove,
        &["stopping dev", volumes],
        Ok(PendingOutcome::Removed("feat+one".into())),
    );
    wait_for_pending(&mut app);

    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(message, volumes, "the header has the warning");
    assert!(is_error);
    let said: Vec<&str> = app.messages.iter().map(|m| m.message.as_str()).collect();
    assert_eq!(
        said,
        vec![
            format!("removing feat/one: stopping dev · {volumes}").as_str(),
            "removed feat/one",
            volumes,
        ],
        "m has every line, then the outcome"
    );
}

// A frozen install that rewrote its lockfile: the warning came just
// before the outcome, and "created feat/x" replaced it within a tick.
#[test]
fn a_hook_warning_outlasts_the_outcome_it_came_with() {
    let mut app = test_app(&["feat+one"]);
    let warning = "warning: the install hook changed one of the files it is keyed on";
    pending_that_says(
        &mut app,
        "feat+x",
        PendingKind::Create,
        &["checking out feat/x", "installing", warning],
        Ok(PendingOutcome::Created("feat+x".into())),
    );
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().unwrap();
    assert_eq!(message, warning);
    assert!(is_error);
    assert!(
        app.messages
            .iter()
            .any(|m| m.message.starts_with("created feat+x")),
        "the outcome is still in m: {:?}",
        app.messages
    );
}

// Stages alone are kept in `m`, and the header still ends on the outcome.
#[test]
fn an_action_that_only_narrated_ends_on_its_outcome() {
    let mut app = test_app(&["feat+one"]);
    pending_that_says(
        &mut app,
        "feat+one",
        PendingKind::Stop,
        &["stopping dev"],
        Ok(PendingOutcome::Stopped("feat+one".into())),
    );
    wait_for_pending(&mut app);
    assert_eq!(app.active_status(), Some(("stopped feat/one", false)));
    assert!(
        app.messages
            .iter()
            .any(|m| m.message == "stopping feat/one: stopping dev")
    );

    // A failure keeps what was said before it, and ends on the error.
    let mut app = test_app(&["feat+one"]);
    pending_that_says(
        &mut app,
        "feat+one",
        PendingKind::Start,
        &["starting services: db"],
        Err("db did not become ready".into()),
    );
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().unwrap();
    assert!(message.starts_with("could not start feat/one"), "{message}");
    assert!(is_error);
    assert!(
        app.messages
            .iter()
            .any(|m| m.message == "starting feat/one: starting services: db")
    );
}

// `X` leaves running what came up after its list was shown, and said so
// only to `m`, by directory: the header said "stopped feat/one", or
// "nothing was running" beside a row that ran.
#[test]
fn a_stop_all_names_on_the_header_what_it_left_running() {
    let came_up = "feat+two came up after the list of what stops was shown — left running";
    for (stopped, said) in [
        (
            vec!["feat+one".to_string()],
            "stopped feat/one · feat/two came up since and was left running",
        ),
        (
            Vec::new(),
            "nothing listed was still running · feat/two came up since and was left running",
        ),
    ] {
        let mut app = test_app(&["feat+one", "feat+two"]);
        pending_that_says(
            &mut app,
            "",
            PendingKind::StopAll,
            &[came_up],
            Ok(PendingOutcome::StoppedAll(actions::StopAllReport {
                stopped,
                kept: vec!["feat+two".to_string()],
            })),
        );
        wait_for_pending(&mut app);
        assert_eq!(app.active_status(), Some((said, false)));
    }
}

// ---- errors about one worktree ---------------------------------------

// An error raised by feat/db's start does not follow the reader into
// feat/login's log viewer. `m` still has it.
#[test]
fn another_worktrees_error_stays_out_of_the_log_viewer() {
    let (_dir, mut app) = app_with_logs(&["feat+db", "feat+login"]);
    write_log(&app, "feat+login", "dev", &["login up"]);
    press(&mut app, KeyCode::Char('j'));
    open_viewer(&mut app, 80, 24);
    assert_eq!(viewer(&app).name, "feat+login");
    app.spawn_pending("feat+db".into(), PendingKind::Start, || {
        Err("docker is not running".to_string())
    });
    wait_for_pending(&mut app);
    assert!(app.flash().is_some_and(|s| s.is_error()));

    assert!(
        app.flash_for("feat+login").is_none(),
        "feat/db's error is not feat/login's"
    );
    assert!(
        app.flash_for("feat+db").is_some(),
        "and it is still feat/db's"
    );
    assert!(
        app.messages.iter().any(|m| m.message.contains("docker")),
        "m keeps it"
    );
}

// A message about nothing in particular shows in any viewer.
#[test]
fn an_error_about_no_worktree_shows_everywhere() {
    let mut app = test_app(&["feat+one"]);
    app.set_error("refresh failed: disk full");
    assert!(app.flash_for("feat+one").is_some());
}

// ---- nothing to run --------------------------------------------------

#[test]
fn enter_on_a_project_with_nothing_to_run_says_where_to_add_it() {
    let mut app = test_app(&["main-lib"]);
    app.nothing_to_run = true;
    press(&mut app, KeyCode::Enter);
    assert!(app.pending.is_none(), "enter does not pretend to start");
    let (message, is_error) = app.active_status().unwrap();
    assert!(!is_error);
    assert!(
        message.starts_with("nothing to run: add a [dev] command in "),
        "{message}"
    );
    assert!(
        message.ends_with(&app.paths.config_file().display().to_string()),
        "the absolute path of the file to edit: {message}"
    );
    for code in [KeyCode::Char('s'), KeyCode::Char('i'), KeyCode::Char('r')] {
        press(&mut app, code);
        assert!(app.pending.is_none(), "{code:?} does not start anything");
    }
}

// A start that fails for want of a process teaches the session: the next
// enter says so up front.
#[test]
fn a_start_that_found_no_process_turns_on_nothing_to_run() {
    let mut app = test_app(&["main-lib"]);
    app.spawn_pending("main-lib".into(), PendingKind::Start, || {
        Err("no processes configured; add [dev] to pando.toml".to_string())
    });
    wait_for_pending(&mut app);
    assert!(app.nothing_to_run);
    let (message, _) = app.active_status().unwrap();
    assert!(
        message.starts_with("nothing to run: add a [dev] command in "),
        "{message}"
    );
}

// ---- ready, not just started -----------------------------------------

#[test]
fn a_start_that_returns_before_ready_waits_to_say_so() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", Phase::Starting { since: Utc::now() });
    app.spawn_pending("feat+one".into(), PendingKind::Start, || {
        Ok(PendingOutcome::Started(
            "feat+one".into(),
            Some("http://localhost:17342".into()),
            vec![("dev".into(), 4242)],
        ))
    });
    wait_for_pending(&mut app);
    let status = app.flash().unwrap();
    assert_ne!(status.kind, StatusKind::Success, "not ✓ while it starts");
    assert!(
        status.message.contains("waiting for it to be ready"),
        "{}",
        status.message
    );

    let mut state = app.state.clone();
    state
        .worktrees
        .get_mut("feat+one")
        .unwrap()
        .processes
        .get_mut("dev")
        .unwrap()
        .phase = running_phase();
    app.handle_event(refreshed(state));
    let status = app.flash().unwrap();
    assert_eq!(status.kind, StatusKind::Success);
    assert_eq!(status.message, "feat/one is ready — http://localhost:17342");
    assert!(app.awaiting_ready.is_none());
}

// A restart came back while the state on hand was still the run it
// replaced, every process running — or, for `P`, only the siblings it left
// alone — and said "ready" at once of a process still coming up. Ready is
// what it spawned running, by pid.
#[test]
fn a_restart_is_not_ready_on_a_read_from_before_it() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    app.spawn_pending("feat+one".into(), PendingKind::Restart, || {
        Ok(PendingOutcome::Started(
            "feat+one".into(),
            None,
            vec![("api".into(), 9191)],
        ))
    });
    wait_for_pending(&mut app);
    let status = app.flash().unwrap();
    assert_ne!(status.kind, StatusKind::Success, "{}", status.message);
    assert!(app.awaiting_ready.is_some());

    let old_api = app.state.worktrees["feat+one"].processes["api"].clone();
    let refreshed = |app: &mut App, api: Option<Phase>| {
        let mut state = app.state.clone();
        let processes = &mut state.worktrees.get_mut("feat+one").unwrap().processes;
        processes.remove("api");
        if let Some(phase) = api {
            let new_api = ProcessRecord {
                pid: 9191,
                phase,
                ..old_api.clone()
            };
            processes.insert("api".into(), new_api);
        }
        app.handle_event(refreshed(state));
    };
    // Between the stop and the start: only the sibling, running.
    refreshed(&mut app, None);
    assert!(
        app.awaiting_ready.is_some(),
        "the sibling says nothing of api"
    );
    refreshed(&mut app, Some(Phase::Starting { since: Utc::now() }));
    assert!(app.awaiting_ready.is_some());
    assert_ne!(app.flash().unwrap().kind, StatusKind::Success);

    refreshed(&mut app, Some(running_phase()));
    let status = app.flash().unwrap();
    assert_eq!(status.kind, StatusKind::Success);
    assert_eq!(status.message, "feat/one is ready");
    assert!(app.awaiting_ready.is_none());
}

// A whole restart — `r`, or `P` on a worktree with one process — closes
// its share and says so on its way. The lines after that one replaced
// it, then the outcome, then the "ready" the next refresh brought: the
// header never said the URL handed out was gone.
#[test]
fn a_restart_that_closed_a_public_url_still_says_so_once_it_is_ready() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", Phase::Starting { since: Utc::now() });
    let closed = "feat+one: its public URL https://x.trycloudflare.com is closed — \
                  `pando share feat+one` gives it a new one";
    pending_that_says(
        &mut app,
        "feat+one",
        PendingKind::Restart,
        &["stopping dev", closed, "starting dev"],
        Ok(PendingOutcome::Started(
            "feat+one".into(),
            Some("http://localhost:17342".into()),
            vec![("dev".into(), 4242)],
        )),
    );
    wait_for_pending(&mut app);
    assert_eq!(app.active_status(), Some((closed, true)));

    let mut state = app.state.clone();
    let record = state.worktrees.get_mut("feat+one").unwrap();
    record.processes.get_mut("dev").unwrap().phase = running_phase();
    app.handle_event(refreshed(state));
    assert!(app.awaiting_ready.is_none(), "ready was said");
    assert_eq!(
        app.messages.back().map(|m| m.message.as_str()),
        Some("feat/one is ready — http://localhost:17342"),
        "in m"
    );
    assert_eq!(
        app.active_status(),
        Some((closed, true)),
        "and the header still says which URL is gone"
    );
}

// And when the start half fails after the stop half closed the share: the
// header said only why the restart failed, and the row just lost its
// public URL.
#[test]
fn a_restart_that_closed_a_public_url_and_then_failed_says_both() {
    let mut app = test_app(&["feat+one"]);
    let closed = "feat+one: its public URL https://x.trycloudflare.com is closed — \
                  `pando share feat+one` gives it a new one";
    pending_that_says(
        &mut app,
        "feat+one",
        PendingKind::Restart,
        &["stopping dev", closed, "starting dev"],
        Err("port 17342 is in use".into()),
    );
    wait_for_pending(&mut app);
    let (message, is_error) = app.active_status().expect("a status");
    assert_eq!(
        message,
        format!("could not restart feat/one: port 17342 is in use · {closed}")
    );
    assert!(is_error);
}

// A start that returned while dev was still starting waited on dev's pid,
// and a stop — `x`, or `pando stop` in another pane — never ended the
// wait: every later read, missing dev, said "waiting". Missing is only
// news once a read has shown it, though, since a read from before the
// start is missing it too.
#[test]
fn a_stop_while_a_start_waits_to_be_ready_ends_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let dev =
        crate::testutil::spawn_guarded("exec sleep 30", dir.path(), &dir.path().join("dev.log"));
    let pid = dev.pid;
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", Phase::Starting { since: Utc::now() });
    let record = app.state.worktrees.get_mut("feat+one").unwrap();
    record.processes.get_mut("dev").unwrap().pid = pid;
    let starting = app.state.clone();
    // What a read from before the start and one after a stop both show:
    // the record, its ports, and no dev.
    let mut without_dev = starting.clone();
    let record = without_dev.worktrees.get_mut("feat+one").unwrap();
    record.processes.clear();

    app.state = without_dev.clone();
    app.spawn_pending("feat+one".into(), PendingKind::Start, move || {
        Ok(PendingOutcome::Started(
            "feat+one".into(),
            None,
            vec![("dev".into(), pid)],
        ))
    });
    wait_for_pending(&mut app);
    app.handle_event(refreshed(without_dev.clone()));
    assert!(
        app.awaiting_ready.is_some(),
        "a read from before the start says nothing of dev"
    );
    app.handle_event(refreshed(starting));
    assert!(app.awaiting_ready.is_some(), "dev is still starting");

    drop(dev);
    app.handle_event(refreshed(without_dev.clone()));
    assert!(app.awaiting_ready.is_none(), "dev was stopped");

    // Not on a read nothing before it has shown dev in, dead or not: one
    // that died by itself is in the reads after this, failed.
    app.awaiting_ready = Some(AwaitingReady {
        name: "feat+one".into(),
        url: None,
        spawned: vec![("dev".into(), pid)],
        seen: false,
    });
    app.handle_event(refreshed(without_dev));
    assert!(app.awaiting_ready.is_some());
}

// ---- commit age ------------------------------------------------------

#[test]
fn git_relative_dates_are_read_and_written_compactly() {
    assert_eq!(parse_git_relative("82 seconds ago"), Some(82));
    assert_eq!(parse_git_relative("1 minute ago"), Some(60));
    assert_eq!(parse_git_relative("3 hours ago"), Some(3 * 3600));
    assert_eq!(
        parse_git_relative("2 years, 4 months ago"),
        Some(2 * 365 * 86_400 + 4 * 30 * 86_400)
    );
    assert_eq!(parse_git_relative("yesterday"), None);
    // Git's own words for a commit dated ahead of the clock.
    assert_eq!(parse_git_relative("in the future"), None);
    // A number no date can mean — a corrupt cache entry — is no date, not
    // an overflow panic in the paint.
    assert_eq!(parse_git_relative("99999999999999999 years ago"), None);
    assert_eq!(
        parse_git_relative("9000000000000000000 seconds, 9000000000000000000 seconds ago"),
        None
    );
    assert_eq!(compact_age(82), "1m");
    assert_eq!(compact_age(59), "59s");
    assert_eq!(compact_age(3 * 3600 + 59 * 60), "3h");
    assert_eq!(compact_age(2 * 86_400), "2d");
    assert_eq!(compact_age(30 * 86_400), "4w");
    assert_eq!(compact_age(90 * 86_400), "3mo");
    assert_eq!(compact_age(800 * 86_400), "2y");
}

// The age keeps counting from when enrichment reported it, instead of
// saying `82 seconds ago` for as long as the TUI is open.
#[test]
fn a_commit_age_keeps_counting_after_git_reported_it() {
    let mut app = test_app(&["feat+one"]);
    app.worktrees[0].head_age = Some("82 seconds ago".into());
    assert_eq!(app.commit_age(&app.worktrees[0]).as_deref(), Some("1m ago"));
    app.commit_seen.insert(
        "feat+one".into(),
        (
            "82 seconds ago".into(),
            82,
            Instant::now() - Duration::from_secs(3 * 3600),
        ),
    );
    assert_eq!(app.commit_age(&app.worktrees[0]).as_deref(), Some("3h ago"));
    // Words it cannot read are shown as git wrote them.
    app.worktrees[0].head_age = Some("in the future".into());
    assert_eq!(
        app.commit_age(&app.worktrees[0]).as_deref(),
        Some("in the future")
    );
}

// ---- git state stays fresh -------------------------------------------

// The selected worktree's git state is read again on the slow tick, and
// quietly: the header's `reading git` is for the first read only.
#[test]
fn the_slow_tick_re_reads_the_selected_worktrees_git_quietly() {
    let mut app = test_app(&["feat+one"]);
    app.tick = GIT_SELECTED_EVERY - 1;
    app.handle_event(AppEvent::Tick);
    assert!(app.git_refreshing, "a re-read is in flight");
    assert_eq!(app.enriching, 0, "and the header does not announce it");
}

// A worktree made while a quiet re-read runs is read at once, alongside
// it. Whichever read finishes first ends only itself: the new worktree's
// git row said its status could not be read while it was being read.
#[test]
fn a_read_of_git_that_finishes_leaves_the_one_still_under_way() {
    let discovered = |names: &[&str]| {
        let worktrees = names
            .iter()
            .map(|name| Worktree {
                dirty: None,
                ..wt(name)
            })
            .collect();
        AppEvent::Discovered(Box::new(Ok(Snapshot {
            main: wt("acme-shop"),
            worktrees,
            created_by_pando: BTreeMap::new(),
            state: State::new(),
            warning: None,
            notices: Vec::new(),
            default_base: Some("main".into()),
        })))
    };
    let mut app = test_app(&["feat+one"]);
    app.refresh_git(None);
    assert!(app.git_refreshing);
    app.handle_event(discovered(&["feat+one", "feat+two"]));
    assert_eq!(app.enriching, 1, "the new worktree is being read");
    app.handle_event(AppEvent::EnrichDone { quiet: true });
    assert!(!app.git_refreshing);
    assert_eq!(app.enriching, 1, "and still is once the re-read is over");

    // Two announced reads at once are the same: the header says
    // `reading git` until both are over.
    app.handle_event(discovered(&["feat+one", "feat+two", "feat+three"]));
    assert_eq!(app.enriching, 2);
    app.handle_event(AppEvent::EnrichDone { quiet: false });
    assert_eq!(app.enriching, 1);
    app.refresh_git(None);
    assert!(
        !app.git_refreshing,
        "no re-read starts while one is under way"
    );
    app.handle_event(AppEvent::EnrichDone { quiet: false });
    assert_eq!(app.enriching, 0);
}

// A worktree that became dirty after the TUI opened shows its `*` once
// the re-read lands.
#[test]
fn an_enrichment_that_finds_changes_marks_the_worktree_dirty() {
    let mut app = test_app(&["feat+one"]);
    let update = crate::worktree::EnrichUpdate {
        name: "feat+one".into(),
        branch: Some("feat/one".into()),
        prunable: false,
        head_sha: Some("abc1234".into()),
        head_subject: Some("do the thing".into()),
        head_age: Some("5 minutes ago".into()),
        dirty: Some(true),
        ahead_behind: Some((1, 0)),
        in_progress: None,
    };
    assert!(
        app.handle_event(AppEvent::Enrich(update.clone())),
        "repaints"
    );
    assert_eq!(app.worktrees[0].dirty, Some(true));
    assert!(
        !app.handle_event(AppEvent::Enrich(update)),
        "the same answer again costs no paint"
    );
}

// ---- the create modal ------------------------------------------------

fn open_create_with(app: &mut App, branches: &[(&str, BranchSource)]) {
    press(app, KeyCode::Char('n'));
    let entries = branches
        .iter()
        .map(|(name, source)| BranchEntry {
            name: name.to_string(),
            source: source.clone(),
        })
        .collect();
    app.handle_event(AppEvent::BranchesReady(entries));
}

// `main` is the main checkout's branch: git will not check it out twice,
// so enter says so instead of starting a create that fails.
#[test]
fn the_main_checkouts_branch_cannot_be_created_from_the_picker() {
    let mut app = test_app(&["feat+one"]);
    let mut main = wt("acme-shop");
    main.branch = Some("main".into());
    app.main = Some(main);
    open_create_with(&mut app, &[("main", BranchSource::Local)]);
    type_str(&mut app, "main");
    press(&mut app, KeyCode::Enter);
    assert!(app.pending.is_none(), "nothing started");
    assert!(
        matches!(app.modal, Some(Modal::Create { .. })),
        "still open"
    );
    let (message, is_error) = app.active_status().unwrap();
    assert!(is_error);
    assert!(
        message.contains("checked out in the main checkout"),
        "{message}"
    );
}

// Tab walks the base a new branch forks from: the default first, then
// every branch, and round again.
#[test]
fn tab_in_the_create_modal_cycles_the_base() {
    let mut app = test_app(&["feat+one"]);
    app.default_base = Some("origin/main".into());
    open_create_with(
        &mut app,
        &[
            ("develop", BranchSource::Local),
            ("release", BranchSource::Remote),
        ],
    );
    let base = |app: &App| match &app.modal {
        Some(Modal::Create { base, .. }) => base.clone(),
        other => panic!("expected the create modal, got {other:?}"),
    };
    assert_eq!(base(&app), None, "the default to begin with");
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("develop"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("origin/release"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(
        base(&app).as_deref(),
        Some("origin/main"),
        "and back to the default, chosen now"
    );
    press(&mut app, KeyCode::BackTab);
    assert_eq!(base(&app).as_deref(), Some("origin/release"));
}

// A repository with no default base is the one where tab matters most —
// `new` refuses without a base there — and its first choice must be
// reachable in one press, not only after going all the way round.
#[test]
fn with_no_default_base_the_first_tab_picks_the_first_branch() {
    let mut app = test_app(&["feat+one"]);
    app.default_base = None;
    open_create_with(
        &mut app,
        &[
            ("develop", BranchSource::Local),
            ("release", BranchSource::Local),
        ],
    );
    let base = |app: &App| match &app.modal {
        Some(Modal::Create { base, .. }) => base.clone(),
        other => panic!("expected the create modal, got {other:?}"),
    };
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("develop"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("release"));

    let mut app = test_app(&["feat+one"]);
    app.default_base = None;
    open_create_with(
        &mut app,
        &[
            ("develop", BranchSource::Local),
            ("release", BranchSource::Local),
        ],
    );
    press(&mut app, KeyCode::BackTab);
    assert_eq!(
        base(&app).as_deref(),
        Some("release"),
        "back from nothing is the last"
    );
    // No branches and no default: tab has nothing to walk, and says nothing
    // wrong by staying put.
    let mut app = test_app(&["feat+one"]);
    app.default_base = None;
    open_create_with(&mut app, &[]);
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app), None);
}

#[test]
fn base_choices_lead_with_the_default_and_name_each_branch_once() {
    let branches = vec![
        BranchEntry {
            name: "main".into(),
            source: BranchSource::Local,
        },
        BranchEntry {
            name: "main".into(),
            source: BranchSource::Remote,
        },
    ];
    assert_eq!(
        base_choices(&["origin/main"], &branches),
        vec!["origin/main", "main"]
    );
    assert_eq!(
        base_choices(&["develop", "origin/main"], &branches),
        vec!["develop", "origin/main", "main"]
    );
    assert_eq!(base_choices(&[], &[]), Vec::<String>::new());
}

// With `[project] base` set, `new` forks from it, and so does the
// dialog's untouched choice; the repository's default is one tab away
// and, once chosen, is what the new branch forks from.
#[test]
fn tab_starts_from_the_configured_base_and_reaches_the_repository_default() {
    let mut app = test_app(&["feat+one"]);
    app.default_base = Some("origin/main".into());
    app.config.project.base = Some("develop".into());
    open_create_with(&mut app, &[("release", BranchSource::Local)]);
    for c in "feat/x".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let base = |app: &App| match &app.modal {
        Some(Modal::Create { base, .. }) => base.clone(),
        other => panic!("expected the create modal, got {other:?}"),
    };
    assert_eq!(app.implied_base("feat/x").as_deref(), Some("develop"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("origin/main"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("release"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(
        base(&app).as_deref(),
        Some("develop"),
        "and back to the configured base"
    );
}

// A base tab chose stays chosen as the name is typed, even one that was
// the untouched base when tab reached it: the untouched base follows the
// name, and `hotfix/y` would fork from its rule's base instead.
#[test]
fn a_base_tab_chose_survives_typing_a_name_with_another_base() {
    let mut app = test_app(&["feat+one"]);
    app.default_base = Some("origin/main".into());
    app.config.project.base = Some("develop".into());
    app.config.branches.rules.push(crate::config::BranchRule {
        match_: "hotfix/*".into(),
        base: "release".into(),
    });
    open_create_with(&mut app, &[]);
    let base = |app: &App| match &app.modal {
        Some(Modal::Create { base, .. }) => base.clone(),
        other => panic!("expected the create modal, got {other:?}"),
    };
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("origin/main"));
    press(&mut app, KeyCode::Tab);
    assert_eq!(base(&app).as_deref(), Some("develop"), "walked round to it");
    type_str(&mut app, "hotfix/y");
    assert_eq!(app.implied_base("hotfix/y").as_deref(), Some("release"));
    assert_eq!(
        base(&app).as_deref(),
        Some("develop"),
        "the branch forks from what was picked"
    );
}

// ---- the all tab -----------------------------------------------------

fn two_process_app() -> (tempfile::TempDir, App) {
    let (dir, mut app) = app_with_logs(&["feat+one"]);
    app.config.processes.clear();
    for process in ["api", "web"] {
        app.config
            .processes
            .insert(process.to_string(), crate::config::ProcessConfig::default());
    }
    (dir, app)
}

fn append_log(app: &App, worktree: &str, source: &str, line: &str) {
    use std::io::Write as _;
    let path = app.paths.log_file(worktree, source);
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{line}").unwrap();
}

#[test]
fn two_processes_get_an_all_tab_first_and_one_does_not() {
    let (_dir, mut app) = two_process_app();
    write_log(&app, "feat+one", "api", &["api up"]);
    write_log(&app, "feat+one", "web", &["web up"]);
    write_log(&app, "feat+one", "install", &["installed"]);
    open_viewer(&mut app, 80, 20);
    assert_eq!(
        viewer(&app).available,
        vec!["all", "api", "web", "install"],
        "all first; a hook is not merged into it, but keeps its tab"
    );
    assert_eq!(
        viewer(&app).source,
        "api",
        "the viewer still opens on one log"
    );

    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["up"]);
    write_log(&app, "feat+one", "install", &["installed"]);
    open_viewer(&mut app, 80, 20);
    assert_eq!(viewer(&app).available, vec!["dev", "install"]);
}

// Every process's lines, each marked with its source, and what arrives
// afterwards in the order it arrives.
#[test]
fn the_all_tab_merges_every_process_with_a_source_prefix() {
    let (_dir, mut app) = two_process_app();
    write_log(&app, "feat+one", "api", &["api up"]);
    write_log(&app, "feat+one", "web", &["web up"]);
    open_viewer(&mut app, 100, 20);
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(viewer(&app).source, "all");
    let plain = |app: &App| -> Vec<String> {
        viewer(app)
            .tail
            .lines()
            .iter()
            .map(|p| p.plain.clone())
            .collect()
    };
    assert_eq!(plain(&app), vec!["api │ api up", "web │ web up"]);

    append_log(&app, "feat+one", "web", "GET / 200");
    app.handle_event(AppEvent::Tick);
    append_log(&app, "feat+one", "api", "query took 3ms");
    app.handle_event(AppEvent::Tick);
    assert_eq!(
        plain(&app),
        vec![
            "api │ api up",
            "web │ web up",
            "web │ GET / 200",
            "api │ query took 3ms"
        ],
        "in the order the lines arrived"
    );

    // `y` copies the line as the process wrote it.
    press(&mut app, KeyCode::Char('y'));
    assert_eq!(app.clipboard.as_deref(), Some("query took 3ms"));

    // Search sees the prefix too, so `/web` finds the web server's lines.
    search_for(&mut app, "web │");
    assert_eq!(viewer(&app).search.matches.len(), 2);
}

// A process started while the `all` tab is open got a tab of its own on
// the next paint and never appeared in the merge.
#[test]
fn a_process_that_starts_while_the_all_tab_is_open_joins_the_merge() {
    let (_dir, mut app) = two_process_app();
    write_log(&app, "feat+one", "api", &["api up"]);
    write_log(&app, "feat+one", "web", &["web up"]);
    open_viewer(&mut app, 100, 20);
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(viewer(&app).source, "all");

    app.config.processes.insert(
        "worker".to_string(),
        crate::config::ProcessConfig::default(),
    );
    write_log(&app, "feat+one", "worker", &["worker up"]);
    paint(&mut app, 100, 20);
    app.handle_event(AppEvent::Tick);
    let plain: Vec<String> = viewer(&app)
        .tail
        .lines()
        .iter()
        .map(|p| p.plain.clone())
        .collect();
    assert!(
        plain.iter().any(|line| line.ends_with("│ worker up")),
        "{plain:?}"
    );
    // And what it writes next arrives like anybody else's.
    append_log(&app, "feat+one", "worker", "job done");
    app.handle_event(AppEvent::Tick);
    let last = viewer(&app).tail.lines().back().unwrap().plain.clone();
    assert_eq!(last, "worker │ job done");
}

// A restart empties the log in place. The tail keeps what it read of the
// old run, so the viewer showed the old run's lines above the new ones as
// if they were one log.
#[test]
fn a_log_that_starts_over_shows_only_the_new_run() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["old run 1", "old run 2", "old crash"],
    );
    open_viewer(&mut app, 80, 20);
    press(&mut app, KeyCode::Char('g'));
    write_log(&app, "feat+one", "dev", &["new run 1"]);
    app.handle_event(AppEvent::Tick);
    let plain: Vec<String> = viewer(&app)
        .tail
        .lines()
        .iter()
        .map(|p| p.plain.clone())
        .collect();
    assert_eq!(plain, vec!["new run 1"]);
    assert!(viewer(&app).follow, "back on the live tail");
    let (message, _) = app.active_status().expect("it says so");
    assert!(message.contains("started over"), "{message}");
    // And lines appended after that are an ordinary poll again.
    append_log(&app, "feat+one", "dev", "new run 2");
    app.handle_event(AppEvent::Tick);
    assert_eq!(viewer(&app).tail.lines().len(), 2);
}

// The all tab keeps the other processes' lines, so it marks where one
// process's log started over instead of dropping anything.
#[test]
fn the_all_tab_marks_where_a_process_log_started_over() {
    let (_dir, mut app) = two_process_app();
    write_log(&app, "feat+one", "api", &["api old"]);
    write_log(&app, "feat+one", "web", &["web up"]);
    open_viewer(&mut app, 100, 20);
    press(&mut app, KeyCode::Char('1'));
    write_log(&app, "feat+one", "api", &["api new run"]);
    app.handle_event(AppEvent::Tick);
    let plain: Vec<String> = viewer(&app)
        .tail
        .lines()
        .iter()
        .map(|p| p.plain.clone())
        .collect();
    assert_eq!(
        plain,
        vec![
            "api │ api old",
            "web │ web up",
            &format!("api │ {}", super::merged::RESTART_MARKER),
            "api │ api new run",
        ]
    );
}

// Block ids are made unique per source; one that depended on how many
// sources there were collided once the count changed.
#[test]
fn merged_block_ids_stay_distinct_when_a_source_joins() {
    let dir = tempfile::tempdir().unwrap();
    let write = |name: &str, lines: &[&str]| {
        let path = dir.path().join(format!("{name}.log"));
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    };
    let block = ["{", "  \"a\": 1", "}"];
    let a = write("a", &block.repeat(3));
    let mut merged = MergedTail::new(vec![("a".into(), a)], 100);
    merged.poll().unwrap();
    let b = write("b", &["x"]);
    let c = write("c", &block);
    merged.add_source("b".into(), b);
    merged.add_source("c".into(), c);
    merged.poll().unwrap();
    let ids: std::collections::BTreeSet<u64> =
        merged.lines().iter().filter_map(|p| p.block_id).collect();
    assert_eq!(ids.len(), 4, "one id per block: {ids:?}");
}

/// A log of `count` lines, `name 0` to `name {count - 1}`, in `dir`.
fn numbered_log(dir: &std::path::Path, name: &str, count: usize) -> PathBuf {
    let path = dir.join(format!("{name}.log"));
    let body: String = (0..count).map(|i| format!("{name} {i}\n")).collect();
    std::fs::write(&path, body).unwrap();
    path
}

fn merged_plain(merged: &MergedTail) -> Vec<String> {
    merged.lines().iter().map(|p| p.plain.clone()).collect()
}

// Backlogs were pushed one source after another into one buffer, so a
// busy log evicted every line of the quiet one merged before it.
#[test]
fn a_backlog_that_fills_the_all_tab_leaves_the_other_processes_their_lines() {
    let dir = tempfile::tempdir().unwrap();
    let web = numbered_log(dir.path(), "web", 3);
    let api = numbered_log(dir.path(), "api", 30);
    let mut merged = MergedTail::new(vec![("web".into(), web), ("api".into(), api)], 10);
    merged.poll().unwrap();
    let plain = merged_plain(&merged);
    let from = |name: &str| plain.iter().filter(|l| l.starts_with(name)).count();
    assert_eq!(from("web"), 3, "{plain:?}");
    assert_eq!(from("api"), 7, "the rest of the buffer: {plain:?}");
    assert_eq!(plain.last().unwrap(), "api │ api 29", "the newest kept");
}

#[test]
fn two_backlogs_that_each_fill_the_all_tab_share_it_evenly() {
    let dir = tempfile::tempdir().unwrap();
    let web = numbered_log(dir.path(), "web", 30);
    let api = numbered_log(dir.path(), "api", 30);
    let mut merged = MergedTail::new(vec![("web".into(), web), ("api".into(), api)], 10);
    merged.poll().unwrap();
    let plain = merged_plain(&merged);
    let expected: Vec<String> = (25..30)
        .map(|i| format!("web │ web {i}"))
        .chain((25..30).map(|i| format!("api │ api {i}")))
        .collect();
    assert_eq!(plain, expected);
}

#[test]
fn a_process_that_joins_with_a_long_log_takes_only_its_share_of_the_all_tab() {
    let dir = tempfile::tempdir().unwrap();
    let web = numbered_log(dir.path(), "web", 4);
    let api = numbered_log(dir.path(), "api", 4);
    let mut merged = MergedTail::new(vec![("web".into(), web), ("api".into(), api)], 12);
    merged.poll().unwrap();
    let worker = numbered_log(dir.path(), "worker", 100);
    merged.add_source("worker".into(), worker);
    merged.poll().unwrap();
    let plain = merged_plain(&merged);
    let from = |name: &str| plain.iter().filter(|l| l.starts_with(name)).count();
    assert_eq!(from("worker"), 4, "a third of the buffer: {plain:?}");
    assert_eq!((from("web"), from("api")), (4, 4), "{plain:?}");
}

// Each source kept a buffer the size of the whole tab beside it, though
// the tab only ever reads back a source's newest line.
#[test]
fn the_all_tab_holds_each_line_it_merged_once() {
    let dir = tempfile::tempdir().unwrap();
    let web = numbered_log(dir.path(), "web", 30);
    let api = numbered_log(dir.path(), "api", 30);
    let mut merged = MergedTail::new(vec![("web".into(), web.clone()), ("api".into(), api)], 10);
    merged.poll().unwrap();
    assert_eq!(merged.held_by_sources(), [1, 1]);

    // And it reads on from there as it did.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&web)
        .and_then(|mut file| std::io::Write::write_all(&mut file, b"web 30\nweb 31\n"))
        .unwrap();
    merged.poll().unwrap();
    let plain = merged_plain(&merged);
    assert_eq!(plain.len(), 10, "{plain:?}");
    assert_eq!(plain[plain.len() - 2..], ["web │ web 30", "web │ web 31"]);
    assert_eq!(merged.held_by_sources(), [1, 1]);
}

// A restart is judged on the first line of the new run, which the share
// may leave out, and its marker is not evicted by the lines after it.
#[test]
fn a_restart_is_marked_when_the_all_tab_takes_only_part_of_the_new_run() {
    let dir = tempfile::tempdir().unwrap();
    let api = numbered_log(dir.path(), "api", 1);
    let web = numbered_log(dir.path(), "web", 1);
    let mut merged = MergedTail::new(
        vec![("api".into(), api.clone()), ("web".into(), web.clone())],
        10,
    );
    merged.poll().unwrap();
    let body: String = (0..8).map(|i| format!("api new {i}\n")).collect();
    std::fs::write(&api, body).unwrap();
    let more: String = (1..9).map(|i| format!("web {i}\n")).collect();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&web)
        .and_then(|mut file| std::io::Write::write_all(&mut file, more.as_bytes()))
        .unwrap();
    merged.poll().unwrap();
    let plain = merged_plain(&merged);
    let marker = format!("api │ {}", super::merged::RESTART_MARKER);
    assert_eq!(plain.len(), 10, "{plain:?}");
    assert_eq!(plain[0], marker, "{plain:?}");
    assert!(plain[1].starts_with("api │ api new"), "{plain:?}");
    assert_eq!(plain.last().unwrap(), "web │ web 8");
}

#[test]
fn a_json_block_in_the_all_tab_still_inspects_as_json() {
    let (_dir, mut app) = two_process_app();
    write_log(
        &app,
        "feat+one",
        "api",
        &["{", "  \"level\": \"error\",", "  \"msg\": \"boom\"", "}"],
    );
    write_log(&app, "feat+one", "web", &["web up"]);
    open_viewer(&mut app, 100, 30);
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('J'));
    let inspect = app.inspect.as_ref().expect("the overlay is open");
    assert!(
        inspect.text.contains("\"msg\": \"boom\""),
        "{}",
        inspect.text
    );
    assert!(!inspect.text.contains("api │"), "{}", inspect.text);
}

// ---- dogfood: a fresh project after `n` ------------------------------

// `new` writes a `pando.toml` with only `[project]` in it. That file
// existing, with no process named, was taken to mean "nothing to run",
// and every start of the fresh worktree was refused before it could ask
// for the command.
#[test]
fn a_config_file_with_no_process_still_lets_enter_start() {
    let dir = tempfile::tempdir().unwrap();
    let paths = PandoPaths::new(
        dir.path().join("home"),
        ProjectRef {
            id: "acme-shop-3f9a2c1d".into(),
            root: dir.path().join("acme-shop"),
            display_name: "acme-shop".into(),
        },
    );
    std::fs::create_dir_all(paths.config_file().parent().unwrap()).unwrap();
    std::fs::write(paths.config_file(), "[project]\nname = \"acme-shop\"\n").unwrap();
    let mut app = App::new_for_test(paths, Config::default(), vec![wt("feat+x")]);
    // What `n` sends once `new` has resolved: the config, still with no
    // process in it.
    app.handle_event(AppEvent::ConfigResolved(Box::default()));
    assert!(!app.nothing_to_run, "a file alone concludes nothing");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Start),
        "enter runs the start, and with it the question that finds the command"
    );
}

// Once a start has said so, a config that gains a process clears it.
#[test]
fn a_resolved_config_with_a_process_clears_nothing_to_run() {
    let mut app = test_app(&["feat+x"]);
    app.nothing_to_run = true;
    let mut config = Config::default();
    config.processes.insert(
        "dev".to_string(),
        toml::from_str("cmd = \"pnpm dev\"").unwrap(),
    );
    app.handle_event(AppEvent::ConfigResolved(Box::new(config)));
    assert!(!app.nothing_to_run);
}

// ---- dogfood: "none" in the question dialog --------------------------

fn a_question_that_allows_none() -> actions::Question {
    actions::Question {
        slot: crate::detect::Slot::SchemaHook,
        prompt: "Which command sets up the database schema?".to_string(),
        options: vec![("pnpm db:push".to_string(), "package.json".to_string())],
        preselect: Some(0),
        allow_custom: true,
        allow_none: true,
        multi: false,
        checked: Vec::new(),
        details: Vec::new(),
        answer_file: None,
        snippet: String::new(),
    }
}

#[test]
fn n_answers_none_when_the_question_allows_it() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_question_that_allows_none());
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(rx.try_recv().unwrap(), Ok(actions::Answer::None));
    assert!(app.modal.is_none());
}

#[test]
fn n_does_nothing_when_the_question_has_no_none() {
    let mut app = test_app(&["feat+one"]);
    let rx = open_question(&mut app, a_question());
    press(&mut app, KeyCode::Char('n'));
    assert!(rx.try_recv().is_err(), "no answer was sent");
    assert!(matches!(app.modal, Some(Modal::Question { .. })));
}

// With nothing to suggest the input line used to open at once, and it
// would take the `n` that says "none" as the first letter of a command.
#[test]
fn a_question_with_no_options_that_allows_none_waits_for_n_or_c() {
    let mut app = test_app(&["feat+one"]);
    let mut question = a_question_that_allows_none();
    question.options.clear();
    let rx = open_question(&mut app, question);
    assert!(matches!(
        app.modal,
        Some(Modal::Question { custom: None, .. })
    ));
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(rx.try_recv().unwrap(), Ok(actions::Answer::None));
}

// ---- dogfood: X lists only what is up --------------------------------

#[test]
fn x_leaves_out_worktrees_with_nothing_up() {
    let mut app = test_app(&["feat+m", "feat+second", "feat+db"]);
    // Only a failed process: nothing of it runs.
    with_process(
        &mut app,
        "feat+m",
        Phase::Failed {
            reason: "exit 1".into(),
            at: Utc::now(),
        },
    );
    // A record left behind by a stop — of an isolated worktree, which
    // keeps the record naming its compose project until `rm`.
    let mut stopped = WorktreeRecord::new("/trees/feat+second", true);
    stopped.services.push(crate::state::ServiceRecord {
        name: "postgres".into(),
        kind: crate::state::ServiceKind::Compose,
        port: None,
        pid: None,
        pgid: None,
        compose_project: Some("pando-x-feat_second".into()),
    });
    app.state
        .worktrees
        .insert("feat+second".to_string(), stopped);
    press(&mut app, KeyCode::Char('X'));
    assert!(app.modal.is_none(), "nothing to confirm");
    assert_eq!(app.active_status(), Some(("nothing is running", false)));

    // A database still up behind a crashed dev server is something.
    let mut record = WorktreeRecord::new("/trees/feat+db", true);
    record.services.push(crate::state::ServiceRecord {
        name: "postgres".into(),
        kind: crate::state::ServiceKind::Native,
        port: Some(17_004),
        pid: Some(1),
        pgid: Some(Group::from_raw(1)),
        compose_project: None,
    });
    app.state.worktrees.insert("feat+db".to_string(), record);
    assert_eq!(app.stop_all_targets(), vec!["feat+db".to_string()]);
}

// ---- dogfood: a process that dies says so ----------------------------

fn fail_process(state: &mut State, name: &str, process: &str, reason: &str) {
    state
        .worktrees
        .get_mut(name)
        .unwrap()
        .processes
        .get_mut(process)
        .unwrap()
        .phase = Phase::Failed {
        at: Utc::now(),
        reason: reason.into(),
    };
}

#[test]
fn a_process_that_dies_after_ready_says_so() {
    let mut app = test_app(&["feat+m"]);
    with_process(&mut app, "feat+m", running_phase());
    with_second_process(&mut app, "feat+m", "api", running_phase());
    app.set_success("feat/m is ready");
    let mut state = app.state.clone();
    fail_process(&mut state, "feat+m", "api", "exited with status 1");
    app.handle_event(refreshed(state.clone()));
    let status = app.flash().expect("a flash");
    assert!(status.is_error());
    assert_eq!(
        status.message,
        "api of feat/m exited — exited with status 1"
    );
    assert_eq!(status.about.as_deref(), Some("feat+m"));

    // Said once: the next refresh finds it already failed.
    let before = app.messages.len();
    app.handle_event(refreshed(state));
    assert_eq!(app.messages.len(), before);
}

// A discovery runs the full refresh itself, so on the ticks it replaces
// the quick one it is the read that finds a process dead. It replaced the
// state without a word, and the refresh after it saw nothing change.
#[test]
fn a_death_that_a_discovery_finds_is_said_too() {
    let mut app = test_app(&["feat+m"]);
    with_process(&mut app, "feat+m", running_phase());
    let mut state = app.state.clone();
    fail_process(&mut state, "feat+m", "dev", "exited with status 1");
    let snapshot = |state: State| Snapshot {
        main: wt("acme-shop"),
        worktrees: vec![wt("feat+m")],
        created_by_pando: BTreeMap::from([("feat+m".to_string(), true)]),
        state,
        warning: None,
        notices: Vec::new(),
        default_base: Some("main".into()),
    };
    app.handle_event(AppEvent::Discovered(Box::new(Ok(snapshot(state.clone())))));
    let status = app.flash().expect("a flash");
    assert_eq!(
        status.message,
        "dev of feat/m exited — exited with status 1"
    );

    // And once: neither the refresh after it, nor an older discovery that
    // lands late with the process still up, says it again.
    let before = app.messages.len();
    app.handle_event(refreshed(state.clone()));
    let mut stale = state.clone();
    stale
        .worktrees
        .get_mut("feat+m")
        .unwrap()
        .processes
        .get_mut("dev")
        .unwrap()
        .phase = running_phase();
    app.handle_event(AppEvent::Discovered(Box::new(Ok(snapshot(stale)))));
    app.handle_event(refreshed(state));
    assert_eq!(app.messages.len(), before, "{:?}", app.messages);
}

// A start waiting to say "ready" that sees a process die says which, and
// only once.
#[test]
fn a_death_during_a_start_is_said_once_by_name() {
    let mut app = test_app(&["feat+m"]);
    with_process(&mut app, "feat+m", Phase::Starting { since: Utc::now() });
    app.awaiting_ready = Some(AwaitingReady {
        name: "feat+m".into(),
        url: None,
        spawned: vec![("dev".into(), 4242)],
        seen: false,
    });
    let mut state = app.state.clone();
    fail_process(&mut state, "feat+m", "dev", "boom");
    let before = app.messages.len();
    app.handle_event(refreshed(state));
    assert_eq!(app.messages.len(), before + 1);
    assert_eq!(app.flash().unwrap().message, "dev of feat/m exited — boom");
    assert!(app.awaiting_ready.is_none());
}

// ---- dogfood: an error about one worktree leaves with the cursor ------

#[test]
fn an_error_about_a_worktree_clears_when_the_cursor_leaves_it() {
    let mut app = test_app(&["feat+m", "feat+second"]);
    press(&mut app, KeyCode::Char('j'));
    app.set_error_about("feat+second", "could not start feat/second: boom");
    press(&mut app, KeyCode::Char('j'));
    assert!(app.flash().is_some(), "still on feat/second: it stays");
    press(&mut app, KeyCode::Char('k'));
    assert!(app.flash().is_none(), "feat/second's error is not feat/m's");
    assert!(
        app.messages.iter().any(|m| m.message.contains("boom")),
        "m keeps it"
    );
    // An error about nothing in particular stays wherever the cursor goes.
    app.set_error("refresh failed: disk full");
    press(&mut app, KeyCode::Char('j'));
    assert!(app.flash().is_some());
}

// ---- dogfood: help scrolls with g and G ------------------------------

#[test]
fn g_and_capital_g_scroll_help_rather_than_closing_it() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('?'));
    app.help_scroll_max = 20;
    press(&mut app, KeyCode::Char('G'));
    assert!(matches!(app.modal, Some(Modal::Help)), "G keeps help open");
    assert_eq!(app.help_scroll, 20, "at the bottom");
    press(&mut app, KeyCode::Char('k'));
    assert_eq!(app.help_scroll, 19, "and k moves at once from there");
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.help_scroll, 0);
    assert!(matches!(app.modal, Some(Modal::Help)));
}

// The overlay's scroll keys are a table too: each keeps help open, and
// every other key closes it.
#[test]
fn the_overlay_answers_exactly_the_scroll_keys_it_documents() {
    let documented: Vec<KeyCode> = OVERLAY_KEYS
        .iter()
        .flat_map(|k| k.codes.iter().copied())
        .collect();
    for code in candidate_keys() {
        let mut app = test_app(&["feat+one"]);
        press(&mut app, KeyCode::Char('?'));
        press(&mut app, code);
        let open = matches!(app.modal, Some(Modal::Help));
        assert_eq!(open, documented.contains(&code), "{code:?}");
    }
}

// ---- dogfood: uptime from the oldest process -------------------------

// A web server up and the api beside it silent: said once in the header,
// not on every refresh.
#[test]
fn a_silent_port_is_announced_once() {
    let mut app = test_app(&["feat+one"]);
    let mut state = State::new();
    let mut record = crate::state::WorktreeRecord::new("/trees/feat+one", true);
    let since = Utc::now() - chrono::Duration::minutes(1);
    record.processes.insert(
        "dev".to_string(),
        ProcessRecord {
            pid: 4242,
            pgid: Group::from_raw(4242),
            started_at: since,
            log_path: PathBuf::from("/does/not/exist/dev.log"),
            ready_port: Some(17_342),
            ready_timeout_s: None,
            observed_ports: vec![17_342],
            swept: false,
            phase: Phase::Running { since },
        },
    );
    record.ports.insert("web".to_string(), 17_342);
    record.ports.insert("api".to_string(), 17_343);
    record.roles.insert(
        "dev".to_string(),
        vec!["api".to_string(), "web".to_string()],
    );
    record.observed_ports = vec![17_342];
    state.worktrees.insert("feat+one".to_string(), record);
    app.adopt_state(state.clone());
    let (message, is_error) = app.active_status().expect("announced");
    assert!(
        is_error && message.contains("nothing listens on api's port 17343"),
        "{message}"
    );
    app.status = None;
    app.adopt_state(state);
    assert!(app.status.is_none(), "said once");
}

#[test]
fn a_worktree_is_up_since_its_oldest_running_process() {
    let mut app = test_app(&["feat+m"]);
    let two_minutes_ago = Utc::now() - chrono::Duration::minutes(2);
    with_process(
        &mut app,
        "feat+m",
        Phase::Running {
            since: two_minutes_ago,
        },
    );
    // `P` just restarted the other one.
    with_second_process(&mut app, "feat+m", "worker", running_phase());
    assert_eq!(app.up_since("feat+m"), Some(two_minutes_ago));
}

// ---- the one-second refresh skips the socket scan when it cannot matter --

#[test]
fn a_refresh_with_everything_running_and_alive_has_nothing_to_advance() {
    let mut app = test_app(&["feat+a", "feat+b"]);
    with_process(&mut app, "feat+a", running_phase());
    with_second_process(&mut app, "feat+a", "api", running_phase());
    with_process(&mut app, "feat+b", running_phase());
    with_share(&mut app, "feat+b", Some(18_000));
    assert!(!background::needs_advance(&app.state, |_| true, |_| false));
}

#[test]
fn a_starting_process_always_takes_the_full_refresh() {
    // The scan is how a starting process becomes running.
    let mut app = test_app(&["feat+a"]);
    with_process(&mut app, "feat+a", Phase::Starting { since: Utc::now() });
    assert!(background::needs_advance(&app.state, |_| true, |_| false));
}

#[test]
fn any_dead_pid_the_state_vouches_for_takes_the_full_refresh() {
    let mut app = test_app(&["feat+a"]);
    with_process(&mut app, "feat+a", running_phase());
    with_second_process(&mut app, "feat+a", "api", running_phase());
    // The second process (4343) died.
    assert!(background::needs_advance(
        &app.state,
        |pid| pid != 4343,
        |_| false
    ));

    let mut app = test_app(&["feat+a"]);
    with_process(&mut app, "feat+a", running_phase());
    with_share(&mut app, "feat+a", Some(18_000));
    assert!(background::needs_advance(
        &app.state,
        |pid| pid != 5151,
        |_| false
    ));
    assert!(background::needs_advance(
        &app.state,
        |pid| pid != 5252,
        |_| false
    ));

    let mut app = test_app(&["feat+a"]);
    with_process(&mut app, "feat+a", running_phase());
    let record = app.state.worktrees.get_mut("feat+a").unwrap();
    record.services.push(crate::state::ServiceRecord {
        name: "postgres".into(),
        kind: crate::state::ServiceKind::Native,
        port: Some(15_432),
        pid: Some(6161),
        pgid: Some(Group::from_raw(6161)),
        compose_project: None,
    });
    assert!(!background::needs_advance(&app.state, |_| true, |_| false));
    assert!(background::needs_advance(
        &app.state,
        |pid| pid != 6161,
        |_| false
    ));
}

#[test]
fn a_portless_process_whose_leader_backgrounded_it_is_not_a_reason_to_scan() {
    // `cmd = "./bin/worker &"`: the shell leader is gone for as long as
    // the worker runs, and advancing keeps it Running by its group.
    let mut app = test_app(&["feat+a"]);
    with_process(&mut app, "feat+a", running_phase());
    let record = app.state.worktrees.get_mut("feat+a").unwrap();
    record.processes.get_mut("dev").unwrap().ready_port = None;
    assert!(!background::needs_advance(&app.state, |_| false, |_| true));
    // Its group gone too: advancing fails it, so the full path runs.
    assert!(background::needs_advance(&app.state, |_| false, |_| false));
}

#[test]
fn a_ported_process_whose_leader_died_takes_the_full_refresh_whatever_its_group() {
    // An orphan holding the group open is how a crashed server looks, and
    // advancing fails it.
    let mut app = test_app(&["feat+a"]);
    with_process(&mut app, "feat+a", running_phase());
    assert!(background::needs_advance(&app.state, |_| false, |_| true));
}

#[test]
fn a_dead_compose_log_pump_is_not_a_reason_to_scan() {
    // Its container stopped under it; the refresh has nothing to do about
    // that, and would be asked to every second until the next mutation.
    let mut app = test_app(&["feat+a"]);
    with_process(&mut app, "feat+a", running_phase());
    let record = app.state.worktrees.get_mut("feat+a").unwrap();
    record.services.push(crate::state::ServiceRecord {
        name: "postgres".into(),
        kind: crate::state::ServiceKind::Compose,
        port: Some(15_432),
        pid: Some(7171),
        pgid: Some(Group::from_raw(7171)),
        compose_project: Some("pando-feat-a".into()),
    });
    assert!(!background::needs_advance(
        &app.state,
        |pid| pid != 7171,
        |_| false
    ));
}

#[test]
fn a_failed_process_whose_pid_is_gone_is_not_a_reason_to_scan() {
    // Failed is the phase that outlives its process; nothing advances it.
    let mut app = test_app(&["feat+a"]);
    with_process(
        &mut app,
        "feat+a",
        Phase::Failed {
            at: Utc::now(),
            reason: "exited".into(),
        },
    );
    assert!(!background::needs_advance(&app.state, |_| false, |_| false));
}

#[test]
fn the_gated_refresh_reads_a_quiet_state_as_is_and_advances_a_death() {
    let (_dir, mut app) = app_with_logs(&["feat+a"]);
    // A pid that is certainly this test's own, and one past any pid a
    // system hands out.
    let alive = std::process::id();
    let dead = i32::MAX as u32 - 1;

    with_process(&mut app, "feat+a", running_phase());
    let record = app.state.worktrees.get_mut("feat+a").unwrap();
    let process = record.processes.get_mut("dev").unwrap();
    process.pid = alive;
    // No group of that id: were the full refresh taken, nothing would
    // answer for it, which is what makes the skip visible.
    process.pgid = Group::from_raw(i32::MAX - 11);
    std::fs::create_dir_all(app.paths.state_file().parent().unwrap()).unwrap();
    crate::state::save(&app.paths.state_file(), &app.state).unwrap();
    let quiet = background::refresh_if_needed(&app.paths);
    assert!(!quiet.ran);
    assert_eq!(quiet.refreshed.state, app.state);
    assert!(quiet.refreshed.warning.is_none());

    let record = app.state.worktrees.get_mut("feat+a").unwrap();
    record.processes.get_mut("dev").unwrap().pid = dead;
    crate::state::save(&app.paths.state_file(), &app.state).unwrap();
    let advanced = background::refresh_if_needed(&app.paths);
    assert!(advanced.ran);
    assert!(matches!(
        advanced.refreshed.state.worktrees["feat+a"].processes["dev"].phase,
        Phase::Failed { .. }
    ));
}

// The answer arrives from a worker; a repeat of the same answer is not a
// reason to repaint.
#[test]
fn the_gh_account_answer_is_kept_and_repaints_only_on_a_change() {
    use crate::worktree::GhAccount;
    let mut app = test_app(&["feat+one"]);
    assert_eq!(app.gh_account, None, "unknown until gh answers");
    let login = GhAccount::Login("octocat".into());
    assert!(app.handle_event(AppEvent::GhAccountReady(login.clone())));
    assert_eq!(app.gh_account, Some(login.clone()));
    assert!(!app.handle_event(AppEvent::GhAccountReady(login)));
    assert!(app.handle_event(AppEvent::GhAccountReady(GhAccount::SignedOut)));
}

// ---- the theme picker --------------------------------------------------

#[test]
fn capital_t_opens_the_theme_picker_on_the_theme_in_use() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('T'));
    match app.modal {
        Some(Modal::Theme { selected, .. }) => {
            assert_eq!(app.theme.themes[selected].name, app.theme.name);
        }
        ref other => panic!("{other:?}"),
    }
}

#[test]
fn moving_in_the_picker_repaints_with_that_theme_and_esc_puts_it_back() {
    let mut app = test_app(&["feat+one"]);
    let before = crate::theme::palette();
    press(&mut app, KeyCode::Char('T'));
    press(&mut app, KeyCode::Char('j'));
    let Some(Modal::Theme { selected, .. }) = app.modal else {
        panic!("still open");
    };
    let previewed = app.theme.themes[selected].palette(app.theme.appearance);
    assert_eq!(crate::theme::palette(), previewed, "the screen shows it");
    assert_ne!(previewed, before);
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
    assert_eq!(
        crate::theme::palette(),
        before,
        "and esc puts back what was"
    );
    assert_eq!(
        app.theme.name,
        crate::theme::DEFAULT_THEME,
        "nothing chosen"
    );
}

#[test]
fn enter_in_the_picker_keeps_the_theme_and_hands_it_to_the_watcher() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('T'));
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Enter);
    let last = app.theme.themes.last().unwrap().clone();
    assert!(app.modal.is_none());
    assert_eq!(app.theme.name, last.name);
    assert_eq!(crate::theme::palette(), last.palette(app.theme.appearance));
    assert_eq!(
        app.theme.settings.lock().unwrap().theme.as_deref(),
        Some(last.name.as_str()),
        "the watcher compares against the choice, not the old config"
    );
    assert!(app.active_status().unwrap().0.contains(&last.name));
}

#[test]
fn a_theme_is_saved_to_the_ui_section_and_nothing_else_moves() {
    let mut doc: toml_edit::DocumentMut = "# mine\n[runtime]\nversion_manager = \"mise\"\n"
        .parse()
        .unwrap();
    super::themes::set_theme(&mut doc, "gruvbox");
    let text = doc.to_string();
    assert!(
        text.starts_with("# mine\n[runtime]\nversion_manager = \"mise\"\n"),
        "{text}"
    );
    assert!(text.contains("[ui]\ntheme = \"gruvbox\""), "{text}");
    super::themes::set_theme(&mut doc, "github");
    assert_eq!(doc.to_string().matches("theme =").count(), 1);
}

// A slot to free is chosen from the list: `c` types nothing, enter sends
// the choice, and `n` frees none.
#[test]
fn the_free_slot_question_takes_a_choice_and_nothing_typed() {
    let (reply, answers) = std::sync::mpsc::channel();
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Question {
        question: actions::Question {
            slot: crate::detect::Slot::FreeSlot,
            prompt: "Which stopped worktree gives up its slot?".into(),
            options: vec![
                ("feat+old".into(), "slot 1".into()),
                ("feat+older".into(), "slot 2".into()),
            ],
            preselect: None,
            allow_custom: false,
            allow_none: true,
            multi: false,
            checked: Vec::new(),
            details: Vec::new(),
            answer_file: None,
            snippet: String::new(),
        },
        selected: 0,
        custom: None,
        reply,
    });
    press(&mut app, KeyCode::Char('c'));
    assert!(
        matches!(&app.modal, Some(Modal::Question { custom: None, .. })),
        "nothing to type"
    );
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    assert_eq!(answers.try_recv().unwrap(), Ok(actions::Answer::Choice(1)));
}

// ---- the setup screen ---------------------------------------------------

use crate::setup::{CheckOutcome, CheckRecord, FailureKind, RanBy, SetupMemory, SetupState};

/// A new project's TUI with the setup screen up: a real repository with a
/// worktree listed, a real pando home, nothing configured. The receiver
/// is the test's, so the workers' answers are handed over by `settle`.
fn setup_app() -> (tempfile::TempDir, App, Receiver<AppEvent>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    let paths = PandoPaths::new(
        dir.path().join("pando-home"),
        ProjectRef::from_root(&root).unwrap(),
    );
    let mut app = App::new_for_test(paths, Config::default(), vec![wt("feat+one")]);
    let rx = app.event_rx.take().expect("the app owns its receiver");
    app.open_setup_if_new();
    assert!(app.setup_screen.is_some(), "a new project gets the screen");
    (dir, app, rx)
}

fn screen(app: &App) -> &SetupScreen {
    app.setup_screen.as_ref().expect("the setup screen is up")
}

/// Ticks, handing the workers' answers to the app, until `done`.
fn tick_until(app: &mut App, rx: &Receiver<AppEvent>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.handle_event(AppEvent::Tick);
        while let Ok(event) = rx.recv_timeout(Duration::from_millis(20)) {
            app.handle_event(event);
        }
        if done(app) {
            return;
        }
        assert!(Instant::now() < deadline, "never saw {what}");
    }
}

/// Writes a file and gives it a modification time no earlier write had,
/// so a filesystem that keeps whole seconds cannot hide the change.
fn write_seen(path: &std::path::Path, text: &str) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1_000_000);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
    let at = std::time::UNIX_EPOCH + Duration::from_secs(NEXT.fetch_add(1, Ordering::SeqCst));
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(at)
        .unwrap();
}

/// What `init --answers` leaves: a process to run.
fn save_settings(app: &mut App, rx: &Receiver<AppEvent>) {
    write_seen(
        &app.paths.config_file(),
        "[processes.web]\ncmd = \"pnpm dev\"\n",
    );
    tick_until(app, rx, "the settings", |app| {
        has_settings(&screen(app).config)
    });
}

/// A check's record, against the settings the screen has now.
fn record(app: &App, outcome: CheckOutcome, ran_by: RanBy) -> CheckRecord {
    let fingerprint = crate::setup::fingerprint(&screen(app).config);
    let mut record = CheckRecord::begin(fingerprint.clone(), ran_by);
    if outcome != CheckOutcome::Running {
        record.fingerprint_after = Some(fingerprint);
        record.finished_at = Some(Utc::now());
    }
    record.outcome = outcome;
    record
}

fn save_record(app: &App, record: &CheckRecord) {
    write_seen(
        &app.paths.check_file(),
        &serde_json::to_string(record).unwrap(),
    );
}

// The first `pando` in a new project is the setup screen, even when git
// already lists worktrees; the dashboard's welcome needed none.
#[test]
fn a_new_project_opens_the_setup_screen_whatever_git_lists() {
    let (_dir, mut app, rx) = setup_app();
    assert_eq!(app.worktrees.len(), 1);
    assert_eq!(screen(&app).line(), SetupLine::Reading);
    tick_until(&mut app, &rx, "detection", |app| {
        screen(app).detected.is_some()
    });
    assert_eq!(screen(&app).line(), SetupLine::Waiting);
}

// A project that has something to run is untested, never new: it is
// never sent back to the setup screen.
#[test]
fn a_configured_project_never_sees_the_setup_screen() {
    let (_dir, mut app) = a_setup_screen_every_key_can_act_on();
    app.setup_screen = None;
    app.open_setup_if_new();
    assert!(app.setup_screen.is_none());
}

// The screen moves with pando's own files: the agent's `init --answers`
// is a changed pando.toml, seen on the tick.
#[test]
fn saved_settings_show_on_the_next_tick_and_offer_a_test() {
    let (_dir, mut app, rx) = setup_app();
    assert!(!screen(&app).may_test(), "nothing to test yet");
    save_settings(&mut app, &rx);
    assert_eq!(screen(&app).line(), SetupLine::SettingsSaved);
    assert_eq!(screen(&app).setup.state, SetupState::Untested);
    assert!(screen(&app).may_test());
}

// A committed pando.toml is one of the four files: a teammate's settings
// arriving with a pull is a setup too.
#[test]
fn a_committed_config_is_watched_too() {
    let (_dir, mut app, rx) = setup_app();
    write_seen(
        &app.paths.root().join("pando.toml"),
        "[processes.web]\ncmd = \"pnpm dev\"\n",
    );
    tick_until(&mut app, &rx, "the committed settings", |app| {
        screen(app).line() == SetupLine::SettingsSaved
    });
}

// While a check holds the lock, the screen shows the check's own lines,
// and it stays up as the check passes: the ready view, not the dashboard.
#[test]
fn a_running_check_shows_its_lines_and_a_pass_turns_the_screen_ready() {
    let (_dir, mut app, rx) = setup_app();
    save_settings(&mut app, &rx);
    let held = crate::state::lock(&app.paths.check_lock_file()).unwrap();
    let mut running = record(&app, CheckOutcome::Running, RanBy::Program);
    running.progress = vec!["made a test worktree".into(), "installing".into()];
    save_record(&app, &running);
    tick_until(&mut app, &rx, "the check running", |app| {
        matches!(screen(app).line(), SetupLine::Testing(_))
    });
    assert_eq!(
        screen(&app).line(),
        SetupLine::Testing(vec!["made a test worktree".into(), "installing".into()])
    );
    assert!(!screen(&app).may_test(), "no second check while one runs");

    save_record(&app, &record(&app, CheckOutcome::Passed, RanBy::Program));
    drop(held);
    tick_until(&mut app, &rx, "the pass", |app| screen(app).is_ready());
    assert_eq!(screen(&app).line(), SetupLine::Passed);
}

// `⏎` on the ready view opens the dashboard on the settings the files
// hold now, and forgets what a start concluded from the old ones.
#[test]
fn enter_on_the_ready_view_opens_pando_with_the_settings_read_since() {
    let (_dir, mut app, rx) = setup_app();
    app.nothing_to_run = true;
    save_settings(&mut app, &rx);
    save_record(&app, &record(&app, CheckOutcome::Passed, RanBy::Program));
    tick_until(&mut app, &rx, "the pass", |app| screen(app).is_ready());
    assert!(
        app.config.processes.is_empty(),
        "adopted on leaving, not before"
    );
    press(&mut app, KeyCode::Enter);
    assert!(app.setup_screen.is_none());
    assert!(app.config.processes.contains_key("web"));
    assert!(!app.nothing_to_run);
}

// A failure says why. "Your agent is probably on it" only when a program
// ran the check.
#[test]
fn a_failed_check_says_why_and_whether_an_agent_ran_it() {
    for (ran_by, by_program) in [(RanBy::Program, true), (RanBy::Terminal, false)] {
        let (_dir, mut app, rx) = setup_app();
        save_settings(&mut app, &rx);
        let failed = CheckOutcome::Failed {
            kind: FailureKind::Settings,
            reason: "web exited after 0.8s".into(),
        };
        save_record(&app, &record(&app, failed, ran_by));
        tick_until(&mut app, &rx, "the failure", |app| {
            matches!(screen(app).line(), SetupLine::Failed { .. })
        });
        assert_eq!(
            screen(&app).line(),
            SetupLine::Failed {
                reason: "web exited after 0.8s".into(),
                kind: FailureKind::Settings,
                by_program,
            }
        );
        assert!(app.setup_screen.is_some(), "the screen stays up");
    }
}

// A check killed outright only lets go of its lock: its record still says
// running. The lock is asked on the tick, so the screen says interrupted
// without the record changing.
#[test]
fn a_check_that_lets_go_of_its_lock_unfinished_is_interrupted() {
    let (_dir, mut app, rx) = setup_app();
    save_settings(&mut app, &rx);
    let held = crate::state::lock(&app.paths.check_lock_file()).unwrap();
    save_record(&app, &record(&app, CheckOutcome::Running, RanBy::Tui));
    tick_until(&mut app, &rx, "the check running", |app| {
        matches!(screen(app).line(), SetupLine::Testing(_))
    });
    drop(held);
    tick_until(&mut app, &rx, "the interruption", |app| {
        screen(app).line() == SetupLine::Interrupted
    });
    assert!(screen(&app).may_test(), "v tests again");
}

// A check that exits 3 names the question still open.
#[test]
fn a_check_with_a_question_open_names_it() {
    let (_dir, mut app, rx) = setup_app();
    save_settings(&mut app, &rx);
    let open = CheckOutcome::NotSetUp {
        slot: "dev_cmd".into(),
    };
    save_record(&app, &record(&app, open, RanBy::Program));
    tick_until(&mut app, &rx, "the open question", |app| {
        matches!(screen(app).line(), SetupLine::NotSetUp { .. })
    });
    assert_eq!(
        screen(&app).line(),
        SetupLine::NotSetUp {
            slot: "dev_cmd".into()
        }
    );
}

// A settings file somebody is halfway through writing does not load: the
// last good settings stay, and `m` has why.
#[test]
fn settings_that_do_not_load_keep_the_last_good_ones() {
    let (_dir, mut app, rx) = setup_app();
    save_settings(&mut app, &rx);
    write_seen(&app.paths.config_file(), "[processes.web\n");
    tick_until(&mut app, &rx, "the error", |app| {
        app.flash().is_some_and(|s| s.is_error())
    });
    assert!(has_settings(&screen(&app).config));
    assert!(
        app.active_status()
            .unwrap()
            .0
            .contains("the settings do not read"),
        "{:?}",
        app.active_status()
    );
}

// `esc` is the skip, remembered in setup.json: the next `pando` in the
// project opens the dashboard.
#[test]
fn esc_is_remembered_and_the_screen_never_comes_back() {
    let (_dir, mut app, _rx) = setup_app();
    press(&mut app, KeyCode::Esc);
    assert!(app.setup_screen.is_none(), "straight to the dashboard");
    assert!(!app.should_quit);
    assert!(SetupMemory::load(&app.paths).skipped_at.is_some());

    let mut next = App::new_for_test(app.paths.clone(), Config::default(), Vec::new());
    next.open_setup_if_new();
    assert!(
        next.setup_screen.is_none(),
        "a skipped project is not asked again"
    );
}

// A library has nothing to run and never will: pando's own guess says
// so, every way off the screen still works, and nothing offers to test
// it.
#[test]
fn a_library_is_never_stuck() {
    let (_dir, mut app, rx) = setup_app();
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.checks_started, 0, "nothing to test");
    assert!(app.flash().is_some_and(|s| s.is_error()));
    press(&mut app, KeyCode::Enter);
    tick_until(&mut app, &rx, "the guess", |app| {
        screen(app).line() == SetupLine::OwnGuess(Trying::CannotTell)
    });
    assert_eq!(app.checks_started, 0, "nothing to test");
    press(&mut app, KeyCode::Esc);
    assert!(app.setup_screen.is_none(), "esc opens the dashboard");

    let (_dir, mut app, _rx) = setup_app();
    press(&mut app, KeyCode::Char('q'));
    assert!(app.should_quit, "q quits");
}

// `v` starts `pando check` apart from the TUI, once there is something
// to test, and not a second one while the first is on its way. The
// single action slot is never the check's.
#[test]
fn v_starts_one_check_once_settings_exist() {
    let (_dir, mut app, rx) = setup_app();
    save_settings(&mut app, &rx);
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.checks_started, 1);
    assert!(app.pending.is_none(), "the action slot stays free");
    assert_eq!(screen(&app).line(), SetupLine::Starting);
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.checks_started, 1, "one at a time");

    // The check's record, once written, is what the screen shows.
    let _held = crate::state::lock(&app.paths.check_lock_file()).unwrap();
    save_record(&app, &record(&app, CheckOutcome::Running, RanBy::Tui));
    tick_until(&mut app, &rx, "the check running", |app| {
        matches!(screen(app).line(), SetupLine::Testing(_))
    });
    assert!(screen(&app).check_requested.is_none());
}

#[test]
fn a_copies_the_setup_prompt() {
    let (_dir, mut app, _rx) = setup_app();
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.clipboard.as_deref(), Some(crate::setup::SETUP_PROMPT));
    assert!(app.setup_screen.is_some(), "copying leaves the screen up");
}

/// A setup screen every one of its keys can act on: settings saved and
/// no check running.
fn a_setup_screen_every_key_can_act_on() -> (tempfile::TempDir, App) {
    app_on_setup_screen(true)
}

/// An app with the setup screen up, with a process to run or with none.
/// No repository: nothing here reads git, and the detection it starts
/// finds an empty directory.
pub fn app_on_setup_screen(settings: bool) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let paths = PandoPaths::new(
        dir.path().join("home"),
        ProjectRef {
            id: "acme-shop-3f9a2c1d".into(),
            root: dir.path().join("acme-shop"),
            display_name: "acme-shop".into(),
        },
    );
    let mut config = Config::default();
    if settings {
        config.processes.insert(
            "web".into(),
            crate::config::ProcessConfig {
                cmd: "pnpm dev".into(),
                ..Default::default()
            },
        );
    }
    let mut app = App::new_for_test(paths, config, vec![wt("feat+one")]);
    let setup = crate::setup::read(&app.paths, &app.config);
    app.open_setup(setup);
    (dir, app)
}

fn setup_fingerprint(app: &App) -> String {
    format!(
        "{}|{:?}|{:?}|{:?}|{}|{}",
        app.setup_screen.is_some(),
        app.modal.as_ref().map(std::mem::discriminant),
        app.status.as_ref().map(|s| s.message.clone()),
        app.clipboard,
        app.checks_started,
        app.should_quit,
    )
}

// Help on the setup screen lists its own keys: exactly the ones it
// answers, and none of the dashboard's.
#[test]
fn help_lists_exactly_the_keys_the_setup_screen_answers() {
    let documented: Vec<KeyCode> = SETUP_KEYS
        .iter()
        .flat_map(|k| k.codes.iter().copied())
        .collect();
    let mut answered = Vec::new();
    for code in candidate_keys() {
        let (_dir, mut app) = a_setup_screen_every_key_can_act_on();
        let before = setup_fingerprint(&app);
        press(&mut app, code);
        if setup_fingerprint(&app) != before {
            answered.push(code);
        }
    }
    for code in &answered {
        assert!(
            documented.contains(code),
            "{code:?} does something on the setup screen, and help does not say so"
        );
    }
    for code in &documented {
        assert!(
            answered.contains(code),
            "help lists {code:?}, and pressing it on the setup screen does nothing"
        );
    }
}

// ---- the dashboard's setup line, and a and v from the list ----------------

// `a` on the list copies the setup prompt: the header's hint and the
// failing line both offer it.
#[test]
fn a_on_the_list_copies_the_setup_prompt() {
    let mut app = test_app(&["feat+one"]);
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.clipboard.as_deref(), Some(crate::setup::SETUP_PROMPT));
}

// `v` on the list starts one check, only when there is something to
// test, and not while one runs; the action slot is never the check's.
#[test]
fn v_on_the_list_starts_one_check_when_there_are_settings() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(
        app.checks_started, 0,
        "a process with no command runs nothing"
    );
    assert!(app.flash().is_some_and(|s| s.is_error()));

    let (_dir, mut app) = a_setup_screen_every_key_can_act_on();
    app.setup_screen = None;
    app.read_setup_row();
    assert_eq!(
        app.setup_row.hint(),
        Some(SetupHint::Note {
            text: "not tested yet · v tests it".into(),
            failed: false
        })
    );
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.checks_started, 1);
    assert!(app.pending.is_none(), "the action slot stays free");
    assert_eq!(app.setup_row.hint(), Some(SetupHint::Starting));
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.checks_started, 1, "one at a time");

    // A check someone else started holds the lock: refused too.
    app.setup_row.check_requested = None;
    let _held = crate::state::lock(&app.paths.check_lock_file()).unwrap();
    app.read_setup_row();
    assert!(matches!(app.setup_row.hint(), Some(SetupHint::Testing(_))));
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(app.checks_started, 1, "refused while a check runs");
}

// The dashboard follows a check as the setup screen does: its record's
// latest line while it runs, gone once it passes, and a line in `m` to
// say so.
#[test]
fn the_dashboard_follows_a_check_on_the_tick() {
    let (_dir, mut app) = a_setup_screen_every_key_can_act_on();
    app.setup_screen = None;
    app.read_setup_row();
    let fingerprint = crate::setup::fingerprint(&app.config);
    let held = crate::state::lock(&app.paths.check_lock_file()).unwrap();
    let mut running = CheckRecord::begin(fingerprint.clone(), RanBy::Terminal);
    running.progress = vec!["made a test worktree".into(), "installing".into()];
    save_record(&app, &running);
    app.handle_event(AppEvent::Tick);
    assert_eq!(
        app.setup_row.hint(),
        Some(SetupHint::Testing(Some("installing".into())))
    );

    let mut passed = running.clone();
    passed.outcome = CheckOutcome::Passed;
    passed.fingerprint_after = Some(fingerprint);
    passed.finished_at = Some(Utc::now());
    save_record(&app, &passed);
    drop(held);
    // Ticked until the lock reads free: a test beside this one forking a
    // child holds a copy of its descriptor until the child execs.
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.setup_row.hint().is_some() && Instant::now() < deadline {
        app.handle_event(AppEvent::Tick);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.setup_row.hint(), None, "ready says nothing");
    assert!(
        app.active_status()
            .is_some_and(|(said, _)| said.contains("the test passed")),
        "{:?}",
        app.active_status()
    );
}

// A config a worker resolved is a new fingerprint: the line is read again
// against it, so a passed test on other settings reads as stale.
#[test]
fn a_config_change_rereads_the_setup_line() {
    let (_dir, mut app) = a_setup_screen_every_key_can_act_on();
    app.setup_screen = None;
    app.read_setup_row();
    let fingerprint = crate::setup::fingerprint(&app.config);
    let mut passed = CheckRecord::begin(fingerprint.clone(), RanBy::Terminal);
    passed.outcome = CheckOutcome::Passed;
    passed.fingerprint_after = Some(fingerprint);
    passed.finished_at = Some(Utc::now());
    save_record(&app, &passed);
    app.handle_event(AppEvent::Tick);
    assert_eq!(app.setup_row.hint(), None);

    let mut config = app.config.clone();
    config.processes.get_mut("web").unwrap().cmd = "pnpm start".into();
    app.handle_event(AppEvent::ConfigResolved(Box::new(config)));
    assert_eq!(
        app.setup_row.hint(),
        Some(SetupHint::Note {
            text: "settings changed since the last test · v tests it".into(),
            failed: false
        })
    );
}

// Leaving the setup screen hands the dashboard the settings and the
// setup as they are, not as the TUI started.
#[test]
fn leaving_the_setup_screen_reads_the_setup_line_against_its_settings() {
    let (_dir, mut app, rx) = setup_app();
    app.read_setup_row();
    save_settings(&mut app, &rx);
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.setup_row.hint(),
        Some(SetupHint::Note {
            text: "not tested yet · v tests it".into(),
            failed: false
        })
    );
}

// The agent fixes a failing setup with `init --answers --replace` and
// tests again. The dashboard reads the new settings from their file and
// the passed record against them: ready, not "settings changed". The
// session's own config, which starts worktrees, is left as it was.
#[test]
fn the_dashboard_reads_a_corrected_setup_against_the_new_settings() {
    let (_dir, mut app, rx) = setup_app();
    app.read_setup_row();
    save_settings(&mut app, &rx);
    press(&mut app, KeyCode::Enter);
    let failed = CheckOutcome::Failed {
        kind: FailureKind::Settings,
        reason: "web exited after 0.8s".into(),
    };
    let mut record = CheckRecord::begin(crate::setup::fingerprint(&app.config), RanBy::Program);
    record.outcome = failed;
    record.finished_at = Some(Utc::now());
    save_record(&app, &record);
    tick_until(&mut app, &rx, "the failure", |app| {
        matches!(
            app.setup_row.hint(),
            Some(SetupHint::Note { failed: true, .. })
        )
    });

    write_seen(
        &app.paths.config_file(),
        "[processes.web]\ncmd = \"pnpm start\"\n",
    );
    let corrected = crate::config::load(&app.paths).unwrap().config;
    let fingerprint = crate::setup::fingerprint(&corrected);
    let mut passed = CheckRecord::begin(fingerprint.clone(), RanBy::Program);
    passed.outcome = CheckOutcome::Passed;
    passed.fingerprint_after = Some(fingerprint);
    passed.finished_at = Some(Utc::now());
    save_record(&app, &passed);
    tick_until(&mut app, &rx, "ready", |app| {
        app.setup_row
            .setup
            .as_ref()
            .is_some_and(|s| s.state == SetupState::Ready)
    });
    assert_eq!(app.setup_row.hint(), None, "ready says nothing");
    assert_eq!(
        app.config.processes["web"].cmd, "pnpm dev",
        "the session's config is adopted only as before"
    );
}

// ---- letting pando try on its own ---------------------------------------

/// Puts a project into the setup screen's repository and commits it.
fn project_files(app: &App, files: &[(&str, &str)]) {
    let root = app.paths.root();
    for (name, text) in files {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    crate::testutil::git(root, &["add", "."]);
    crate::testutil::git(root, &["commit", "--quiet", "-m", "project"]);
}

/// A project pando can read whole: a dev script, a lockfile, a port.
const READABLE: &[(&str, &str)] = &[
    (
        "package.json",
        "{ \"scripts\": { \"dev\": \"next dev\" } }\n",
    ),
    ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
    (".env.example", "PORT=3000\n"),
];

/// What `git worktree list` says, beside the main checkout.
fn worktree_list(app: &App) -> Vec<PathBuf> {
    crate::worktree::discover(&app.paths.project)
        .unwrap()
        .into_iter()
        .map(|w| w.path)
        .collect()
}

/// The files a guess that stopped must not have written.
fn written_by_a_guess(app: &App) -> Vec<PathBuf> {
    [
        app.paths.config_file(),
        app.paths.user_config_file(),
        app.paths.setup_file(),
        app.paths.home.join("preview"),
    ]
    .into_iter()
    .filter(|path| path.exists())
    .collect()
}

/// The frame as text, a row per line.
fn painted(app: &mut App, width: u16, height: u16) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| crate::tui::render::render(f, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            (0..width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `⏎` with no settings, until the guess has started its check.
fn guess_and_start_the_check(app: &mut App, rx: &Receiver<AppEvent>) {
    press(app, KeyCode::Enter);
    assert_eq!(
        screen(app).line(),
        SetupLine::OwnGuess(Trying::Resolving),
        "the screen says pando is trying"
    );
    assert!(app.pending.is_none(), "the action slot stays free");
    press(app, KeyCode::Enter);
    assert!(app.setup_screen.is_some(), "a second ⏎ waits for the first");
    tick_until(app, rx, "the check", |app| app.checks_started == 1);
    assert!(has_settings(&screen(app).config));
    assert_eq!(screen(app).line(), SetupLine::Starting);
}

// ⏎ with no settings: pando's first choices are written under its home,
// setup.json says they were pando's guess, and the check they start is
// the same detached `pando check` the screen watches for an agent. Its
// pass turns the screen ready.
#[test]
fn enter_lets_pando_try_on_its_own_and_a_pass_turns_the_screen_ready() {
    let (_dir, mut app, rx) = setup_app();
    project_files(&app, READABLE);
    let before = worktree_list(&app);
    guess_and_start_the_check(&mut app, &rx);

    assert!(SetupMemory::load(&app.paths).tried_by_pando_at.is_some());
    let written = std::fs::read_to_string(app.paths.config_file()).unwrap();
    assert!(written.contains("pnpm dev"), "{written}");
    assert!(
        !app.paths.user_config_file().exists(),
        "nothing machine-wide"
    );
    assert!(!app.paths.root().join("pando.toml").exists());
    assert_eq!(worktree_list(&app), before, "the check makes its own");

    tick_until(&mut app, &rx, "the settings read", |app| {
        screen(app).setup.state == SetupState::Untested
    });
    save_record(&app, &record(&app, CheckOutcome::Passed, RanBy::Tui));
    tick_until(&mut app, &rx, "the pass", |app| screen(app).is_ready());
    assert_eq!(screen(&app).line(), SetupLine::Passed);
    let shown = painted(&mut app, 120, 30);
    assert!(
        shown.contains("set up by pando's own guess and tested"),
        "the ready view says whose guess it was:\n{shown}"
    );
    press(&mut app, KeyCode::Enter);
    assert!(app.setup_screen.is_none(), "⏎ now opens pando");
    assert!(app.config.processes.contains_key("dev"));
}

// The guess's check fails like any other: the failure line, and `a`.
#[test]
fn a_guess_whose_check_fails_shows_the_failure() {
    let (_dir, mut app, rx) = setup_app();
    project_files(&app, READABLE);
    guess_and_start_the_check(&mut app, &rx);
    tick_until(&mut app, &rx, "the settings read", |app| {
        screen(app).setup.state == SetupState::Untested
    });
    let failed = CheckOutcome::Failed {
        kind: FailureKind::Settings,
        reason: "dev exited after 0.8s".into(),
    };
    save_record(&app, &record(&app, failed, RanBy::Tui));
    tick_until(&mut app, &rx, "the failure", |app| {
        matches!(screen(app).line(), SetupLine::Failed { .. })
    });
    assert_eq!(
        screen(&app).line(),
        SetupLine::Failed {
            reason: "dev exited after 0.8s".into(),
            kind: FailureKind::Settings,
            by_program: false,
        }
    );
}

// A server pando cannot say how to start: nothing is written, no
// worktree is made, no check is asked for, and the screen hands it to
// the agent.
#[test]
fn a_project_pando_cannot_start_gets_no_worktree_and_no_settings() {
    let (_dir, mut app, rx) = setup_app();
    project_files(
        &app,
        &[
            (
                "package.json",
                "{ \"scripts\": { \"build\": \"node build.js\" } }\n",
            ),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        ],
    );
    let before = worktree_list(&app);
    press(&mut app, KeyCode::Enter);
    tick_until(&mut app, &rx, "the guess", |app| {
        screen(app).line() == SetupLine::OwnGuess(Trying::CannotTell)
    });
    assert_eq!(worktree_list(&app), before);
    assert_eq!(written_by_a_guess(&app), Vec::<PathBuf>::new());
    assert_eq!(app.checks_started, 0);
    assert!(!has_settings(&screen(&app).config));
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.clipboard.as_deref(), Some(crate::setup::SETUP_PROMPT));
}

// A runtime pando's shell does not meet: the prelude that would fix it
// is machine-wide, so pando never takes it. The screen shows doctor's
// line for it, nothing is written, and no check is asked for.
#[test]
fn a_needed_prelude_shows_doctors_line_and_writes_nothing() {
    let (_dir, mut app, rx) = setup_app();
    let mut files = READABLE.to_vec();
    // A version no machine resolves, so the probe disagrees whatever this
    // one has on its PATH, or with nothing there at all.
    files.push((".nvmrc", "1.2.3\n"));
    project_files(&app, &files);
    press(&mut app, KeyCode::Enter);
    tick_until(&mut app, &rx, "the guess", |app| {
        matches!(
            screen(app).line(),
            SetupLine::OwnGuess(Trying::NeedsPrelude { .. })
        )
    });
    let SetupLine::OwnGuess(Trying::NeedsPrelude { line, .. }) = screen(&app).line() else {
        unreachable!()
    };
    let config = crate::config::load(&app.paths).unwrap().config;
    let shell = actions::runtime_shell(app.paths.root());
    let machine = actions::Machine::here(&shell);
    let doctor = crate::doctor::runtime_findings(&app.paths, &config, &machine);
    assert_eq!(
        Some(&line),
        doctor.first().map(|f| &f.message),
        "doctor's own"
    );
    assert!(line.contains("asks for node 1.2.3"), "{line}");
    assert_eq!(written_by_a_guess(&app), Vec::<PathBuf>::new());
    assert_eq!(app.checks_started, 0);
}

// The setup screen's grove quakes, so every tick draws it again — on the
// ready view too, where nothing else on the screen spins.
#[test]
fn the_setup_screen_is_drawn_again_on_every_tick_for_its_grove() {
    let (_dir, mut app) = app_on_setup_screen(true);
    app.setup_screen.as_mut().unwrap().setup.state = crate::setup::SetupState::Ready;
    assert!(!app.setup_screen.as_ref().unwrap().spinning());
    for _ in 0..3 {
        assert!(
            app.handle_event(AppEvent::Tick),
            "a tick that did not repaint"
        );
    }
}

// ---- the main checkout -----------------------------------------------

/// The main checkout as discovery finds it: the repository's own
/// directory, on `main`.
pub fn main_checkout() -> Worktree {
    Worktree {
        path: PathBuf::from("/pando-test-does-not-exist/acme-shop"),
        branch: Some("main".into()),
        ..wt("acme-shop")
    }
}

/// An app whose last discovery listed the main checkout and `names`.
pub fn app_with_main(names: &[&str]) -> App {
    let mut app = test_app(names);
    let snapshot = Snapshot {
        main: main_checkout(),
        worktrees: names.iter().map(|n| wt(n)).collect(),
        created_by_pando: names.iter().map(|n| (n.to_string(), true)).collect(),
        state: app.state.clone(),
        warning: None,
        notices: Vec::new(),
        default_base: None,
    };
    app.apply_snapshot(snapshot);
    app.select_index(0);
    app
}

#[test]
fn the_main_checkout_is_the_first_row_and_not_counted_as_a_worktree() {
    let app = app_with_main(&["feat+one", "feat+two"]);
    let rows: Vec<&str> = app
        .filtered_indices
        .iter()
        .map(|&i| app.worktrees[i].name.as_str())
        .collect();
    assert_eq!(rows, ["acme-shop", "feat+one", "feat+two"]);
    assert!(app.is_main("acme-shop") && !app.is_main("feat+one"));
    assert_eq!(app.linked_count(), 2);
    assert_eq!(app.label_of("acme-shop"), "main");
}

// A project with no worktree is a first run, whose welcome is about
// making one: a main checkout pando never ran is no row yet. Started from
// a shell, it gets its row with the refresh that reads its record.
#[test]
fn a_main_checkout_pando_never_ran_waits_for_its_record_before_the_first_worktree() {
    let mut app = app_with_main(&[]);
    assert!(!app.main_row_shown());
    assert!(app.selected_worktree().is_none());
    press(&mut app, KeyCode::Char('s'));
    assert!(app.pending.is_none(), "nothing is selected on the welcome");

    let mut state = app.state.clone();
    state.worktrees.insert(
        "acme-shop".to_string(),
        WorktreeRecord::new("/pando-test-does-not-exist/acme-shop", false),
    );
    app.handle_event(refreshed(state));
    assert!(app.main_row_shown());
    assert_eq!(app.selected_worktree().unwrap().name, "acme-shop");
}

#[test]
fn the_row_keys_act_on_the_main_checkout() {
    for (key, kind) in [
        (KeyCode::Char('s'), PendingKind::Start),
        (KeyCode::Char('x'), PendingKind::Stop),
        (KeyCode::Char('r'), PendingKind::Restart),
    ] {
        let mut app = app_with_main(&["feat+one"]);
        with_process(&mut app, "acme-shop", running_phase());
        assert_eq!(app.selected_worktree().unwrap().name, "acme-shop");
        press(&mut app, key);
        if kind != PendingKind::Start {
            press(&mut app, key);
        }
        let pending = app.pending.as_ref().expect("the key started something");
        assert_eq!((pending.name.as_str(), pending.kind), ("acme-shop", kind));
    }
    let mut app = app_with_main(&["feat+one"]);
    with_process(&mut app, "acme-shop", running_phase());
    press(&mut app, KeyCode::Char('l'));
    assert!(matches!(app.view, View::Log(_)), "l opens its log");
}

// It has one mode, so ⏎ has nothing to choose: it starts it, shared.
#[test]
fn enter_starts_the_main_checkout_with_no_chooser() {
    let mut app = app_with_main(&["feat+one"]);
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none(), "no mode chooser for it");
    let pending = app.pending.as_ref().expect("enter started it");
    assert_eq!(
        (pending.name.as_str(), pending.kind),
        ("acme-shop", PendingKind::Start)
    );

    let mut app = app_with_main(&["feat+one"]);
    with_process(&mut app, "acme-shop", running_phase());
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none() && app.pending.is_none());
    let (message, _) = app.active_status().unwrap();
    assert!(message.contains("the main checkout"), "{message}");
}

// `i` would give it data apart from its own, and `d` would remove it:
// both refuse, with the reason, and open nothing.
#[test]
fn the_main_checkout_refuses_isolation_and_removal() {
    let mut app = app_with_main(&["feat+one"]);
    press(&mut app, KeyCode::Char('i'));
    assert!(app.pending.is_none() && app.modal.is_none());
    let (message, is_error) = app.active_status().unwrap();
    assert!(is_error && message.contains("isolated and namespaced are for worktrees"));

    press(&mut app, KeyCode::Char('d'));
    assert!(app.modal.is_none(), "no remove dialog");
    let (message, is_error) = app.active_status().unwrap();
    assert!(is_error, "{message}");
    assert!(message.contains("never removes it"), "{message}");

    // A worktree's `d` still asks.
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('d'));
    assert!(matches!(app.modal, Some(Modal::Remove { .. })));
}

// ---- the list's order -----------------------------------------------------

fn row_names(app: &App) -> Vec<&str> {
    app.filtered_indices
        .iter()
        .map(|&i| app.worktrees[i].name.as_str())
        .collect()
}

/// A start `minutes_ago`, then a stop: the time a start leaves behind.
fn ran(app: &App, name: &str, minutes_ago: i64) -> State {
    let mut state = app.state.clone();
    let record = state
        .worktrees
        .entry(name.to_string())
        .or_insert_with(|| WorktreeRecord::new(format!("/trees/{name}"), true));
    record.last_started = Some(Utc::now() - chrono::Duration::minutes(minutes_ago));
    state
}

#[test]
fn the_list_is_by_pull_request_by_default_the_highest_number_first() {
    use crate::worktree::PrState::Open;
    let mut app = app_with_main(&["feat+new", "feat+one", "feat+old"]);
    assert_eq!(app.sort, ListSort::Pr);
    app.handle_event(AppEvent::PrsReady(Ok(vec![
        a_pr(7, "feat/old", Open),
        a_pr(12, "feat/one", Open),
    ])));
    assert_eq!(
        row_names(&app),
        ["acme-shop", "feat+one", "feat+old", "feat+new"],
        "the main checkout first, then the pull requests, then the rest as discovered"
    );
}

#[test]
fn b_cycles_the_order_keeps_the_cursor_and_says_so() {
    use crate::worktree::PrState::Open;
    let mut app = app_with_main(&["zeta", "alpha", "mid"]);
    app.handle_event(AppEvent::PrsReady(Ok(vec![a_pr(3, "mid", Open)])));
    app.select_index(1);
    assert_eq!(app.selected_worktree().unwrap().name, "mid");

    press(&mut app, KeyCode::Char(','));
    assert_eq!(app.sort, ListSort::Newest);
    assert_eq!(row_names(&app), ["acme-shop", "zeta", "alpha", "mid"]);
    assert_eq!(app.selected_worktree().unwrap().name, "mid");
    assert!(
        app.active_status()
            .is_some_and(|(m, _)| m.contains("newest first")),
        "{:?}",
        app.active_status()
    );

    let state = ran(&app, "alpha", 30);
    app.state = state;
    let state = ran(&app, "zeta", 5);
    app.state = state;
    press(&mut app, KeyCode::Char(','));
    assert_eq!(app.sort, ListSort::Run);
    assert_eq!(
        row_names(&app),
        ["acme-shop", "zeta", "alpha", "mid"],
        "the one started last first, one never started after"
    );

    press(&mut app, KeyCode::Char(','));
    assert_eq!(app.sort, ListSort::Name);
    assert_eq!(row_names(&app), ["acme-shop", "alpha", "mid", "zeta"]);
    assert_eq!(app.selected_worktree().unwrap().name, "mid");

    press(&mut app, KeyCode::Char(','));
    assert_eq!(app.sort, ListSort::Pr);
    assert_eq!(row_names(&app), ["acme-shop", "mid", "zeta", "alpha"]);
}

#[test]
fn a_start_moves_its_row_up_a_list_sorted_by_last_run() {
    let mut app = app_with_main(&["feat+a", "feat+b"]);
    app.sort = ListSort::Run;
    let state = ran(&app, "feat+a", 10);
    app.handle_event(refreshed(state));
    assert_eq!(row_names(&app), ["acme-shop", "feat+a", "feat+b"]);

    let state = ran(&app, "feat+b", 0);
    app.handle_event(refreshed(state));
    assert_eq!(row_names(&app), ["acme-shop", "feat+b", "feat+a"]);
}

#[test]
fn a_list_sorted_otherwise_does_not_move_for_when_one_last_ran() {
    let mut app = app_with_main(&["feat+a", "feat+b"]);
    app.sort = ListSort::Newest;
    let state = ran(&app, "feat+b", 0);
    app.handle_event(refreshed(state));
    assert_eq!(row_names(&app), ["acme-shop", "feat+a", "feat+b"]);
}

/// A refresh that finds `name` with one process in `phase`.
fn refresh_with_process(app: &mut App, name: &str, phase: Phase) {
    let before = app.state.clone();
    with_process(app, name, phase);
    let after = std::mem::replace(&mut app.state, before);
    app.handle_event(refreshed(after));
}

#[test]
fn what_runs_comes_right_after_the_main_checkout_in_every_order() {
    use crate::worktree::PrState::Open;
    let mut app = app_with_main(&["feat+c", "feat+b", "feat+a"]);
    app.handle_event(AppEvent::PrsReady(Ok(vec![a_pr(9, "feat/a", Open)])));
    app.select_index(3);
    assert_eq!(app.selected_worktree().unwrap().name, "feat+b");

    refresh_with_process(&mut app, "feat+b", running_phase());
    assert_eq!(
        row_names(&app),
        ["acme-shop", "feat+b", "feat+a", "feat+c"],
        "the running one first, then the pull request, then the rest"
    );
    assert_eq!(
        app.selected_worktree().unwrap().name,
        "feat+b",
        "the cursor goes with it"
    );
    for _ in 1..ListSort::ALL.len() {
        press(&mut app, KeyCode::Char(','));
        assert_eq!(row_names(&app)[1], "feat+b", "{:?}", app.sort);
    }

    let starting = Phase::Starting { since: Utc::now() };
    refresh_with_process(&mut app, "feat+c", starting);
    press(&mut app, KeyCode::Char(','));
    assert_eq!(app.sort, ListSort::Pr);
    assert_eq!(
        row_names(&app),
        ["acme-shop", "feat+c", "feat+b", "feat+a"],
        "starting counts, and what runs keeps the chosen order among itself"
    );
}

#[test]
fn a_stop_puts_the_row_back_where_the_order_has_it() {
    let mut app = app_with_main(&["feat+a", "feat+b"]);
    app.sort = ListSort::Newest;
    refresh_with_process(&mut app, "feat+b", running_phase());
    assert_eq!(row_names(&app), ["acme-shop", "feat+b", "feat+a"]);

    // A stop keeps the record and clears its processes.
    let mut stopped = app.state.clone();
    stopped
        .worktrees
        .get_mut("feat+b")
        .unwrap()
        .processes
        .clear();
    app.handle_event(refreshed(stopped));
    assert_eq!(row_names(&app), ["acme-shop", "feat+a", "feat+b"]);
}

#[test]
fn a_worktree_whose_only_process_failed_is_not_up() {
    let mut app = app_with_main(&["feat+a", "feat+b"]);
    app.sort = ListSort::Newest;
    let failed = Phase::Failed {
        at: Utc::now(),
        reason: "exited with 1".into(),
    };
    refresh_with_process(&mut app, "feat+b", failed);
    assert_eq!(row_names(&app), ["acme-shop", "feat+a", "feat+b"]);
}

#[test]
fn the_order_comes_from_the_config() {
    let paths = test_app(&[]).paths.clone();
    let mut config = Config::default();
    config.ui.sort = Some("name".into());
    let app = App::new_for_test(paths, config, vec![wt("b"), wt("a")]);
    assert_eq!(app.sort, ListSort::Name);
    assert_eq!(row_names(&app), ["a", "b"]);
}

#[test]
fn every_order_is_a_word_the_config_takes_in_the_order_b_cycles_them() {
    let words: Vec<&str> = ListSort::ALL.iter().map(|s| s.word()).collect();
    assert_eq!(words, crate::config::LIST_SORTS);
    for sort in ListSort::ALL {
        assert_eq!(ListSort::from_word(sort.word()), Some(sort));
    }
    let mut sort = ListSort::default();
    for expected in ListSort::ALL.iter().cycle().skip(1).take(4) {
        sort = sort.next();
        assert_eq!(sort, *expected);
    }
}

#[test]
fn the_order_and_the_theme_are_saved_into_an_inline_or_dotted_ui_table_too() {
    for text in ["ui = { theme = \"gruvbox\" }\n", "ui.theme = \"gruvbox\"\n"] {
        let mut doc: toml_edit::DocumentMut = text.parse().unwrap();
        sort::set_sort(&mut doc, ListSort::Run);
        super::themes::set_theme(&mut doc, "github");
        let saved: toml::Value = toml::from_str(&doc.to_string()).unwrap();
        assert_eq!(saved["ui"]["sort"].as_str(), Some("run"), "{text}: {doc}");
        assert_eq!(
            saved["ui"]["theme"].as_str(),
            Some("github"),
            "{text}: {doc}"
        );
    }
}

#[test]
fn the_order_is_saved_beside_the_theme() {
    let mut doc: toml_edit::DocumentMut = "[ui]\ntheme = \"gruvbox\"\n".parse().unwrap();
    sort::set_sort(&mut doc, ListSort::Run);
    let text = doc.to_string();
    assert!(text.contains("theme = \"gruvbox\""), "{text}");
    assert!(text.contains("sort = \"run\""), "{text}");
    let mut doc = toml_edit::DocumentMut::new();
    sort::set_sort(&mut doc, ListSort::Name);
    assert!(doc.to_string().contains("[ui]\nsort = \"name\""), "{doc}");
}

// ---- the git menu ----------------------------------------------------

use crate::actions::git::{GitAction, GitRead, Ran};

/// A worktree ten commits behind origin/main with 46 of its own, all of
/// them pushed, and up to date with its upstream.
pub fn a_git_read(main: bool) -> GitRead {
    GitRead {
        checkout: PathBuf::from("/trees/feat+one"),
        main,
        branch: Some("feat/one".into()),
        base: Some("origin/main".into()),
        base_drift: Some((46, 10)),
        upstream: Some("origin/feat/one".into()),
        upstream_remote: Some("origin".into()),
        upstream_drift: Some((0, 0)),
        pushed: 46,
        dirty: Some(0),
        in_progress: None,
        has_origin: true,
        fetched: None,
    }
}

/// `space g` on the selected row, and its read landing as the worker
/// sends it.
pub fn open_git_menu(app: &mut App, name: &str, read: GitRead) {
    press(app, KeyCode::Char(' '));
    press(app, KeyCode::Char('g'));
    assert!(
        matches!(
            &app.modal,
            Some(Modal::Git {
                stage: GitStage::Reading,
                ..
            })
        ),
        "space g opens the menu, reading"
    );
    assert!(app.handle_event(AppEvent::GitRead(Box::new((name.to_string(), read)))));
}

fn git_stage(app: &App) -> &GitStage {
    match &app.modal {
        Some(Modal::Git { stage, .. }) => stage,
        other => panic!("the git menu is not open: {other:?}"),
    }
}

fn status_text(app: &App) -> String {
    app.status
        .as_ref()
        .map(|s| s.message.clone())
        .unwrap_or_default()
}

/// Runs the previewed action, and swaps the worker's channel for one the
/// test answers on.
fn run_git_preview(app: &mut App) -> mpsc::Sender<Result<PendingOutcome, String>> {
    press(app, KeyCode::Enter);
    assert!(matches!(git_stage(app), GitStage::Running { .. }));
    let (tx, rx) = mpsc::channel();
    app.pending.as_mut().expect("a run is in flight").rx = rx;
    tx
}

#[test]
fn u_opens_the_git_menu_on_the_move_the_row_most_likely_wants() {
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", a_git_read(false));
    match git_stage(&app) {
        // f p r m: the base has ten new commits, the upstream none.
        GitStage::Menu { selected, .. } => assert_eq!(*selected, 2),
        other => panic!("{other:?}"),
    }
    let mut behind_upstream = a_git_read(true);
    behind_upstream.upstream_drift = Some((0, 3));
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", behind_upstream);
    assert!(matches!(
        git_stage(&app),
        GitStage::Menu { selected: 1, .. }
    ));
}

#[test]
fn a_read_for_another_row_is_not_the_menus() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Char('g'));
    let elsewhere = AppEvent::GitRead(Box::new(("feat+two".to_string(), a_git_read(false))));
    let selected = app.selected_worktree().unwrap().name.clone();
    if selected != "feat+two" {
        assert!(!app.handle_event(elsewhere));
        assert_eq!(git_stage(&app), &GitStage::Reading);
    }
}

#[test]
fn esc_steps_the_git_menu_back_one_stage_at_a_time() {
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", a_git_read(false));
    press(&mut app, KeyCode::Enter);
    assert!(matches!(
        git_stage(&app),
        GitStage::Preview {
            action: GitAction::Rebase,
            ..
        }
    ));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(
        git_stage(&app),
        GitStage::Menu { selected: 2, .. }
    ));
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
}

#[test]
fn a_letter_opens_its_preview_and_a_refused_one_says_why() {
    let mut app = test_app(&["feat+one"]);
    let mut dirty = a_git_read(false);
    dirty.dirty = Some(2);
    open_git_menu(&mut app, "feat+one", dirty);
    press(&mut app, KeyCode::Char('r'));
    assert!(matches!(
        git_stage(&app),
        GitStage::Menu { selected: 2, .. }
    ));
    assert_eq!(
        status_text(&app),
        "✎ 2 uncommitted files — commit or stash first"
    );
    press(&mut app, KeyCode::Char('f'));
    assert!(matches!(
        git_stage(&app),
        GitStage::Preview {
            action: GitAction::Fetch,
            ..
        }
    ));
}

#[test]
fn enter_on_a_refused_row_says_why_and_stays() {
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", a_git_read(true));
    // The main checkout's cursor starts on pull; rebase is the next row.
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    assert!(matches!(
        git_stage(&app),
        GitStage::Menu { selected: 2, .. }
    ));
    assert_eq!(
        status_text(&app),
        "not on the main checkout — pando only fast-forwards it"
    );
}

#[test]
fn by_hand_hands_the_checkout_to_a_shell() {
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", a_git_read(false));
    press(&mut app, KeyCode::Char('!'));
    assert!(app.modal.is_none());
    assert!(app.launch.is_some(), "a shell was asked for");
}

#[test]
fn a_run_cannot_be_stepped_out_of_and_lands_in_the_menu() {
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", a_git_read(false));
    press(&mut app, KeyCode::Enter);
    let tx = run_git_preview(&mut app);
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Git(GitAction::Rebase))
    );
    press(&mut app, KeyCode::Esc);
    assert!(matches!(git_stage(&app), GitStage::Running { .. }));
    assert!(status_text(&app).contains("cannot be stopped halfway"));

    tx.send(Ok(PendingOutcome::Git(
        "feat+one".into(),
        Ran::Moved("rebased feat/one onto origin/main · 46 commits on top of 10 new".into()),
    )))
    .unwrap();
    app.handle_event(AppEvent::Tick);
    assert!(app.pending.is_none());
    match git_stage(&app) {
        GitStage::Result { ran, restart, .. } => {
            assert!(matches!(ran, Ok(Ran::Moved(_))));
            assert!(!restart, "nothing runs there");
        }
        other => panic!("{other:?}"),
    }
    assert!(status_text(&app).contains("rebased feat/one onto origin/main"));
    press(&mut app, KeyCode::Esc);
    assert!(app.modal.is_none());
}

#[test]
fn a_branch_moved_under_a_running_worktree_restarts_on_one_press() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    open_git_menu(&mut app, "feat+one", a_git_read(false));
    press(&mut app, KeyCode::Enter);
    let tx = run_git_preview(&mut app);
    tx.send(Ok(PendingOutcome::Git(
        "feat+one".into(),
        Ran::Moved("rebased".into()),
    )))
    .unwrap();
    app.handle_event(AppEvent::Tick);
    assert!(matches!(
        git_stage(&app),
        GitStage::Result { restart: true, .. }
    ));
    assert!(status_text(&app).contains("until it restarts"));
    press(&mut app, KeyCode::Char('r'));
    assert!(app.modal.is_none());
    assert_eq!(
        app.pending.as_ref().map(|p| p.kind),
        Some(PendingKind::Restart),
        "one press: the preview was the asking"
    );
}

#[test]
fn a_conflict_is_said_and_offers_the_shell() {
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", a_git_read(false));
    press(&mut app, KeyCode::Enter);
    let tx = run_git_preview(&mut app);
    tx.send(Ok(PendingOutcome::Git(
        "feat+one".into(),
        Ran::Conflict {
            op: crate::worktree::InProgress::Rebase,
            files: vec!["apps/web/cart.ts".into()],
            at: Some("abc1234 cart: new totals".into()),
        },
    )))
    .unwrap();
    app.handle_event(AppEvent::Tick);
    assert!(app.status.as_ref().is_some_and(|s| s.is_error()));
    assert_eq!(
        status_text(&app),
        "feat/one: conflict in apps/web/cart.ts — rebase aborted, nothing changed"
    );
    assert!(matches!(
        git_stage(&app),
        GitStage::Result { restart: false, .. }
    ));
    press(&mut app, KeyCode::Char('!'));
    assert!(app.modal.is_none());
    assert!(app.launch.is_some());
}

#[test]
fn a_run_that_fails_says_so_in_the_menu_and_the_header() {
    let mut app = test_app(&["feat+one"]);
    open_git_menu(&mut app, "feat+one", a_git_read(false));
    press(&mut app, KeyCode::Char('f'));
    let tx = run_git_preview(&mut app);
    tx.send(Err(
        "`git fetch origin` did not answer in 30s — nothing changed".into(),
    ))
    .unwrap();
    app.handle_event(AppEvent::Tick);
    assert!(matches!(
        git_stage(&app),
        GitStage::Result { ran: Err(_), .. }
    ));
    assert_eq!(
        status_text(&app),
        "could not fetch feat/one: `git fetch origin` did not answer in 30s — nothing changed"
    );
}

// The menu's keys are the table's: every action's letter and `!` answer
// in the menu, and no other letter does.
#[test]
fn the_git_menu_answers_exactly_its_tables_keys() {
    let mut read = a_git_read(false);
    // Abort is shown only while something is half-done.
    read.in_progress = Some(crate::worktree::InProgress::Rebase);
    let half_done = read.clone();
    for c in ('a'..='z').chain(['!']) {
        for read in [a_git_read(false), half_done.clone()] {
            let mut app = test_app(&["feat+one"]);
            open_git_menu(&mut app, "feat+one", read.clone());
            let before = format!("{:?}|{:?}", app.modal, app.launch);
            press(&mut app, KeyCode::Char(c));
            let answered = format!("{:?}|{:?}", app.modal, app.launch) != before;
            let shown = crate::actions::git::offers(&read)
                .iter()
                .any(|o| o.action.key() == c);
            // j k move; q and u close.
            let expected = shown || matches!(c, '!' | 'j' | 'k' | 'q');
            assert_eq!(answered, expected, "{c:?} on {:?}", read.in_progress);
        }
    }
}

// ---- the leader ----------------------------------------------------------

// `space` waits for one more key, as neovim's leader does: exactly the
// keys `LEADER_KEYS` lists do something after it, and anything else
// takes it back without acting as the list's own key would.
#[test]
fn after_space_exactly_the_leader_keys_do_something() {
    let documented: Vec<KeyCode> = LEADER_KEYS
        .iter()
        .flat_map(|k| k.codes.iter().copied())
        .collect();
    for code in candidate_keys() {
        let (_dir, mut app) = a_list_every_key_can_act_on();
        press(&mut app, KeyCode::Char(' '));
        assert!(app.leader, "space waits for the next key");
        let selected = app.list_state.selected();
        press(&mut app, code);
        assert!(!app.leader, "{code:?} leaves the leader waiting");
        assert_eq!(
            app.modal.is_some(),
            documented.contains(&code),
            "{code:?} after space"
        );
        assert_eq!(
            app.list_state.selected(),
            selected,
            "{code:?} moved the cursor"
        );
        assert!(!app.should_quit, "{code:?} after space quit");
    }
}

// Help shows a leader key as `space <key>`, so every one of them is a
// row of the list's keys too.
#[test]
fn every_leader_key_is_in_help_after_space() {
    for leader in LEADER_KEYS {
        assert!(
            LIST_KEYS
                .iter()
                .any(|k| k.keys == format!("space {}", leader.keys)),
            "help has no `space {}`",
            leader.keys
        );
    }
}

#[test]
fn space_g_opens_the_git_menu_and_g_alone_still_goes_to_the_first_row() {
    let mut app = test_app(&["a", "b", "c"]);
    press(&mut app, KeyCode::Char('G'));
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(
        app.list_state.selected(),
        Some(0),
        "g goes to the first row"
    );
    assert!(app.modal.is_none());
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Char('g'));
    assert!(
        matches!(app.modal, Some(Modal::Git { .. })),
        "space g opens the git menu"
    );
}

#[test]
fn esc_after_space_takes_it_back() {
    let mut app = test_app(&["a"]);
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Esc);
    assert!(!app.leader);
    assert!(!app.should_quit, "esc after space does not quit");
    assert!(app.modal.is_none());
}
