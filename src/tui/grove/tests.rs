use super::*;
use crate::actions::{LiveWorktree, ProjectSummary};
use crate::config::Config;
use crate::paths::PandoPaths;
use crate::project::ProjectRef;
use crate::state::Aggregate;
use crate::tui::app::{
    AppEvent, COLUMN_KEYS, GROVE_KEYS, LIST_KEYS, Modal, PendingAction, PendingKind,
};
use crate::tui::render::{render, render_grove};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::collections::{BTreeMap, BTreeSet};

/// A project with `up` worktrees running and `failed` failed, under a
/// path that cannot exist.
fn summary(name: &str, up: usize, failed: usize) -> ProjectSummary {
    let mut worktrees = Vec::new();
    for i in 0..up {
        worktrees.push(live(&format!("feat+up{i}"), running()));
    }
    for i in 0..failed {
        worktrees.push(live(
            &format!("feat+broken{i}"),
            Aggregate::Failed {
                process: "dev".into(),
                at: chrono::Utc::now(),
                reason: "exited".into(),
            },
        ));
    }
    ProjectSummary {
        id: id_of(name),
        name: name.to_string(),
        root: std::path::PathBuf::from("/pando-test-does-not-exist").join(name),
        repos: Vec::new(),
        moved: false,
        up,
        starting: 0,
        failed,
        stopped: 1,
        worktrees,
        last_used: None,
        warning: None,
    }
}

fn running() -> Aggregate {
    Aggregate::Running {
        since: chrono::Utc::now(),
    }
}

fn live(name: &str, phase: Aggregate) -> LiveWorktree {
    LiveWorktree {
        name: name.to_string(),
        path: std::path::PathBuf::from("/trees").join(name),
        phase,
        processes: BTreeMap::new(),
        ports: BTreeMap::new(),
        claimed: BTreeSet::new(),
    }
}

fn id_of(name: &str) -> String {
    format!("{name}-0000abcd")
}

/// An app for `name`'s project, as `App::new_for_test` makes them: no
/// workers, nothing on disk.
fn app_for(name: &str) -> crate::tui::app::App {
    let paths = PandoPaths::new(
        "/pando-test-does-not-exist/home",
        ProjectRef {
            id: id_of(name),
            root: std::path::PathBuf::from("/pando-test-does-not-exist").join(name),
            display_name: name.into(),
            ..Default::default()
        },
    );
    let worktrees = vec![crate::tui::app::tests::wt("feat+one")];
    crate::tui::app::App::new_for_test(paths, Config::default(), worktrees)
}

/// Four projects, the way the column orders them: one up, one failed,
/// one idle, one moved; `shop` open and in front.
fn a_grove() -> Grove {
    let mut moved = summary("old", 0, 0);
    moved.moved = true;
    let summaries = vec![
        summary("shop", 2, 0),
        summary("api", 0, 1),
        summary("docs", 0, 0),
        moved,
    ];
    let mut grove = Grove::new_for_test(summaries);
    grove.adopt(app_for("shop"), true);
    grove
}

fn draw(grove: &mut Grove, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render_grove(f, grove)).unwrap();
    terminal.backend().buffer().clone()
}

fn text_of(buf: &Buffer) -> String {
    (0..buf.area().height)
        .map(|y| row_of(buf, y))
        .collect::<Vec<_>>()
        .join("\n")
}

fn row_of(buf: &Buffer, y: u16) -> String {
    cells(buf, y, 0, buf.area().width)
}

/// The cells `x0..x1` of row `y`.
fn cells(buf: &Buffer, y: u16, x0: u16, x1: u16) -> String {
    (x0..x1)
        .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
        .collect()
}

fn press(grove: &mut Grove, code: KeyCode) {
    grove.handle_event(GroveEvent::Input(Event::Key(KeyEvent::new(
        code,
        KeyModifiers::NONE,
    ))));
}

fn click(grove: &mut Grove, x: u16, y: u16) {
    grove.handle_event(GroveEvent::Input(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })));
}

/// An action in flight that never lands.
fn in_flight() -> PendingAction {
    let (_tx, rx) = std::sync::mpsc::channel();
    // Kept open, so the action stays in flight.
    std::mem::forget(_tx);
    PendingAction {
        name: "feat+one".into(),
        kind: PendingKind::Start,
        covers: vec!["feat+one".into()],
        rx,
        started_at: std::time::Instant::now(),
        spinner_frame: 0,
        progress_rx: None,
        stage: None,
        said: Vec::new(),
        label: "feat/one".into(),
        refs_written: false,
        cancel: None,
    }
}

// ---- drawing ---------------------------------------------------------

// Wide: the column on the left, a row per project in the list's glyphs,
// pando's own name in its border, one root line under every stem; the
// project in front drawn right of it, its header starting at the
// project.
#[test]
fn the_column_lists_every_project_beside_the_one_in_front() {
    let mut grove = a_grove();
    let buf = draw(&mut grove, 120, 30);
    let column = |y| cells(&buf, y, 0, COLUMN_WIDTH);
    assert!(column(0).contains("pando"), "{}", column(0));
    assert!(column(0).contains("● 2"), "{}", column(0));
    assert!(column(0).contains("✗ 1"), "{}", column(0));
    assert!(column(1).contains("┃● shop"), "{}", column(1));
    assert!(column(1).trim_end_matches('│').trim_end().ends_with('2'));
    assert!(column(2).contains("┃✗ api"), "{}", column(2));
    assert!(column(2).contains("✗1"), "{}", column(2));
    assert!(column(3).contains("│  docs"), "{}", column(3));
    assert!(column(4).contains("╎  old"), "{}", column(4));
    assert!(column(4).contains("moved"), "{}", column(4));
    let root = column(28);
    assert!(root.contains("╺━┻━"), "{root}");
    assert_eq!(buf[(2, 28)].fg, crate::theme::green(), "lit: shop is up");
    // The project in front, right of the column, its header without
    // pando's name, which the column carries.
    let right = cells(&buf, 0, COLUMN_WIDTH, 120);
    assert!(right.trim_start().starts_with("shop"), "{right}");
    assert!(!right.contains("pando"), "{right}");
    assert_eq!(grove.hits.rows.len(), 4);
    // The front project's name is the blue one.
    let shop = column(1).find("shop").unwrap() as u16;
    let shop_x = column(1)[..shop as usize].chars().count() as u16;
    assert_eq!(buf[(shop_x, 1)].fg, crate::theme::blue());
}

// The border is blue only on the pane with the keys; the cursor's row is
// on the band either way.
#[test]
fn focus_moves_the_blue_border_between_the_column_and_the_list() {
    let mut grove = a_grove();
    let buf = draw(&mut grove, 120, 30);
    assert_eq!(grove.focus, Focus::List);
    assert_eq!(buf[(0, 0)].fg, crate::theme::border());
    assert!(!text_of(&buf).contains("⏎ open"));
    press(&mut grove, KeyCode::Left);
    assert_eq!(grove.focus, Focus::Column);
    let buf = draw(&mut grove, 120, 30);
    assert_eq!(buf[(0, 0)].fg, crate::theme::blue());
    assert_eq!(buf[(1, 1)].bg, crate::theme::highlight_bg());
    // The column's own keys while it has them.
    assert!(text_of(&buf).contains("⏎ open"), "{}", text_of(&buf));
    press(&mut grove, KeyCode::Tab);
    assert_eq!(grove.focus, Focus::List);
    press(&mut grove, KeyCode::Left);
    press(&mut grove, KeyCode::Esc);
    assert_eq!(grove.focus, Focus::List);
}

// Narrow: no column, one row above the list, the project in front between
// `[` and `]`, which a click presses.
#[test]
fn under_the_side_by_side_width_the_column_is_a_strip() {
    let mut grove = a_grove();
    let buf = draw(&mut grove, 60, 24);
    let strip = row_of(&buf, 0);
    assert!(strip.starts_with(" [ ◀ ● shop · 2 up · 1/4 ▶ ]"), "{strip}");
    assert!(row_of(&buf, 1).contains("shop"), "{}", row_of(&buf, 1));
    assert_eq!(grove.hits.column, Rect::new(0, 0, 60, 1));
    // With the keys, the strip follows the column's cursor.
    press(&mut grove, KeyCode::Left);
    press(&mut grove, KeyCode::Char('j'));
    let strip = row_of(&draw(&mut grove, 60, 24), 0);
    assert!(strip.contains("✗ api · ✗1 failed · 2/4"), "{strip}");
    // `]` on the strip: the next project, opened.
    let next = grove.hits.next;
    click(&mut grove, next.x + 1, next.y);
    assert_eq!(grove.open_requests, vec![id_of("api")]);
}

// With the column hidden the project's TUI is exactly what it is without
// the grove: nothing of today's screen moves for it.
#[test]
fn with_the_column_hidden_the_screen_is_the_project_alone() {
    let mut grove = a_grove();
    grove.shown = Shown::Off;
    for (w, h) in [(100, 24), (60, 20)] {
        let in_grove = draw(&mut grove, w, h);
        let mut alone = app_for("shop");
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| render(f, &mut alone)).unwrap();
        assert_eq!(text_of(&in_grove), text_of(terminal.backend().buffer()));
    }
    // One project and none other: the column does not show by itself.
    let mut one = Grove::new_for_test(vec![summary("shop", 0, 0)]);
    one.adopt(app_for("shop"), true);
    assert!(!one.column_shown());
}

// Outside a repository: the brand's grove, a stem for each project with
// its name under it, the column beside it, and nothing breaks on a
// screen too small for the picture.
#[test]
fn outside_a_repository_the_home_pane_names_a_stem_for_each_project() {
    let mut grove = a_grove();
    grove.open.clear();
    grove.front = None;
    grove.focus = Focus::Column;
    let buf = draw(&mut grove, 140, 40);
    let all = text_of(&buf);
    assert!(all.contains(" grove "), "{all}");
    assert!(all.contains("one root, every project alive"), "{all}");
    assert!(all.contains("⏎ open"), "{all}");
    assert!(all.contains("┻"), "every stem on one root");
    // A row naming every project that has not moved, in the column's
    // order: the home pane's part of the screen only.
    let names = (0..40)
        .map(|y| cells(&buf, y, COLUMN_WIDTH, 140))
        .find(|row| row.contains("shop") && row.contains("docs"))
        .unwrap_or_else(|| panic!("no row names the stems:\n{all}"));
    let at = |name: &str| names.find(name).unwrap();
    assert!(
        at("shop") < at("✗api") && at("✗api") < at("docs"),
        "{names}"
    );
    assert!(!names.contains("old"), "a moved project has no stem");
    // The cursor's project is named in blue.
    let x = COLUMN_WIDTH + names[..at("shop")].chars().count() as u16;
    let y = (0..40)
        .find(|&y| cells(&buf, y, COLUMN_WIDTH, 140) == names)
        .unwrap();
    assert_eq!(buf[(x, y)].fg, crate::theme::blue());
    // Small screens: no panic, and the brand still shows.
    for (w, h) in [(24, 7), (40, 10), (10, 3), (1, 1)] {
        draw(&mut grove, w, h);
    }
    assert!(text_of(&draw(&mut grove, 80, 24)).contains("pando"));
}

// No projects at all: an empty grove that says how one gets there.
#[test]
fn a_home_with_no_project_says_how_one_grows() {
    let mut grove = Grove::new_for_test(Vec::new());
    let all = text_of(&draw(&mut grove, 100, 30));
    assert!(all.contains("no project yet"), "{all}");
    // `q` on it quits, nothing in flight anywhere.
    press(&mut grove, KeyCode::Char('q'));
    assert!(grove.should_quit);
}

// The front app's regions are where it was drawn: a click on its footer's
// `? help`, right of the column, presses `?` in it; a click on a
// project's row opens that project.
#[test]
fn a_click_right_of_the_column_lands_on_the_front_app() {
    let mut grove = a_grove();
    draw(&mut grove, 120, 30);
    let hits = grove.front_app().unwrap().hits.keys.clone();
    let (rect, _) = hits
        .iter()
        .find(|(_, key)| key == "?")
        .expect("a ? hint in the footer");
    assert!(
        rect.x >= COLUMN_WIDTH,
        "drawn right of the column: {rect:?}"
    );
    click(&mut grove, rect.x, rect.y);
    assert!(matches!(
        grove.front_app().unwrap().modal,
        Some(Modal::Help)
    ));
    grove.front_app_mut().unwrap().modal = None;
    let (row, i) = grove.hits.rows[2];
    assert_eq!(grove.summaries[i].name, "docs");
    click(&mut grove, row.x + 3, row.y);
    assert_eq!(grove.open_requests, vec![id_of("docs")]);
}

// ---- keys ------------------------------------------------------------

#[test]
fn w_hides_and_shows_the_column() {
    let mut grove = a_grove();
    assert!(grove.column_shown());
    press(&mut grove, KeyCode::Char('w'));
    assert!(!grove.column_shown());
    let buf = draw(&mut grove, 120, 30);
    assert!(
        cells(&buf, 0, 0, 10).contains("pando"),
        "the header has it back"
    );
    press(&mut grove, KeyCode::Char('w'));
    assert!(grove.column_shown());
}

#[test]
fn brackets_step_through_the_projects_that_can_be_opened() {
    let mut grove = a_grove();
    press(&mut grove, KeyCode::Char(']'));
    assert_eq!(grove.open_requests, vec![id_of("api")]);
    // Once it lands, it is in front, its list with the keys.
    grove.handle_event(GroveEvent::Opened(
        id_of("api"),
        Box::new(Ok(app_for("api"))),
    ));
    assert_eq!(grove.front.as_deref(), Some(id_of("api").as_str()));
    assert_eq!(grove.focus, Focus::List);
    assert!(grove.front_app().unwrap().in_grove);
    // `[` goes back to the one still open, at once.
    press(&mut grove, KeyCode::Char('['));
    assert_eq!(grove.front.as_deref(), Some(id_of("shop").as_str()));
    // Round the end, past the moved one: `[` from the first is `docs`.
    press(&mut grove, KeyCode::Char('['));
    assert_eq!(grove.open_requests.last(), Some(&id_of("docs")));
    // One that could not be opened says why, and the front stays.
    grove.handle_event(GroveEvent::Opened(
        id_of("docs"),
        Box::new(Err("no such repository".into())),
    ));
    assert_eq!(grove.front.as_deref(), Some(id_of("shop").as_str()));
    let status = grove.front_app().unwrap().status.clone().unwrap();
    assert_eq!(status.message, "no such repository");
}

#[test]
fn braces_narrow_and_widen_the_column_within_bounds() {
    let mut grove = a_grove();
    press(&mut grove, KeyCode::Char('}'));
    assert_eq!(grove.width, COLUMN_WIDTH + COLUMN_STEP);
    for _ in 0..40 {
        press(&mut grove, KeyCode::Char('{'));
    }
    assert_eq!(grove.width, COLUMN_MIN_WIDTH);
    for _ in 0..40 {
        press(&mut grove, KeyCode::Char('}'));
    }
    assert_eq!(grove.width, COLUMN_MAX_WIDTH);
    let buf = draw(&mut grove, 120, 30);
    assert_eq!(buf[(COLUMN_MAX_WIDTH - 1, 0)].symbol(), "╮");
}

#[test]
fn in_the_column_jk_move_and_enter_opens() {
    let mut grove = a_grove();
    press(&mut grove, KeyCode::Left);
    press(&mut grove, KeyCode::Char('j'));
    press(&mut grove, KeyCode::Char('j'));
    assert_eq!(grove.selected().unwrap().name, "docs");
    press(&mut grove, KeyCode::Char('G'));
    assert_eq!(grove.selected().unwrap().name, "old");
    // A moved project is never opened.
    press(&mut grove, KeyCode::Enter);
    assert!(grove.open_requests.is_empty());
    let status = grove.front_app().unwrap().status.clone().unwrap();
    assert!(status.message.contains("not at"), "{}", status.message);
    press(&mut grove, KeyCode::Char('k'));
    press(&mut grove, KeyCode::Enter);
    assert_eq!(grove.open_requests, vec![id_of("docs")]);
    press(&mut grove, KeyCode::Char('g'));
    press(&mut grove, KeyCode::Right);
    assert_eq!(grove.focus, Focus::List, "→ on the open one: its list");
}

// `X` names every worktree it would stop before `y` stops them, and a
// project not open yet is opened to stop them.
#[test]
fn x_in_the_column_asks_naming_each_worktree_then_stops_them() {
    let mut grove = a_grove();
    grove.focus = Focus::Column;
    press(&mut grove, KeyCode::Char('j'));
    press(&mut grove, KeyCode::Char('X'));
    let confirm = grove.confirm.clone().expect("asks first");
    assert_eq!(confirm.worktrees, vec!["feat+broken0".to_string()]);
    let all = text_of(&draw(&mut grove, 120, 30));
    assert!(all.contains("stop the one worktree up in api?"), "{all}");
    assert!(all.contains("✗ feat+broken0"), "{all}");
    // Any other key leaves it asking; esc takes it back.
    press(&mut grove, KeyCode::Char('j'));
    assert!(grove.confirm.is_some());
    press(&mut grove, KeyCode::Esc);
    assert!(grove.confirm.is_none());
    press(&mut grove, KeyCode::Char('X'));
    press(&mut grove, KeyCode::Char('y'));
    assert_eq!(
        grove.stop_after_open,
        Some((id_of("api"), vec!["feat+broken0".to_string()]))
    );
    assert_eq!(grove.open_requests, vec![id_of("api")]);
    // Nothing up, nothing to ask.
    press(&mut grove, KeyCode::Char('j'));
    press(&mut grove, KeyCode::Char('X'));
    assert!(grove.confirm.is_none());
    // Every worktree, named, at narrow widths too.
    let mut grove = a_grove();
    grove.focus = Focus::Column;
    press(&mut grove, KeyCode::Char('X'));
    let all = text_of(&draw(&mut grove, 50, 20));
    assert!(all.contains("stop all 2 worktrees up in shop?"), "{all}");
    assert!(
        all.contains("feat+up0") && all.contains("feat+up1"),
        "{all}"
    );
}

#[test]
fn q_with_work_in_flight_elsewhere_asks_for_a_second() {
    let mut grove = a_grove();
    grove.adopt(app_for("api"), false);
    grove
        .open
        .get_mut(&id_of("api"))
        .unwrap()
        .app
        .pending
        .push(in_flight());
    press(&mut grove, KeyCode::Char('q'));
    assert!(!grove.should_quit);
    let status = grove.front_app().unwrap().status.clone().unwrap();
    assert!(
        status.message.contains("in flight in api"),
        "{}",
        status.message
    );
    press(&mut grove, KeyCode::Char('q'));
    assert!(grove.should_quit);
    // ctrl-c, at once.
    let mut grove = a_grove();
    grove.handle_event(GroveEvent::Input(Event::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    ))));
    assert!(grove.should_quit);
}

// A dialog on the project in front has the keys, wherever the focus is:
// `w` pressed into it is not the grove's.
#[test]
fn a_modal_on_the_front_app_keeps_the_keys() {
    let mut grove = a_grove();
    grove.front_app_mut().unwrap().modal = Some(Modal::Help);
    grove.focus = Focus::Column;
    press(&mut grove, KeyCode::Char('w'));
    assert!(grove.column_shown());
}

/// What a grove key can change.
fn grove_fingerprint(grove: &Grove) -> String {
    format!(
        "{:?}|{}|{:?}|{:?}|{}|{:?}|{:?}|{}|{}",
        grove.focus,
        grove.column_shown(),
        grove.front,
        grove.open_requests,
        grove.width,
        grove.confirm,
        grove.front_app().map(|a| a.modal.is_some()),
        grove.should_quit,
        grove.cursor,
    )
}

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
    ]);
    keys
}

// From the list the grove takes exactly the keys help lists for it, and
// none the list itself answers.
#[test]
fn from_the_list_the_grove_takes_exactly_its_documented_keys() {
    let documented: BTreeSet<String> = GROVE_KEYS
        .iter()
        .flat_map(|k| k.codes.iter().map(|c| format!("{c:?}")))
        .collect();
    let mut taken = BTreeSet::new();
    for code in candidate_keys() {
        let mut grove = a_grove();
        if grove.grove_key(KeyEvent::new(code, KeyModifiers::NONE)) {
            taken.insert(format!("{code:?}"));
        }
    }
    assert_eq!(taken, documented);
    for key in LIST_KEYS {
        for code in key.codes {
            assert!(
                !documented.contains(&format!("{code:?}")),
                "{code:?} is the list's and the grove's"
            );
        }
    }
}

// The column's help lists exactly the keys it answers: each pressed on a
// column with something for it to do.
#[test]
fn help_lists_exactly_the_keys_the_column_answers() {
    let documented: BTreeSet<String> = COLUMN_KEYS
        .iter()
        .flat_map(|k| k.codes.iter().map(|c| format!("{c:?}")))
        .collect();
    let mut answered = BTreeSet::new();
    for code in candidate_keys() {
        let mut grove = a_grove();
        grove.focus = Focus::Column;
        grove.cursor = 1;
        let before = grove_fingerprint(&grove);
        press(&mut grove, code);
        if grove_fingerprint(&grove) != before {
            answered.insert(format!("{code:?}"));
        }
    }
    assert_eq!(answered, documented);
}

#[test]
fn every_column_hint_is_a_key_help_documents() {
    for (key, _) in COLUMN_HINTS {
        assert!(
            COLUMN_KEYS
                .iter()
                .any(|k| k.keys.split(' ').any(|k| k == key)),
            "{key} is hinted and not in help"
        );
    }
}

// Help in the grove says what reaches every project; the project alone
// says nothing of it.
#[test]
fn help_in_the_grove_lists_its_keys() {
    let mut grove = a_grove();
    grove.focus = Focus::Column;
    press(&mut grove, KeyCode::Char('?'));
    let all = text_of(&draw(&mut grove, 120, 80));
    assert!(all.contains("across projects"), "{all}");
    assert!(all.contains("in the project column"), "{all}");
    let mut alone = app_for("shop");
    alone.modal = Some(Modal::Help);
    let mut terminal = Terminal::new(TestBackend::new(120, 80)).unwrap();
    terminal.draw(|f| render(f, &mut alone)).unwrap();
    assert!(!text_of(terminal.backend().buffer()).contains("across projects"));
}

// ---- events ----------------------------------------------------------

// An app behind still gets its own events and time, and costs no frame.
#[test]
fn an_app_behind_gets_its_events_and_ticks() {
    let mut grove = a_grove();
    grove.adopt(app_for("api"), false);
    let repaint = grove.handle_event(GroveEvent::Project(
        id_of("api"),
        AppEvent::Notice("from behind".into()),
    ));
    assert!(!repaint, "nothing of it is on screen");
    let api = &grove.open[&id_of("api")].app;
    assert_eq!(api.status.as_ref().unwrap().message, "from behind");
    grove.handle_event(GroveEvent::Tick);
    assert_eq!(grove.open[&id_of("api")].app.tick, 1);
    assert_eq!(grove.front_app().unwrap().tick, 1);
    // The front one's own events repaint.
    assert!(grove.handle_event(GroveEvent::Project(
        id_of("shop"),
        AppEvent::Notice("in front".into()),
    )));
    // Work in flight behind shows on its row.
    grove
        .open
        .get_mut(&id_of("api"))
        .unwrap()
        .app
        .pending
        .push(in_flight());
    let buf = draw(&mut grove, 120, 30);
    assert!(cells(&buf, 2, 0, COLUMN_WIDTH).contains("┃◌ api"));
}

// Read again, the projects move; the cursor stays on its project.
#[test]
fn new_summaries_keep_the_cursor_on_its_project() {
    let mut grove = a_grove();
    grove.cursor = 2;
    let mut summaries = grove.summaries.clone();
    summaries.rotate_left(1);
    grove.handle_event(GroveEvent::Summaries(summaries));
    assert_eq!(grove.selected().unwrap().name, "docs");
    grove.handle_event(GroveEvent::Summaries(Vec::new()));
    assert_eq!(grove.cursor, 0);
}

// What `main` does before the TUI, for a project opened from the grove:
// found where it was registered, and refused when that is another
// project now. On a fixture repository and a home of the test's own.
#[test]
fn a_project_opens_from_where_it_is_registered() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("acme-shop");
    crate::testutil::init_repo(&root);
    let home = dir.path().join("pando-home");
    let project = ProjectRef::from_root(&root).unwrap();
    let mut summary = summary("acme-shop", 0, 0);
    summary.id = project.id.clone();
    summary.root = root.clone();
    let app = open::open_project(&home, &summary).expect("opens");
    assert_eq!(app.paths.project.id, project.id);
    assert!(home.join("projects").join(&project.id).is_dir());
    summary.id = "acme-shop-ffffffff".into();
    let refused = open::open_project(&home, &summary).err().unwrap();
    assert!(
        format!("{refused:#}").contains("another project"),
        "{refused:#}"
    );
}
