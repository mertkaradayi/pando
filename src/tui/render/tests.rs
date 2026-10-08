use super::*;
use super::{chrome::*, detail::*, list::*, log_viewer::*};
use crate::log_tail::{LogLevel, ParsedLine};
use crate::theme::{
    blue, cyan, green, highlight_bg, red, search_cursor_bg, search_match_bg, text_dim, text_muted,
    yellow,
};
use crate::tui::app::App;
use crate::tui::app::tests::{
    app_with_logs, app_with_main, running_phase, test_app, with_process, with_second_process,
    write_log, wt,
};
use crate::tui::app::{BranchLoadState, GitStage, Modal};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| render(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn text_of(buf: &Buffer) -> String {
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Shared mode runs no containers of pando's, so the header is the only
// place a developer finds out the project's database is down.
#[test]
fn the_header_carries_a_chip_for_each_shared_service() {
    let mut app = test_app(&["feat+one"]);
    app.main = Some(wt("acme-shop"));
    app.service_health = crate::tui::app::ServiceHealth {
        shared: vec![
            crate::actions::ServiceStatus {
                name: "postgres".into(),
                port: Some(5432),
                up: true,
                logging: false,
                env_file: None,
            },
            crate::actions::ServiceStatus {
                name: "redis".into(),
                port: Some(6379),
                up: false,
                logging: false,
                env_file: None,
            },
        ],
        worktrees: std::collections::BTreeMap::new(),
    };
    let text = text_of(&draw(&mut app, 100, 12));
    let header = text.lines().next().unwrap_or_default().to_string();
    assert!(header.contains("postgres"), "{header}");
    assert!(header.contains("redis"), "{header}");
    assert!(header.contains("●"), "{header}");
}

// Isolated mode puts them in the detail pane instead, one row apiece,
// because there they belong to a worktree rather than to the project.
#[test]
fn the_detail_pane_has_a_row_for_each_private_service() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    app.service_health = crate::tui::app::ServiceHealth {
        shared: Vec::new(),
        worktrees: std::collections::BTreeMap::from([(
            "feat+one".to_string(),
            vec![
                crate::actions::ServiceStatus {
                    name: "postgres".into(),
                    port: Some(17_004),
                    up: true,
                    logging: false,
                    env_file: None,
                },
                crate::actions::ServiceStatus {
                    name: "redis".into(),
                    port: Some(17_006),
                    up: false,
                    logging: false,
                    env_file: None,
                },
            ],
        )]),
    };
    let text = text_of(&draw(&mut app, 120, 24));
    assert!(text.contains("postgres"), "{text}");
    assert!(text.contains("port 17004"), "{text}");
    assert!(
        text.contains("down      port 17006"),
        "a service that is not answering says so: {text}"
    );
}

#[test]
fn renders_at_any_terminal_size_without_panicking() {
    let mut app = test_app(&["feat+one", "feat+two", "a-very-long-worktree-name-here"]);
    app.worktrees[1].dirty = Some(true);
    app.worktrees[2].prunable = true;
    app.main = Some(wt("acme-shop"));
    // The detail pane has something to paint in every phase, including
    // a failure whose reason is longer than any pane.
    with_process(&mut app, "feat+one", running_phase());
    with_process(
        &mut app,
        "feat+two",
        crate::state::Phase::Starting {
            since: chrono::Utc::now(),
        },
    );
    with_process(
        &mut app,
        "a-very-long-worktree-name-here",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited — something else is listening on port 17342; stop it, \
                     or `pando stop` the worktree that owns it"
                .into(),
        },
    );
    // And a worktree running two processes, which adds a row per
    // process to a pane that may have room for none of them.
    with_second_process(
        &mut app,
        "feat+one",
        "api",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited".into(),
        },
    );
    // Private services add another row apiece to the same pane, and
    // another chip apiece to a header that may be one column wide.
    app.service_health = crate::tui::app::ServiceHealth {
        shared: vec![
            crate::actions::ServiceStatus {
                name: "postgres".into(),
                port: Some(5432),
                up: true,
                logging: false,
                env_file: None,
            },
            crate::actions::ServiceStatus {
                name: "an-extremely-long-service-name".into(),
                port: None,
                up: false,
                logging: false,
                env_file: None,
            },
        ],
        worktrees: std::collections::BTreeMap::from([(
            "feat+one".to_string(),
            vec![
                crate::actions::ServiceStatus {
                    name: "postgres".into(),
                    port: Some(17_004),
                    up: true,
                    logging: false,
                    env_file: None,
                },
                crate::actions::ServiceStatus {
                    name: "an-extremely-long-service-name".into(),
                    port: None,
                    up: false,
                    logging: false,
                    env_file: None,
                },
            ],
        )]),
    };
    // And a public URL, which adds a marker to every list row and a
    // line to a detail pane that may have room for neither.
    crate::tui::app::tests::with_share(&mut app, "feat+one", Some(17_009));

    for width in 1..=120u16 {
        for height in [1u16, 2, 3, 5, 12, 40] {
            draw(&mut app, width, height);
        }
    }
    // Well past the width where a percentage of it stops fitting in a
    // u16 (1093 * 60 overflows), and as tall again: a large display with
    // a small font really does reach four digits.
    for width in [1092u16, 1093, 1500, 2000, 3000] {
        for height in [1u16, 3, 40] {
            draw(&mut app, width, height);
        }
    }
    for height in [1092u16, 1093, 3000] {
        draw(&mut app, 80, height);
    }
}

fn a_pr(number: u32, branch: &str, draft: bool, fork: bool) -> crate::worktree::PrInfo {
    crate::worktree::PrInfo {
        number,
        title: format!("a pull request with a title long enough to be cut {number}"),
        branch: branch.into(),
        author: "someone".into(),
        draft,
        state: crate::worktree::PrState::Open,
        url: String::new(),
        cross_repository: fork,
        base: "main".into(),
    }
}

// Open pull requests with a title each, the one that already has a
// worktree marked, and merged ones left out.
#[test]
fn the_pull_request_picker_lists_the_open_ones() {
    let mut app = test_app(&["feat+one"]);
    app.pr_list = vec![
        a_pr(12, "feat/new", true, false),
        a_pr(11, "feat/one", false, false),
        crate::worktree::PrInfo {
            state: crate::worktree::PrState::Merged,
            title: "long gone".into(),
            ..a_pr(10, "feat/old", false, false)
        },
    ];
    app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(rendered.contains("open pull requests"), "{rendered}");
    assert!(rendered.contains("#12"), "{rendered}");
    assert!(rendered.contains("draft · @someone"), "{rendered}");
    assert!(rendered.contains("has a worktree"), "{rendered}");
    assert!(!rendered.contains("long gone"), "{rendered}");
    assert!(rendered.contains("⏎ makes a worktree for it"), "{rendered}");
}

#[test]
fn renders_every_modal_at_any_terminal_size() {
    let (reply, _rx) = std::sync::mpsc::channel();
    let modals = [
        Modal::Help,
        Modal::Messages,
        Modal::Create {
            input: "feat/new".into(),
            branches: BranchLoadState::Loading,
            selected: 0,
            base: Some("origin/some-rather-long-release-branch".into()),
        },
        Modal::PullRequests {
            input: "a filter that is rather long for the box it is typed in".into(),
            selected: 3,
        },
        Modal::Remove {
            name: "feat+one".into(),
            created_by_pando: false,
        },
        Modal::Unshare {
            name: "feat+one".into(),
            url: "https://a-rather-long-quick-tunnel-hostname.trycloudflare.com".into(),
        },
        Modal::StopAll {
            names: (0..20).map(|i| format!("feat+number-{i}")).collect(),
        },
        Modal::Share {
            name: "feat+one".into(),
        },
        Modal::SwitchMode {
            name: "feat+one".into(),
            to: crate::state::ServiceMode::Isolated,
        },
        Modal::Mode {
            name: "feat+one".into(),
            selected: 1,
        },
        Modal::Question {
            question: free_slot_question(&["feat+two", "feat+three"]),
            selected: 1,
            custom: None,
            reply: reply.clone(),
        },
        Modal::Question {
            question: crate::actions::Question {
                slot: crate::detect::Slot::DevCmd,
                prompt: "Which command starts the local development server?".into(),
                options: (0..12)
                    .map(|i| {
                        (
                            format!("pnpm dev:{i}"),
                            format!("package.json scripts.dev:{i}"),
                        )
                    })
                    .collect(),
                preselect: Some(11),
                allow_custom: true,
                allow_none: false,
                multi: false,
                checked: Vec::new(),
                details: Vec::new(),
                answer_file: None,
                snippet: String::new(),
            },
            selected: 11,
            custom: Some("a rather long command typed by hand".into()),
            reply,
        },
    ];
    let read = || Box::new(crate::tui::app::tests::a_git_read(false));
    let git = |stage| Modal::Git {
        name: "feat+one".into(),
        stage,
    };
    let modals = modals.into_iter().chain([
        git(GitStage::Reading),
        git(GitStage::Menu {
            read: read(),
            selected: 4,
        }),
        git(GitStage::Preview {
            read: read(),
            action: crate::actions::git::GitAction::Rebase,
        }),
        git(GitStage::Running {
            read: read(),
            action: crate::actions::git::GitAction::Merge,
        }),
        git(GitStage::Result {
            read: read(),
            action: crate::actions::git::GitAction::Rebase,
            ran: Ok(crate::actions::git::Ran::Conflict {
                op: crate::worktree::InProgress::Rebase,
                files: (0..9).map(|i| format!("apps/web/src/file-{i}.ts")).collect(),
                at: Some("abc1234 a commit subject rather longer than the box".into()),
            }),
            restart: false,
        }),
        git(GitStage::Result {
            read: read(),
            action: crate::actions::git::GitAction::Fetch,
            ran: Err("`git fetch origin` failed: a reason long enough to wrap twice over in a narrow box".into()),
            restart: true,
        }),
    ]);
    for modal in modals {
        let mut app = test_app(&["feat+one"]);
        app.pr_list = (0..12)
            .map(|i| a_pr(900 + i, &format!("feat/{i}"), i % 3 == 0, i % 4 == 0))
            .chain([a_pr(7, "feat/one", false, false)])
            .collect();
        app.modal = Some(modal);
        for width in [1u16, 4, 20, 41, 80, 200, 1092, 1093, 1500, 2000, 3000] {
            for height in [1u16, 3, 8, 24] {
                draw(&mut app, width, height);
            }
        }
        for height in [1092u16, 1093, 3000] {
            draw(&mut app, 80, height);
        }
    }
}

/// A log with one of everything the viewer has to paint: ANSI colour, a
/// multi-line JSON block, errors and warnings, a duration, a URL that
/// has to wrap, wide glyphs, and a line longer than any terminal.
fn a_log_of_everything() -> Vec<String> {
    vec![
        "\x1b[32mready\x1b[0m in 85ms".to_string(),
        "listening on https://a-rather-long-hostname.example.com/a/b/c".to_string(),
        "WARN slow query took 450 ms".to_string(),
        "{".to_string(),
        "  \"level\": \"error\",".to_string(),
        "  \"msg\": \"boom\",".to_string(),
        "  \"detail\": \"…\"".to_string(),
        "}".to_string(),
        "日本語のログ行 with 🎉 emoji and e\u{0301} combining".to_string(),
        // A date-ish token eleven bytes after a multi-byte character:
        // the timestamp match starts inside the character unless the
        // parser checks.
        "日 21-09-26T10:00:00 起動".to_string(),
        "x".repeat(4000),
        "ERROR the last one".to_string(),
    ]
}

#[test]
fn renders_the_viewer_at_any_terminal_size_without_panicking() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &a_log_of_everything());
    write_log(&app, "feat+one", "install", &["installed"]);
    app.open_log_viewer();

    // One pass per state the viewer can be painted in. The keys are
    // pressed between passes, so each sweep starts from the one before.
    let states: [&[KeyEvent]; 8] = [
        // Following the live tail, which is how it opens.
        &[],
        // A cursor part way up, with wrap off.
        &[
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE),
        ],
        // A query being typed.
        &[
            KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::NONE),
        ],
        // And confirmed.
        &[KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)],
        // Collapsed to the matches.
        &[KeyEvent::new(KeyCode::Char('&'), KeyModifiers::NONE)],
        // Errors only, with a count prefix half typed.
        &[
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE),
        ],
        // The inspect overlay over all of it.
        &[KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE)],
        // And the help overlay over that.
        &[
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        ],
    ];
    for keys in states {
        for key in keys {
            app.handle_key(*key);
        }
        for width in 1..=120u16 {
            for height in [1u16, 2, 3, 5, 12, 40] {
                draw(&mut app, width, height);
            }
        }
        // Past the width where a percentage of it stops fitting in a
        // u16 (1093 * 60 overflows), and as tall again.
        for width in [1092u16, 1093, 1500, 2000, 3000] {
            for height in [1u16, 3, 40] {
                draw(&mut app, width, height);
            }
        }
        for height in [1092u16, 1093, 3000] {
            draw(&mut app, 80, height);
        }
    }
}

#[test]
fn renders_a_viewer_with_no_log_and_one_with_nothing_in_it_at_any_size() {
    for lines in [Vec::<String>::new(), vec![String::new()]] {
        let (_dir, mut app) = app_with_logs(&["feat+one"]);
        if !lines.is_empty() {
            write_log(&app, "feat+one", "dev", &lines);
        }
        app.open_log_viewer();
        for width in [1u16, 2, 5, 40, 120, 1093, 3000] {
            for height in [1u16, 2, 3, 40] {
                draw(&mut app, width, height);
            }
        }
    }
}

// Wide and zero-width glyphs are counted as one cell by every budget in
// here; what must never happen is a panic or a lost line.
#[test]
fn a_line_of_wide_glyphs_paints_at_every_width() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "日本語".repeat(40),
            "🎉🎉🎉".repeat(20),
            "e\u{0301}".repeat(60),
        ],
    );
    app.open_log_viewer();
    for width in 1..=60u16 {
        draw(&mut app, width, 10);
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
    }
}

#[test]
fn an_escape_the_terminal_cannot_read_is_painted_as_text_not_as_an_escape() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[
            "\x1b[38;5;mbroken \x1b[999Xthing",
            "a bare \x1b in the middle",
            "trailing escape \x1b[",
            "\x1b[mzero length",
        ],
    );
    app.open_log_viewer();
    let buffer = draw(&mut app, 60, 12);
    let painted: String = text_of(&buffer);
    assert!(
        !painted.contains('\x1b'),
        "an escape must never reach the screen as text: {painted:?}"
    );
    assert!(painted.contains("thing"), "{painted}");
    assert!(painted.contains("zero length"), "{painted}");
}

#[test]
fn a_real_ansi_colour_reaches_the_screen_as_colour() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["\x1b[32mready\x1b[0m now"]);
    app.open_log_viewer();
    let buffer = draw(&mut app, 60, 8);
    let painted = text_of(&buffer);
    assert!(painted.contains("ready now"), "{painted}");
    assert!(!painted.contains("[32m"), "never as text: {painted}");
    let colours: Vec<_> = (0..60)
        .map(|x| buffer.cell((x, 1)).unwrap().style().fg)
        .collect();
    assert!(
        colours.contains(&Some(ratatui::style::Color::Green)),
        "the escape painted the colour it asked for: {colours:?}"
    );
}

#[test]
fn renders_an_empty_list_and_a_filter_with_no_matches() {
    // Wide enough that the list pane, now sharing the body with the
    // detail pane, still fits the sentence.
    let mut app = test_app(&[]);
    let first_run = text_of(&draw(&mut app, 120, 24));
    assert!(first_run.contains("welcome"), "{first_run}");
    assert!(first_run.contains("create a worktree"), "{first_run}");

    let mut app = test_app(&["feat+one"]);
    for code in [KeyCode::Char('/'), KeyCode::Char('z'), KeyCode::Char('z')] {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }
    let rendered = text_of(&draw(&mut app, 120, 10));
    assert!(rendered.contains("no matches"), "{rendered}");
}

#[test]
fn the_header_shows_the_project_branch_and_count() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    app.main = Some(wt("acme-shop"));
    let rendered = text_of(&draw(&mut app, 80, 10));
    assert!(rendered.contains("acme-shop"), "{rendered}");
    assert!(rendered.contains("2 worktrees"), "{rendered}");
}

#[test]
fn the_header_gives_the_whole_bar_to_a_status_message() {
    let mut app = test_app(&["feat+one"]);
    app.main = Some(wt("acme-shop"));
    app.set_status("created feat+one");
    let first_line = text_of(&draw(&mut app, 80, 10))
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(first_line.contains("created feat+one"), "{first_line}");
    assert!(!first_line.contains("worktrees"), "{first_line}");
}

fn every_column(width: usize) -> Vec<(Col, usize)> {
    vec![
        (Col::Aside, 10),
        (Col::Port, 6),
        (Col::Ports, 9),
        (Col::Share, 1),
        (Col::Signals, width),
        (Col::Pr, 4),
    ]
}

#[test]
fn list_columns_shed_the_least_useful_first_and_git_last() {
    let all = every_column(4);
    assert_eq!(
        list_columns(200, &all),
        vec![
            Col::Pr,
            Col::Aside,
            Col::Port,
            Col::Ports,
            Col::Share,
            Col::Signals,
        ],
        "wide enough for everything, the pull request first, git last"
    );
    let medium = list_columns(66, &all);
    assert!(!medium.contains(&Col::Ports), "{medium:?}");
    assert!(
        medium.contains(&Col::Aside) && medium.contains(&Col::Port),
        "{medium:?}"
    );

    let narrow = list_columns(35, &all);
    assert!(!narrow.contains(&Col::Port), "{narrow:?}");
    assert!(narrow.contains(&Col::Pr), "{narrow:?}");
    // Narrower still, the uncommitted mark outlasts the pull request.
    let narrower = list_columns(24, &all);
    assert_eq!(narrower, vec![Col::Signals], "{narrower:?}");

    assert_eq!(
        list_columns(16, &all),
        Vec::<Col>::new(),
        "a sliver keeps only the glyph and the label"
    );
}

#[test]
fn list_columns_keep_everything_when_the_optional_columns_are_empty() {
    let empty: Vec<(Col, usize)> = every_column(0).into_iter().map(|(c, _)| (c, 0)).collect();
    assert_eq!(
        list_columns(20, &empty),
        Vec::<Col>::new(),
        "columns with no content cost nothing"
    );
}

#[test]
fn a_narrow_list_still_shows_the_name() {
    let mut app = test_app(&["feat+one"]);
    let rendered = text_of(&draw(&mut app, 30, 8));
    assert!(rendered.contains("feat/one"), "{rendered}");
}

/// The list pane's part of the row that mentions `needle`.
fn list_row(rendered: &str, needle: &str) -> String {
    rendered
        .lines()
        .filter(|line| line.starts_with('│'))
        .map(|line| line.split("││").next().unwrap_or_default())
        .find(|row| row.contains(needle))
        .unwrap_or_default()
        .to_string()
}

// The branch is the name people use. The directory it lives in is shown
// only when it is not simply the branch with its slashes encoded.
#[test]
fn a_row_is_labelled_by_its_branch() {
    let mut app = test_app(&["feat+one", "scratch"]);
    app.worktrees[0].branch = Some("feat/one".into());
    app.worktrees[1].branch = Some("fix/typo".into());
    let rendered = text_of(&draw(&mut app, 120, 10));
    let one = list_row(&rendered, "feat/one");
    assert!(
        !one.contains("feat+one"),
        "the encoded name adds nothing:\n{rendered}"
    );
    assert!(
        list_row(&rendered, "fix/typo").contains("scratch"),
        "a directory named otherwise says so:\n{rendered}"
    );
}

#[test]
fn a_row_says_what_it_is_doing_and_where() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    crate::tui::app::tests::with_process(
        &mut app,
        "feat+one",
        crate::tui::app::tests::running_phase(),
    );
    let rendered = text_of(&draw(&mut app, 140, 10));
    let row = |name: &str| list_row(&rendered, name);
    let one = row("feat/one");
    assert!(one.contains("● feat/one"), "{rendered}");
    assert!(one.contains(":17342"), "the port, not the whole URL: {one}");
    assert!(!one.contains("http"), "{one}");
    assert!(!one.contains("running"), "the glyph says it: {one}");
    let two = row("feat/two");
    assert!(two.contains("○ feat/two"), "{rendered}");
    assert!(!two.contains("stopped"), "the glyph says it: {two}");
    assert!(
        !two.contains(":17342"),
        "a stopped row promises no page:\n{rendered}"
    );
}

// The URL is one process's port: with that one stopped on its own and a
// sibling still up, the row and the pane showed it as a live link.
#[test]
fn a_url_whose_own_process_is_stopped_is_not_drawn_while_a_sibling_runs() {
    let mut app = test_app(&["feat+one"]);
    crate::tui::app::tests::with_process(
        &mut app,
        "feat+one",
        crate::tui::app::tests::running_phase(),
    );
    crate::tui::app::tests::with_second_process(
        &mut app,
        "feat+one",
        "api",
        crate::tui::app::tests::running_phase(),
    );
    app.state
        .worktrees
        .get_mut("feat+one")
        .unwrap()
        .processes
        .remove("dev");
    let rendered = text_of(&draw(&mut app, 140, 20));
    assert!(
        !list_row(&rendered, "feat/one").contains(":17342"),
        "{rendered}"
    );
    assert!(!rendered.contains("http://localhost:17342"), "{rendered}");
}

#[test]
fn the_row_leaves_adoption_to_the_detail_pane() {
    let mut app = test_app(&["mine", "theirs"]);
    app.created_by_pando.insert("mine".into(), true);
    app.created_by_pando.insert("theirs".into(), false);
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 120, 20));
    assert!(
        !list_row(&rendered, "theirs").contains("adopted"),
        "{rendered}"
    );
    assert!(
        rendered.contains("adopted"),
        "the detail pane says it: {rendered}"
    );
}

#[test]
fn a_header_row_names_each_column_over_its_cells() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    with_process(&mut app, "feat+one", running_phase());
    app.state.worktrees.get_mut("feat+one").unwrap().mode =
        Some(crate::state::ServiceMode::Isolated);
    app.worktrees[0].dirty = Some(true);
    let rendered = text_of(&draw(&mut app, 180, 10));
    let header = list_row(&rendered, "branch");
    let row = list_row(&rendered, "feat/one");
    for (title, cell) in [("branch", "feat/one"), ("port", ":17342"), ("git", "↑1 ✎")] {
        let at = |line: &str, needle: &str| {
            line.find(needle)
                .map(|i| line[..i].chars().count())
                .unwrap_or_else(|| panic!("no {needle}:\n{rendered}"))
        };
        assert_eq!(
            at(&header, title),
            at(&row, cell),
            "{title} is not over {cell}:\n{rendered}"
        );
    }
    // Nobody has a public URL or a pull request: no column, no title.
    assert!(
        !header.contains("public") && !header.contains("PR"),
        "{header}"
    );
}

#[test]
fn no_header_row_when_the_list_is_empty() {
    let mut app = test_app(&["feat+one"]);
    app.filter = "nothing matches this".into();
    app.filtered_indices.clear();
    let rendered = text_of(&draw(&mut app, 140, 10));
    assert!(!rendered.contains("branch"), "{rendered}");
}

#[test]
fn the_port_is_read_from_the_host_whatever_follows_it() {
    assert_eq!(port_of("http://localhost:17342").as_deref(), Some("17342"));
    assert_eq!(port_of("http://localhost:17342/").as_deref(), Some("17342"));
    assert_eq!(
        port_of("http://127.0.0.1:3000/app?x=1:2").as_deref(),
        Some("3000")
    );
    assert_eq!(port_of("https://example.com/a:b"), None);
}

// Thirty branches that share a long prefix must still read as thirty
// different rows.
#[test]
fn long_labels_keep_the_part_that_tells_them_apart() {
    let names: Vec<String> = (1..=12)
        .map(|n| format!("feature+very-long-branch-name-number-{n}-with-extra-words"))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut app = test_app(&refs);
    for wt in &mut app.worktrees {
        wt.branch = Some(wt.name.replace('+', "/"));
    }
    let rendered = text_of(&draw(&mut app, 80, 30));
    for n in [1, 7, 12] {
        // The number whole, whether the dash after it fits or not.
        assert!(
            rendered.contains(&format!("number-{n}-"))
                || rendered.contains(&format!("number-{n}…")),
            "row {n} is told apart:\n{rendered}"
        );
    }
}

#[test]
fn truncate_distinct_cuts_where_it_costs_nothing() {
    let label = "feature/very-long-branch-name-number-12-with-extra-words";
    let at = label.find("12").unwrap();
    let cut = truncate_distinct(label, 24, at);
    assert!(cut.contains("12"), "{cut}");
    assert!(cut.starts_with("feature/…"), "{cut}");
    assert_eq!(cut.chars().count(), 24, "{cut}");
    assert_eq!(
        truncate_distinct("feat/short-and-different", 12, 2),
        "feat/short-…",
        "a difference near the front is kept by a plain cut"
    );
}

#[test]
fn distinct_offsets_find_where_each_label_leaves_its_nearest_neighbour() {
    let labels: Vec<String> = ["feat/a1", "feat/a2", "fix/b"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(distinct_offsets(&labels), vec![6, 6, 1]);
}

#[test]
fn truncate_middle_keeps_both_ends() {
    assert_eq!(truncate_middle("abcdefghij", 10), "abcdefghij");
    let cut = truncate_middle("/very/long/path/to/worktrees/feat+one", 20);
    assert_eq!(cut.chars().count(), 20, "{cut}");
    assert!(cut.starts_with("/very"), "{cut}");
    assert!(cut.ends_with("feat+one"), "{cut}");
}

#[test]
fn wrap_text_breaks_at_spaces_and_cuts_only_what_it_must() {
    assert_eq!(
        wrap_text("isolated mode needs Docker", 12),
        vec!["isolated", "mode needs", "Docker"]
    );
    assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    assert_eq!(wrap_text("", 4), vec![""]);
}

#[test]
fn a_shared_worktree_is_marked_in_the_list_and_only_then() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    let plain = text_of(&draw(&mut app, 100, 10));
    assert!(
        !plain.contains('◈'),
        "no row is shared, so no row pays for the column:\n{plain}"
    );

    crate::tui::app::tests::with_process(
        &mut app,
        "feat+one",
        crate::tui::app::tests::running_phase(),
    );
    crate::tui::app::tests::with_share(&mut app, "feat+one", None);
    let shared = text_of(&draw(&mut app, 120, 10));
    assert!(list_row(&shared, "feat/one").contains('◈'), "{shared}");
    assert!(!list_row(&shared, "feat/two").contains('◈'), "{shared}");
}

#[test]
fn the_detail_pane_shows_the_public_url_under_the_local_one() {
    let mut app = test_app(&["feat+one"]);
    crate::tui::app::tests::with_process(
        &mut app,
        "feat+one",
        crate::tui::app::tests::running_phase(),
    );
    crate::tui::app::tests::with_share(&mut app, "feat+one", Some(17_009));

    let rendered = text_of(&draw(&mut app, 120, 24));
    assert!(rendered.contains("public"), "{rendered}");
    assert!(
        rendered.contains("https://fake-host.trycloudflare.com"),
        "{rendered}"
    );
    let local = rendered.find("http://localhost").expect("a local url");
    let public = rendered.find("https://fake-host").expect("a public url");
    assert!(local < public, "the public URL goes under the local one");
}

#[test]
fn signals_report_gone_locked_dirty_and_drift() {
    let mut gone = wt("g");
    gone.prunable = true;
    assert_eq!(signal_text(&gone), "prunable");

    let mut locked = wt("l");
    locked.locked = true;
    assert_eq!(signal_text(&locked), "locked");

    let mut dirty = wt("d");
    dirty.dirty = Some(true);
    dirty.ahead_behind = Some((2, 3));
    assert_eq!(signal_text(&dirty), "*↑2↓3");

    let mut clean = wt("c");
    clean.dirty = Some(false);
    clean.ahead_behind = Some((0, 0));
    assert_eq!(signal_text(&clean), "");

    let mut lots = wt("m");
    lots.dirty = Some(false);
    lots.ahead_behind = Some((250, 0));
    assert_eq!(signal_text(&lots), "↑99+");
}

#[test]
fn keep_hints_drops_optional_hints_from_the_tail_first() {
    let items = [(5, true), (4, false), (6, false), (4, true)];
    assert_eq!(keep_hints(&items, 3, 100), vec![true, true, true, true]);
    assert_eq!(
        keep_hints(&items, 3, 22),
        vec![true, true, false, true],
        "the last optional hint goes first"
    );
    assert_eq!(keep_hints(&items, 3, 12), vec![true, false, false, true]);
    assert_eq!(
        keep_hints(&items, 3, 1),
        vec![true, false, false, true],
        "essentials survive even when they cannot fit"
    );
}

#[test]
fn truncate_adds_an_ellipsis_only_when_it_cuts() {
    assert_eq!(truncate("short", 10), "short");
    assert_eq!(truncate("abcdefghij", 5), "abcd…");
    assert_eq!(truncate("abc", 0), "");
    assert_eq!(truncate("", 5), "");
}

#[test]
fn truncate_line_keeps_the_surviving_spans_styled() {
    let line = Line::from(vec![
        Span::styled("abc", Style::new().fg(green())),
        Span::styled("defgh", Style::new().fg(red())),
    ]);
    let cut = truncate_line(line, 5);
    assert_eq!(cut.spans.len(), 2);
    assert_eq!(cut.spans[0].content, "abc");
    assert_eq!(cut.spans[1].content, "d…");
    assert_eq!(cut.spans[1].style.fg, Some(red()));
}

#[test]
fn centered_rect_honours_the_minimum_width_and_never_escapes_the_area() {
    let area = Rect::new(0, 0, 100, 40);
    let popup = centered_rect(50, 40, 10, area);
    assert_eq!(popup.width, 50);

    let narrow = Rect::new(0, 0, 20, 40);
    let popup = centered_rect(50, 40, 10, narrow);
    assert_eq!(popup.width, 20, "a popup never grows past its area");

    let short = Rect::new(0, 0, 100, 4);
    let popup = centered_rect(50, 40, 10, short);
    assert!(popup.height <= 4);
}
// ---- the two panes ---------------------------------------------------

#[test]
fn a_wide_body_puts_the_panes_side_by_side() {
    let body = Rect::new(0, 0, 120, 30);
    let [list, detail] = body_layout(body);
    assert_eq!(list.y, detail.y, "same row means side by side");
    assert!(list.width > 20 && detail.width > 20);
    assert_eq!(list.width + detail.width, 120);
}

// A tmux split is the normal case, so the narrow layout is not an edge.
#[test]
fn a_narrow_body_stacks_the_panes() {
    let body = Rect::new(0, 0, 50, 30);
    let [list, detail] = body_layout(body);
    assert_eq!(list.x, detail.x);
    assert_eq!(list.width, detail.width, "stacked panes are full width");
    assert!(detail.y >= list.y + list.height);
    assert!(list.height >= STACK_MIN_LIST_HEIGHT);
    assert!(detail.height >= STACK_MIN_DETAIL_HEIGHT);
}

// Too short to stack: two panes of three rows each help nobody, so a
// cramped side-by-side wins.
#[test]
fn a_narrow_and_short_body_stays_side_by_side() {
    let [list, detail] = body_layout(Rect::new(0, 0, 50, 8));
    assert_eq!(list.y, detail.y);
}

#[test]
fn the_detail_pane_sheds_its_least_useful_rows_first() {
    let rows: Vec<(u8, Line)> = vec![
        (KEEP_ALWAYS, Line::raw("branch")),
        (KEEP_ALWAYS, Line::raw("status")),
        (KEEP_URL, Line::raw("url")),
        (KEEP_PR, Line::raw("pr")),
        (KEEP_HEAD, Line::raw("head")),
        (KEEP_PATH, Line::raw("path")),
    ];
    let kept = |budget: usize| {
        fit_detail_rows(rows.clone(), budget)
            .iter()
            .map(|l| l.spans[0].content.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(kept(6).len(), 6);
    assert_eq!(kept(5), vec!["branch", "status", "url", "pr", "head"]);
    assert_eq!(kept(3), vec!["branch", "status", "url"]);
    assert_eq!(
        kept(1),
        vec!["branch", "status"],
        "the rows the pane exists for are never shed, even when they clip"
    );
}

// ---- what the detail pane says ---------------------------------------

#[test]
fn the_detail_pane_shows_the_url_ports_and_uptime_of_a_running_worktree() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let rendered = text_of(&draw(&mut app, 120, 20));
    assert!(rendered.contains("running"), "{rendered}");
    assert!(rendered.contains("http://localhost:17342"), "{rendered}");
    assert!(rendered.contains("web 17342"), "{rendered}");
    assert!(rendered.contains("pid 4242"), "{rendered}");
}

// Expo's "press i" is gone with no terminal, so the pane gives the
// command that opens a running Metro's app on the simulator, whole; and
// none once Metro is down, or for a process whose app a browser opens.
#[test]
fn the_detail_pane_gives_the_simulator_command_for_a_running_expo_app() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "mobile", running_phase());
    app.config = toml::from_str(
        "[processes.dev]\ncmd = \"npm run dev\"\nports = { PORT = \"web\" }\n\n\
         [processes.mobile]\ncmd = \"npx expo start\"\nports = [\"mobile\"]\n",
    )
    .unwrap();
    // The pane is too narrow for the whole command on one row: it carries
    // on under itself at a space, so the URL is never cut in two.
    let rendered = text_of(&draw(&mut app, 100, 30));
    assert!(
        rendered.contains(" app    xcrun simctl openurl booted"),
        "{rendered}"
    );
    assert!(
        rendered.contains("        'exp://127.0.0.1:17344'"),
        "{rendered}"
    );
    assert_eq!(rendered.matches("simctl").count(), 1, "{rendered}");
    // And under it, an Android device's or emulator's.
    assert!(
        rendered.contains(" app    adb reverse tcp:17344"),
        "{rendered}"
    );
    assert!(
        rendered.contains("android.intent.action.VIEW"),
        "{rendered}"
    );

    let record = app.state.worktrees.get_mut("feat+one").unwrap();
    record.processes.get_mut("mobile").unwrap().phase = crate::state::Phase::Failed {
        at: chrono::Utc::now(),
        reason: "process exited".into(),
    };
    let rendered = text_of(&draw(&mut app, 200, 30));
    assert!(!rendered.contains("simctl"), "{rendered}");
    assert!(!rendered.contains("adb"), "{rendered}");
}

// The api of a root script that runs web and api died behind the web
// server: the only line on the pane that says so is never cut at its
// edge, however many ports come before it.
#[test]
fn a_port_nothing_listens_on_is_said_whole_under_the_ports() {
    let mut app = test_app(&["feat+one"]);
    with_process(
        &mut app,
        "feat+one",
        crate::state::Phase::Running {
            since: chrono::Utc::now() - chrono::Duration::minutes(2),
        },
    );
    let record = app.state.worktrees.get_mut("feat+one").unwrap();
    record.ports.insert("api".into(), 17_343);
    record
        .roles
        .insert("dev".into(), vec!["api".into(), "web".into()]);
    record.observed_ports = vec![17_342, 45_678, 45_679];
    // Side by side, the detail pane's inside is 46 cells at 120 and 38 at
    // 100; the warning fits on a row of its own at the first, and wraps
    // at the second.
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(rendered.contains("api 17343  web 17342"), "{rendered}");
    assert!(
        rendered.contains("nothing on api 17343 — l shows why"),
        "{rendered}"
    );
    let rendered = text_of(&draw(&mut app, 100, 30));
    assert!(rendered.contains("nothing on api 17343"), "{rendered}");
    // The ports pando did not ask for are what gives way, and say so.
    let ports = rendered.lines().find(|l| l.contains("││ ports ")).unwrap();
    assert!(
        ports.trim_end_matches('│').trim_end().ends_with('…'),
        "{ports}"
    );
    assert!(
        rendered
            .lines()
            .any(|l| l.trim_end_matches('│').trim_end().ends_with("why")),
        "{rendered}"
    );
}

#[test]
fn a_stopped_worktree_is_told_how_to_start() {
    let mut app = test_app(&["feat+one"]);
    let rendered = text_of(&draw(&mut app, 120, 20));
    assert!(rendered.contains("○ stopped"), "{rendered}");
    assert!(rendered.contains("⏎ picks a mode to start"), "{rendered}");
}

#[test]
fn a_failure_shows_its_reason_in_the_detail_pane() {
    let mut app = test_app(&["feat+one"]);
    with_process(
        &mut app,
        "feat+one",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited; dependencies are missing".into(),
        },
    );
    let rendered = text_of(&draw(&mut app, 180, 20));
    assert!(rendered.contains("failed"), "{rendered}");
    assert!(rendered.contains("dependencies are missing"), "{rendered}");
}

#[test]
fn the_list_marks_what_each_worktree_is_doing() {
    let mut app = test_app(&["up", "broken"]);
    with_process(&mut app, "up", running_phase());
    with_process(
        &mut app,
        "broken",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited".into(),
        },
    );
    let rendered = text_of(&draw(&mut app, 120, 20));
    let up = list_row(&rendered, "up ");
    assert!(up.contains("● up") && !up.contains("running"), "{rendered}");
    let broken = list_row(&rendered, "broken");
    assert!(
        broken.contains("✗ broken") && broken.contains("failed"),
        "{rendered}"
    );
}

#[test]
fn the_tail_paints_the_last_lines_of_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("dev.log");
    std::fs::write(&log, "ready in 412ms\nError: it broke\nwarn: slow\n").unwrap();

    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    // Keyed by worktree *and* process: a worktree has one log per
    // process, and the tail shows one of them at a time.
    let (key, process, _) = app.tail_target().expect("a process to tail");
    assert_eq!(process, "dev");
    app.log_tails.touch(&key, log).poll().unwrap();

    let rendered = text_of(&draw(&mut app, 120, 24));
    assert!(rendered.contains("ready in 412ms"), "{rendered}");
    assert!(rendered.contains("Error: it broke"), "{rendered}");
    assert!(rendered.contains("3 lines"), "{rendered}");
}

#[test]
fn the_detail_pane_lists_every_process_with_its_phase() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(
        &mut app,
        "feat+one",
        "api",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited".into(),
        },
    );
    let rendered = text_of(&draw(&mut app, 120, 24));
    assert!(
        rendered.contains("✗ failed") && rendered.contains("api: process exited"),
        "the worktree is failed, and says which process: {rendered}"
    );
    assert!(
        rendered.contains("api") && rendered.contains("dev"),
        "both processes have a row: {rendered}"
    );
    assert!(
        rendered.contains("pid 4242"),
        "each row carries its own pid: {rendered}"
    );
}

// A reason cut to fit says it was cut: the ellipsis is the last cell
// inside the pane, not the first one past its border.
#[test]
fn a_process_row_cut_to_the_pane_ends_in_an_ellipsis() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(
        &mut app,
        "feat+one",
        "api",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "something else is listening on port 17343; stop it, or stop the \
                     worktree that owns it"
                .into(),
        },
    );
    let rendered = text_of(&draw(&mut app, 120, 30));
    let row = rendered
        .lines()
        .find(|l| l.contains("api") && l.contains("failed    "))
        .unwrap_or_else(|| panic!("no process row for api:\n{rendered}"));
    let inside = row.trim_end_matches('│').trim_end();
    assert!(inside.ends_with('…'), "{row}");
}

// One process has nothing to disambiguate, and a tmux split has no rows
// to spare for saying the same thing twice.
#[test]
fn one_process_gets_no_row_of_its_own() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let rendered = text_of(&draw(&mut app, 120, 24));
    assert_eq!(
        rendered.matches("pid 4242").count(),
        1,
        "the status row is the process's status: {rendered}"
    );
}

#[test]
fn the_list_row_shows_the_aggregate_not_the_first_process() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(
        &mut app,
        "feat+one",
        "api",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited".into(),
        },
    );
    let rendered = text_of(&draw(&mut app, 120, 24));
    let row = list_row(&rendered, "feat/one");
    assert!(
        row.contains("✗ feat/one") && row.contains("failed"),
        "a worktree with a dead process is not running: {rendered}"
    );
}

#[test]
fn the_tail_header_names_every_process_and_marks_the_one_it_shows() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    let rendered = text_of(&draw(&mut app, 140, 24));
    assert!(rendered.contains("▸api │ dev"), "{rendered}");
    assert!(
        rendered.contains("tab next"),
        "and says how to see the other one: {rendered}"
    );
    assert!(
        rendered.contains("P restarts it"),
        "and how to restart just this one: {rendered}"
    );
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 140, 24));
    assert!(
        rendered.contains("api │ ▸dev"),
        "tab moves the mark: {rendered}"
    );
}

/// An app whose selected worktree's tail follows a real log of `count`
/// lines, `line 0` onward, polled once.
fn app_tailing_numbered_lines(dir: &std::path::Path, count: usize) -> (App, std::path::PathBuf) {
    let log = dir.join("dev.log");
    let body: String = (0..count).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&log, body).unwrap();
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let record = app.state.worktrees.get_mut("feat+one").unwrap();
    record.processes.get_mut("dev").unwrap().log_path = log.clone();
    app.handle_event(crate::tui::app::AppEvent::Tick);
    (app, log)
}

/// The numbered lines a paint shows, top to bottom.
fn numbered_lines_shown(painted: &str) -> Vec<String> {
    painted
        .lines()
        .filter_map(|row| {
            let at = row.find("line ")?;
            let digits: String = row[at + 5..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            (!digits.is_empty()).then(|| format!("line {digits}"))
        })
        .collect()
}

// The tail's scroll counts back from the newest line, and nothing moved
// it as lines arrived, so what the reader had paged back to slid out of
// view at the log's rate.
#[test]
fn a_scrolled_back_tail_stays_on_its_lines_as_new_ones_arrive() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, log) = app_tailing_numbered_lines(dir.path(), 40);
    draw(&mut app, 120, 30);
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    let before = numbered_lines_shown(&text_of(&draw(&mut app, 120, 30)));
    assert!(!before.is_empty() && !before.contains(&"line 39".to_string()));

    let mut file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
    std::io::Write::write_all(&mut file, b"line 40\nline 41\nline 42\n").unwrap();
    app.handle_event(crate::tui::app::AppEvent::Tick);
    let after = numbered_lines_shown(&text_of(&draw(&mut app, 120, 30)));
    assert_eq!(after, before);
}

// A theme change reads every log again into new tails, and the first
// read of one counted every line in its window as arrived, so a tail
// scrolled back jumped to its oldest page when the system turned dark.
#[test]
fn a_scrolled_back_tail_stays_on_its_lines_through_a_theme_change() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _log) = app_tailing_numbered_lines(dir.path(), 100);
    draw(&mut app, 120, 30);
    app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    let before = numbered_lines_shown(&text_of(&draw(&mut app, 120, 30)));
    assert!(!before.is_empty() && !before.contains(&"line 99".to_string()));

    let resolved = crate::theme::Resolved {
        name: crate::theme::DEFAULT_THEME.to_string(),
        origin: crate::theme::Origin::Default,
        appearance: crate::theme::Appearance::Light,
        appearance_origin: crate::theme::AppearanceOrigin::System,
        palette: crate::theme::palette(),
        warnings: Vec::new(),
    };
    app.handle_event(crate::tui::app::AppEvent::Theme(Box::new(resolved)));
    app.handle_event(crate::tui::app::AppEvent::Tick);
    let after = numbered_lines_shown(&text_of(&draw(&mut app, 120, 30)));
    assert_eq!(after, before);
}

// The scroll stopped one line short of the whole buffer, so paging back
// far enough left one line at the top of the tail and the rows under it
// blank.
#[test]
fn paging_back_through_the_tail_stops_at_a_full_first_page() {
    let dir = tempfile::tempdir().unwrap();
    let (mut app, _log) = app_tailing_numbered_lines(dir.path(), 40);
    draw(&mut app, 120, 30);
    for _ in 0..10 {
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
    }
    let shown = numbered_lines_shown(&text_of(&draw(&mut app, 120, 30)));
    assert_eq!(shown.len(), app.tail_rows, "{shown:?}");
    assert_eq!(shown[0], "line 0");
}

// Level colours are what make an error findable in a wall of output.
#[test]
fn log_levels_are_painted_in_their_own_colours() {
    assert_eq!(level_color(LogLevel::Error), red());
    assert_eq!(level_color(LogLevel::Warn), yellow());
    assert_ne!(level_color(LogLevel::Info), red());
}

#[test]
fn a_worktree_with_no_log_yet_says_so_rather_than_painting_nothing() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let rendered = text_of(&draw(&mut app, 120, 24));
    assert!(rendered.contains("no log yet"), "{rendered}");
}

#[test]
fn a_filter_that_hides_everything_leaves_the_detail_pane_saying_so() {
    let mut app = test_app(&["feat+one"]);
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 120, 20));
    assert!(
        rendered.contains("no worktree matches the filter"),
        "{rendered}"
    );
}

// The footer offers what the selected row can do: a stopped one is
// started, a running one is read, stopped or restarted.
#[test]
fn the_footer_offers_the_keys_the_selected_row_needs() {
    let mut app = test_app(&["feat+one"]);
    let stopped = text_of(&draw(&mut app, 120, 20));
    assert!(stopped.contains("⏎ start"), "{stopped}");
    assert!(stopped.contains("i isolated"), "{stopped}");
    assert!(!stopped.contains("x stop"), "{stopped}");

    with_process(&mut app, "feat+one", running_phase());
    let running = text_of(&draw(&mut app, 120, 20));
    assert!(running.contains("l logs"), "{running}");
    assert!(running.contains("x stop"), "{running}");
    assert!(running.contains("r restart"), "{running}");
}

// Finding 9. Sharing is this phase's whole feature and neither of its
// keys was in the footer at any width, while `o open` was.
#[test]
fn the_footer_offers_the_share_keys_on_a_wide_terminal() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let rendered = text_of(&draw(&mut app, 160, 20));
    assert!(rendered.contains("t share"), "{rendered}");
    crate::tui::app::tests::with_share(&mut app, "feat+one", None);
    let shared = text_of(&draw(&mut app, 160, 20));
    assert!(shared.contains("O public"), "{shared}");
    assert!(shared.contains("C copy public"), "{shared}");
}

// …and they are optional, so a narrow terminal sheds them rather than
// anything essential, and nothing clips.
#[test]
fn the_share_keys_go_before_anything_essential_when_the_footer_will_not_fit() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let rendered = text_of(&draw(&mut app, 60, 20));
    for essential in ["j/k move", "l logs", "x stop", "? help", "q quit"] {
        assert!(rendered.contains(essential), "{essential}: {rendered}");
    }
    for line in rendered.lines() {
        assert!(line.chars().count() <= 60, "{line:?}");
    }
}

// ---- the log viewer --------------------------------------------------

/// Opens the viewer on the first worktree and paints one frame.
fn viewer_frame(app: &mut App, width: u16, height: u16) -> String {
    app.open_log_viewer();
    text_of(&draw(app, width, height))
}

#[test]
fn the_viewer_names_the_worktree_and_the_source_it_is_showing() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["listening on 17342"]);
    let painted = viewer_frame(&mut app, 80, 14);
    assert!(painted.contains("feat/one · dev"), "{painted}");
    assert!(painted.contains("listening on 17342"), "{painted}");
}

#[test]
fn the_viewer_paints_a_tab_per_source_and_marks_the_active_one() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["up"]);
    write_log(&app, "feat+one", "install", &["installed"]);
    let painted = viewer_frame(&mut app, 80, 14);
    assert!(painted.contains("1:dev"), "{painted}");
    assert!(painted.contains("2:install"), "{painted}");
}

#[test]
fn a_single_source_gets_no_tab_bar_at_all() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["up"]);
    let painted = viewer_frame(&mut app, 80, 14);
    assert!(
        !painted.contains("1:dev"),
        "one source needs no tabs:\n{painted}"
    );
}

#[test]
fn narrow_tabs_keep_the_active_label_and_shrink_the_rest_to_digits() {
    let sources = vec![
        "dev".to_string(),
        "api".to_string(),
        "install".to_string(),
        "migrate".to_string(),
    ];
    let wide = source_tabs(&sources, "api", 80);
    let wide_text: String = wide.spans.iter().map(|s| s.content.to_string()).collect();
    assert!(wide_text.contains("1:dev"), "{wide_text}");
    assert!(wide_text.contains("4:migrate"), "{wide_text}");

    let narrow = source_tabs(&sources, "api", 20);
    let narrow_text: String = narrow.spans.iter().map(|s| s.content.to_string()).collect();
    assert!(
        narrow_text.contains("2:api"),
        "the active one keeps its label"
    );
    assert!(!narrow_text.contains("1:dev"), "{narrow_text}");
    assert!(narrow_text.chars().count() <= 20, "{narrow_text}");
}

#[test]
fn the_footer_says_where_the_cursor_is() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
    write_log(&app, "feat+one", "dev", &lines);
    let painted = viewer_frame(&mut app, 80, 14);
    assert!(
        painted.contains("FOLLOW"),
        "it opens on the live tail:\n{painted}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    let painted = text_of(&draw(&mut app, 80, 14));
    assert!(
        painted.contains("1/30"),
        "one-based, out of what is shown:\n{painted}"
    );
}

#[test]
fn a_missing_log_says_so_rather_than_painting_an_empty_pane() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let painted = viewer_frame(&mut app, 80, 14);
    assert!(painted.contains("no log file"), "{painted}");
}

#[test]
fn the_cursor_line_is_painted_across_the_whole_width() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["short", "also short"]);
    app.open_log_viewer();
    draw(&mut app, 40, 10);
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    let buffer = draw(&mut app, 40, 10);
    // The first body row is the cursor line; every cell of it, padding
    // included, carries the highlight.
    let row = 1;
    let painted: Vec<_> = (1..39)
        .map(|x| buffer.cell((x, row)).unwrap().style().bg)
        .collect();
    assert!(
        painted.iter().all(|bg| *bg == Some(highlight_bg())),
        "the cursor bar spans the viewport: {painted:?}"
    );
}

#[test]
fn a_line_longer_than_the_viewer_wraps_and_truncates_with_w() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["a line".to_string(), "ab".repeat(60)],
    );
    app.open_log_viewer();
    let wrapped = text_of(&draw(&mut app, 40, 14));
    assert!(
        wrapped.lines().filter(|row| row.contains("abab")).count() > 3,
        "the long line wraps over several rows:\n{wrapped}"
    );
    // Wrap off, and the cursor on the *other* line: the cursor line
    // always renders in full, so the long one is the one that cuts.
    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    let truncated = text_of(&draw(&mut app, 40, 14));
    assert!(truncated.contains('…'), "wrap off truncates:\n{truncated}");
    assert_eq!(
        truncated.lines().filter(|row| row.contains("abab")).count(),
        1,
        "to exactly one row:\n{truncated}"
    );
}

// A cursor line taller than the body was anchored at its bottom, so its
// start — the stamp, the level, the first words — was never on screen.
#[test]
fn a_cursor_line_taller_than_the_viewer_shows_from_its_start() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let long = format!("HEAD{}", "ab".repeat(400));
    write_log(&app, "feat+one", "dev", &["a line".to_string(), long]);
    app.open_log_viewer();
    draw(&mut app, 40, 14);
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    let wrapped = text_of(&draw(&mut app, 40, 14));
    assert!(wrapped.contains("HEAD"), "{wrapped}");
    // Wrap off, the cursor line still renders in full.
    app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::NONE));
    let unwrapped = text_of(&draw(&mut app, 40, 14));
    assert!(unwrapped.contains("HEAD"), "{unwrapped}");
}

// ---- yank ------------------------------------------------------------

#[test]
fn a_yank_confirmation_is_visible_in_the_viewers_footer() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["a line worth copying"]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(
        painted.contains("copied line"),
        "the viewer paints full screen, so the footer has to say it:\n{painted}"
    );
}

#[test]
fn the_footer_returns_to_its_hints_once_the_confirmation_expires() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["a line"]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert!(text_of(&draw(&mut app, 80, 12)).contains("copied"));
    app.status.as_mut().unwrap().at =
        std::time::Instant::now() - std::time::Duration::from_secs(60);
    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(!painted.contains("copied"), "{painted}");
    assert!(painted.contains("j/k move"), "{painted}");
}

// Phase 2c review, finding 4. The tab list is rebuilt from disk every
// paint, so a deleted file drops out of it — and the viewer was left
// on a tab that no longer existed, with `tab` a no-op because only one
// source was left. The only way out was to close the viewer.
#[test]
fn a_source_whose_file_is_deleted_keeps_its_tab_and_says_the_file_is_gone() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["one"]);
    write_log(&app, "feat+one", "install", &["two"]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    draw(&mut app, 80, 12);
    assert_eq!(
        app.log_view().expect("the viewer is open").source,
        "install"
    );

    std::fs::remove_file(app.paths.log_file("feat+one", "install")).unwrap();
    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(
        painted.contains("log deleted"),
        "the title has to say the file is gone:\n{painted}"
    );
    assert!(
        painted.contains("2:install"),
        "and the tab has to stay, or nothing can leave it:\n{painted}"
    );

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    draw(&mut app, 80, 12);
    assert_eq!(
        app.log_view().expect("the viewer is open").source,
        "dev",
        "tab leaves the dead source"
    );
}

// Phase 2c review, finding 6. `&` collapses to the search matches; with
// none, the empty body blamed the level filter — while the filter was
// `all` and `f` could not have helped.
#[test]
fn a_grep_collapse_with_no_matches_says_the_query_matched_nothing() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["one", "two"]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    for key in ['/', 'z', 'z', 'z', 'z'] {
        app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('&'), KeyModifiers::NONE));

    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(
        painted.contains("no line matches"),
        "the query is what hid them:\n{painted}"
    );
    assert!(
        !painted.contains("press f to change the filter"),
        "the level filter is `all`, so `f` cannot help:\n{painted}"
    );
}

// Phase 2c review, finding 7. `new_below` counts arrivals and is never
// reduced when the ring evicts the very lines it counted, so the badge
// offered to jump to more lines than the buffer holds.
#[test]
fn the_new_below_badge_counts_no_more_than_the_lines_under_the_cursor() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["a", "b", "c", "d", "e"]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    // What twenty ticks of appends into a full ring buffer leave
    // behind: a count of arrivals with nothing like that many lines
    // below the cursor any more.
    app.log_view_mut().expect("the viewer is open").new_below = 999;

    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(
        painted.contains("↓ 3 new"),
        "three lines are below the cursor:\n{painted}"
    );
    assert!(!painted.contains("999"), "{painted}");
}

#[test]
fn a_yank_from_the_overlay_is_confirmed_on_the_overlay() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["{\"a\":1}"]);
    app.open_log_viewer();
    draw(&mut app, 80, 20);
    app.handle_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    let painted = text_of(&draw(&mut app, 80, 20));
    assert!(painted.contains("copied"), "{painted}");
}

// ---- gutters, durations and URL wrapping -----------------------------

#[test]
fn every_row_of_a_line_carries_a_gutter_coloured_by_severity() {
    for (line, want) in [
        ("ERROR boom", red()),
        ("WARN slow", yellow()),
        ("just info", text_muted()),
    ] {
        let (_dir, mut app) = app_with_logs(&["feat+one"]);
        write_log(&app, "feat+one", "dev", &[line]);
        app.open_log_viewer();
        let buffer = draw(&mut app, 40, 10);
        let gutter = buffer.cell((1, 1)).unwrap();
        assert_eq!(gutter.symbol(), "▎", "{line}");
        assert_eq!(gutter.style().fg, Some(want), "{line}");
    }
}

#[test]
fn a_wrapped_line_keeps_one_continuous_gutter() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &[format!("ERROR {}", "x".repeat(90))],
    );
    app.open_log_viewer();
    let buffer = draw(&mut app, 40, 10);
    // Three body rows of one long error line, each with the same bar.
    for row in 1..4 {
        let gutter = buffer.cell((1, row)).unwrap();
        assert_eq!(gutter.symbol(), "▎", "row {row}");
        assert_eq!(gutter.style().fg, Some(red()), "row {row}");
    }
}

#[test]
fn a_json_block_is_bracketed_in_the_gutter() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["{", "  \"msg\": \"hi\",", "  \"n\": 1", "}"],
    );
    app.open_log_viewer();
    let buffer = draw(&mut app, 40, 10);
    let glyphs: Vec<&str> = (1..5)
        .map(|row| buffer.cell((1, row)).unwrap().symbol())
        .collect();
    assert_eq!(
        glyphs,
        vec!["╭", "│", "│", "╰"],
        "one entry reads as one unit"
    );
}

#[test]
fn a_line_that_is_not_in_a_block_keeps_the_plain_bar() {
    let buffer: std::collections::VecDeque<ParsedLine> = std::collections::VecDeque::new();
    assert_eq!(block_glyph(&buffer, 0), "▎", "an index that is not there");
}

// The colouring itself is `log_tail`'s, ported verbatim; this is that
// it survives the trip through the viewer's spans.
#[test]
fn a_duration_reaches_the_viewer_still_coloured_by_speed() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["GET /api/a 200 in 85ms"]);
    app.open_log_viewer();
    let buffer = draw(&mut app, 60, 10);
    let painted = text_of(&buffer);
    assert!(painted.contains("85ms"), "{painted}");
    let colours: Vec<_> = (0..60)
        .map(|x| buffer.cell((x, 1)).unwrap().style().fg)
        .collect();
    assert!(
        colours.contains(&Some(green())),
        "a fast duration is green: {colours:?}"
    );
}

#[test]
fn a_url_wraps_before_itself_rather_than_through_its_host() {
    let line = Line::raw("see https://example.com/a/b for more".to_string());
    // A width that would otherwise split "https://example.com" in two.
    let rows = wrap_line_to_rows(line, 20);
    let texts: Vec<String> = rows
        .iter()
        .map(|r| r.spans.iter().map(|s| s.content.to_string()).collect())
        .collect();
    assert!(
        texts.iter().any(|t| t.starts_with("https://example.com")),
        "the scheme and the host stay together: {texts:?}"
    );
    assert_eq!(
        texts.concat(),
        "see https://example.com/a/b for more",
        "and nothing is lost: {texts:?}"
    );
}

#[test]
fn a_host_wider_than_the_viewport_is_cut_like_anything_else() {
    let host = "a".repeat(40);
    let line = Line::raw(format!("x https://{host}/p"));
    let rows = wrap_line_to_rows(line, 10);
    let texts: Vec<String> = rows
        .iter()
        .map(|r| r.spans.iter().map(|s| s.content.to_string()).collect())
        .collect();
    assert!(
        rows.iter().all(|r| r
            .spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>()
            <= 10),
        "no row may exceed the width: {texts:?}"
    );
    assert_eq!(texts.concat(), format!("x https://{host}/p"));
}

#[test]
fn protected_ranges_cover_the_scheme_and_host_only() {
    let text = "get https://example.com/a?b=1 done";
    let ranges = protected_ranges(text);
    assert_eq!(ranges.len(), 1);
    let (from, to) = ranges[0];
    let chars: Vec<char> = text.chars().collect();
    assert_eq!(
        chars[from..to].iter().collect::<String>(),
        "https://example.com"
    );
    assert!(protected_ranges("no urls here").is_empty());
}

// ---- the inspect overlay ---------------------------------------------

#[test]
fn the_overlay_paints_the_pretty_printed_json_over_the_viewer() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(
        &app,
        "feat+one",
        "dev",
        &["request {\"path\":\"/checkout\",\"ms\":30}"],
    );
    app.open_log_viewer();
    draw(&mut app, 80, 20);
    app.handle_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE));
    let painted = text_of(&draw(&mut app, 80, 20));
    assert!(painted.contains("inspect"), "{painted}");
    assert!(painted.contains("\"path\": \"/checkout\""), "{painted}");
    assert!(painted.contains("y copy"), "{painted}");
}

#[test]
fn the_overlay_colours_the_json_it_shows() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["{\"path\":\"/x\"}"]);
    app.open_log_viewer();
    draw(&mut app, 80, 20);
    app.handle_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE));
    let colours: Vec<_> = app
        .inspect
        .as_ref()
        .unwrap()
        .lines
        .iter()
        .flat_map(|line| line.spans.iter().filter_map(|s| s.style.fg))
        .collect();
    assert!(
        colours.len() > 1,
        "the overlay is syntax-coloured, not one flat colour: {colours:?}"
    );
}

#[test]
fn capital_g_in_the_overlay_lands_on_the_real_last_page() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    let long: String = (0..60)
        .map(|i| format!("\"k{i}\":{i}"))
        .collect::<Vec<_>>()
        .join(",");
    write_log(&app, "feat+one", "dev", &[format!("{{{long}}}")]);
    app.open_log_viewer();
    draw(&mut app, 80, 20);
    app.handle_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE));
    assert_eq!(app.inspect.as_ref().unwrap().scroll, usize::MAX);
    draw(&mut app, 80, 20);
    let scroll = app.inspect.as_ref().unwrap().scroll;
    assert!(
        scroll > 0 && scroll < 1000,
        "the paint clamps it to a real page: {scroll}"
    );
}

#[test]
fn the_overlay_survives_a_terminal_too_small_for_it() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["{\"a\":1}"]);
    app.open_log_viewer();
    draw(&mut app, 80, 20);
    app.handle_key(KeyEvent::new(KeyCode::Char('J'), KeyModifiers::NONE));
    for (width, height) in [(1u16, 1u16), (2, 2), (4, 3), (20, 4), (48, 6)] {
        draw(&mut app, width, height);
    }
}

// ---- search ----------------------------------------------------------

fn content(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.to_string()).collect()
}

/// Only the parts of the line that got a background.
fn highlighted(line: &Line<'static>) -> String {
    line.spans
        .iter()
        .filter(|s| s.style.bg.is_some())
        .map(|s| s.content.to_string())
        .collect()
}

fn type_into(app: &mut App, text: &str) {
    for c in text.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
}

#[test]
fn the_search_bar_shows_the_query_while_it_is_typed() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["compiled in 30ms"]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into(&mut app, "comp");
    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(painted.contains("/comp"), "{painted}");
}

#[test]
fn an_active_search_shows_which_match_the_cursor_is_on() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["hit a", "plain", "hit b"]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    app.handle_key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE));
    type_into(&mut app, "hit");
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(painted.contains("[1/2]"), "{painted}");
    app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
    let painted = text_of(&draw(&mut app, 80, 12));
    assert!(painted.contains("[2/2]"), "{painted}");
}

#[test]
fn a_match_is_painted_and_the_one_under_the_search_cursor_differently() {
    let line = Line::raw("compiled in 30ms");
    let plain = highlight_search_in_line(line.clone(), "compiled in 30ms", "compiled", false);
    assert_eq!(highlighted(&plain), "compiled");
    let backgrounds: Vec<_> = plain.spans.iter().filter_map(|s| s.style.bg).collect();
    assert_eq!(backgrounds, vec![search_match_bg()]);

    let under_cursor = highlight_search_in_line(line, "compiled in 30ms", "compiled", true);
    let backgrounds: Vec<_> = under_cursor
        .spans
        .iter()
        .filter_map(|s| s.style.bg)
        .collect();
    assert_eq!(backgrounds, vec![search_cursor_bg()]);
}

// `to_lowercase` is not length-preserving: `İ` is two bytes and
// lowercases to three. Slicing the original at lowercased offsets cuts
// a character in half and panics.
#[test]
fn highlighting_does_not_panic_when_lowercasing_grows_a_character() {
    let result = highlight_search_in_line(Line::raw("İx"), "İx", "i", false);
    assert_eq!(content(&result), "İx");
    assert_eq!(highlighted(&result), "");
}

#[test]
fn highlighting_lands_on_the_original_range_after_a_growing_character() {
    let result = highlight_search_in_line(Line::raw("İé"), "İé", "é", false);
    assert_eq!(content(&result), "İé");
    assert_eq!(highlighted(&result), "é");
}

// The real crash surface: a coloured line arrives already split into
// spans, so the offset has to be tracked across them.
#[test]
fn highlighting_crosses_spans_after_a_growing_character() {
    let line = Line::from(vec![
        Span::styled("İ ".to_string(), Style::new().fg(text_dim())),
        Span::styled("GET".to_string(), Style::new().fg(cyan())),
    ]);
    let result = highlight_search_in_line(line, "İ GET", "get", false);
    assert_eq!(content(&result), "İ GET");
    assert_eq!(highlighted(&result), "GET");
}

#[test]
fn wrap_line_to_rows_never_returns_nothing() {
    assert_eq!(wrap_line_to_rows(Line::raw(""), 10).len(), 1);
    assert_eq!(wrap_line_to_rows(Line::raw("abcdef"), 2).len(), 3);
    // A zero width would divide by nothing; it is clamped to one.
    assert_eq!(wrap_line_to_rows(Line::raw("ab"), 0).len(), 2);
}

#[test]
fn wrap_keeps_every_span_style_across_the_split() {
    let line = Line::from(vec![
        Span::styled("aaaa", Style::new().fg(red())),
        Span::styled("bbbb", Style::new().fg(green())),
    ]);
    let rows = wrap_line_to_rows(line, 3);
    let flat: Vec<(String, Style)> = rows
        .iter()
        .flat_map(|r| r.spans.iter().map(|s| (s.content.to_string(), s.style)))
        .collect();
    assert_eq!(
        flat.iter().map(|(c, _)| c.as_str()).collect::<String>(),
        "aaaabbbb"
    );
    for (content, style) in &flat {
        let want = if content.starts_with('a') {
            red()
        } else {
            green()
        };
        assert_eq!(style.fg, Some(want), "{content}");
    }
}

// ---- sizes, words and marks -------------------------------------------

/// The rectangle a popup's border draws, found by its top-left corner.
fn popup_size(buf: &Buffer, title: &str) -> (usize, usize) {
    let text = text_of(buf);
    let lines: Vec<&str> = text.lines().collect();
    let (top, line) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| line.contains(&format!("╭ {title} ")))
        .expect("the popup is drawn");
    let chars: Vec<char> = line.chars().collect();
    let start = line[..line.find(&format!("╭ {title} ")).unwrap()]
        .chars()
        .count();
    let end = (start + 1..chars.len())
        .find(|&i| chars[i] == '╮')
        .expect("the popup has a right edge");
    let bottom = (top + 1..lines.len())
        .find(|&y| lines[y].chars().nth(start) == Some('╰'))
        .expect("the popup has a bottom edge");
    (end - start + 1, bottom - top + 1)
}

// A two-line confirmation is a small box on a big screen, not half of it.
#[test]
fn a_confirmation_is_sized_to_what_it_says() {
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Remove {
        name: "feat+one".into(),
        created_by_pando: true,
    });
    let (width, height) = popup_size(&draw(&mut app, 200, 50), "remove worktree");
    assert!(width < 70, "{width} columns for two short lines");
    assert!(height <= 9, "{height} rows for four lines and a margin");
}

// The key column is as wide as its widest key: `PgUp PgDn` used to run
// straight into what it does.
#[test]
fn the_help_key_column_never_runs_into_its_description() {
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Help);
    let rendered = text_of(&draw(&mut app, 120, 60));
    let row = rendered
        .lines()
        .find(|line| line.contains("PgUp PgDn"))
        .expect("help lists the page keys");
    assert!(row.contains("PgUp PgDn  "), "{row}");
    assert!(
        rendered.contains("in the list"),
        "and the legend: {rendered}"
    );
}

// The option is the command being chosen: it is never cut to make room
// for the reason it was offered.
#[test]
fn a_question_shows_each_option_whole() {
    let (reply, _rx) = std::sync::mpsc::channel();
    let command =
        "export NVM_DIR=\"$HOME/.nvm\" && . \"$NVM_DIR/nvm.sh\" --no-use && nvm use >/dev/null";
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Question {
        question: crate::actions::Question {
            slot: crate::detect::Slot::Prelude,
            prompt: "Which line should pando run first, so this shell resolves the runtime \
                     the project asks for?"
                .into(),
            options: vec![(
                command.to_string(),
                "nvm is installed here, and a login shell has to source it".to_string(),
            )],
            preselect: Some(0),
            allow_custom: true,
            allow_none: false,
            multi: false,
            checked: Vec::new(),
            details: vec!["this project asks for node 22 (.nvmrc)".into()],
            answer_file: None,
            snippet: String::new(),
        },
        selected: 0,
        custom: None,
        reply,
    });
    for width in [80u16, 140] {
        let rendered = text_of(&draw(&mut app, width, 30));
        let joined: String = rendered
            .lines()
            .map(|line| line.trim_matches(|c| c == '│' || c == ' '))
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("resolves the runtime"),
            "the prompt wraps rather than being cut at {width}:\n{rendered}"
        );
        for piece in [
            "export NVM_DIR",
            "nvm use >/dev/null",
            "a login shell has to source it",
        ] {
            assert!(
                joined.contains(piece),
                "{piece:?} is shown at {width}:\n{rendered}"
            );
        }
    }
}

// The end of an error is usually the part that says what to do, so an
// error that does not fit on the header's row takes a second one.
#[test]
fn a_long_error_wraps_onto_more_header_rows() {
    let mut app = test_app(&["feat+one"]);
    app.set_error(
        "could not start feat/one: isolated mode needs Docker, and the Docker daemon \
         is not running — start Docker Desktop and press i again",
    );
    let rendered = text_of(&draw(&mut app, 80, 24));
    let header: Vec<&str> = rendered.lines().take(3).collect();
    assert!(
        header[0].contains('✗'),
        "an error is marked as one: {rendered}"
    );
    assert!(
        header.join(" ").contains("press i again"),
        "and read to its end: {rendered}"
    );
}

#[test]
fn a_success_is_marked_differently_from_an_error() {
    let mut app = test_app(&["feat+one"]);
    app.set_success("started feat/one");
    let first = text_of(&draw(&mut app, 80, 24))
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(first.contains("✓ started feat/one"), "{first}");
}

#[test]
fn a_path_under_home_is_written_with_a_tilde() {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return;
    };
    assert_eq!(home_relative(&home.join("code/acme")), "~/code/acme");
    assert_eq!(
        home_relative(std::path::Path::new("/elsewhere/acme")),
        "/elsewhere/acme"
    );
}

#[test]
fn the_tail_header_counts_one_line_as_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("dev.log");
    std::fs::write(&log, "ready\n").unwrap();
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let (key, _, _) = app.tail_target().unwrap();
    app.log_tails.touch(&key, log).poll().unwrap();
    let rendered = text_of(&draw(&mut app, 120, 24));
    assert!(rendered.contains("1 line ·"), "{rendered}");
}

// The public URL is the thing people copy: a narrow pane wraps it, and
// never cuts it.
#[test]
fn a_public_url_is_never_truncated() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    crate::tui::app::tests::with_share(&mut app, "feat+one", None);
    let url = "https://fake-host.trycloudflare.com";
    for width in [60u16, 90, 140] {
        let rendered = text_of(&draw(&mut app, width, 30));
        let detail: String = rendered
            .lines()
            .filter_map(|line| line.rsplit("││").next())
            .map(|part| part.trim_matches(|c| c == '│' || c == ' ').to_string())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            detail.replace(' ', "").contains(url),
            "the whole URL at {width}:\n{rendered}"
        );
    }
}

#[test]
fn the_detail_pane_says_a_start_is_waiting_on_a_question() {
    let (reply, _rx) = std::sync::mpsc::channel();
    let mut app = test_app(&["feat+one"]);
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    app.modal = Some(Modal::Question {
        question: crate::actions::Question {
            slot: crate::detect::Slot::DevCmd,
            prompt: "Which command?".into(),
            options: vec![("pnpm dev".into(), String::new())],
            preselect: Some(0),
            allow_custom: true,
            allow_none: false,
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
    let rendered = text_of(&draw(&mut app, 140, 30));
    assert!(rendered.contains("waiting for your answer"), "{rendered}");
    assert!(!rendered.contains("○ stopped"), "{rendered}");
}

// First run: what pando is, what it knows, and the key to get going.
#[test]
fn the_first_run_welcomes_rather_than_showing_an_empty_box() {
    let mut app = test_app(&[]);
    app.config.processes.insert(
        "dev".into(),
        crate::config::ProcessConfig {
            cmd: "pnpm dev".into(),
            ..Default::default()
        },
    );
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(
        rendered.contains("one dev environment per branch"),
        "{rendered}"
    );
    assert!(rendered.contains("dev: pnpm dev"), "{rendered}");
    assert!(rendered.contains("create a worktree"), "{rendered}");
    assert!(!rendered.contains("worktrees (0)"), "{rendered}");
}

// A source that has written only blank lines is a source with no output,
// not a gutter mark on an empty row.
#[test]
fn a_source_with_only_blank_lines_says_it_has_no_output() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    write_log(&app, "feat+one", "dev", &["listening"]);
    write_log(&app, "feat+one", "install", &[""]);
    app.open_log_viewer();
    draw(&mut app, 80, 12);
    app.handle_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::NONE));
    app.handle_event(crate::tui::app::AppEvent::Tick);
    let rendered = text_of(&draw(&mut app, 80, 12));
    assert!(rendered.contains("(no output yet)"), "{rendered}");
    assert!(!rendered.contains('▎'), "{rendered}");
}

/// The keys a hint names: `j/k` is two, `/` is itself.
fn hint_keys(hint: &str) -> Vec<&str> {
    if hint == "/" {
        vec![hint]
    } else {
        hint.split('/').collect()
    }
}

// The footer's hints are keys help documents; a hint for a key nothing
// answers would be worse than none.
#[test]
fn every_footer_hint_is_a_key_help_documents() {
    use crate::tui::app::{LIST_KEYS, LOG_KEYS};
    // A key's own words, and the whole of a key that takes two presses,
    // such as `space g`.
    let tokens = |keys: &[crate::tui::app::KeyHelp]| -> Vec<String> {
        keys.iter()
            .flat_map(|k| {
                k.keys
                    .split(' ')
                    .map(str::to_string)
                    .chain([k.keys.to_string()])
            })
            .collect()
    };
    let list = tokens(LIST_KEYS);
    let shared = [("O", "public", false), ("C", "copy public", false)];
    for (key, _, _) in RUNNING_HINTS
        .iter()
        .chain(&STOPPED_HINTS)
        .chain(&NOTHING_TO_RUN_HINTS)
        .chain(&EMPTY_HINTS)
        .chain(&shared)
    {
        for part in hint_keys(key) {
            assert!(list.contains(&part.to_string()), "{part} is not in help");
        }
    }
    let log = tokens(LOG_KEYS);
    for (key, _, _) in LOG_HINTS {
        for part in hint_keys(key) {
            assert!(log.contains(&part.to_string()), "{part} is not in help");
        }
    }
}

/// The style of the first cell of `needle` on the first row that shows
/// `row` as well.
fn style_at(buf: &Buffer, row: &str, needle: &str) -> ratatui::style::Style {
    let text = text_of(buf);
    let (y, line) = text
        .lines()
        .enumerate()
        .find(|(_, line)| line.contains(row) && line.contains(needle))
        .unwrap_or_else(|| panic!("{needle} is not on screen:\n{text}"));
    let x = line[..line.find(needle).unwrap()].chars().count();
    buf.cell((x as u16, y as u16)).unwrap().style()
}

// Thirty stopped rows drown out the two that matter: a stopped branch
// recedes, and one that runs does not.
#[test]
fn a_stopped_branch_recedes_and_a_running_one_does_not() {
    let mut app = test_app(&["feat+up", "feat+down"]);
    with_process(&mut app, "feat+up", running_phase());
    // The cursor's highlight restyles its row; put it on neither.
    app.list_state.select(None);
    let buf = draw(&mut app, 120, 12);
    let stopped = style_at(&buf, "feat/down", "feat/down");
    let running = style_at(&buf, "feat/up", "feat/up");
    assert_eq!(stopped.fg, Some(text_dim()), "{stopped:?}");
    assert_eq!(running.fg, Some(crate::theme::text()), "{running:?}");
}

// PANDO_HOME moves the home; the welcome names the one in use.
#[test]
fn the_welcome_names_the_real_pando_home() {
    let mut app = test_app(&[]);
    let rendered = text_of(&draw(&mut app, 140, 30));
    assert!(
        rendered.contains("/pando-test-does-not-exist/home"),
        "{rendered}"
    );
    assert!(!rendered.contains("~/.pando"), "{rendered}");
}

// `[project] worktrees_dir` moves the worktrees out of pando home; the
// welcome's last line says where they went rather than contradicting the
// fact above it.
#[test]
fn the_welcome_names_a_worktrees_dir_outside_pando_home() {
    let mut app = test_app(&[]);
    app.config.project.worktrees_dir = Some("/pando-test-does-not-exist/code/wt".into());
    let rendered = text_of(&draw(&mut app, 200, 30));
    assert!(
        rendered.contains("worktrees live in /pando-test-does-not-exist/code/wt"),
        "{rendered}"
    );
    assert!(
        rendered.contains("logs and config under /pando-test-does-not-exist/home"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("worktrees, logs and config"),
        "{rendered}"
    );
}

#[test]
fn the_share_confirmation_names_the_address_that_goes_public() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    app.modal = Some(Modal::Share {
        name: "feat+one".into(),
    });
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(rendered.contains("share feat/one publicly?"), "{rendered}");
    assert!(rendered.contains("http://localhost:17342"), "{rendered}");
    assert!(rendered.contains("y share"), "{rendered}");
}

#[test]
fn the_switch_confirmation_says_what_restarts_on_which_services() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    use crate::state::ServiceMode;
    for (from, to, title, now) in [
        (
            ServiceMode::Shared,
            ServiceMode::Isolated,
            "restart feat/one isolated?",
            "shared services now",
        ),
        (
            ServiceMode::Isolated,
            ServiceMode::Shared,
            "restart feat/one shared?",
            "private copies",
        ),
        (
            ServiceMode::Namespaced,
            ServiceMode::Shared,
            "restart feat/one shared?",
            "namespaces are kept until rm",
        ),
        (
            ServiceMode::Shared,
            ServiceMode::Namespaced,
            "restart feat/one namespaced?",
            "namespaces of its own",
        ),
    ] {
        app.state.worktrees.get_mut("feat+one").unwrap().mode = Some(from);
        app.modal = Some(Modal::SwitchMode {
            name: "feat+one".into(),
            to,
        });
        let rendered = text_of(&draw(&mut app, 120, 30));
        assert!(rendered.contains(title), "{rendered}");
        assert!(rendered.contains(now), "{rendered}");
        assert!(rendered.contains("api, dev restart"), "{rendered}");
    }
}

#[test]
fn the_stop_all_confirmation_lists_what_goes_down() {
    let mut app = test_app(&["feat+one", "feat+two", "feat+three"]);
    with_process(&mut app, "feat+one", running_phase());
    with_process(&mut app, "feat+three", running_phase());
    app.modal = Some(Modal::StopAll {
        names: vec!["feat+one".into(), "feat+three".into()],
    });
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(rendered.contains("stop all 2 worktrees"), "{rendered}");
    assert!(rendered.contains("feat/one"), "{rendered}");
    assert!(rendered.contains("feat/three"), "{rendered}");
}

// Twelve names were listed whatever the pane's height, so in a 12-row
// split with ten worktrees up the warning and the key line fell off the
// bottom, and `y` still stopped them all.
#[test]
fn a_short_stop_all_confirmation_counts_names_to_keep_its_keys() {
    let names: Vec<String> = (0..10).map(|i| format!("feat+number-{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut app = test_app(&refs);
    for name in &refs {
        with_process(&mut app, name, running_phase());
    }
    app.modal = Some(Modal::StopAll {
        names: names.clone(),
    });
    let rendered = text_of(&draw(&mut app, 60, 12));
    assert!(rendered.contains("y stop all   esc cancel"), "{rendered}");
    assert!(rendered.contains("go down too"), "{rendered}");
    assert!(rendered.contains("… and 5 more"), "{rendered}");

    // Where there is room, the twelve it always listed.
    let rendered = text_of(&draw(&mut app, 120, 40));
    assert!(rendered.contains("feat/number-9"), "{rendered}");
    assert!(!rendered.contains("more"), "{rendered}");
}

// Removing a running worktree stops it first; the confirmation says so.
#[test]
fn removing_a_running_worktree_warns_that_it_stops_it() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    app.modal = Some(Modal::Remove {
        name: "feat+one".into(),
        created_by_pando: true,
    });
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(rendered.contains("removing stops it first"), "{rendered}");

    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Remove {
        name: "feat+one".into(),
        created_by_pando: true,
    });
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(!rendered.contains("removing stops it first"), "{rendered}");
}

// ---- git state on screen ---------------------------------------------

// A worktree with uncommitted changes is marked `✎` in its git cell, even
// on a pane too narrow for the pull request or the status word.
#[test]
fn a_dirty_worktree_is_marked_in_the_list_at_any_width() {
    let mut app = test_app(&["feat+tui", "feat+clean"]);
    app.worktrees[0].dirty = Some(true);
    for width in [60, 100, 200] {
        let rendered = text_of(&draw(&mut app, width, 12));
        let row = list_row(&rendered, "feat/tui");
        assert!(row.contains("✎"), "at {width}:\n{rendered}");
        assert!(
            !list_row(&rendered, "feat/clean").contains("✎"),
            "at {width}:\n{rendered}"
        );
    }
}

// The pull request comes first after the branch: its number, and its state
// as a mark and a word, in the colour GitHub gives that state.
#[test]
fn a_rows_pull_request_says_its_number_and_state_first() {
    use crate::worktree::PrState;
    let mut app = test_app(&["feat+open", "feat+draft", "feat+merged", "feat+closed"]);
    for (branch, number, state, draft) in [
        ("feat/open", 482, PrState::Open, false),
        ("feat/draft", 490, PrState::Open, true),
        ("feat/merged", 463, PrState::Merged, false),
        ("feat/closed", 400, PrState::Closed, false),
    ] {
        app.prs.insert(
            branch.into(),
            crate::worktree::PrInfo {
                state,
                ..a_pr(number, branch, draft, false)
            },
        );
    }
    app.worktrees[0].dirty = Some(true);
    let buf = draw(&mut app, 160, 14);
    let rendered = text_of(&buf);
    let header = list_row(&rendered, "branch");
    let at = |line: &str, needle: &str| {
        line.find(needle)
            .map(|i| line[..i].chars().count())
            .unwrap_or_else(|| panic!("no {needle}:\n{rendered}"))
    };
    for (branch, chip, color) in [
        ("feat/open", "◍ #482", green()),
        ("feat/draft", "◌ #490", crate::theme::text_muted()),
        ("feat/merged", "✓ #463", crate::theme::magenta()),
        ("feat/closed", "✗ #400", red()),
    ] {
        let row = list_row(&rendered, branch);
        assert_eq!(at(&row, chip), at(&header, "PR"), "{branch}:\n{rendered}");
        // The first column after the label's rule.
        let after_label = row
            .trim_start_matches('│')
            .split(" │ ")
            .nth(1)
            .unwrap_or_default();
        assert!(after_label.starts_with(chip), "{branch}:\n{rendered}");
        assert_eq!(style_at(&buf, branch, chip).fg, Some(color), "{chip}");
        // The mark says the state; no word repeats it.
        assert_eq!(after_label.trim_end(), chip, "{branch}:\n{rendered}");
    }
    assert!(!header.contains("changes"), "{header}");
}

// Ahead is work of the branch's own, in green; behind is something to
// rebase onto, in yellow; each with its own arrow.
#[test]
fn ahead_and_behind_are_coloured_apart_in_the_list_and_the_detail_pane() {
    let mut app = test_app(&["feat+one"]);
    app.worktrees[0].ahead_behind = Some((2, 3));
    let buf = draw(&mut app, 160, 30);
    let rendered = text_of(&buf);
    assert!(
        list_row(&rendered, "feat/one").contains("↓3 ↑2"),
        "{rendered}"
    );
    assert_eq!(style_at(&buf, "↓3 ↑2", "↑2").fg, Some(green()));
    assert_eq!(style_at(&buf, "↓3 ↑2", "↓3").fg, Some(yellow()));
    assert_eq!(style_at(&buf, "│ git ", "↑2 ahead").fg, Some(green()));
    assert_eq!(style_at(&buf, "│ git ", "↓3 behind").fg, Some(yellow()));
}

// A mode is a mark after the branch, in the mode's colour, not a column
// of its own; the detail pane puts the word beside it.
#[test]
fn a_mode_is_a_mark_after_the_branch_and_a_mark_and_word_in_the_detail_pane() {
    use crate::state::ServiceMode;
    let mut app = test_app(&["feat+iso", "feat+ns", "feat+plain"]);
    with_process(&mut app, "feat+iso", running_phase());
    with_mode(&mut app, "feat+iso", ServiceMode::Isolated);
    with_process(&mut app, "feat+ns", running_phase());
    with_mode(&mut app, "feat+ns", ServiceMode::Namespaced);
    let buf = draw(&mut app, 180, 30);
    let rendered = text_of(&buf);
    assert!(
        list_row(&rendered, "feat/iso").contains("feat/iso ▣"),
        "{rendered}"
    );
    assert!(
        list_row(&rendered, "feat/ns").contains("feat/ns ◧"),
        "{rendered}"
    );
    assert!(
        !list_row(&rendered, "feat/plain").contains('▣'),
        "{rendered}"
    );
    assert!(
        !list_row(&rendered, "branch").contains("mode"),
        "{rendered}"
    );
    assert_eq!(
        style_at(&buf, "feat/iso ▣", "▣").fg,
        Some(crate::theme::magenta())
    );
    assert_eq!(
        style_at(&buf, "feat/ns ◧", "◧").fg,
        Some(crate::theme::namespaced())
    );
    assert!(
        rendered.contains("▣ isolated"),
        "the selected one's detail:\n{rendered}"
    );

    // A label too long for the pane gives way; the mark does not.
    app.worktrees[1].branch = Some(format!("feat/{}", "n".repeat(120)));
    let rendered = text_of(&draw(&mut app, 100, 30));
    assert!(list_row(&rendered, "feat/nnn").contains(" ◧"), "{rendered}");
}

// The git cell is slots, each as wide as its widest in the list: every
// behind arrow under the one above, then every ahead arrow, then the `✎`.
#[test]
fn the_git_column_lines_behind_ahead_and_uncommitted_up_in_slots() {
    let mut app = test_app(&["feat+a", "feat+b", "feat+c", "feat+d"]);
    app.worktrees[0].ahead_behind = Some((56, 120));
    app.worktrees[0].dirty = Some(true);
    app.worktrees[1].ahead_behind = Some((0, 7));
    app.worktrees[2].ahead_behind = Some((3, 0));
    app.worktrees[2].dirty = Some(true);
    app.worktrees[3].ahead_behind = Some((0, 0));
    let rendered = text_of(&draw(&mut app, 180, 14));
    let column = |needle: &str| {
        rendered
            .lines()
            .filter(|line| line.starts_with('│'))
            .map(|line| line.split("││").next().unwrap_or_default().to_string())
            .filter_map(|row| row.find(needle).map(|i| row[..i].chars().count()))
            .collect::<Vec<_>>()
    };
    let behind = column("↓");
    let ahead = column("↑");
    let dirty = column("✎");
    assert_eq!(behind.len(), 2, "{rendered}");
    assert!(behind.windows(2).all(|w| w[0] == w[1]), "{rendered}");
    assert_eq!(ahead.len(), 2, "{rendered}");
    assert!(ahead.windows(2).all(|w| w[0] == w[1]), "{rendered}");
    assert_eq!(dirty.len(), 2, "{rendered}");
    assert!(dirty.windows(2).all(|w| w[0] == w[1]), "{rendered}");
    assert!(behind[0] < ahead[0] && ahead[0] < dirty[0], "{rendered}");
    assert!(
        list_row(&rendered, "feat/a").contains("↓99+ ↑56 ✎"),
        "{rendered}"
    );
}

#[test]
fn the_detail_pane_has_a_git_row() {
    let mut app = test_app(&["feat+tui"]);
    app.worktrees[0].dirty = Some(true);
    app.worktrees[0].ahead_behind = Some((2, 1));
    let rendered = text_of(&draw(&mut app, 200, 30));
    let row = rendered
        .lines()
        .find(|line| line.contains("││ git "))
        .unwrap_or_default();
    assert!(row.contains("uncommitted changes"), "{rendered}");
    assert!(row.contains("↓1 behind, ↑2 ahead of main"), "{rendered}");

    app.worktrees[0].dirty = Some(false);
    app.worktrees[0].ahead_behind = Some((0, 0));
    let rendered = text_of(&draw(&mut app, 140, 30));
    let row = rendered
        .lines()
        .find(|line| line.contains("││ git "))
        .unwrap_or_default();
    assert!(row.contains("clean · even with main"), "{rendered}");
}

// Git state nobody knows is `reading` only while it is being read: a
// worktree whose directory is gone, or whose `git status` failed, would
// otherwise say it for ever, under a header that stopped saying it.
#[test]
fn the_git_row_says_reading_only_while_git_is_being_read() {
    let git_row = |app: &mut App| {
        let rendered = text_of(&draw(app, 140, 30));
        rendered
            .lines()
            .find(|line| line.contains("││ git "))
            .unwrap_or_else(|| panic!("no git row:\n{rendered}"))
            .to_string()
    };
    let mut app = test_app(&["feat+tui"]);
    app.worktrees[0].dirty = None;
    app.enriching = 1;
    assert!(git_row(&mut app).contains("reading git…"));
    app.enriching = 0;
    app.git_refreshing = true;
    assert!(git_row(&mut app).contains("reading git…"));
    app.git_refreshing = false;
    let row = git_row(&mut app);
    assert!(row.contains("status could not be read"), "{row}");

    app.worktrees[0].prunable = true;
    let row = git_row(&mut app);
    assert!(row.contains("no working tree to read"), "{row}");
    assert!(!row.contains("reading"), "{row}");
}

// One duration style in the pane: the commit's age reads like the uptime.
#[test]
fn the_commit_age_is_compact() {
    let mut app = test_app(&["feat+one"]);
    app.worktrees[0].head_age = Some("82 seconds ago".into());
    let rendered = text_of(&draw(&mut app, 140, 30));
    assert!(rendered.contains("· 1m ago"), "{rendered}");
    assert!(!rendered.contains("seconds ago"), "{rendered}");
}

// ---- the remove dialog -----------------------------------------------

#[test]
fn the_remove_dialog_states_dirty_and_running_and_offers_f() {
    let mut app = test_app(&["feat+tui"]);
    app.worktrees[0].dirty = Some(true);
    with_process(&mut app, "feat+tui", running_phase());
    app.modal = Some(Modal::Remove {
        name: "feat+tui".into(),
        created_by_pando: true,
    });
    let rendered = text_of(&draw(&mut app, 140, 30));
    assert!(rendered.contains("uncommitted changes"), "{rendered}");
    assert!(rendered.contains("removing stops it first"), "{rendered}");
    assert!(
        rendered.contains("F remove, discarding changes"),
        "{rendered}"
    );
    assert!(!rendered.contains("--force"), "{rendered}");
}

#[test]
fn a_clean_removal_offers_y_and_f() {
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Remove {
        name: "feat+one".into(),
        created_by_pando: true,
    });
    let rendered = text_of(&draw(&mut app, 140, 30));
    assert!(rendered.contains("y remove"), "{rendered}");
    assert!(rendered.contains("F force"), "{rendered}");
    assert!(!rendered.contains("uncommitted"), "{rendered}");
}

// ---- nothing to run --------------------------------------------------

#[test]
fn a_project_with_nothing_to_run_says_so_in_the_pane_and_the_footer() {
    let mut app = test_app(&["main-lib"]);
    app.nothing_to_run = true;
    let rendered = text_of(&draw(&mut app, 200, 30));
    assert!(!rendered.contains("⏎ picks a mode"), "{rendered}");
    assert!(
        rendered.contains("nothing to run: add a [dev] command in"),
        "{rendered}"
    );
    let footer = rendered.lines().last().unwrap_or_default();
    assert!(footer.contains("nothing to run"), "{footer}");
    assert!(!footer.contains("start"), "{footer}");
}

// ---- the header ------------------------------------------------------

#[test]
fn the_shared_service_dots_are_labelled() {
    let mut app = test_app(&["feat+one"]);
    app.service_health = crate::tui::app::ServiceHealth {
        shared: vec![crate::actions::ServiceStatus {
            name: "postgres".into(),
            port: Some(5432),
            up: true,
            logging: false,
            env_file: None,
        }],
        worktrees: std::collections::BTreeMap::new(),
    };
    let header = text_of(&draw(&mut app, 120, 12))
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(header.contains("shared: postgres ● up"), "{header}");
}

// At 80×24 an error takes at most two header rows; the rest is in `m`.
#[test]
fn a_long_error_on_a_short_screen_takes_two_rows_at_most() {
    let mut app = test_app(&["feat+one"]);
    app.set_error("word ".repeat(80));
    assert_eq!(header_height(&app, 80, 24), 2);
    let rendered = text_of(&draw(&mut app, 80, 24));
    assert!(
        rendered.lines().nth(1).unwrap().contains("m shows it all"),
        "{rendered}"
    );
    assert_eq!(header_height(&app, 80, 40), 3, "a tall screen keeps three");
}

// ---- the list's columns ----------------------------------------------

// The status word is at the end of the row, so a row going from
// starting to running moves nothing before it.
#[test]
fn the_port_stays_put_between_starting_and_running() {
    let url_column = |phase: crate::state::Phase| {
        let mut app = test_app(&["feat+one"]);
        with_process(&mut app, "feat+one", phase);
        let rendered = text_of(&draw(&mut app, 140, 10));
        list_row(&rendered, "feat/one").find(":17342").unwrap()
    };
    assert_eq!(
        url_column(crate::state::Phase::Starting {
            since: chrono::Utc::now()
        }),
        url_column(running_phase())
    );
}

// On a wide screen the room goes to the label: the columns are as wide
// as their cells, and the last one ends against the right edge.
#[test]
fn a_wide_list_puts_the_columns_against_the_right_edge() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    with_process(&mut app, "feat+one", running_phase());
    app.worktrees[1].ahead_behind = Some((3, 14));
    app.worktrees[1].dirty = Some(true);
    let rendered = text_of(&draw(&mut app, 200, 10));
    let row = list_row(&rendered, "feat/two");
    assert!(row.ends_with("↓14 ↑3 ✎ "), "{row:?}\n{rendered}");
    let header = list_row(&rendered, "branch");
    assert!(header.contains("│ git      "), "{header:?}");
    assert!(
        header.ends_with("git      "),
        "no more room than it needs: {header:?}"
    );
}

// A status word is at the end of the label's cell, which is as wide with
// it as without: a row starting or failing moves no column.
#[test]
fn a_status_word_ends_the_label_cell_and_moves_nothing() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    with_process(&mut app, "feat+one", running_phase());
    let before = text_of(&draw(&mut app, 160, 10));
    with_process(
        &mut app,
        "feat+two",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited".into(),
        },
    );
    let after = text_of(&draw(&mut app, 160, 10));
    let row = list_row(&after, "feat/two");
    assert!(row.contains("failed │"), "{after}");
    let rule = |rendered: &str| list_row(rendered, "branch").find('│');
    assert_eq!(rule(&before), rule(&after), "{before}\n{after}");
    let port = |rendered: &str| list_row(rendered, "feat/one").find(":17342");
    assert_eq!(port(&before), port(&after), "{before}\n{after}");
}

// With room to spare, every port of a multi-process worktree shows.
#[test]
fn a_wide_list_shows_every_port_and_a_narrow_one_sheds_them_first() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    let wide = text_of(&draw(&mut app, 220, 10));
    let row = list_row(&wide, "feat/one");
    assert!(row.contains(":17342 │ api:17344"), "{wide}");
    assert!(
        !row.contains("web:17342"),
        "the URL's port is not said twice: {row}"
    );
    let narrow = text_of(&draw(&mut app, 72, 10));
    let row = list_row(&narrow, "feat/one");
    assert!(!row.contains("api:17344"), "{narrow}");
    assert!(row.contains(":17342"), "{narrow}");
}

// Every port is a nicety; a branch cut short is what the list exists to
// show. With a long branch in the list the ports go before it is cut.
#[test]
fn the_ports_give_way_before_a_long_branch_is_cut() {
    let long = "feature+checkout-flow-for-guests";
    let mut app = test_app(&["feat+one", long]);
    with_process(&mut app, "feat+one", running_phase());
    with_second_process(&mut app, "feat+one", "api", running_phase());
    let rendered = text_of(&draw(&mut app, 95, 10));
    assert!(
        rendered.contains("feature/checkout-flow-for-guests"),
        "{rendered}"
    );
    assert!(
        !list_row(&rendered, "feat/one").contains("api:17344"),
        "{rendered}"
    );
}

// ---- help ------------------------------------------------------------

#[test]
fn help_says_which_keys_close_it() {
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Help);
    let tall = text_of(&draw(&mut app, 140, 80));
    assert!(tall.contains("esc/q/? close"), "{tall}");
    assert!(!tall.contains("any key"), "{tall}");
    let short = text_of(&draw(&mut app, 140, 20));
    assert!(short.contains("j/k g/G scroll · esc/q/? close"), "{short}");
}

#[test]
fn help_explains_the_dirty_mark_and_the_shared_dots() {
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Help);
    let rendered = text_of(&draw(&mut app, 160, 90));
    assert!(rendered.contains("uncommitted changes"), "{rendered}");
    assert!(rendered.contains("default ports"), "{rendered}");
}

// ---- the create modal ------------------------------------------------

#[test]
fn the_picker_marks_the_main_checkouts_branch_and_shows_the_base() {
    let mut app = test_app(&["feat+one"]);
    let mut main = wt("acme-shop");
    main.branch = Some("main".into());
    app.main = Some(main);
    app.modal = Some(Modal::Create {
        input: String::new(),
        branches: BranchLoadState::Ready(vec![crate::worktree::BranchEntry {
            name: "main".into(),
            source: crate::worktree::BranchSource::Local,
        }]),
        selected: 0,
        base: Some("develop".into()),
    });
    let rendered = text_of(&draw(&mut app, 140, 30));
    assert!(
        rendered.contains("checked out in the main checkout"),
        "{rendered}"
    );
    assert!(rendered.contains("fork from develop"), "{rendered}");
    assert!(rendered.contains("tab"), "{rendered}");
}

// The base the dialog names is the one `new` would fork from: a branch
// rule that matches the typed name, then `[project] base`, and only then
// the repository's default.
#[test]
fn the_create_dialog_names_the_base_the_config_gives_the_typed_branch() {
    let mut app = test_app(&["feat+one"]);
    app.default_base = Some("origin/main".into());
    app.config.project.base = Some("develop".into());
    app.config.branches.rules.push(crate::config::BranchRule {
        match_: "hotfix/*".into(),
        base: "release".into(),
    });
    let mut dialog_for = |input: &str| {
        app.modal = Some(Modal::Create {
            input: input.into(),
            branches: BranchLoadState::Ready(Vec::new()),
            selected: 0,
            base: None,
        });
        text_of(&draw(&mut app, 140, 30))
    };
    let rendered = dialog_for("feat/x");
    assert!(rendered.contains("fork from develop"), "{rendered}");
    let rendered = dialog_for("hotfix/y");
    assert!(rendered.contains("fork from release"), "{rendered}");
}

// ---- the all tab -----------------------------------------------------

#[test]
fn the_all_tab_paints_each_line_with_its_source() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    app.config.processes.clear();
    for process in ["api", "web"] {
        app.config
            .processes
            .insert(process.to_string(), crate::config::ProcessConfig::default());
    }
    write_log(&app, "feat+one", "api", &["api up"]);
    write_log(&app, "feat+one", "web", &["web up"]);
    app.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    let _ = draw(&mut app, 100, 20);
    app.handle_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 100, 20));
    assert!(rendered.contains("1:all"), "{rendered}");
    assert!(rendered.contains("api │ api up"), "{rendered}");
    assert!(rendered.contains("web │ web up"), "{rendered}");
}

// ---- dogfood ---------------------------------------------------------

// A question that may be answered "none" offers it, and the footer says
// how.
#[test]
fn a_question_that_allows_none_offers_n_in_its_keys() {
    let mut app = test_app(&["feat+one"]);
    let (tx, _rx) = std::sync::mpsc::channel();
    app.handle_event(crate::tui::app::AppEvent::AskQuestion(Box::new((
        crate::actions::Question {
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
        },
        tx,
    ))));
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(rendered.contains("n none"), "{rendered}");
}

// A port answer is variable names, not a command: the key says which.
#[test]
fn the_port_question_says_to_type_variable_names() {
    let mut app = test_app(&["feat+one"]);
    let (tx, _rx) = std::sync::mpsc::channel();
    app.handle_event(crate::tui::app::AppEvent::AskQuestion(Box::new((
        crate::actions::Question {
            slot: crate::detect::Slot::PortEnv,
            prompt: "Which environment variables carry this project's ports?".to_string(),
            options: vec![("PORT".to_string(), "the Node convention".to_string())],
            preselect: Some(0),
            allow_custom: true,
            allow_none: true,
            multi: false,
            checked: Vec::new(),
            details: Vec::new(),
            answer_file: None,
            snippet: String::new(),
        },
        tx,
    ))));
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(rendered.contains("c type the variable names"), "{rendered}");
    assert!(!rendered.contains("type a command"), "{rendered}");
}

// While a worker waits on the dialog, the footer behind it says so rather
// than offering keys the dialog has taken.
#[test]
fn the_footer_says_a_question_is_waiting() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let (_done, rx) = std::sync::mpsc::channel();
    app.pending = Some(crate::tui::app::PendingAction {
        name: "feat+one".into(),
        kind: crate::tui::app::PendingKind::Start,
        rx,
        started_at: std::time::Instant::now(),
        spinner_frame: 0,
        progress_rx: None,
        stage: None,
        said: Vec::new(),
        label: "feat/one".into(),
    });
    let (tx, _rx) = std::sync::mpsc::channel();
    app.handle_event(crate::tui::app::AppEvent::AskQuestion(Box::new((
        crate::actions::Question {
            slot: crate::detect::Slot::DevCmd,
            prompt: "Which command starts it?".to_string(),
            options: vec![("pnpm dev".to_string(), "package.json".to_string())],
            preselect: Some(0),
            allow_custom: true,
            allow_none: false,
            multi: false,
            checked: Vec::new(),
            details: Vec::new(),
            answer_file: None,
            snippet: String::new(),
        },
        tx,
    ))));
    let rendered = text_of(&draw(&mut app, 120, 30));
    let footer = rendered.lines().last().unwrap_or_default();
    assert!(footer.contains("waiting for your answer"), "{footer}");
    assert!(!footer.contains("logs"), "{footer}");
    assert!(!footer.contains("stop"), "{footer}");
}

// The title describes the source on screen; the worktree's failure, when
// it is another process's, is named apart from it.
#[test]
fn the_viewer_title_describes_the_source_not_the_worktree() {
    let mut app = test_app(&["feat+m"]);
    with_process(&mut app, "feat+m", running_phase());
    with_second_process(
        &mut app,
        "feat+m",
        "worker",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "exit 1".into(),
        },
    );
    let suffix = |source: &str| viewer_title_suffix(&app, "feat+m", source, false, false);
    assert_eq!(suffix("dev"), " (running) · worker failed");
    assert_eq!(suffix("worker"), " (failed)");
    assert_eq!(suffix("all"), " · worker failed");
    assert_eq!(
        viewer_title_suffix(&app, "feat+m", "dev", true, false),
        " (no log yet)"
    );
}

// Not colour alone: the word says it too.
#[test]
fn the_shared_service_chips_say_up_or_down_in_words() {
    let mut app = test_app(&["feat+one"]);
    app.service_health = crate::tui::app::ServiceHealth {
        shared: vec![
            crate::actions::ServiceStatus {
                name: "postgres".into(),
                port: Some(5432),
                up: true,
                logging: false,
                env_file: None,
            },
            crate::actions::ServiceStatus {
                name: "redis".into(),
                port: Some(6379),
                up: false,
                logging: false,
                env_file: None,
            },
        ],
        worktrees: std::collections::BTreeMap::new(),
    };
    let header = text_of(&draw(&mut app, 140, 12))
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(header.contains("postgres ● up"), "{header}");
    assert!(header.contains("redis ● down"), "{header}");
}

// A path longer than the row breaks after a `/`, not mid-name.
#[test]
fn wrap_text_breaks_a_long_path_after_a_slash() {
    let rows = wrap_text(
        "nothing to run: add a [dev] command in /Users/dev/.pando/projects/acme-shop-3f9a2c1d/pando.toml",
        40,
    );
    assert!(rows.iter().all(|r| r.chars().count() <= 40), "{rows:?}");
    assert!(
        rows.iter().any(|r| r.ends_with("pando.toml")),
        "the file name is whole: {rows:?}"
    );
    assert!(
        rows.iter().filter(|r| r.contains('/')).all(|r| {
            let last = r.split(' ').next_back().unwrap_or_default();
            !last.contains('/') || last.ends_with('/') || r.ends_with("pando.toml")
        }),
        "every break in the path is after a slash: {rows:?}"
    );
    // A token with no slash wider than the row is still cut.
    assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
}

// `p` restarting one process does not reset the worktree's uptime.
#[test]
fn the_detail_uptime_counts_from_the_oldest_running_process() {
    let mut app = test_app(&["feat+m"]);
    with_process(
        &mut app,
        "feat+m",
        crate::state::Phase::Running {
            since: chrono::Utc::now() - chrono::Duration::minutes(2),
        },
    );
    with_second_process(&mut app, "feat+m", "worker", running_phase());
    let rendered = text_of(&draw(&mut app, 140, 24));
    let status = rendered
        .lines()
        .find(|l| l.contains("status"))
        .unwrap_or_default();
    // The worker's own row may say it is new; the worktree is not.
    assert!(status.contains("up 2m"), "{rendered}");
}

// A failed worktree keeps its URL (another process may still hold it) but
// does not draw it as a live link.
#[test]
fn a_failed_worktrees_url_is_not_drawn_as_a_live_link() {
    let style_of_url = |phase: crate::state::Phase| {
        let mut app = test_app(&["feat+one"]);
        with_process(&mut app, "feat+one", phase);
        let buffer = draw(&mut app, 140, 20);
        let text = text_of(&buffer);
        let (row, line) = text
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains(" url "))
            .expect("a url row");
        let at = line.find("http://").expect("the url itself");
        let x = line[..at].chars().count() as u16;
        buffer.cell((x, row as u16)).unwrap().style()
    };
    let live = style_of_url(running_phase());
    assert!(
        live.add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
    let failed = style_of_url(crate::state::Phase::Failed {
        at: chrono::Utc::now(),
        reason: "process exited".into(),
    });
    assert!(
        !failed
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED)
    );
}

// Every budget is in terminal cells. Counted in characters, a CJK name
// is given twice the room it has: it runs over the column beside it, and
// whatever passes the pane's edge is never painted.
#[test]
fn the_text_helpers_measure_terminal_cells_not_characters() {
    assert_eq!(text_width("日本語"), 6);
    assert_eq!(text_width("e\u{0301}"), 1);
    let cut = truncate("日本語のブランチ", 7);
    assert_eq!(cut, "日本語…");
    // A glyph wider than the room is not half-painted.
    assert_eq!(truncate("日本", 2), "…");
    let middle = truncate_middle("日本語/のブランチ/名前", 11);
    assert!(text_width(&middle) <= 11, "{middle}");
    assert!(
        middle.starts_with('日') && middle.ends_with('前'),
        "{middle}"
    );
    assert_eq!(text_width(&pad("日本", 6)), 6);
    for row in wrap_text("日本語のエラー 日本語のエラーが起きました", 10) {
        assert!(text_width(&row) <= 10, "{row:?}");
    }
    // A glyph wider than the whole row still makes progress.
    assert_eq!(wrap_text("日本", 1), vec!["日", "本"]);
    let line = truncate_line(Line::from(vec![Span::raw("ab"), Span::raw("日本語")]), 5);
    assert!(line.width() <= 5, "{line:?}");
    for row in chunk_cells("日本語のブランチ", 5) {
        assert!(text_width(&row) <= 5, "{row:?}");
    }
    assert_eq!(chunk_cells("", 5), vec![String::new()]);
    let distinct = truncate_distinct("日本語/とても長いブランチの名前-1", 12, 20);
    assert!(text_width(&distinct) <= 12, "{distinct}");
}

/// The cell column where `needle` starts on row `y`, reading the buffer
/// cell by cell so a wide glyph counts as the two cells it takes.
fn column_of(buf: &Buffer, y: u16, needle: &str) -> Option<u16> {
    let width = buf.area().width;
    (0..width).find(|&x| {
        needle.chars().enumerate().all(|(i, c)| {
            buf.cell((x + i as u16, y))
                .is_some_and(|cell| cell.symbol() == c.to_string())
        })
    })
}

#[test]
fn a_wide_branch_name_keeps_the_list_columns_straight() {
    let mut app = test_app(&["feat+one", "日本語のブランチ名前がとても長い+x"]);
    with_process(&mut app, "feat+one", running_phase());
    with_process(
        &mut app,
        "日本語のブランチ名前がとても長い+x",
        running_phase(),
    );
    for width in [60u16, 100] {
        let buf = draw(&mut app, width, 14);
        // The list's two rows, under the header and the border.
        // The list's two rows, wherever the header and the rules put them.
        let columns: Vec<u16> = (2..buf.area().height)
            .filter_map(|y| column_of(&buf, y, "│ :17342"))
            .collect();
        assert_eq!(columns.len(), 2, "{}", text_of(&buf));
        assert_eq!(columns[0], columns[1], "{}", text_of(&buf));
    }
}

// Wide names everywhere a name is painted — the list, the detail pane,
// the header's error, every modal — at every size, including widths
// narrower than one glyph.
#[test]
fn wide_names_paint_at_any_terminal_size() {
    let names = [
        "日本語のブランチ名前がとても長い+x",
        "feat+🎉🎉🎉-party",
        "e\u{0301}e\u{0301}+z",
    ];
    let (reply, _rx) = std::sync::mpsc::channel();
    let modals = [
        None,
        Some(Modal::Help),
        Some(Modal::Messages),
        Some(Modal::Create {
            input: "日本語".into(),
            branches: BranchLoadState::Ready(vec![crate::worktree::BranchEntry {
                name: "ブランチ".into(),
                source: crate::worktree::BranchSource::Local,
            }]),
            selected: 1,
            base: Some("起動".into()),
        }),
        Some(Modal::PullRequests {
            input: String::new(),
            selected: 0,
        }),
        Some(Modal::Remove {
            name: names[0].into(),
            created_by_pando: false,
        }),
        Some(Modal::StopAll {
            names: names.iter().map(|n| n.to_string()).collect(),
        }),
        Some(Modal::Mode {
            name: names[0].into(),
            selected: 2,
        }),
        Some(Modal::Question {
            question: free_slot_question(&names),
            selected: 0,
            custom: None,
            reply: reply.clone(),
        }),
        Some(Modal::Question {
            question: crate::actions::Question {
                slot: crate::detect::Slot::DevCmd,
                prompt: "どのコマンドで開発サーバーを起動しますか？".into(),
                options: vec![("pnpm 開発".into(), "package.json の scripts".into())],
                preselect: Some(0),
                allow_custom: true,
                allow_none: true,
                multi: false,
                checked: Vec::new(),
                details: vec!["日本語の説明".repeat(5)],
                answer_file: None,
                snippet: String::new(),
            },
            selected: 0,
            custom: None,
            reply,
        }),
    ];
    for modal in modals {
        let mut app = test_app(&names);
        app.main = Some(wt("日本語"));
        app.pr_list = vec![crate::worktree::PrInfo {
            title: "日本語のプルリクエストの題名がとても長い🎉".repeat(3),
            ..a_pr(1, "ブランチ", true, true)
        }];
        with_process(&mut app, names[0], running_phase());
        with_second_process(&mut app, names[0], "起動", running_phase());
        app.set_error(format!("{} 失敗しました", names[0]).repeat(4));
        app.modal = modal;
        for width in [1u16, 2, 3, 4, 5, 7, 10, 15, 21, 33, 41, 60, 71, 72, 90, 300] {
            for height in [1u16, 2, 3, 6, 14, 30] {
                draw(&mut app, width, height);
            }
        }
    }
}

#[test]
fn a_wide_log_line_wraps_without_losing_what_passes_the_edge() {
    let (_dir, mut app) = app_with_logs(&["feat+one"]);
    // Twenty distinct ideographs, forty cells: at a width of twenty-odd
    // columns every one of them has to land on some row.
    let line = "一二三四五六七八九十百千万億兆京垓秭穣溝".to_string();
    write_log(&app, "feat+one", "dev", std::slice::from_ref(&line));
    app.open_log_viewer();
    let buf = draw(&mut app, 24, 12);
    let painted: String = (0..buf.area().height)
        .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
        .filter_map(|at| buf.cell(at).map(|cell| cell.symbol().to_string()))
        .collect();
    for c in line.chars() {
        assert!(painted.contains(c), "{c} was cut off:\n{}", text_of(&buf));
    }
}

// ---- the GitHub account ----------------------------------------------

// Which account `gh` acts as for this project sits right after the
// branch, and says why when there is none.
#[test]
fn the_header_names_the_gh_account_of_this_project() {
    use crate::worktree::GhAccount;
    let header = |account: Option<GhAccount>, width: u16| {
        let mut app = test_app(&["feat+one"]);
        app.gh_account = account;
        let rendered = text_of(&draw(&mut app, width, 12));
        rendered.lines().next().unwrap().to_string()
    };
    let line = header(Some(GhAccount::Login("octocat".into())), 120);
    assert!(line.contains("gh @octocat"), "{line}");
    assert!(
        line.find("@octocat").unwrap() < line.find("worktree").unwrap(),
        "right after the branch: {line}"
    );
    assert!(header(None, 120).contains("gh …"));
    assert!(header(Some(GhAccount::SignedOut), 120).contains("gh not signed in"));
    assert!(header(Some(GhAccount::Missing), 120).contains("gh not installed"));
    assert!(
        header(Some(GhAccount::Unknown("x".into())), 120).contains("gh unknown"),
        "a stderr line is not spliced into the header"
    );
    // A narrow header drops the counts before the account.
    let narrow = header(Some(GhAccount::Login("octocat".into())), 50);
    assert!(narrow.contains("@octocat"), "{narrow}");
}

// ---- the list's grid -------------------------------------------------

#[test]
fn columns_are_divided_and_only_the_header_is_ruled_off() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    with_process(&mut app, "feat+one", running_phase());
    let rendered = text_of(&draw(&mut app, 140, 20));
    let header = list_row(&rendered, "branch");
    assert!(
        header.contains(" │ port"),
        "a line before each title: {header}"
    );
    let one = list_row(&rendered, "feat/one");
    assert!(one.contains(" │ :17342"), "{one}");
    let rules = rendered
        .lines()
        .filter(|line| line.starts_with("│─") && line.contains('┼'))
        .count();
    assert_eq!(rules, 1, "under the header, and nowhere else:\n{rendered}");
    let lines: Vec<&str> = rendered.lines().collect();
    let first = lines
        .iter()
        .position(|l| l.contains("▸ ● feat/one"))
        .unwrap();
    assert!(
        lines[first + 1].contains("○ feat/two"),
        "rows sit together:\n{rendered}"
    );
}

// The cursor gets the accent colour as well as the band, so it is not
// lost on a screen where the band barely shows.
#[test]
fn the_cursor_marker_is_in_the_accent_colour() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    let buf = draw(&mut app, 140, 20);
    assert_eq!(style_at(&buf, "feat/one", "▸").fg, Some(blue()));
}

#[test]
fn the_detail_title_leads_with_the_rows_glyph() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let buf = draw(&mut app, 140, 20);
    let rendered = text_of(&buf);
    assert!(rendered.contains("╭ ● feat/one"), "{rendered}");
    assert_eq!(style_at(&buf, "╭ ● feat/one", "●").fg, Some(green()));
}

// A stop in flight is not `running` in the title either: the title's
// glyph is the row's, action and all.
#[test]
fn the_detail_title_shows_the_action_in_flight_as_the_row_does() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    let (_done, rx) = std::sync::mpsc::channel();
    app.pending = Some(crate::tui::app::PendingAction {
        name: "feat+one".into(),
        kind: crate::tui::app::PendingKind::Stop,
        rx,
        started_at: std::time::Instant::now(),
        spinner_frame: 0,
        progress_rx: None,
        stage: None,
        said: Vec::new(),
        label: "feat/one".into(),
    });
    let buf = draw(&mut app, 140, 20);
    let rendered = text_of(&buf);
    assert!(rendered.contains("╭ ◌ feat/one"), "{rendered}");
    assert_eq!(style_at(&buf, "╭ ◌ feat/one", "◌").fg, Some(yellow()));
    assert_eq!(style_at(&buf, "stopping", "◌").fg, Some(yellow()));
}

// Magenta means isolated, or a merged pull request, and nothing else: a
// shared worktree's branch is painted as a branch is everywhere, and a
// lock is something to look at.
#[test]
fn only_isolation_is_painted_magenta_in_the_detail_pane() {
    let mut app = test_app(&["feat+one"]);
    app.worktrees[0].locked = true;
    let buf = draw(&mut app, 140, 30);
    let branch = style_at(&buf, "│ branch ", "feat/one");
    assert_eq!(branch.fg, Some(crate::theme::text()), "{branch:?}");
    assert_eq!(style_at(&buf, "│ branch ", "locked").fg, Some(yellow()));
    assert_ne!(yellow(), crate::theme::magenta());

    app.modal = Some(Modal::Remove {
        name: "feat+one".into(),
        created_by_pando: true,
    });
    let buf = draw(&mut app, 140, 30);
    assert_eq!(fg_of(&buf, "locked"), Some(yellow()));
}

#[test]
fn the_header_counts_with_the_lists_glyphs() {
    let mut app = test_app(&["up", "broken"]);
    with_process(&mut app, "up", running_phase());
    with_process(
        &mut app,
        "broken",
        crate::state::Phase::Failed {
            at: chrono::Utc::now(),
            reason: "process exited".into(),
        },
    );
    let rendered = text_of(&draw(&mut app, 140, 20));
    let header = rendered.lines().next().unwrap();
    assert!(header.contains("● 1 running"), "{header}");
    assert!(header.contains("✗ 1 failed"), "{header}");
}

#[test]
fn the_selected_row_stays_on_screen_as_the_cursor_moves_past_the_bottom() {
    let names: Vec<String> = (0..12).map(|i| format!("feat+n{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut app = test_app(&refs);
    for _ in 0..11 {
        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    }
    let rendered = text_of(&draw(&mut app, 140, 12));
    assert!(list_row(&rendered, "feat/n11").contains("▸"), "{rendered}");
    assert!(
        !rendered.contains("feat/n00"),
        "scrolled past the top:\n{rendered}"
    );
}

// The highlight is the row's own, and stops at the row under it.
#[test]
fn the_highlight_covers_the_row_and_not_the_one_under_it() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    let buf = draw(&mut app, 140, 20);
    let text = text_of(&buf);
    let y = text
        .lines()
        .position(|line| line.contains("▸ ○ feat/one"))
        .unwrap() as u16;
    let bar = buf.cell((6, y)).unwrap().style().bg;
    let under = buf.cell((6, y + 1)).unwrap().style().bg;
    assert_eq!(bar, Some(highlight_bg()), "{text}");
    assert_ne!(under, Some(highlight_bg()), "{text}");
}

// A login typed into the question: the user is shown, the password after
// its first colon is dots, and the screen never carries it.
#[test]
fn a_login_typed_into_the_question_shows_its_password_as_dots() {
    let (reply, _rx) = std::sync::mpsc::channel();
    let mut app = test_app(&["feat+one"]);
    let paths = app.paths.clone();
    let question =
        crate::actions::login_question(&paths, "mariadb", &["DATABASE_PORT".to_string()]);
    app.modal = Some(Modal::Question {
        question,
        selected: 0,
        custom: Some("root:hunter2".into()),
        reply,
    });
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(!rendered.contains("hunter2"), "{rendered}");
    assert!(rendered.contains("root:•••••••"), "{rendered}");
    assert!(rendered.contains("mariadb"), "{rendered}");
}

// The remove dialog says what goes with the worktree before anything
// does: its own database and its slot, in the destructive colour's words.
#[test]
fn the_remove_dialog_says_which_database_and_slot_go_with_the_worktree() {
    let mut app = test_app(&["feat+one"]);
    with_namespaces(&mut app, "feat+one");
    app.modal = Some(Modal::Remove {
        name: "feat+one".into(),
        created_by_pando: true,
    });
    let rendered = text_of(&draw(&mut app, 140, 30));
    assert!(
        rendered.contains("drops database shop__feat_one, empties redis slot 3 with it"),
        "{rendered}"
    );
}

// A 40 by 10 tmux split: the key line went first, then the tail of what
// removing drops, and the caption was cut at the border — while `y`
// still removed the worktree and dropped its database.
#[test]
fn a_short_remove_dialog_keeps_its_keys_and_what_it_drops() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_namespaces(&mut app, "feat+one");
    app.created_by_pando.insert("feat+one".into(), false);
    app.modal = Some(Modal::Remove {
        name: "feat+one".into(),
        created_by_pando: false,
    });
    let rendered = text_of(&draw(&mut app, 40, 10));
    assert!(rendered.contains("y remove   F force"), "{rendered}");
    assert!(rendered.contains("esc cancel"), "{rendered}");
    assert!(rendered.contains("drops database"), "{rendered}");
    assert!(rendered.contains("redis slot 3 with it"), "{rendered}");

    // Narrow but tall enough: nothing goes, and the caption wraps.
    let rendered = text_of(&draw(&mut app, 50, 30));
    assert!(rendered.contains("its logs and data"), "{rendered}");
    assert!(rendered.contains("are deleted"), "{rendered}");
    assert!(rendered.contains("removing stops it"), "{rendered}");
    assert!(rendered.contains("pando did not create"), "{rendered}");
    assert!(rendered.contains("esc cancel"), "{rendered}");
}

/// Gives `name` a database and a redis slot of its own, which `rm` drops.
fn with_namespaces(app: &mut App, name: &str) {
    let record = app
        .state
        .worktrees
        .entry(name.into())
        .or_insert_with(|| crate::state::WorktreeRecord::new(format!("/abs/{name}"), true));
    for (service, kind, namespace) in [
        (
            "mariadb",
            crate::state::NamespaceKind::Database,
            "shop__feat_one",
        ),
        ("redis", crate::state::NamespaceKind::Slot, "3"),
    ] {
        record.namespaces.push(crate::state::NamespaceRecord {
            service: service.into(),
            recipe: service.into(),
            kind,
            host: "localhost".into(),
            port: 1,
            name: namespace.into(),
            main: "0".into(),
            mains: Vec::new(),
            keys: Vec::new(),
            used_at: chrono::Utc::now(),
        });
    }
}

// ---- namespaced in the TUI ------------------------------------------------------

/// The question a namespaced start asks when every slot is held.
fn free_slot_question(holders: &[&str]) -> crate::actions::Question {
    crate::actions::Question {
        slot: crate::detect::Slot::FreeSlot,
        prompt:
            "Every slot of redis on 127.0.0.1:6379 is held. Which stopped worktree gives up its \
                 slot?"
                .into(),
        options: holders
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (
                    name.to_string(),
                    format!("slot {}, last ran {} h ago", i + 1, i + 2),
                )
            })
            .collect(),
        preselect: None,
        allow_custom: false,
        allow_none: true,
        multi: false,
        checked: Vec::new(),
        details: vec!["the one chosen has its slot emptied".into()],
        answer_file: None,
        snippet: String::new(),
    }
}

/// The foreground of the first cell where `needle` starts, on the first
/// row that has it. For ASCII needles, where a character is a cell.
fn fg_of(buf: &Buffer, needle: &str) -> Option<ratatui::style::Color> {
    let area = buf.area;
    for y in area.top()..area.bottom() {
        let row: String = (area.left()..area.right())
            .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
            .collect();
        if let Some(at) = row.find(needle) {
            let x = area.left() + row[..at].chars().count() as u16;
            return Some(buf[(x, y)].fg);
        }
    }
    None
}

fn with_mode(app: &mut App, name: &str, mode: crate::state::ServiceMode) {
    app.state
        .worktrees
        .entry(name.into())
        .or_insert_with(|| crate::state::WorktreeRecord::new(format!("/abs/{name}"), true))
        .mode = Some(mode);
}

// Decision 6: the chooser offers all three, says which one is
// experimental, and marks the one a stopped worktree last used — or the
// one a running worktree runs in, with what choosing another costs.
#[test]
fn the_mode_chooser_marks_what_it_last_ran_in_or_runs_in() {
    use crate::state::ServiceMode;
    let mut app = test_app(&["feat+one"]);
    with_mode(&mut app, "feat+one", ServiceMode::Namespaced);
    app.modal = Some(Modal::Mode {
        name: "feat+one".into(),
        selected: 1,
    });
    let rendered = text_of(&draw(&mut app, 140, 30));
    for wanted in [
        "shared",
        "namespaced (experimental)",
        "isolated",
        "starts it",
    ] {
        assert!(rendered.contains(wanted), "{wanted}: {rendered}");
    }
    let row = rendered
        .lines()
        .find(|l| l.contains("namespaced (experimental)"))
        .unwrap();
    assert!(row.contains("last used"), "{row}");
    assert!(!row.contains("running"), "{row}");

    with_process(&mut app, "feat+one", running_phase());
    with_mode(&mut app, "feat+one", ServiceMode::Namespaced);
    app.modal = Some(Modal::Mode {
        name: "feat+one".into(),
        selected: 2,
    });
    let rendered = text_of(&draw(&mut app, 140, 30));
    let row = rendered
        .lines()
        .find(|l| l.contains("namespaced (experimental)"))
        .unwrap();
    assert!(row.contains("running"), "{row}");
    assert!(rendered.contains("every process restarts"), "{rendered}");

    // Never started: nothing is labelled, and shared is under the cursor.
    let mut fresh = test_app(&["feat+one"]);
    fresh.modal = Some(Modal::Mode {
        name: "feat+one".into(),
        selected: 0,
    });
    let rendered = text_of(&draw(&mut fresh, 140, 30));
    let rows: Vec<&str> = rendered
        .lines()
        .filter(|l| {
            l.contains("its data") || l.contains("(experimental)") || l.contains("of its own, on")
        })
        .collect();
    assert_eq!(rows.len(), 3, "{rendered}");
    assert!(
        rows.iter()
            .all(|r| !r.contains("last used") && !r.contains("running")),
        "{rows:?}"
    );
}

// A worktree 0.3.0 started shared wrote no mode down. Still running, it
// runs shared, as ⏎ on that row treats it: marked `running`, and choosing
// it keeps it as it is rather than promising a restart that never comes.
#[test]
fn a_running_worktree_with_no_mode_written_runs_shared_in_the_chooser() {
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    assert_eq!(app.state.worktrees["feat+one"].mode, None);
    app.modal = Some(Modal::Mode {
        name: "feat+one".into(),
        selected: 0,
    });
    let rendered = text_of(&draw(&mut app, 140, 30));
    let row = rendered
        .lines()
        .find(|l| l.contains("the main checkout's servers and its data"))
        .unwrap();
    assert!(row.contains("running"), "{row}");
    assert!(rendered.contains("keeps it as it is"), "{rendered}");
    assert!(!rendered.contains("every process restarts"), "{rendered}");
}

// The theme picker says why it shows the half it does: the system, or
// whatever pins it — never the system when the system had no say.
#[test]
fn the_theme_picker_says_what_decided_dark_or_light() {
    use crate::theme::{Appearance, AppearanceOrigin};
    let mut app = test_app(&["feat+one"]);
    for (appearance, origin, wanted) in [
        (
            Appearance::Dark,
            AppearanceOrigin::System,
            "dark half, as the system is — [ui] appearance pins one",
        ),
        (
            Appearance::Light,
            AppearanceOrigin::Config,
            "light half, as [ui] appearance pins it",
        ),
        (
            Appearance::Light,
            AppearanceOrigin::Env,
            "light half, as PANDO_APPEARANCE pins it",
        ),
    ] {
        app.theme.appearance = appearance;
        app.theme.appearance_origin = origin;
        app.modal = Some(Modal::Theme {
            selected: 0,
            before: crate::theme::palette(),
        });
        let rendered = text_of(&draw(&mut app, 160, 40));
        assert!(rendered.contains(wanted), "{wanted}:\n{rendered}");
        if origin != AppearanceOrigin::System {
            assert!(!rendered.contains("as the system is"), "{rendered}");
        }
    }
}

// Decision 11: namespaced has a colour of its own, wherever the word is.
#[test]
fn namespaced_is_painted_in_its_own_colour_in_the_list_and_the_chooser() {
    use crate::state::ServiceMode;
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_mode(&mut app, "feat+one", ServiceMode::Namespaced);
    let buf = draw(&mut app, 180, 20);
    assert_eq!(fg_of(&buf, "namespaced"), Some(crate::theme::namespaced()));
    assert_ne!(crate::theme::namespaced(), crate::theme::magenta());
    app.modal = Some(Modal::Mode {
        name: "feat+one".into(),
        selected: 0,
    });
    let buf = draw(&mut app, 180, 20);
    assert_eq!(
        fg_of(&buf, "namespaced (ex"),
        Some(crate::theme::namespaced())
    );
}

// Decision 9: choosing whose slot to empty is the same list, with its
// action in the destructive colour and nothing to type instead.
#[test]
fn the_free_slot_chooser_names_its_action_in_the_destructive_colour() {
    let (reply, _rx) = std::sync::mpsc::channel();
    let mut app = test_app(&["feat+one"]);
    app.modal = Some(Modal::Question {
        question: free_slot_question(&["feat+old", "feat+older"]),
        selected: 0,
        custom: None,
        reply,
    });
    let buf = draw(&mut app, 140, 30);
    let rendered = text_of(&buf);
    for wanted in [
        "free a slot",
        "feat+old",
        "slot 1, last ran 2 h ago",
        "free none",
    ] {
        assert!(rendered.contains(wanted), "{wanted}: {rendered}");
    }
    assert!(!rendered.contains("type the"), "{rendered}");
    assert_eq!(fg_of(&buf, "empty its slot"), Some(crate::theme::red()));
}

// The detail pane says per service what a namespaced worktree got: its own
// database and slot — and, once it runs in another mode, that they are
// kept until rm.
#[test]
fn the_detail_pane_says_what_each_service_holds_for_the_worktree() {
    use crate::state::ServiceMode;
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_mode(&mut app, "feat+one", ServiceMode::Namespaced);
    let record = app.state.worktrees.get_mut("feat+one").unwrap();
    for (service, kind, name) in [
        (
            "mariadb",
            crate::state::NamespaceKind::Database,
            "shop__feat_one",
        ),
        ("redis", crate::state::NamespaceKind::Slot, "3"),
    ] {
        record.namespaces.push(crate::state::NamespaceRecord {
            service: service.into(),
            recipe: service.into(),
            kind,
            host: "localhost".into(),
            port: 1,
            name: name.into(),
            main: "0".into(),
            mains: Vec::new(),
            keys: Vec::new(),
            used_at: chrono::Utc::now(),
        });
    }
    let rendered = text_of(&draw(&mut app, 200, 40));
    assert!(
        rendered.lines().any(|l| l.contains("mariadb")
            && l.contains("own")
            && l.contains("database shop__feat_one")),
        "{rendered}"
    );
    assert!(
        rendered
            .lines()
            .any(|l| l.contains("redis") && l.contains("slot 3")),
        "{rendered}"
    );

    with_mode(&mut app, "feat+one", ServiceMode::Shared);
    let rendered = text_of(&draw(&mut app, 200, 40));
    assert!(
        rendered
            .lines()
            .any(|l| l.contains("kept") && l.contains("shop__feat_one until rm")),
        "{rendered}"
    );
}

// ---- the setup screen ---------------------------------------------------

use crate::setup::{CheckOutcome, CheckRecord, FailureKind, RanBy, SETUP_PROMPT, SetupState};
use crate::tui::app::tests::app_on_setup_screen;

/// The rows that show `needle`.
fn rows_with<'a>(text: &'a str, needle: &str) -> Vec<&'a str> {
    text.lines().filter(|l| l.contains(needle)).collect()
}

/// The setup screen with a check's finished record behind it.
fn finished(app: &mut App, state: SetupState, outcome: CheckOutcome, ran_by: RanBy) {
    let screen = app.setup_screen.as_mut().unwrap();
    let mut record = CheckRecord::begin(screen.setup.fingerprint.clone(), ran_by);
    record.outcome = outcome;
    record.finished_at = Some(chrono::Utc::now());
    screen.setup.last_check = Some(record);
    screen.setup.state = state;
}

// The screen as the plan draws it: header, the three lines, the prompt
// on a row of its own with no border either side, the steps, the live
// line and the keys.
// A terminal of the classic 80 columns — and a little less — still has
// the prompt whole on one row, so a mouse selection carries no line
// break into the agent.
#[test]
fn the_prompt_is_one_row_on_an_eighty_column_terminal() {
    assert!(SETUP_PROMPT.chars().count() <= 72, "{SETUP_PROMPT}");
    for width in [80, 76] {
        let (_dir, mut app) = app_on_setup_screen(false);
        let text = text_of(&draw(&mut app, width, 30));
        let prompt = rows_with(&text, SETUP_PROMPT);
        assert_eq!(prompt.len(), 1, "{width} columns, one row, whole:\n{text}");
        assert_eq!(prompt[0].trim(), SETUP_PROMPT, "nothing beside it");
    }
}

#[test]
fn the_setup_screen_draws_the_prompt_without_side_borders() {
    let (_dir, mut app) = app_on_setup_screen(false);
    let text = text_of(&draw(&mut app, 120, 30));
    let header = text.lines().next().unwrap();
    assert!(header.contains("pando — acme-shop"), "{header}");
    assert!(header.contains("first time here"), "{header}");
    assert!(text.contains("Let's set pando up for acme-shop."), "{text}");
    assert!(
        text.contains("It never changes a file in your project."),
        "{text}"
    );
    assert!(text.contains("1  Copy this prompt"), "{text}");
    assert!(text.contains(" a  copy"), "{text}");
    let prompt = rows_with(&text, SETUP_PROMPT);
    assert_eq!(prompt.len(), 1, "one row, whole:\n{text}");
    assert_eq!(prompt[0].trim(), SETUP_PROMPT, "nothing beside it");
    assert!(
        text.contains("2  Paste it into Claude Code or Codex"),
        "{text}"
    );
    assert!(text.contains("3  Come back here"), "{text}");
    assert!(text.contains("reading acme-shop…"), "{text}");
    let footer = text.lines().last().unwrap();
    assert!(footer.contains("a copy the prompt"), "{footer}");
    assert!(footer.contains("esc just manage worktrees"), "{footer}");
    assert!(!footer.contains("v test"), "nothing to test yet: {footer}");
    assert!(
        !text.contains("worktrees ("),
        "no dashboard behind it:\n{text}"
    );
}

// Every size a tmux split can be: no panic, and the prompt and the live
// line are the last things to go.
#[test]
fn the_setup_screen_fits_every_size() {
    for (width, height) in [
        (160, 50),
        (120, 30),
        (80, 24),
        (60, 16),
        (40, 12),
        (30, 8),
        (20, 6),
        (12, 4),
        (5, 3),
        (1, 1),
    ] {
        let (_dir, mut app) = app_on_setup_screen(false);
        let text = text_of(&draw(&mut app, width, height));
        if width >= 80 && height >= 12 {
            // Wrapped at 80, whole above it; every word of it either way.
            let shown: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(shown.contains(SETUP_PROMPT), "{width}×{height}:\n{text}");
            assert!(
                text.contains("reading acme-shop"),
                "{width}×{height}:\n{text}"
            );
        }
        if width >= 40 && height >= 12 {
            assert!(text.contains("Set up pando"), "{width}×{height}:\n{text}");
            assert!(text.contains("a copy"), "{width}×{height}:\n{text}");
        }
    }
}

// Short but wide: the explanation gives way before the prompt, the steps
// and the live line do.
#[test]
fn a_short_setup_screen_drops_the_explanation_first() {
    let (_dir, mut app) = app_on_setup_screen(false);
    let text = text_of(&draw(&mut app, 120, 9));
    assert!(text.contains(SETUP_PROMPT), "{text}");
    assert!(text.contains("reading acme-shop"), "{text}");
    assert!(text.contains("1  Copy this prompt"), "{text}");
    assert!(text.contains("3  Come back here"), "{text}");
    assert!(
        !text.contains("Every project is a little different"),
        "{text}"
    );
}

// Once settings exist, `v` is on the screen, so it never waits forever on
// an agent that saved and never tested.
#[test]
fn saved_settings_offer_v() {
    let (_dir, mut app) = app_on_setup_screen(true);
    app.setup_screen.as_mut().unwrap().detected = Some(Vec::new());
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(text.contains("✓ settings saved · v tests them"), "{text}");
    assert!(text.lines().last().unwrap().contains("v test it"), "{text}");
}

#[test]
fn a_failed_check_names_the_reason_and_says_the_agent_is_on_it_only_for_a_program() {
    for (ran_by, on_it) in [(RanBy::Program, true), (RanBy::Tui, false)] {
        let (_dir, mut app) = app_on_setup_screen(true);
        let failed = CheckOutcome::Failed {
            kind: FailureKind::Settings,
            reason: "web exited after 0.8s".into(),
        };
        finished(&mut app, SetupState::Failing, failed, ran_by);
        let text = text_of(&draw(&mut app, 140, 30));
        assert!(
            text.contains(
                "✗ the test failed: web exited after 0.8s · a copies the prompt, which now \
                 includes this"
            ),
            "{text}"
        );
        assert_eq!(
            text.contains("Your agent is probably on it"),
            on_it,
            "{ran_by:?}:\n{text}"
        );
    }
}

// A machine's failure, or a base's, is the developer's even when an agent
// ran the check: the agent stopped, and the reason's command is theirs.
#[test]
fn a_failure_no_setting_fixes_is_the_developers_whoever_ran_the_check() {
    for (kind, says) in [
        (
            FailureKind::Machine,
            "no setting fixes it. Run the command above",
        ),
        (
            FailureKind::Base,
            "which branch work starts from is your call",
        ),
    ] {
        let (_dir, mut app) = app_on_setup_screen(true);
        let failed = CheckOutcome::Failed {
            kind,
            reason: "nothing answers on localhost:5432 — start it first: `brew services start \
                     postgresql`"
                .into(),
        };
        finished(&mut app, SetupState::Failing, failed, RanBy::Program);
        let text = text_of(&draw(&mut app, 160, 30));
        assert!(
            text.contains("`brew services start postgresql`"),
            "the command is shown: {text}"
        );
        assert!(
            text.contains("This one is yours, not your agent's"),
            "{text}"
        );
        assert!(text.contains(says), "{kind:?}:\n{text}");
        assert!(!text.contains("Your agent is probably on it"), "{text}");
        assert!(!text.contains("which now includes this"), "{text}");
    }
}

#[test]
fn an_interrupted_or_unfinished_setup_says_what_to_press() {
    let (_dir, mut app) = app_on_setup_screen(true);
    finished(
        &mut app,
        SetupState::Interrupted,
        CheckOutcome::Running,
        RanBy::Tui,
    );
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(
        text.contains("the last test was interrupted · v tests again"),
        "{text}"
    );

    let open = CheckOutcome::NotSetUp {
        slot: "dev_cmd".into(),
    };
    finished(&mut app, SetupState::Failing, open, RanBy::Program);
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(
        text.contains("not set up: dev_cmd is open · a copies the prompt"),
        "{text}"
    );
}

#[test]
fn a_running_check_shows_its_own_lines() {
    let (_dir, mut app) = app_on_setup_screen(true);
    let screen = app.setup_screen.as_mut().unwrap();
    let mut record = CheckRecord::begin(screen.setup.fingerprint.clone(), RanBy::Program);
    record.progress = vec!["made a test worktree".into(), "installing".into()];
    screen.setup.last_check = Some(record);
    screen.setup.state = SetupState::Testing;
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(
        text.contains("testing: made a test worktree · installing…"),
        "{text}"
    );
    assert!(!text.lines().last().unwrap().contains("v test"), "{text}");
}

// The check passed: the screen turns green by itself, and ⏎ opens pando.
#[test]
fn a_passed_check_turns_the_screen_into_the_ready_view() {
    for (width, height) in [(120, 30), (80, 24), (40, 10), (20, 5), (1, 1)] {
        let (_dir, mut app) = app_on_setup_screen(true);
        finished(
            &mut app,
            SetupState::Ready,
            CheckOutcome::Passed,
            RanBy::Program,
        );
        let text = text_of(&draw(&mut app, width, height));
        if width >= 80 {
            assert!(
                text.contains("✓ You're ready to use pando in acme-shop"),
                "{text}"
            );
            assert!(text.contains("set up and tested just now"), "{text}");
            assert!(!text.contains("first time here"), "{text}");
            assert!(!text.contains(SETUP_PROMPT), "{text}");
            let footer = text.lines().last().unwrap();
            assert!(footer.contains("⏎ open pando"), "{footer}");
        }
    }
}

// What a key said goes where the screen can show it: its own header.
#[test]
fn the_setup_screen_header_carries_the_flash() {
    let (_dir, mut app) = app_on_setup_screen(false);
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    let text = text_of(&draw(&mut app, 120, 30));
    let header = text.lines().next().unwrap();
    assert!(header.contains("✓ copied the setup prompt"), "{header}");
}

// Help on the setup screen is the setup screen's keys, not the list's.
#[test]
fn help_on_the_setup_screen_shows_its_keys() {
    let (_dir, mut app) = app_on_setup_screen(false);
    app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
    let text = text_of(&draw(&mut app, 120, 40));
    assert!(text.contains("copy the setup prompt"), "{text}");
    assert!(text.contains("just manage worktrees"), "{text}");
    assert!(!text.contains("new worktree"), "{text}");
    app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(app.modal.is_none(), "any key closes it");
    assert!(app.setup_screen.is_some(), "and the screen is still there");
}

// The footer names only keys the setup screen's help documents.
#[test]
fn every_setup_footer_hint_is_a_key_help_documents() {
    use crate::tui::app::SETUP_KEYS;
    let keys: Vec<&str> = SETUP_KEYS.iter().flat_map(|k| k.keys.split(' ')).collect();
    for (settings, state) in [
        (false, SetupState::New { skipped: false }),
        (true, SetupState::Untested),
        (true, SetupState::Ready),
    ] {
        let (_dir, mut app) = app_on_setup_screen(settings);
        app.setup_screen.as_mut().unwrap().setup.state = state;
        for (key, _, _) in super::setup::setup_hints(app.setup_screen.as_ref().unwrap()) {
            assert!(
                keys.contains(&key),
                "{key} is not in the setup screen's help"
            );
        }
    }
}

// ---- the ready view and the dashboard's setup line ------------------------

use crate::setup::{ProcessResult, Setup, SetupMemory};

fn process_result(name: &str, port: Option<u16>, http: Option<u16>) -> ProcessResult {
    ProcessResult {
        name: name.into(),
        ready: true,
        port,
        http_status: http,
        secs: 1.5,
    }
}

/// A passed check of `web` and `api`, probed on web, at a commit of main.
fn passed_record(fingerprint: &str, processes: Vec<ProcessResult>) -> CheckRecord {
    let mut record = CheckRecord::begin(fingerprint.to_string(), RanBy::Program);
    record.outcome = CheckOutcome::Passed;
    record.fingerprint_after = Some(fingerprint.to_string());
    record.finished_at = Some(chrono::Utc::now());
    record.pando_version = "0.5.0".into();
    record.commit = Some("a1b2c3d4e5f60718".into());
    record.base_ref = Some("main".into());
    record.processes = processes;
    record
}

/// A project with two apps on one command, an install and a native
/// service: the plan's acme-shop.
fn with_acme_settings(config: &mut crate::config::Config) {
    for name in ["web", "api"] {
        config.processes.insert(
            name.into(),
            crate::config::ProcessConfig {
                cmd: "pnpm dev".into(),
                ..Default::default()
            },
        );
    }
    config.project.install = Some("pnpm install --frozen-lockfile".into());
    config.services.push(crate::config::ServiceConfig::Native {
        name: "mariadb".into(),
        preset: Some("mariadb".into()),
        port_env: None,
        init: None,
        cmd: None,
        ready: None,
        ready_timeout_s: None,
        env: Default::default(),
    });
}

/// The setup screen turned green: the plan's acme-shop, passed.
fn ready_screen(processes: Vec<ProcessResult>, memory: SetupMemory) -> (tempfile::TempDir, App) {
    let (dir, mut app) = app_on_setup_screen(true);
    let screen = app.setup_screen.as_mut().unwrap();
    with_acme_settings(&mut screen.config);
    let fingerprint = crate::setup::fingerprint(&screen.config);
    let record = passed_record(&fingerprint, processes);
    // Whatever pando tried on its own, it tried before this test.
    let memory = SetupMemory {
        tried_by_pando_at: memory
            .tried_by_pando_at
            .map(|_| record.started_at - chrono::Duration::seconds(30)),
        ..memory
    };
    screen.setup = Setup {
        state: SetupState::Ready,
        last_check: Some(record),
        memory,
        fingerprint,
    };
    (dir, app)
}

#[test]
fn the_ready_view_says_what_the_test_proved() {
    let processes = vec![
        process_result("api", Some(20280), None),
        process_result("web", Some(20281), Some(200)),
    ];
    let (_dir, mut app) = ready_screen(processes, SetupMemory::default());
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(
        text.contains("✓ You're ready to use pando in acme-shop"),
        "{text}"
    );
    assert!(
        text.contains("set up and tested just now · tested with pando 0.5.0"),
        "{text}"
    );
    assert!(!text.contains("pando's own guess"), "{text}");
    assert!(text.contains("apps       api + web   pnpm dev"), "{text}");
    assert!(
        text.contains("install    pnpm install --frozen-lockfile"),
        "{text}"
    );
    assert!(
        text.contains("services   mariadb (yours, used as main uses them)"),
        "{text}"
    );
    assert!(
        text.contains("test       ✓ passed   web answered (HTTP 200) · commit a1b2c3d of main"),
        "{text}"
    );
    // A temporary home is a long path, and wraps.
    assert!(text.contains("settings: "), "{text}");
    assert!(text.contains("pando.toml,"), "{text}");
    assert!(text.contains("yours to edit"), "{text}");
    assert!(text.contains("⏎ open pando"), "{text}");
}

// "pando's own guess" only when the passing test followed pando trying
// on its own.
#[test]
fn the_ready_view_says_pandos_own_guess_only_after_pando_tried() {
    let memory = SetupMemory {
        tried_by_pando_at: Some(chrono::Utc::now()),
        ..Default::default()
    };
    let processes = vec![process_result("web", Some(20281), Some(404))];
    let (_dir, mut app) = ready_screen(processes, memory);
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(
        text.contains("set up by pando's own guess and tested just now"),
        "{text}"
    );
    assert!(text.contains("web answered (HTTP 404)"), "{text}");

    // Tried, but the test that passed came before it: somebody else's.
    let (_dir, mut app) = ready_screen(Vec::new(), SetupMemory::default());
    let screen = app.setup_screen.as_mut().unwrap();
    let started = screen.setup.last_check.as_ref().unwrap().started_at;
    screen.setup.memory.tried_by_pando_at = Some(started + chrono::Duration::seconds(5));
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(text.contains("set up and tested just now"), "{text}");
    assert!(!text.contains("pando's own guess"), "{text}");
}

// No app the probe asked: what was ready, and the commit.
#[test]
fn the_ready_view_of_a_portless_app_names_what_was_ready() {
    let processes = vec![process_result("worker", None, None)];
    let (_dir, mut app) = ready_screen(processes, SetupMemory::default());
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(
        text.contains("✓ passed   worker ready · commit a1b2c3d of main"),
        "{text}"
    );
    assert!(!text.contains("HTTP"), "{text}");
}

#[test]
fn the_ready_view_fits_every_size() {
    for (width, height) in [
        (160, 50),
        (120, 30),
        (80, 24),
        (60, 16),
        (40, 12),
        (30, 8),
        (20, 6),
        (12, 4),
        (1, 1),
    ] {
        let processes = vec![process_result("web", Some(20281), Some(200))];
        let (_dir, mut app) = ready_screen(processes, SetupMemory::default());
        let text = text_of(&draw(&mut app, width, height));
        if width >= 40 && height >= 8 {
            assert!(text.contains("You're ready"), "{width}×{height}:\n{text}");
            assert!(text.contains("open pando"), "{width}×{height}:\n{text}");
        }
        if width >= 80 && height >= 16 {
            assert!(text.contains("✓ passed"), "{width}×{height}:\n{text}");
        }
    }
}

/// A dashboard app whose setup reads as `state`, with a check record for
/// the states that have one.
fn dashboard(names: &[&str], state: SetupState, outcome: Option<CheckOutcome>) -> App {
    let mut app = test_app(names);
    with_acme_settings(&mut app.config);
    let fingerprint = crate::setup::fingerprint(&app.config);
    let last_check = outcome.map(|outcome| {
        let mut record = passed_record(&fingerprint, Vec::new());
        record.outcome = outcome;
        record.progress = vec!["made a test worktree".into(), "installing".into()];
        record
    });
    app.setup_row.setup = Some(Setup {
        state,
        last_check,
        memory: SetupMemory::default(),
        fingerprint,
    });
    app
}

/// The header's rows, above the list or the welcome.
fn header_rows(app: &mut App) -> Vec<String> {
    let text = text_of(&draw(app, 140, 30));
    let first_border = text.lines().position(|l| l.contains('╭')).unwrap_or(1);
    text.lines()
        .take(first_border)
        .map(str::to_string)
        .collect()
}

// Each state has one line in the header, with the key that moves it on,
// whether or not there are worktrees; a ready project has none.
#[test]
fn the_header_says_where_the_setup_stands() {
    let failed = CheckOutcome::Failed {
        kind: FailureKind::Settings,
        reason: "web exited after 0.8s".into(),
    };
    let open = CheckOutcome::NotSetUp {
        slot: "dev_cmd".into(),
    };
    let cases: Vec<(SetupState, Option<CheckOutcome>, Option<&str>)> = vec![
        (
            SetupState::Untested,
            None,
            Some("setup: not tested yet · v tests it"),
        ),
        (
            SetupState::Stale,
            Some(CheckOutcome::Passed),
            Some("setup: settings changed since the last test · v tests it"),
        ),
        (
            SetupState::Interrupted,
            Some(CheckOutcome::Running),
            Some("setup: the last test was interrupted · v tests again"),
        ),
        (
            SetupState::Failing,
            Some(failed),
            Some("setup: ✗ the test failed: web exited after 0.8s · a copies the prompt"),
        ),
        (
            SetupState::Failing,
            Some(open),
            Some("setup: not set up: dev_cmd is open · a copies the prompt"),
        ),
        (
            SetupState::New { skipped: true },
            None,
            Some("setup: not set up · a copies the setup prompt"),
        ),
        (
            SetupState::Testing,
            Some(CheckOutcome::Running),
            Some("testing the settings · installing"),
        ),
        (SetupState::Ready, Some(CheckOutcome::Passed), None),
    ];
    for names in [&["feat+one"][..], &[][..]] {
        for (state, outcome, said) in &cases {
            let mut app = dashboard(names, *state, outcome.clone());
            let rows = header_rows(&mut app);
            match said {
                Some(said) => {
                    assert_eq!(rows.len(), 2, "{state:?}: {rows:?}");
                    assert!(rows[1].contains(said), "{state:?} {names:?}: {rows:?}");
                }
                None => {
                    assert_eq!(rows.len(), 1, "{state:?}: {rows:?}");
                    assert!(!rows[0].contains("setup:"), "{rows:?}");
                }
            }
        }
    }
}

// A flash takes the header's first row for its few seconds; the setup
// line keeps the last.
#[test]
fn a_flash_leaves_the_setup_line_where_it_is() {
    let mut app = dashboard(&["feat+one"], SetupState::Untested, None);
    app.set_success("copied something");
    let rows = header_rows(&mut app);
    assert!(rows[0].contains("copied something"), "{rows:?}");
    assert!(rows[1].contains("not tested yet"), "{rows:?}");
}

// A ready project with nothing listed shows what the test proved, never
// "not settled yet".
#[test]
fn a_ready_project_with_no_worktrees_welcomes_with_the_ready_view() {
    let mut app = dashboard(&[], SetupState::Ready, None);
    let fingerprint = crate::setup::fingerprint(&app.config);
    let processes = vec![process_result("web", Some(20281), Some(200))];
    app.setup_row.setup.as_mut().unwrap().last_check = Some(passed_record(&fingerprint, processes));
    let text = text_of(&draw(&mut app, 120, 34));
    assert!(
        text.contains("✓ You're ready to use pando in acme-shop"),
        "{text}"
    );
    assert!(text.contains("web answered (HTTP 200)"), "{text}");
    assert!(text.contains("api + web   pnpm dev"), "{text}");
    assert!(
        text.contains("create a worktree"),
        "the key onward:\n{text}"
    );
    assert!(!text.contains("not settled yet"), "{text}");
    assert!(
        !text.contains("⏎ open pando"),
        "the dashboard is open:\n{text}"
    );

    // Not ready: today's welcome.
    let mut app = dashboard(&[], SetupState::Untested, None);
    let text = text_of(&draw(&mut app, 120, 34));
    assert!(text.contains("one dev environment per branch"), "{text}");
}

// A running `pando check` is live in state, so `X` stops it with the
// rest; its confirmation names it for what it is, never by the
// directory name nobody chose, and counts only the worktrees as such.
#[test]
fn the_stop_all_confirmation_names_a_running_check() {
    let check = crate::paths::CHECK_WORKTREE;
    let mut app = test_app(&["feat+one"]);
    with_process(&mut app, "feat+one", running_phase());
    with_process(&mut app, check, running_phase());
    app.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE));
    assert!(
        matches!(&app.modal, Some(Modal::StopAll { names }) if names.iter().any(|n| n == check)),
        "the check is stopped with the rest"
    );
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(
        rendered.contains("stop the one worktree that is up, and the pando check?"),
        "{rendered}"
    );
    assert!(rendered.contains("the running pando check"), "{rendered}");
    assert!(rendered.contains("feat/one"), "{rendered}");
    assert!(!rendered.contains(check), "{rendered}");

    // The check alone.
    app.modal = Some(Modal::StopAll {
        names: vec![check.to_string()],
    });
    let rendered = text_of(&draw(&mut app, 120, 30));
    assert!(
        rendered.contains("stop the running pando check?"),
        "{rendered}"
    );
    assert!(!rendered.contains(check), "{rendered}");
}

// `⏎` lets pando try on its own only while there is nothing to run; once
// there is, the footer never offers it.
#[test]
fn the_footer_offers_pandos_own_guess_only_with_no_settings() {
    let (_dir, mut app) = app_on_setup_screen(false);
    let text = text_of(&draw(&mut app, 120, 30));
    let footer = text.lines().last().unwrap();
    assert!(footer.contains("⏎ let pando try on its own"), "{footer}");

    let (_dir, mut app) = app_on_setup_screen(true);
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(!text.contains("try on its own"), "{text}");
    finished(
        &mut app,
        SetupState::Ready,
        CheckOutcome::Passed,
        RanBy::Tui,
    );
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(!text.contains("try on its own"), "{text}");
}

// pando's own guess on the live line: while it works it out, and each
// way it stops with nothing written.
#[test]
fn pandos_own_guess_says_where_it_stands() {
    use crate::tui::app::Trying;
    let (_dir, mut app) = app_on_setup_screen(false);
    app.setup_screen.as_mut().unwrap().trying = Some(Trying::Resolving);
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(
        text.contains("pando is trying its own guess for acme-shop…"),
        "{text}"
    );

    app.setup_screen.as_mut().unwrap().trying = Some(Trying::CannotTell);
    let text = text_of(&draw(&mut app, 140, 30));
    assert!(
        text.contains(
            "✗ pando can't tell how acme-shop starts; this one needs your agent · a copies the \
             prompt"
        ),
        "{text}"
    );

    app.setup_screen.as_mut().unwrap().trying = Some(Trying::NeedsPrelude {
        line: "this project asks for node 22 (.nvmrc), and `bash -lc` here resolves 18.20.0".into(),
        fix: Some("set [runtime].prelude to one of:\n  . ~/.nvm/nvm.sh && nvm use".into()),
    });
    let text = text_of(&draw(&mut app, 140, 30));
    assert!(
        text.contains("! this project asks for node 22 (.nvmrc)"),
        "{text}"
    );
    assert!(text.contains(". ~/.nvm/nvm.sh && nvm use"), "{text}");
    assert!(
        text.contains("pando never sets a runtime prelude on its own"),
        "{text}"
    );
    assert!(text.contains("a copies the prompt"), "{text}");
}

// ---- the grove ---------------------------------------------------------

/// The rows the grove takes on a drawn setup screen: from its first row
/// to its caption, which ends it.
fn grove_rows(buf: &Buffer, caption: &str) -> std::ops::Range<u16> {
    let text = text_of(buf);
    let rows: Vec<&str> = text.lines().collect();
    let end = rows
        .iter()
        .position(|row| row.contains(caption))
        .unwrap_or_else(|| panic!("no grove caption {caption:?}:\n{text}"));
    let start = rows[..end]
        .iter()
        .rposition(|row| row.trim().is_empty())
        .map_or(1, |blank| blank + 1);
    start as u16..end as u16
}

/// The colours drawn in `rows`, cell by cell, for the cells that are not
/// blank.
fn colours_in(buf: &Buffer, rows: std::ops::Range<u16>) -> Vec<(char, ratatui::style::Color)> {
    let mut out = Vec::new();
    for y in rows {
        for x in 0..buf.area().width {
            let cell = buf.cell((x, y)).unwrap();
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            if ch != ' ' {
                out.push((ch, cell.fg));
            }
        }
    }
    out
}

const WAITING_CAPTION: &str = "Pando: one aspen, 47,000 stems, one root.";
const ALIVE_CAPTION: &str = "one root, every branch alive";

// The first thing a first run shows is pando's namesake: a grove of
// aspen stems over one root system, drawn in dithered blocks, above the
// prompt — at a classic 80×24 as well as on a tall screen.
#[test]
fn the_setup_screen_opens_on_a_grove_above_the_prompt() {
    for (width, height) in [(120, 42), (80, 30), (80, 24)] {
        let (_dir, mut app) = app_on_setup_screen(false);
        let buf = draw(&mut app, width, height);
        let text = text_of(&buf);
        let grove = grove_rows(&buf, WAITING_CAPTION);
        // Its top row may be all sky, which reads as blank here.
        assert!(
            grove.len() >= crate::art::GROVE_MIN_HEIGHT - 1,
            "{width}×{height}: a grove of {} rows\n{text}",
            grove.len()
        );
        let prompt = text
            .lines()
            .position(|row| row.contains(SETUP_PROMPT))
            .unwrap_or_else(|| panic!("{width}×{height}: no prompt\n{text}"));
        assert!(prompt > grove.end as usize, "{width}×{height}\n{text}");
        // Stems, and the dither between them.
        let drawn: String = colours_in(&buf, grove.clone())
            .iter()
            .map(|(c, _)| *c)
            .collect();
        for shade in ['█', '▓', '▒', '░'] {
            assert!(
                drawn.contains(shade),
                "{width}×{height}: no {shade}\n{text}"
            );
        }
    }
}

// A short screen gives the grove up before anything it is there to say:
// the prompt, the steps, the live line.
#[test]
fn a_short_screen_gives_the_grove_up_first() {
    let (_dir, mut app) = app_on_setup_screen(false);
    let text = text_of(&draw(&mut app, 80, 18));
    assert!(!text.contains(WAITING_CAPTION), "{text}");
    assert!(!text.contains('█'), "{text}");
    assert_eq!(rows_with(&text, SETUP_PROMPT).len(), 1, "{text}");
    assert!(text.contains("reading acme-shop"), "{text}");
}

// The leaves are gold, like Pando's in the fall, and the stems white.
// The root line is dark while pando is being set up; once the check
// passes it lights up green, joining every stem.
#[test]
fn the_roots_light_up_when_the_setup_is_ready() {
    use crate::theme::{green, text, text_dim, yellow};
    let roots = |cells: &[(char, ratatui::style::Color)]| -> Vec<ratatui::style::Color> {
        cells
            .iter()
            .filter(|(ch, _)| "┃┻━╺╸".contains(*ch))
            .map(|(_, fg)| *fg)
            .collect()
    };
    let (_dir, mut app) = app_on_setup_screen(false);
    let buf = draw(&mut app, 100, 40);
    let waiting = colours_in(&buf, grove_rows(&buf, WAITING_CAPTION));
    let colours: Vec<_> = waiting.iter().map(|(_, fg)| *fg).collect();
    assert!(colours.contains(&yellow()), "gold leaves");
    assert!(colours.contains(&text()), "white stems");
    assert!(
        !colours.contains(&green()),
        "nothing green before it is ready"
    );
    let dark = roots(&waiting);
    assert!(!dark.is_empty() && dark.iter().all(|fg| *fg == text_dim()));

    let (_dir, mut app) = ready_screen(
        vec![ProcessResult {
            name: "web".into(),
            ready: true,
            port: Some(20_280),
            http_status: Some(200),
            secs: 2.1,
        }],
        SetupMemory::default(),
    );
    let buf = draw(&mut app, 100, 40);
    let alive = colours_in(&buf, grove_rows(&buf, ALIVE_CAPTION));
    assert!(
        alive.iter().any(|(_, fg)| *fg == yellow()),
        "still gold leaves"
    );
    let lit = roots(&alive);
    assert!(
        !lit.is_empty() && lit.iter().all(|fg| *fg == green()),
        "the roots lit"
    );
}

// Aspens quake: from one tick to the next the leaves move and nothing
// else on the screen does.
#[test]
fn the_grove_quakes_on_the_tick_and_nothing_else_moves() {
    let (_dir, mut app) = app_on_setup_screen(false);
    let before = text_of(&draw(&mut app, 100, 40));
    let grove = {
        let (_dir, mut probe) = app_on_setup_screen(false);
        grove_rows(&draw(&mut probe, 100, 40), WAITING_CAPTION)
    };
    app.tick = app.tick.wrapping_add(3);
    let after = text_of(&draw(&mut app, 100, 40));
    let mut moved = 0;
    for (y, (a, b)) in before.lines().zip(after.lines()).enumerate() {
        if a == b {
            continue;
        }
        let y = y as u16;
        // The spinner turns with the tick too.
        if a.contains("reading acme-shop") {
            continue;
        }
        assert!(
            grove.contains(&y),
            "row {y} moved outside the grove:\n{a}\n{b}"
        );
        moved += 1;
    }
    assert!(moved > 0, "the grove did not quake");
}

// Copying is the one thing the screen asks for, so it looks like it: a
// key cap for `a` beside step one, and the prompt on a panel of its own
// — edges above and below, the panel's colour behind it, and nothing
// either side on its row, so a mouse selection is the prompt alone. Once
// `a` is pressed, the key cap says it worked.
#[test]
fn the_prompt_is_a_card_with_a_key_that_says_when_it_copied() {
    use crate::theme::{green, orange, surface};
    let (_dir, mut app) = app_on_setup_screen(false);
    let buf = draw(&mut app, 100, 40);
    let text = text_of(&buf);
    let rows: Vec<&str> = text.lines().collect();
    let step = rows
        .iter()
        .position(|r| r.contains("1  Copy this prompt"))
        .unwrap();
    let prompt = rows.iter().position(|r| r.contains(SETUP_PROMPT)).unwrap();
    assert_eq!(
        prompt,
        step + 2,
        "the card sits right under step one\n{text}"
    );
    assert!(rows[prompt - 1].trim().chars().all(|c| c == '▄'), "{text}");
    assert!(rows[prompt + 1].trim().chars().all(|c| c == '▀'), "{text}");
    assert_eq!(rows[prompt].trim(), SETUP_PROMPT, "nothing beside it");
    // The panel's colour is behind the prompt, not a character.
    let at = rows[prompt].find("Set up").unwrap() as u16;
    let cell = buf.cell((at, prompt as u16)).unwrap();
    assert_eq!(cell.bg, surface());
    // The key cap: `a` on the accent, right after the words, not off at
    // the far edge.
    assert!(
        rows[step]
            .trim_end()
            .ends_with("Copy this prompt:   a  copy"),
        "{}",
        rows[step]
    );
    let key = rows[step].rfind(" a ").unwrap() as u16 + 1;
    let cap = buf.cell((key, step as u16)).unwrap();
    assert_eq!((cap.symbol(), cap.bg), ("a", orange()), "{}", rows[step]);

    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    let buf = draw(&mut app, 100, 40);
    let text = text_of(&buf);
    let row = text
        .lines()
        .find(|r| r.contains("1  Copy this prompt"))
        .unwrap();
    assert!(
        row.trim_end().ends_with("Copy this prompt:  ✓ copied"),
        "{row}"
    );
    let y = text
        .lines()
        .position(|r| r.contains("1  Copy this prompt"))
        .unwrap() as u16;
    let x = row.find('✓').map(|b| row[..b].chars().count()).unwrap() as u16;
    assert_eq!(buf.cell((x, y)).unwrap().fg, green());
}

// The stems carry branch names, `main` first: the picture's own names,
// never the project's — a real branch there would show a project's work
// in a picture meant for any project.
#[test]
fn the_stems_carry_branch_names_of_the_pictures_own() {
    let (_dir, mut app) = app_on_setup_screen(false);
    let text = text_of(&draw(&mut app, 100, 40));
    let names = text
        .lines()
        .find(|r| r.contains("feat/checkout"))
        .unwrap_or_else(|| panic!("no names under the stems\n{text}"));
    let main = names.find("main").expect("main first");
    assert!(main < names.find("feat/checkout").unwrap(), "{names}");
    // The fixture's own worktree is `feat/one`: not in the picture.
    assert!(!text.contains("feat/one"), "{text}");
    // Each name stands under a stem: the root line has a joint above it.
    let rows: Vec<&str> = text.lines().collect();
    let at = rows.iter().position(|r| *r == names).unwrap();
    let line: Vec<char> = rows[at - 1].chars().collect();
    for name in ["main", "feat/checkout"] {
        let x = names[..names.find(name).unwrap()].chars().count();
        assert_eq!(
            line[x],
            '┻',
            "{name} is not under a stem\n{}\n{names}",
            rows[at - 1]
        );
    }
}

// PANDO, in block letters, over the grove where the screen has room for
// both; a smaller screen gets the compact wordmark, then none.
#[test]
fn the_wordmark_heads_the_screen_where_it_fits() {
    let big = crate::art::to_text(&crate::art::wordmark(crate::art::WordmarkSize::Big, 0));
    let first_row = big.lines().next().unwrap().trim_end().to_string();
    let (_dir, mut app) = app_on_setup_screen(false);
    let text = text_of(&draw(&mut app, 100, 40));
    assert!(text.contains(&first_row), "{text}");
    let at = text.lines().position(|r| r.contains(&first_row)).unwrap();
    let grove = text.lines().position(|r| r.contains('┻')).unwrap();
    assert!(at < grove, "the wordmark heads the grove\n{text}");
    // Centred, with who made it centred under it.
    let row = text.lines().nth(at).unwrap();
    let left = row.find(&first_row).unwrap();
    assert_eq!(left, (100 - crate::art::WORDMARK_WIDTH) / 2, "{row}");
    let credit = crate::art::credit();
    let under = text.lines().nth(at + crate::art::WORDMARK_HEIGHT).unwrap();
    assert_eq!(under.trim(), credit, "{text}");
    let credit_left = under.find(&credit).unwrap();
    assert_eq!(credit_left, (100 - credit.chars().count()) / 2, "{under}");

    let (_dir, mut app) = app_on_setup_screen(false);
    let text = text_of(&draw(&mut app, 60, 40));
    assert!(!text.contains(&first_row), "{text}");
    assert!(text.contains("█▀█ ▄▀█ █▄ █ █▀▄ █▀█"), "{text}");
}

// The main checkout is the first row, by its branch, with its mark and
// the words for it; the list's title and the header count worktrees.
#[test]
fn the_list_draws_the_main_checkout_first_with_its_mark() {
    let mut app = app_with_main(&["feat+one", "feat+two"]);
    with_process(&mut app, "acme-shop", running_phase());
    let text = text_of(&draw(&mut app, 120, 14));
    let rows: Vec<&str> = text
        .lines()
        .filter(|l| l.contains("main ⌂") || l.contains("feat/one") || l.contains("feat/two"))
        .collect();
    assert_eq!(rows.len(), 3, "{text}");
    assert!(rows[0].contains("main ⌂"), "main first: {text}");
    assert!(rows[0].contains("main checkout"), "{text}");
    assert!(rows[1].contains("feat/one"), "{text}");
    assert!(text.contains("worktrees (2) · by PR"), "{text}");
    assert!(text.contains("2 worktrees"), "{text}");
    assert!(
        text.contains("main checkout"),
        "the detail pane says it too: {text}"
    );
}

// And a first run keeps its welcome: a main checkout pando never ran is
// not a worktree to list.
#[test]
fn a_project_with_no_worktree_still_gets_the_welcome() {
    let mut app = app_with_main(&[]);
    let text = text_of(&draw(&mut app, 120, 30));
    assert!(text.contains("welcome"), "{text}");
    assert!(!text.contains("main ⌂"), "{text}");
}

// ---- a development build's label ---------------------------------------

#[test]
fn a_development_build_names_its_branch_in_the_header_and_a_release_does_not() {
    let text = |spans: Vec<Span>| {
        spans
            .iter()
            .map(|s| s.content.to_string())
            .collect::<String>()
    };
    assert_eq!(
        text(build_label_spans(Some("git-menu@b7bb2b4"))),
        "⎇ git-menu@b7bb2b4 · "
    );
    assert!(build_label_spans(None).is_empty());
}

// ---- the git menu ----------------------------------------------------

#[test]
fn the_footer_offers_the_git_menu() {
    let mut app = test_app(&["feat+one"]);
    let rendered = text_of(&draw(&mut app, 200, 12));
    assert!(rendered.contains("space g git"), "{rendered}");
}

// A rebase stopped in a shell: the row says so, and the detail pane says
// what to do about it.
#[test]
fn a_rebase_left_half_done_is_on_its_row_and_in_the_detail_pane() {
    let mut app = test_app(&["feat+one", "feat+two"]);
    app.worktrees[0].in_progress = Some(crate::worktree::InProgress::Rebase);
    let rendered = text_of(&draw(&mut app, 160, 30));
    assert!(
        list_row(&rendered, "feat/one").contains("rebasing"),
        "{rendered}"
    );
    assert!(!list_row(&rendered, "feat/two").contains("rebasing"));
    assert!(
        rendered.contains("a rebase is half-done — space g aborts, ! finishes"),
        "{rendered}"
    );
}

#[test]
fn the_git_menu_shows_where_it_stands_and_every_move() {
    let mut app = test_app(&["feat+one"]);
    crate::tui::app::tests::open_git_menu(
        &mut app,
        "feat+one",
        crate::tui::app::tests::a_git_read(false),
    );
    let rendered = text_of(&draw(&mut app, 140, 40));
    for expected in [
        "git · feat/one",
        "base     ↓10 ↑46 origin/main · never fetched",
        "upstream ↓0 origin/feat/one",
        "tree     clean",
        "f  fetch    bring origin up to date",
        "▸ r  rebase   onto origin/main · replays 46 commits onto 10 new",
        "m  merge    origin/main into it",
        "!  by hand  a shell in it",
        "⏎ or its letter: preview",
    ] {
        assert!(rendered.contains(expected), "{expected:?} in:\n{rendered}");
    }
}

#[test]
fn the_preview_shows_the_exact_commands_and_the_warning() {
    let mut app = test_app(&["feat+one"]);
    crate::tui::app::tests::with_process(&mut app, "feat+one", running_phase());
    crate::tui::app::tests::open_git_menu(
        &mut app,
        "feat+one",
        crate::tui::app::tests::a_git_read(false),
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 140, 40));
    for expected in [
        "rebase feat/one onto origin/main",
        "runs   git fetch origin main",
        "git rebase origin/main",
        "replays 46 commits onto 10 new",
        "on a conflict: git rebase --abort, and nothing changes",
        "! origin/feat/one has 46 of these commits: the next push needs --force-with-lease",
        "it runs: r restarts it after",
        "⏎ rebase",
    ] {
        assert!(rendered.contains(expected), "{expected:?} in:\n{rendered}");
    }
}

// While it runs, the row says what is being done to it, like any action.
#[test]
fn a_running_move_is_the_rows_word() {
    let mut app = test_app(&["feat+one"]);
    crate::tui::app::tests::open_git_menu(
        &mut app,
        "feat+one",
        crate::tui::app::tests::a_git_read(false),
    );
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 160, 40));
    let row = list_row(&rendered, "feat/one");
    assert!(row.contains("◌ feat/one"), "{rendered}");
    assert!(row.contains("rebasing"), "{rendered}");
    assert!(
        rendered.contains("it cannot be stopped halfway"),
        "{rendered}"
    );
}

// After space, the footer says what may follow it, as neovim's which-key
// does, and how to take it back.
#[test]
fn after_space_the_footer_lists_what_may_follow_it() {
    let mut app = test_app(&["feat+one"]);
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let rendered = text_of(&draw(&mut app, 120, 12));
    assert!(
        rendered.contains("space › g git · esc cancel"),
        "{rendered}"
    );
}
