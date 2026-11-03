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
    assert!(
        !strip.contains("tab list"),
        "the list has the keys: {strip}"
    );
    assert!(row_of(&buf, 1).contains("shop"), "{}", row_of(&buf, 1));
    assert_eq!(grove.hits.column, Rect::new(0, 0, 60, 1));
    // With the keys, the strip follows the column's cursor.
    press(&mut grove, KeyCode::Left);
    press(&mut grove, KeyCode::Char('j'));
    let strip = row_of(&draw(&mut grove, 60, 24), 0);
    assert!(strip.contains("✗ api · ✗1 failed · 2/4"), "{strip}");
    // And says what its keys do now, and how to give them back.
    assert!(strip.trim_end().ends_with("⏎ open  tab list"), "{strip}");
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

// A click on the project in front gives its list the keys: `j` after
// clicking a row moves the list, not the column's cursor.
#[test]
fn a_click_on_the_front_project_gives_it_the_keys() {
    let mut grove = a_grove();
    draw(&mut grove, 120, 30);
    grove.focus = Focus::Column;
    let (rect, _) = grove
        .front_app()
        .unwrap()
        .hits
        .list
        .clone()
        .expect("the list is drawn");
    click(&mut grove, rect.x + 2, rect.y);
    assert_eq!(grove.focus, Focus::List);
    press(&mut grove, KeyCode::Char('j'));
    assert_eq!(grove.cursor, 0, "the column's cursor stays");
    // A press in the column keeps it there.
    let (row, _) = grove.hits.rows[1];
    click(&mut grove, row.x + 3, row.y);
    assert_eq!(grove.focus, Focus::Column);
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
    let mut summaries = a_grove().summaries;
    // api: one up beside the one that failed.
    summaries[1]
        .worktrees
        .insert(0, live("feat+up0", running()));
    summaries[1].up = 1;
    let mut grove = Grove::new_for_test(summaries);
    grove.adopt(app_for("shop"), true);
    grove.focus = Focus::Column;
    press(&mut grove, KeyCode::Char('j'));
    press(&mut grove, KeyCode::Char('X'));
    let confirm = grove.confirm.clone().expect("asks first");
    assert_eq!(confirm.worktrees, vec!["feat+up0".to_string()]);
    let all = text_of(&draw(&mut grove, 120, 30));
    assert!(all.contains("stop the one worktree up in api?"), "{all}");
    assert!(all.contains("feat+up0"), "{all}");
    // Any other key leaves it asking; esc takes it back.
    press(&mut grove, KeyCode::Char('j'));
    assert!(grove.confirm.is_some());
    press(&mut grove, KeyCode::Esc);
    assert!(grove.confirm.is_none());
    press(&mut grove, KeyCode::Char('X'));
    press(&mut grove, KeyCode::Char('y'));
    assert_eq!(
        grove.stop_after_open,
        BTreeMap::from([(id_of("api"), vec!["feat+up0".to_string()])])
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

// A project whose only worktree failed has nothing up: `X` says so, as
// the project's own `X` does, rather than asking to stop a ✗ as "up".
#[test]
fn x_on_a_project_with_only_failures_has_nothing_to_stop() {
    let mut grove = a_grove();
    grove.focus = Focus::Column;
    press(&mut grove, KeyCode::Char('j'));
    assert_eq!(grove.selected().unwrap().name, "api");
    press(&mut grove, KeyCode::Char('X'));
    assert!(grove.confirm.is_none(), "{:?}", grove.confirm);
    let status = grove.front_app().unwrap().status.clone().unwrap();
    assert_eq!(status.message, "nothing is up in api");
}

// Two stops confirmed for two projects still being opened: each carried
// out in its own project once that one opens, neither dropped for the
// other.
#[test]
fn stops_confirmed_for_two_projects_opening_each_land_in_their_own() {
    let mut summaries = a_grove().summaries;
    summaries[2] = summary("docs", 1, 0);
    let mut grove = Grove::new_for_test(summaries);
    grove.adopt(app_for("shop"), true);
    grove.focus = Focus::Column;
    for name in ["api", "docs"] {
        grove.cursor = grove.summaries.iter().position(|s| s.name == name).unwrap();
        if name == "api" {
            grove.summaries[1]
                .worktrees
                .push(live("feat+up0", running()));
        }
        press(&mut grove, KeyCode::Char('X'));
        press(&mut grove, KeyCode::Char('y'));
    }
    assert_eq!(grove.open_requests, vec![id_of("api"), id_of("docs")]);
    assert_eq!(
        grove.stop_after_open.len(),
        2,
        "{:?}",
        grove.stop_after_open
    );
    // api opens first: its stop starts, and docs' still waits.
    let mut api = app_for("api");
    api.state.worktrees.clear();
    grove.handle_event(GroveEvent::Opened(id_of("api"), Box::new(Ok(api))));
    let api = &grove.open[&id_of("api")].app;
    assert!(
        api.pending.iter().any(|p| p.kind == PendingKind::StopAll),
        "api stops what was confirmed"
    );
    assert_eq!(
        grove.stop_after_open,
        BTreeMap::from([(id_of("docs"), vec!["feat+up0".to_string()])])
    );
    // docs could not be opened: its stop goes with it.
    grove.handle_event(GroveEvent::Opened(
        id_of("docs"),
        Box::new(Err("gone".into())),
    ));
    assert!(grove.stop_after_open.is_empty());
}

// A stop confirmed for a project open behind that its app refuses — an
// action already covers the worktree — is said where it can be read, on
// the front project's header, never only on the hidden app's.
#[test]
fn a_stop_refused_behind_says_so_in_front() {
    let mut grove = a_grove();
    let mut restart = in_flight();
    restart.kind = PendingKind::Restart;
    busy_behind(&mut grove, "api", restart);
    grove.summaries[1].worktrees = vec![live("feat+one", running())];
    grove.focus = Focus::Column;
    grove.cursor = 1;
    press(&mut grove, KeyCode::Char('X'));
    press(&mut grove, KeyCode::Char('y'));
    let api = &grove.open[&id_of("api")].app;
    assert_eq!(api.pending.len(), 1, "the restart alone is in flight");
    let status = grove.front_app().unwrap().status.clone().unwrap();
    assert!(
        status.message.contains("nothing was stopped in api")
            && status.message.contains("already busy"),
        "{}",
        status.message
    );
}

/// `name`'s app, open behind, with `pending` in flight.
fn busy_behind(grove: &mut Grove, name: &str, pending: PendingAction) {
    grove.adopt(app_for(name), false);
    grove
        .open
        .get_mut(&id_of(name))
        .unwrap()
        .app
        .pending
        .push(pending);
}

/// A start in flight that a quit cancels, and the cancel it reads.
fn cancellable() -> (PendingAction, crate::cancel::Cancel) {
    let cancel = crate::cancel::Cancel::new();
    let mut pending = in_flight();
    pending.cancel = Some(cancel.clone());
    (pending, cancel)
}

#[test]
fn q_with_work_in_flight_elsewhere_asks_for_a_second() {
    let mut grove = a_grove();
    busy_behind(&mut grove, "api", in_flight());
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

// Work in flight in front and behind: the front app's two presses for its
// own, then one for the grove's — not a fourth, which the front app's own
// question asked again would have taken, overwriting the grove's.
#[test]
fn work_in_front_and_behind_quits_on_the_third_q() {
    let mut grove = a_grove();
    grove.front_app_mut().unwrap().pending.push(in_flight());
    busy_behind(&mut grove, "api", in_flight());
    press(&mut grove, KeyCode::Char('q'));
    press(&mut grove, KeyCode::Char('q'));
    assert!(!grove.should_quit, "the grove names api first");
    let status = grove.front_app().unwrap().status.clone().unwrap();
    assert!(status.message.contains("in api"), "{}", status.message);
    press(&mut grove, KeyCode::Char('q'));
    assert!(grove.should_quit, "the third q is the grove's second");
}

// The front app's `q` `q` cancelled its start and asks to quit once it is
// undone; with work behind, the grove asks first. Its question is answered
// only by a key: the front app asking again on the next tick is not one.
#[test]
fn a_front_app_asking_again_never_answers_the_groves_question() {
    let mut grove = a_grove();
    busy_behind(&mut grove, "api", in_flight());
    // As the front app's second `q` leaves it once its undo has run.
    grove.front_app_mut().unwrap().quitting = true;
    grove.handle_event(GroveEvent::Tick);
    assert!(!grove.should_quit, "the grove names api first");
    assert!(grove.quit_armed.is_some());
    for _ in 0..3 {
        grove.handle_event(GroveEvent::Tick);
    }
    assert!(!grove.should_quit, "no tick answers for the developer");
    press(&mut grove, KeyCode::Char('q'));
    assert!(grove.should_quit, "the q the grove asked for does");
}

// The confirming `q` cancels a start in flight behind and waits for its
// undo, as a project's own second `q` does, rather than abandoning it
// half done; a `q` meanwhile quits at once.
#[test]
fn the_groves_second_q_cancels_the_starts_behind_and_waits() {
    let mut grove = a_grove();
    let (pending, cancel) = cancellable();
    busy_behind(&mut grove, "api", pending);
    press(&mut grove, KeyCode::Char('q'));
    press(&mut grove, KeyCode::Char('q'));
    assert!(cancel.is_cancelled(), "the start behind is cancelled");
    assert!(!grove.should_quit, "and its undo waited for");
    let status = grove.front_app().unwrap().status.clone().unwrap();
    assert!(
        status.message.contains("cancelling feat/one in api"),
        "{}",
        status.message
    );
    grove.handle_event(GroveEvent::Tick);
    assert!(!grove.should_quit, "still undoing");
    // Its undo ran.
    grove
        .open
        .get_mut(&id_of("api"))
        .unwrap()
        .app
        .pending
        .clear();
    grove.handle_event(GroveEvent::Tick);
    assert!(grove.should_quit, "pando goes once it is undone");
    // A q while it is undone quits at once.
    let mut grove = a_grove();
    let (pending, _cancel) = cancellable();
    busy_behind(&mut grove, "api", pending);
    press(&mut grove, KeyCode::Char('q'));
    press(&mut grove, KeyCode::Char('q'));
    assert!(!grove.should_quit);
    press(&mut grove, KeyCode::Char('q'));
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
        "{:?}|{}|{:?}|{:?}|{}|{:?}|{:?}|{:?}|{}|{}",
        grove.focus,
        grove.column_shown(),
        grove.front,
        grove.open_requests,
        grove.width,
        grove.detail,
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
        // Something up in it, which `X` stops.
        grove.summaries[1]
            .worktrees
            .push(live("feat+up0", running()));
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

// ---- the layout ------------------------------------------------------

/// The detail pane's top-left corner in `buf`, right of `x0`: where its
/// rounded border starts, below the list or beside it.
fn panes(buf: &Buffer, x0: u16) -> Vec<(u16, u16)> {
    let mut corners = Vec::new();
    for y in 1..buf.area().height {
        for x in x0..buf.area().width {
            if buf[(x, y)].symbol() == "╭" {
                corners.push((x, y));
            }
        }
    }
    corners
}

// `[ui]` is where the grove starts: the column as it was left, its width,
// and the detail pane, for every project it opens.
#[test]
fn the_grove_starts_as_the_user_layer_left_it() {
    let ui = UiSection {
        grove: Some("off".into()),
        grove_width: Some(30),
        detail: Some("bottom".into()),
        detail_size: Some(55),
        ..Default::default()
    };
    let mut grove = Grove::new(
        std::path::PathBuf::from("/pando-test-does-not-exist/home"),
        ui,
        a_grove().summaries,
    );
    assert!(!grove.column_shown());
    assert_eq!(grove.width, 30);
    grove.adopt(app_for("shop"), true);
    let app = grove.front_app().unwrap();
    assert_eq!(
        app.detail.placement,
        crate::tui::app::DetailPlacement::Bottom
    );
    assert_eq!(app.detail.size, Some(55));
    // A width the loader would have refused is still one it draws.
    let wide = UiSection {
        grove_width: Some(400),
        ..Default::default()
    };
    let grove = Grove::new(std::path::PathBuf::from("/x"), wide, Vec::new());
    assert_eq!(grove.width, COLUMN_MAX_WIDTH);
}

// Every key that changes the layout saves exactly what it changed, and a
// key that changes nothing saves nothing.
#[test]
fn each_layout_key_saves_what_it_changed() {
    use crate::tui::app::DetailPlacement;
    let mut grove = a_grove();
    draw(&mut grove, 120, 30);
    for code in ['w', 'w', '}', '{', '|', '>', '<', '|', '|'] {
        press(&mut grove, KeyCode::Char(code));
    }
    assert_eq!(
        grove.saves,
        vec![
            Saved::Grove(Shown::Off),
            Saved::Grove(Shown::On),
            Saved::GroveWidth(COLUMN_WIDTH + COLUMN_STEP),
            Saved::GroveWidth(COLUMN_WIDTH),
            Saved::Detail(DetailPlacement::Bottom),
            // Stacked by `|` on the last frame drawn: wide, side by side,
            // so from its share there.
            Saved::DetailSize(45),
            Saved::DetailSize(40),
            Saved::Detail(DetailPlacement::Off),
            Saved::Detail(DetailPlacement::Right),
        ]
    );
    // At the edges nothing moves and nothing is saved.
    grove.saves.clear();
    grove.width = COLUMN_MIN_WIDTH;
    press(&mut grove, KeyCode::Char('{'));
    grove.detail.size = Some(crate::config::DETAIL_SIZE_MAX);
    press(&mut grove, KeyCode::Char('>'));
    grove.detail.placement = DetailPlacement::Off;
    press(&mut grove, KeyCode::Char('<'));
    assert_eq!(grove.saves, vec![]);
    // Every open project lays its detail pane out the same.
    grove.adopt(app_for("api"), false);
    press(&mut grove, KeyCode::Char('|'));
    assert_eq!(
        grove.open[&id_of("api")].app.detail,
        grove.front_app().unwrap().detail
    );
}

// The spellings `[ui]` takes are the ones the keys save.
#[test]
fn the_layouts_words_are_the_ones_config_takes() {
    use crate::tui::app::DetailPlacement;
    let modes: Vec<&str> = Shown::ALL.iter().map(|s| s.word()).collect();
    assert_eq!(modes, crate::config::GROVE_MODES);
    let placements: Vec<&str> = DetailPlacement::ALL.iter().map(|p| p.word()).collect();
    assert_eq!(placements, crate::config::DETAIL_PLACEMENTS);
    for word in modes {
        assert_eq!(Shown::from_word(word).unwrap().word(), word);
    }
    assert_eq!(COLUMN_MIN_WIDTH, crate::config::GROVE_WIDTH_MIN);
    assert_eq!(COLUMN_MAX_WIDTH, crate::config::GROVE_WIDTH_MAX);
}

// Written into `[ui]` beside what is there, whatever its form.
#[test]
fn a_saved_layout_keeps_the_rest_of_the_file() {
    use crate::tui::app::DetailPlacement;
    let mut doc: toml_edit::DocumentMut = "# mine\n[ui]\ntheme = \"gruvbox\"\n".parse().unwrap();
    layout::set_layout(
        &mut doc,
        &[
            Saved::Grove(Shown::Off),
            Saved::GroveWidth(30),
            Saved::Detail(DetailPlacement::Bottom),
            Saved::DetailSize(55),
        ],
    );
    assert_eq!(
        doc.to_string(),
        "# mine\n[ui]\ntheme = \"gruvbox\"\ngrove = \"off\"\ngrove_width = 30\ndetail = \"bottom\"\ndetail_size = 55\n"
    );
    let mut inline: toml_edit::DocumentMut = "ui = { sort = \"name\" }\n".parse().unwrap();
    layout::set_layout(&mut inline, &[Saved::GroveWidth(20)]);
    assert!(inline.to_string().contains("grove_width = 20"), "{inline}");
    // What it writes, the loader reads back.
    let ui: UiSection = toml::from_str::<toml::Table>(&doc.to_string()).unwrap()["ui"]
        .clone()
        .try_into()
        .unwrap();
    assert_eq!(ui.grove_width, Some(30));
}

// The worker writes in the order the keys came, so the last press wins;
// into a home of the test's own.
#[test]
fn the_saver_writes_the_last_of_each_key() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("pando-home");
    std::fs::create_dir_all(&home).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let saver = spawn_layout_saver(home.clone(), tx);
    for width in [28, 30, 32, 22] {
        saver.send(Saved::GroveWidth(width)).unwrap();
    }
    saver.send(Saved::Grove(Shown::On)).unwrap();
    drop(saver);
    let file = home.join("config.toml");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        if text.contains("grove_width = 22") && text.contains("grove = \"on\"") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "never saved: {text}");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(rx.try_recv().is_err(), "a save that worked said it failed");
    // A home that is not there is said, not made.
    let (tx, rx) = std::sync::mpsc::channel();
    let saver = spawn_layout_saver(dir.path().join("gone"), tx);
    saver.send(Saved::GroveWidth(20)).unwrap();
    match rx.recv_timeout(std::time::Duration::from_secs(10)) {
        Ok(GroveEvent::NotSaved(why)) => assert!(why.contains("no pando home"), "{why}"),
        _ => panic!("a failed save said nothing"),
    }
    assert!(!dir.path().join("gone").exists());
}

// Each placement of the detail pane, drawn: right of the list, under it,
// or not at all, the list then the whole body; and `<` `>` move its edge.
#[test]
fn the_detail_pane_sits_where_it_was_put() {
    use crate::tui::app::{DetailLayout, DetailPlacement};
    let mut grove = a_grove();
    let x0 = COLUMN_WIDTH;
    let at = |grove: &mut Grove, detail: DetailLayout| {
        grove.detail = detail;
        grove.share_detail();
        let buf = draw(grove, 120, 30);
        (panes(&buf, x0), buf)
    };
    let right = |size| DetailLayout {
        placement: DetailPlacement::Right,
        size,
    };
    // Right, by default two fifths of what is right of the column.
    let (corners, _) = at(&mut grove, right(None));
    assert_eq!(corners.len(), 2, "{corners:?}");
    assert_eq!(corners[0].1, corners[1].1, "side by side");
    let body = 120 - x0;
    let edge = x0 + body - body * 40 / 100;
    assert!(corners[1].0.abs_diff(edge) <= 1, "{corners:?}, {edge}");
    // Bigger by `>`: the edge moves left.
    let (wider, _) = at(&mut grove, right(Some(60)));
    assert!(wider[1].0 < corners[1].0, "{wider:?} {corners:?}");
    // Bottom: under the list, the same left edge.
    let (stacked, _) = at(
        &mut grove,
        DetailLayout {
            placement: DetailPlacement::Bottom,
            size: None,
        },
    );
    assert_eq!(stacked.len(), 2, "{stacked:?}");
    assert_eq!(stacked[0].0, stacked[1].0, "stacked");
    assert!(stacked[1].1 > stacked[0].1);
    assert!(grove.front_app().unwrap().detail_stacked);
    let (bigger, _) = at(
        &mut grove,
        DetailLayout {
            placement: DetailPlacement::Bottom,
            size: Some(80),
        },
    );
    assert!(bigger[1].1 < stacked[1].1, "{bigger:?} {stacked:?}");
    // Off: the list alone, as wide as the body.
    let (alone, buf) = at(
        &mut grove,
        DetailLayout {
            placement: DetailPlacement::Off,
            size: Some(80),
        },
    );
    assert_eq!(alone.len(), 1, "{alone:?}");
    assert_eq!(buf[(119, alone[0].1)].symbol(), "╮", "the list to the edge");
    assert!(grove.front_app().unwrap().hits.preview.is_none());
}

// `{` and `}` widen the column as drawn, within what the screen leaves the
// project: never more than half of it.
#[test]
fn the_column_is_as_wide_as_it_was_set() {
    let mut grove = a_grove();
    for width in [COLUMN_MIN_WIDTH, COLUMN_WIDTH, COLUMN_MAX_WIDTH] {
        grove.width = width;
        let buf = draw(&mut grove, 120, 30);
        assert_eq!(buf[(width - 1, 0)].symbol(), "╮", "width {width}");
        assert_eq!(grove.hits.column.width, width);
    }
    grove.width = COLUMN_MAX_WIDTH;
    draw(&mut grove, 76, 24);
    assert_eq!(grove.hits.column.width, 38, "half of a narrow screen");
}

// Every state of a row reads apart in every built-in theme, either half,
// by more than a colour a theme may not keep apart: the cursor's band,
// the stem's weight and glyph, and bold for a project that has something
// to say. Tokyo Night's light half draws dim text in its text colour.
/// Where a row's stem is drawn: inside the border, after the cursor.
const STEM_X: u16 = 3;

#[test]
fn every_row_state_reads_apart_in_every_theme() {
    use crate::theme::{Appearance, BUILT_IN, Source, Theme};
    use ratatui::style::Modifier;
    for (name, text) in BUILT_IN {
        let theme = Theme::parse(name, text, Source::BuiltIn).unwrap();
        for appearance in [Appearance::Dark, Appearance::Light] {
            let palette = theme.palette(appearance);
            crate::theme::set_palette(palette);
            let mut grove = a_grove();
            grove.focus = Focus::Column;
            grove.cursor = 2; // docs, idle
            let buf = draw(&mut grove, 100, 20);
            let at = |name: &str| {
                let y = (1..6)
                    .find(|&y| cells(&buf, y, 0, COLUMN_WIDTH).contains(name))
                    .unwrap();
                let x = (0..COLUMN_WIDTH)
                    .find(|&x| buf[(x, y)].symbol() == name.chars().next().unwrap().to_string())
                    .unwrap();
                (x, y)
            };
            let says = format!("{name} {appearance:?}");
            // The cursor's band, and only its row.
            let (docs_x, docs_y) = at("docs");
            assert_eq!(buf[(docs_x, docs_y)].bg, palette.highlight_bg, "{says}");
            let (shop_x, shop_y) = at("shop");
            assert_ne!(buf[(shop_x, shop_y)].bg, palette.highlight_bg, "{says}");
            // Something up or failed is bold; idle and moved are not.
            let bold = |(x, y): (u16, u16)| buf[(x, y)].modifier.contains(Modifier::BOLD);
            assert!(bold(at("shop")) && bold(at("api")), "{says}");
            assert!(!bold(at("docs")) && !bold(at("old")), "{says}");
            // Each state its own stem, and a failed one red, not the
            // trunk's colour.
            let stem = |y: u16| buf[(STEM_X, y)].clone();
            let stems: Vec<String> = [shop_y, at("api").1, docs_y, at("old").1]
                .iter()
                .map(|&y| stem(y).symbol().to_string())
                .collect();
            assert_eq!(stems, ["┃", "┃", "│", "╎"], "{says}");
            assert_eq!(stem(at("api").1).fg, palette.red, "{says}");
            assert_ne!(palette.red, palette.text, "{says}");
            assert_ne!(palette.red, palette.green, "{says}");
            // The pane with the keys has the blue border and a blue
            // cursor; without them, neither.
            assert_eq!(buf[(0, 0)].fg, palette.blue, "{says}");
            assert_eq!(buf[(1, docs_y)].fg, palette.blue, "{says}");
            grove.focus = Focus::List;
            let buf = draw(&mut grove, 100, 20);
            assert_eq!(buf[(0, 0)].fg, palette.border, "{says}");
            assert_ne!(buf[(1, docs_y)].fg, palette.blue, "{says}");
            assert_eq!(buf[(docs_x, docs_y)].bg, palette.highlight_bg, "{says}");
        }
    }
}

// The column's border makes room for what is up before the build's
// branch, which shortens and then goes.
#[test]
fn the_columns_title_keeps_the_tally_and_shortens_the_branch() {
    let label = Some("feat/grove@abc1234");
    let text = |line: &ratatui::text::Line| line.to_string();
    let (title, tally) = crate::tui::render::column_title(40, label, 2, 1);
    assert_eq!(text(&title), " pando ⎇ feat/grove@abc12… ");
    assert_eq!(text(&tally), " ● 2 ✗ 1 ");
    let (title, _) = crate::tui::render::column_title(26, label, 2, 1);
    assert_eq!(text(&title), " pando ⎇ fe… ");
    let (title, _) = crate::tui::render::column_title(COLUMN_MIN_WIDTH, label, 2, 1);
    assert_eq!(text(&title), " pando ");
    // Nothing up: the branch has the room, and no tally is drawn.
    let (title, tally) = crate::tui::render::column_title(26, label, 0, 0);
    assert_eq!(text(&title), " pando ⎇ feat/grove@… ");
    assert_eq!(text(&tally), "");
    // Tight when spaced out does not fit.
    let (title, tally) = crate::tui::render::column_title(COLUMN_MIN_WIDTH, label, 12, 3);
    assert_eq!(
        (text(&title), text(&tally)),
        (" pando ".into(), " ●12 ✗3 ".into())
    );
    // Every one fits its border: corners, title, a cell, tally.
    for width in COLUMN_MIN_WIDTH..=COLUMN_MAX_WIDTH {
        let (title, tally) = crate::tui::render::column_title(width, label, 12, 3);
        assert!(
            title.width() + tally.width() + 3 <= usize::from(width),
            "{width}: {title} {tally}"
        );
    }
}
