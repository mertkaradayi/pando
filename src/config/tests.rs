use super::schema::glob_match;
use super::validate::normalize;
use super::*;
use crate::paths::PandoPaths;
use crate::project::ProjectRef;
use chrono::Utc;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

struct Fixture {
    _dir: TempDir,
    root: PathBuf,
    paths: PandoPaths,
}

fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("acme-shop");
    std::fs::create_dir_all(&root).unwrap();
    let project = ProjectRef::from_root(&root).unwrap();
    let paths = PandoPaths::new(dir.path().join("pando-home"), project);
    Fixture {
        root: paths.root().to_path_buf(),
        paths,
        _dir: dir,
    }
}

fn write_committed(f: &Fixture, text: &str) {
    std::fs::write(f.root.join("pando.toml"), text).unwrap();
}

fn write_home(f: &Fixture, text: &str) {
    std::fs::create_dir_all(f.paths.project_dir()).unwrap();
    std::fs::write(f.paths.config_file(), text).unwrap();
}

/// The machine-wide layer: one file for every project on this laptop.
fn write_user(f: &Fixture, text: &str) {
    std::fs::create_dir_all(&f.paths.home).unwrap();
    std::fs::write(f.paths.user_config_file(), text).unwrap();
}

fn home_text(f: &Fixture) -> String {
    std::fs::read_to_string(f.paths.config_file()).unwrap()
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Every temp file a write left beside the config.
fn temp_files(f: &Fixture) -> Vec<String> {
    std::fs::read_dir(f.paths.project_dir())
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect()
}

/// A file a developer wrote by hand: comments above and beside keys,
/// tables out of alphabetical order, and a key pando is about to change.
const HANDWRITTEN: &str = r#"# my project
# two comment lines

[share]
provider = "cloudflared"

[project]
# the base everything forks from
base = "trunk"
install = "npm ci"   # frozen on purpose
provision = [".env"]

[runtime]
prelude = "nvm use"
"#;

#[test]
fn a_patch_leaves_every_line_it_did_not_touch_byte_for_byte() {
    let f = fixture();
    write_home(&f, HANDWRITTEN);
    set_detected(
        &f.paths,
        Layer::Project,
        &["project"],
        "install",
        "pnpm install --frozen-lockfile",
        Note::Detected("pnpm-lock.yaml".into()),
    )
    .unwrap();

    let after = home_text(&f);
    let before_lines: Vec<&str> = HANDWRITTEN.lines().collect();
    let after_lines: Vec<&str> = after.lines().collect();
    assert_eq!(
        before_lines.len(),
        after_lines.len(),
        "a patch must not add or remove lines:\n{after}"
    );
    for (before, after) in before_lines.iter().zip(&after_lines) {
        if before.starts_with("install") {
            assert_eq!(
                *after, "install = \"pnpm install --frozen-lockfile\"  # detected: pnpm-lock.yaml",
                "the patched line carries the new value and its note"
            );
        } else {
            assert_eq!(before, after, "an untouched line changed");
        }
    }
    // And it is still the config pando reads back.
    let loaded = load(&f.paths).unwrap();
    assert_eq!(
        loaded.config.project.install.as_deref(),
        Some("pnpm install --frozen-lockfile")
    );
    assert_eq!(loaded.config.project.base.as_deref(), Some("trunk"));
    assert_eq!(loaded.config.runtime.prelude.as_deref(), Some("nvm use"));
}

#[test]
fn a_new_key_lands_in_its_table_without_disturbing_the_others() {
    let f = fixture();
    write_home(&f, HANDWRITTEN);
    set_detected(
        &f.paths,
        Layer::Project,
        &["runtime"],
        "version_files",
        toml_edit::Array::from_iter([".nvmrc"]),
        Note::Detected(".nvmrc".into()),
    )
    .unwrap();

    let after = home_text(&f);
    assert!(
        after.contains("version_files = [\".nvmrc\"]  # detected: .nvmrc"),
        "{after}"
    );
    assert!(after.contains("prelude = \"nvm use\""), "{after}");
    assert!(
        after.contains("# the base everything forks from"),
        "{after}"
    );
    assert!(after.starts_with("# my project\n"), "{after}");
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.runtime.version_files, vec![".nvmrc"]);
}

#[test]
fn a_missing_table_is_created_and_the_file_gets_a_header() {
    let f = fixture();
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev",
        Note::Detected("package.json scripts.dev".into()),
    )
    .unwrap();

    let after = home_text(&f);
    assert!(after.starts_with("# pando.toml"), "{after}");
    assert!(
        after.contains("[dev]\n"),
        "the table is not inline: {after}"
    );
    assert!(
        after.contains("cmd = \"pnpm dev\"  # detected: package.json scripts.dev"),
        "{after}"
    );
    let loaded = load(&f.paths).unwrap();
    assert_eq!(
        loaded.config.processes["dev"].cmd, "pnpm dev",
        "[dev] normalises into processes.dev"
    );
}

#[test]
fn an_answered_note_records_the_date() {
    let f = fixture();
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev:web",
        Note::Answered,
    )
    .unwrap();
    let after = home_text(&f);
    let today = Utc::now().format("%Y-%m-%d").to_string();
    assert!(after.contains(&format!("# answered: {today}")), "{after}");
}

/// agent/json.md lists the forms a provenance note takes and calls any
/// other comment the developer's own, so a form missing from the list
/// reads to a program as a person's decision: the `--yes` answer to the
/// services question was missing from it. Each note is written the way
/// pando writes it and looked for in the document with its variable parts
/// spelled as the document spells them. The match is exhaustive: a new
/// note does not compile here until the document says what it looks like.
#[test]
fn every_provenance_note_pando_writes_is_a_form_agent_json_documents() {
    let doc = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("agent/json.md"))
        .expect("read agent/json.md");
    // Hard-wrapped, so a form can straddle lines.
    let doc = doc.split_whitespace().collect::<Vec<_>>().join(" ");
    for note in [
        Note::Detected("<evidence>".into()),
        Note::Answered,
        Note::TookFirst(7),
        Note::TookFirst(1),
        Note::TookRuled {
            taken: 3,
            offered: 5,
        },
        Note::Program,
    ] {
        let f = fixture();
        set_detected(
            &f.paths,
            Layer::Project,
            &["dev"],
            "cmd",
            "pnpm dev",
            note.clone(),
        )
        .unwrap();
        let text = home_text(&f);
        let written = text
            .lines()
            .find_map(|line| line.strip_prefix("cmd = \"pnpm dev\"  "))
            .unwrap_or_else(|| panic!("no note beside the key: {text}"));
        let form = match note {
            Note::Detected(_) => written.to_string(),
            Note::Answered | Note::Program => {
                let (head, date) = written.split_at(written.len() - "YYYY-MM-DD".len());
                assert!(
                    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_ok(),
                    "{written}"
                );
                format!("{head}<date>")
            }
            Note::TookFirst(_) => written.replace('7', "N"),
            Note::TookRuled { .. } => written.replace('3', "<taken>").replace('5', "<offered>"),
        };
        assert!(
            doc.contains(&format!("`{form}`")),
            "agent/json.md does not document the note {form:?}"
        );
    }
}

#[test]
fn patching_the_same_value_twice_replaces_the_note_and_not_the_file() {
    let f = fixture();
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "a",
        Note::Detected("one".into()),
    )
    .unwrap();
    let first = home_text(&f);
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "b",
        Note::Answered,
    )
    .unwrap();
    let second = home_text(&f);
    assert!(first.contains("cmd = \"a\"  # detected: one"));
    assert!(second.contains("cmd = \"b\"  # answered:"), "{second}");
    assert!(
        !second.contains("detected: one"),
        "the stale note must go with the stale value: {second}"
    );
}

#[test]
fn the_config_pando_writes_is_private() {
    let f = fixture();
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev",
        Note::Answered,
    )
    .unwrap();
    assert_eq!(mode_of(&f.paths.config_file()), 0o600);
    // Writing again over an existing file keeps it that way.
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cwd",
        "apps/web",
        Note::Answered,
    )
    .unwrap();
    assert_eq!(mode_of(&f.paths.config_file()), 0o600);
    assert_eq!(
        temp_files(&f),
        Vec::<String>::new(),
        "the temp file must be renamed away"
    );
}

// Answers are resolved before any state lock is taken, so two starts in
// a fresh project patch one file at once. With one temp name for every
// writer, one of them failed on a config nothing was wrong with; with no
// lock around the read and the write, one of two answers was lost.
#[test]
fn patches_made_at_the_same_time_all_land() {
    let f = fixture();
    let (writers, answers) = (8, 10);
    std::thread::scope(|scope| {
        for writer in 0..writers {
            let paths = &f.paths;
            scope.spawn(move || {
                for answer in 0..answers {
                    patch(paths, Layer::Project, |doc| {
                        let table = doc
                            .entry("answers")
                            .or_insert_with(toml_edit::table)
                            .as_table_mut()
                            .unwrap();
                        table.insert(&format!("w{writer}a{answer}"), toml_edit::value(true));
                        Ok(())
                    })
                    .unwrap();
                }
            });
        }
    });
    let text = home_text(&f);
    for writer in 0..writers {
        for answer in 0..answers {
            assert!(
                text.contains(&format!("w{writer}a{answer} = true")),
                "w{writer}a{answer} was lost: {text}"
            );
        }
    }
    assert_eq!(temp_files(&f), Vec::<String>::new());
    assert_eq!(mode_of(&f.paths.config_file()), 0o600);
}

// The no-op path compares the rendered document with the file's own
// bytes, so anything `toml_edit` normalises on the way through would
// make an empty patch rewrite the file. These are the shapes that
// normalisation would show up in.
#[test]
fn an_empty_patch_rewrites_nothing_whatever_the_file_looks_like() {
    for (label, original) in [
        ("no trailing newline", "[project]\nbase = \"main\""),
        ("crlf line endings", "[project]\r\nbase = \"main\"\r\n"),
        (
            "blank lines and indentation",
            "\n\n[project]\n  base = \"main\"\n\n\n",
        ),
        ("comments only", "# nothing but a comment\n"),
        ("an empty file", ""),
    ] {
        let f = fixture();
        write_home(&f, original);
        patch(&f.paths, Layer::Project, |_doc| Ok(())).unwrap();
        assert_eq!(home_text(&f), original, "{label}");
    }
}

#[test]
fn patching_never_touches_a_committed_pando_toml() {
    let f = fixture();
    let committed = "[project]\nbase = \"main\"\n";
    write_committed(&f, committed);
    set_detected(
        &f.paths,
        Layer::Project,
        &["project"],
        "install",
        "pnpm install --frozen-lockfile",
        Note::Detected("pnpm-lock.yaml".into()),
    )
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(f.root.join("pando.toml")).unwrap(),
        committed,
        "the repository's own file is read-only to pando"
    );
    assert!(f.paths.config_file().starts_with(&f.paths.home));
}

// A dotfiles manager links `~/.pando/config.toml` to a file of its own.
// The first theme picked renamed a regular file over the link, and from
// then on the dotfile the developer edited was not the one pando read.
#[test]
fn a_config_that_is_a_link_is_written_through_and_stays_a_link() {
    let f = fixture();
    let dotfiles = f._dir.path().join("dotfiles");
    std::fs::create_dir_all(&dotfiles).unwrap();
    let target = dotfiles.join("pando.toml");
    std::fs::write(&target, "[runtime]\nprelude = \"nvm use\"\n").unwrap();
    f.paths.ensure_home().unwrap();
    let link = f.paths.user_config_file();
    std::os::unix::fs::symlink(&target, &link).unwrap();

    set_detected(
        &f.paths,
        Layer::User,
        &["ui"],
        "theme",
        "nord",
        Note::Answered,
    )
    .unwrap();

    let meta = std::fs::symlink_metadata(&link).unwrap();
    assert!(meta.file_type().is_symlink(), "the link is still a link");
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
    let written = std::fs::read_to_string(&target).unwrap();
    assert!(written.contains("prelude = \"nvm use\""), "{written}");
    assert!(written.contains("theme = \"nord\""), "{written}");
    let leftovers: Vec<_> = std::fs::read_dir(&dotfiles)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name != "pando.toml")
        .collect();
    assert_eq!(
        leftovers,
        Vec::<String>::new(),
        "no temp file or lock left there"
    );
}

// A link is followed only to a file pando may write: never into the
// repository, and never to a file that is not there.
#[test]
fn a_config_linked_into_the_repository_or_nowhere_is_never_written() {
    let f = fixture();
    let committed = "[project]\nbase = \"main\"\n";
    write_committed(&f, committed);
    std::fs::create_dir_all(f.paths.project_dir()).unwrap();
    let link = f.paths.config_file();
    std::os::unix::fs::symlink(f.root.join("pando.toml"), &link).unwrap();
    let write = || {
        set_detected(
            &f.paths,
            Layer::Project,
            &["dev"],
            "cmd",
            "pnpm dev",
            Note::Answered,
        )
    };
    let e = format!("{:#}", write().unwrap_err());
    assert!(e.contains("is inside the repository"), "{e}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("pando.toml")).unwrap(),
        committed
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );

    let nowhere = f._dir.path().join("gone.toml");
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&nowhere, &link).unwrap();
    let e = format!("{:#}", write().unwrap_err());
    assert!(e.contains("which is not there"), "{e}");
    assert!(!nowhere.exists());
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_home_file_that_is_not_valid_toml_is_never_overwritten() {
    let f = fixture();
    let broken = "[project\nbase = \"main\"\n";
    write_home(&f, broken);
    let err = set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "x",
        Note::Answered,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("not valid TOML"), "{msg}");
    assert_eq!(
        home_text(&f),
        broken,
        "a file pando cannot parse is a file someone is editing"
    );
}

// `[dev]` is shorthand for `[processes.dev]` and the two forms may not
// both be in one file, so a document that already has a `[processes]`
// table gets the long form — otherwise pando writes a file it then
// refuses to read.
#[test]
fn a_dev_key_takes_the_long_form_when_the_file_already_has_processes() {
    let f = fixture();
    write_home(&f, "[processes.dev]\ncwd = \"apps/web\"\n");
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev",
        Note::Detected("package.json scripts.dev".into()),
    )
    .unwrap();

    let text = home_text(&f);
    assert!(
        !text.contains("[dev]"),
        "the shorthand beside [processes] is a file pando cannot load: {text}"
    );
    assert!(text.contains("[processes.dev]"), "{text}");
    assert!(text.contains("cmd = \"pnpm dev\""), "{text}");
    let loaded = load(&f.paths).expect("the file pando wrote must load");
    assert_eq!(loaded.config.processes["dev"].cmd, "pnpm dev");
    assert_eq!(
        loaded.config.processes["dev"].cwd.as_deref(),
        Some("apps/web"),
        "and what was already there is untouched"
    );
}

// Phase 2b review, finding 3. The conflict `normalize` refuses is
// between the *merged* layers, so a `[processes]` table in the file the
// team committed was invisible to the redirect above — and a `[dev]`
// written beside it left `start`, `new`, `restart` and the TUI refusing
// to run until a human edited pando's own file.
#[test]
fn a_dev_key_takes_the_long_form_when_the_committed_layer_has_processes() {
    let f = fixture();
    write_committed(&f, "[processes.dev]\ncwd = \"apps/web\"\n");
    write_home(&f, "[project]\ninstall = \"true\"\n");
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev",
        Note::Detected("package.json scripts.dev".into()),
    )
    .unwrap();

    let text = home_text(&f);
    assert!(
        !text.contains("[dev]"),
        "[dev] here and [processes.dev] there cannot both apply: {text}"
    );
    assert!(text.contains("[processes.dev]"), "{text}");
    let loaded = load(&f.paths).expect("the file pando wrote must load");
    assert_eq!(loaded.config.processes["dev"].cmd, "pnpm dev");
    assert_eq!(
        loaded.config.processes["dev"].cwd.as_deref(),
        Some("apps/web"),
        "the committed layer's own key still applies"
    );
}

// One question, one note. Ten identical `# answered:` lines for a
// two-app workspace say the same thing ten times, and the bare
// `[processes]` header above them is a line no human would write.
// And the same for the machine-wide layer: `[dev]` written beside a
// `[processes]` table in *any* other layer is a merged config pando's
// own loader refuses.
#[test]
fn a_dev_key_takes_the_long_form_when_the_user_layer_has_processes() {
    let f = fixture();
    write_user(&f, "[processes.dev]\nenv = { TZ = \"UTC\" }\n");
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev",
        Note::Detected("package.json scripts.dev".into()),
    )
    .unwrap();

    let text = home_text(&f);
    assert!(
        !text.contains("[dev]"),
        "[dev] here and [processes.dev] there cannot both apply: {text}"
    );
    assert!(text.contains("[processes.dev]"), "{text}");
    let loaded = load(&f.paths).expect("the file pando wrote must load");
    assert_eq!(loaded.config.processes["dev"].cmd, "pnpm dev");
    assert_eq!(
        loaded.config.processes["dev"]
            .env
            .get("TZ")
            .map(String::as_str),
        Some("UTC"),
        "the user layer's own key still applies"
    );
}

#[test]
fn a_whole_table_carries_one_note_on_its_header_and_no_bare_parent() {
    let f = fixture();
    set_detected_table(
        &f.paths,
        Layer::Project,
        &["processes", "web"],
        vec![
            ("cmd".to_string(), "pnpm dev".into()),
            ("cwd".to_string(), "apps/web".into()),
        ],
        Note::TookFirst(2),
    )
    .unwrap();

    let text = home_text(&f);
    assert!(
        !text.lines().any(|l| l.trim() == "[processes]"),
        "the intermediate table is implicit: {text}"
    );
    // The file's own header mentions the marker, so only lines that
    // are not themselves comments count.
    assert_eq!(
        text.lines()
            .filter(|line| !line.starts_with('#') && line.contains("# answered:"))
            .count(),
        1,
        "one note for the whole table: {text}"
    );
    assert!(
        text.lines()
            .any(|l| l.starts_with("[processes.web]") && l.contains("# answered:")),
        "and it is on the header: {text}"
    );
    let loaded = load(&f.paths).expect("the file pando wrote must load");
    assert_eq!(loaded.config.processes["web"].cmd, "pnpm dev");
}

#[test]
fn a_key_whose_table_is_a_scalar_is_refused_by_name() {
    let f = fixture();
    write_home(&f, "dev = 3\n");
    let err = set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "x",
        Note::Answered,
    )
    .unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("[dev]"), "{msg}");
    assert_eq!(home_text(&f), "dev = 3\n");
}

// `dev = { cwd = "apps/web" }` is the shape detection fills in, written
// inline. It was refused as "not a table", and since nothing was written
// every start stopped on the same refusal.
#[test]
fn a_process_written_as_an_inline_table_is_filled_in_like_any_other() {
    for (written, header) in [
        (
            "[processes]\ndev = { cwd = \"apps/web\" }\n",
            "[processes.dev]",
        ),
        (
            "processes = { dev = { cwd = \"apps/web\" } }\n",
            "[processes.dev]",
        ),
        (
            "processes.dev = { cwd = \"apps/web\" }\n",
            "[processes.dev]",
        ),
        ("dev = { cwd = \"apps/web\" }\n", "[dev]"),
    ] {
        let f = fixture();
        write_home(&f, written);
        set_detected(
            &f.paths,
            Layer::Project,
            &["dev"],
            "cmd",
            "pnpm dev",
            Note::Detected("package.json scripts.dev".into()),
        )
        .unwrap();

        let text = home_text(&f);
        assert!(
            text.lines()
                .any(|l| l.starts_with("cmd = \"pnpm dev\"") && l.contains("# detected:")),
            "{text}"
        );
        assert!(text.contains(header), "{text}");
        let loaded = load(&f.paths).expect("the file pando wrote must load");
        assert_eq!(loaded.config.processes["dev"].cmd, "pnpm dev", "{text}");
        assert_eq!(
            loaded.config.processes["dev"].cwd.as_deref(),
            Some("apps/web"),
            "{text}"
        );
    }
}

#[test]
fn an_inline_table_that_only_held_the_next_leaves_no_bare_header() {
    let f = fixture();
    write_home(&f, "processes = { dev = { cwd = \"apps/web\" } }\n");
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev",
        Note::Answered,
    )
    .unwrap();
    let text = home_text(&f);
    assert!(!text.lines().any(|l| l.trim() == "[processes]"), "{text}");
}

#[test]
fn an_empty_inline_array_of_tables_takes_an_entry_like_any_other() {
    let f = fixture();
    write_home(&f, "hooks = []\n");
    set_detected_array_entry(
        &f.paths,
        Layer::Project,
        "hooks",
        vec![
            ("name".to_string(), "migrate".into()),
            ("after".to_string(), "services".into()),
            ("cmd".to_string(), "true".into()),
        ],
        Note::Answered,
    )
    .unwrap();
    let text = home_text(&f);
    assert!(text.contains("[[hooks]]"), "{text}");
    let loaded = load(&f.paths).expect("the file pando wrote must load");
    assert_eq!(loaded.config.hooks[0].name, "migrate", "{text}");
}

/// Every comment in `written` is in `text` once, above `header`. None of
/// the files these tests write has a `#` in a string.
fn assert_comments_kept_above(written: &str, text: &str, header: &str) {
    let header = text
        .find(header)
        .unwrap_or_else(|| panic!("no {header}: {text}"));
    for line in written.lines() {
        let Some(at) = line.find('#') else { continue };
        let comment = &line[at..];
        assert_eq!(text.matches(comment).count(), 1, "{comment}: {text}");
        assert!(text.find(comment).unwrap() < header, "{comment}: {text}");
    }
}

// The lines above `dev = { ... }` are its key's and the comment beside it
// is its value's, and writing the table out dropped both: the developer's
// comments, and on a file that opens with the table, pando's own header.
#[test]
fn a_table_written_out_keeps_the_comments_on_its_line_above_its_header() {
    for (written, header) in [
        (
            "[processes]\n# the web app, not the docs site\n\
             dev = { cwd = \"apps/web\" }  # keep in sync with turbo.json\n",
            "[processes.dev]",
        ),
        (
            "# the web app, not the docs site\n\
             processes = { dev = { cwd = \"apps/web\" } }  # keep in sync with turbo.json\n",
            "[processes.dev]",
        ),
        (
            "# the web app, not the docs site\n\
             processes.dev = { cwd = \"apps/web\" }  # keep in sync with turbo.json\n",
            "[processes.dev]",
        ),
        (
            "# pando.toml, and everything here is yours to edit.\n\n\
             # the web app, not the docs site\n\
             dev = { cwd = \"apps/web\" }  # keep in sync with turbo.json\n",
            "[dev]",
        ),
        (
            "dev = {\n\
             \x20 # the web app, not the docs site\n\
             \x20 cwd = \"apps/web\",  # keep in sync with turbo.json\n\
             }\n",
            "[dev]",
        ),
    ] {
        let f = fixture();
        write_home(&f, written);
        set_detected(
            &f.paths,
            Layer::Project,
            &["dev"],
            "cmd",
            "pnpm dev",
            Note::Answered,
        )
        .unwrap();
        let text = home_text(&f);
        assert_comments_kept_above(written, &text, header);
        let loaded = load(&f.paths).expect("the file pando wrote must load");
        assert_eq!(loaded.config.processes["dev"].cmd, "pnpm dev", "{text}");
    }
}

// A level inline only to hold the next has no header of its own left, and
// one that still holds a value of its own does.
#[test]
fn an_inline_level_s_comments_go_above_the_first_header_the_rewrite_leaves() {
    let f = fixture();
    let written = "# both apps\n\
                   processes = { dev = { cwd = \"apps/web\" }, docs = { cmd = \"x\" } }  # beside\n";
    write_home(&f, written);
    set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "pnpm dev",
        Note::Answered,
    )
    .unwrap();
    let text = home_text(&f);
    assert_comments_kept_above(written, &text, "[processes]");
    assert!(text.contains("[processes.dev]"), "{text}");
}

// `hooks = []` becomes an array of tables whose first header is the entry
// pando appends; a list of inline tables keeps the comments written
// between its entries.
#[test]
fn an_array_written_out_keeps_the_comments_on_its_line_above_its_first_entry() {
    for written in [
        "# run in order\nhooks = []  # none yet\n",
        "# run in order\n\
         hooks = [\n\
         \x20 # the schema first\n\
         \x20 { name = \"schema\", after = \"services\", cmd = \"true\" },\n\
         ]  # none yet\n",
    ] {
        let f = fixture();
        write_home(&f, written);
        set_detected_array_entry(
            &f.paths,
            Layer::Project,
            "hooks",
            vec![
                ("name".to_string(), "migrate".into()),
                ("after".to_string(), "services".into()),
                ("cmd".to_string(), "true".into()),
            ],
            Note::Answered,
        )
        .unwrap();
        let text = home_text(&f);
        assert_comments_kept_above(written, &text, "[[hooks]]");
        let loaded = load(&f.paths).expect("the file pando wrote must load");
        assert_eq!(
            loaded.config.hooks.last().unwrap().name,
            "migrate",
            "{text}"
        );
    }
}

#[test]
fn a_patch_that_changes_nothing_leaves_the_file_alone() {
    let f = fixture();
    write_home(&f, HANDWRITTEN);
    let before = std::fs::metadata(f.paths.config_file())
        .unwrap()
        .modified()
        .unwrap();
    patch(&f.paths, Layer::Project, |_doc| Ok(())).unwrap();
    assert_eq!(home_text(&f), HANDWRITTEN);
    assert_eq!(
        std::fs::metadata(f.paths.config_file())
            .unwrap()
            .modified()
            .unwrap(),
        before,
        "an empty patch must not rewrite the file"
    );
}

#[test]
fn defaults_load_when_no_file_exists() {
    let f = fixture();
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config, Config::default());
    assert!(loaded.warnings.is_empty());
    assert_eq!(
        loaded.config.worktrees_dir(&f.paths),
        f.paths.worktrees_dir()
    );
}

// `pando check` makes its worktree beside the real ones, under a name no
// branch's directory can have: git refuses a ref component that starts
// with a dot.
#[test]
fn the_check_worktree_sits_beside_the_real_ones_under_a_name_no_branch_has() {
    let f = fixture();
    let config = load(&f.paths).unwrap().config;
    let path = config.check_worktree_path(&f.paths);
    assert_eq!(
        path.parent(),
        Some(config.worktrees_dir(&f.paths).as_path())
    );
    assert_eq!(path.file_name().unwrap(), crate::paths::CHECK_WORKTREE);
    let refused = std::process::Command::new("git")
        .args(["check-ref-format", "--branch", crate::paths::CHECK_WORKTREE])
        .output()
        .unwrap();
    assert!(
        !refused.status.success(),
        "git took {CHECK:?} as a branch",
        CHECK = crate::paths::CHECK_WORKTREE
    );
}

#[test]
fn the_pando_home_layer_overrides_the_committed_layer() {
    let f = fixture();
    write_committed(
        &f,
        "[project]\nbase = \"main\"\ninstall = \"pnpm install --frozen-lockfile\"\n",
    );
    write_home(&f, "[project]\nbase = \"develop\"\n");

    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.project.base.as_deref(), Some("develop"));
    assert_eq!(
        loaded.config.project.install.as_deref(),
        Some("pnpm install --frozen-lockfile"),
        "keys the home layer does not mention survive the merge"
    );
}

// Committed < user < project. The middle layer is the machine's: it
// beats what the repository ships and loses to what pando decided for
// this project.
#[test]
fn the_user_layer_sits_between_the_committed_and_project_layers() {
    let f = fixture();
    write_committed(
        &f,
        "[project]\nbase = \"main\"\ninstall = \"pnpm install --frozen-lockfile\"\n\
             \n[runtime]\nprelude = \"committed\"\n",
    );
    write_user(
        &f,
        "[project]\nbase = \"user\"\n\n[runtime]\nprelude = \"user\"\n",
    );
    write_home(&f, "[project]\nbase = \"project\"\n");

    let loaded = load(&f.paths).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(
        loaded.config.project.base.as_deref(),
        Some("project"),
        "the project layer wins over both"
    );
    assert_eq!(
        loaded.config.runtime.prelude.as_deref(),
        Some("user"),
        "the user layer wins over a committed one"
    );
    assert_eq!(
        loaded.config.project.install.as_deref(),
        Some("pnpm install --frozen-lockfile"),
        "keys no higher layer mentions survive the merge"
    );
}

// The same rule the committed layer lives under, for the same reason:
// one file shared by every project on the machine must not be able to
// say where pando writes for one of them.
#[test]
fn a_user_file_cannot_set_root_or_worktrees_dir() {
    let f = fixture();
    let inside = f.root.join("worktrees");
    write_user(
        &f,
        &format!(
            "[project]\nroot = \"/somewhere/else\"\nworktrees_dir = \"{}\"\nbase = \"main\"\n",
            inside.display()
        ),
    );

    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.project.root, None);
    assert_eq!(loaded.config.project.worktrees_dir, None);
    assert_eq!(
        loaded.config.project.base.as_deref(),
        Some("main"),
        "the rest of the user section still applies"
    );
    assert_eq!(
        loaded.warnings.len(),
        2,
        "both keys warn: {:?}",
        loaded.warnings
    );
    assert!(
        loaded
            .warnings
            .iter()
            .all(|w| w.contains("machine-wide config")
                && w.contains(&f.paths.user_config_file().display().to_string())),
        "{:?}",
        loaded.warnings
    );
}

// A file pando did not write, so it is dropped with a warning rather
// than taking every command in every project down with it.
#[test]
fn a_user_file_that_is_broken_or_invalid_is_dropped_with_a_warning() {
    for bad in [
        "this is not toml {{{",
        "[project]\nbase = \"main\"\nnope = 1\n",
        "[dev]\ncmd = \"x\"\n\n[processes.api]\ncmd = \"y\"\n",
    ] {
        let f = fixture();
        write_user(&f, bad);
        let loaded = load(&f.paths).unwrap_or_else(|e| panic!("{bad:?} bricked load: {e:#}"));
        assert_eq!(
            loaded.config,
            Config::default(),
            "the whole layer is dropped: {bad:?}"
        );
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(
            loaded.warnings[0].contains("ignoring")
                && loaded.warnings[0].contains(&f.paths.user_config_file().display().to_string()),
            "{:?}",
            loaded.warnings
        );
    }
}

// `load_without_home` runs when pando's *own* file is unusable. The
// developer's machine-wide file is not implicated by that, and `stop`
// and `logs` should still honour what it says.
#[test]
fn the_user_layer_is_still_read_when_pandos_own_layer_is_skipped() {
    let f = fixture();
    write_user(&f, "[runtime]\nprelude = \"user\"\n");
    write_home(&f, "this is not toml {{{");
    assert!(
        load(&f.paths).is_err(),
        "pando's own layer still fails hard"
    );
    let loaded = load_without_home(&f.paths);
    assert_eq!(loaded.config.runtime.prelude.as_deref(), Some("user"));
}

// Neither file is wrong on its own, so neither is dropped and both are
// named — the same treatment a committed and a project layer get.
#[test]
fn a_conflict_between_the_committed_and_user_layers_names_both() {
    let f = fixture();
    write_committed(&f, "[dev]\ncmd = \"pnpm dev\"\n");
    write_user(&f, "[processes.api]\ncmd = \"node api\"\n");
    let msg = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(msg.contains("may not both be set"), "{msg}");
    assert!(
        msg.contains(&f.root.join("pando.toml").display().to_string()),
        "{msg}"
    );
    assert!(
        msg.contains(&f.paths.user_config_file().display().to_string()),
        "{msg}"
    );
}

#[test]
fn a_committed_file_cannot_set_root_or_worktrees_dir() {
    let f = fixture();
    let inside = f.root.join("worktrees");
    write_committed(
        &f,
        &format!(
            "[project]\nroot = \"/somewhere/else\"\nworktrees_dir = \"{}\"\nbase = \"main\"\n",
            inside.display()
        ),
    );

    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.project.root, None);
    assert_eq!(loaded.config.project.worktrees_dir, None);
    assert_eq!(
        loaded.config.project.base.as_deref(),
        Some("main"),
        "the rest of the committed section still applies"
    );
    assert_eq!(
        loaded.warnings.len(),
        2,
        "both keys warn: {:?}",
        loaded.warnings
    );
    assert!(
        loaded
            .warnings
            .iter()
            .all(|w| w.contains("committed config"))
    );
}

#[test]
fn a_worktrees_dir_inside_the_repository_is_refused() {
    let f = fixture();
    write_home(
        &f,
        &format!(
            "[project]\nworktrees_dir = \"{}\"\n",
            f.root.join(".pando-worktrees").display()
        ),
    );
    let err = load(&f.paths).unwrap_err();
    assert!(
        format!("{err:#}").contains("inside the repository"),
        "unexpected error: {err:#}"
    );
}

// The repository root is canonical; a configured path that reaches it
// through a symlinked ancestor (/var vs /private/var on macOS) must be
// refused just the same.
#[test]
fn a_non_canonical_worktrees_dir_inside_the_repository_is_refused() {
    let f = fixture();
    let dir = TempDir::new().unwrap();
    let link = dir.path().join("link-to-root");
    std::os::unix::fs::symlink(&f.root, &link).unwrap();
    write_home(
        &f,
        &format!(
            "[project]\nworktrees_dir = \"{}\"\n",
            link.join("wt").display()
        ),
    );
    let err = load(&f.paths).unwrap_err();
    assert!(
        format!("{err:#}").contains("inside the repository"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn a_worktrees_dir_outside_the_repository_is_accepted() {
    let f = fixture();
    let outside = f.root.parent().unwrap().join("trees");
    write_home(
        &f,
        &format!("[project]\nworktrees_dir = \"{}\"\n", outside.display()),
    );
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.worktrees_dir(&f.paths), outside);
}

// git takes a relative path from the repository root, whichever
// directory pando was run from. Taken from pando's own directory
// instead, `wt` passed the check wherever it did not exist yet, and git
// then checked the worktree out inside the repository.
#[test]
fn a_relative_worktrees_dir_is_taken_from_the_repository_root() {
    let f = fixture();
    write_home(&f, "[project]\nworktrees_dir = \"wt\"\n");
    let err = load(&f.paths).unwrap_err();
    assert!(
        format!("{err:#}").contains("inside the repository"),
        "unexpected error: {err:#}"
    );

    write_home(&f, "[project]\nworktrees_dir = \"../trees\"\n");
    let loaded = load(&f.paths).unwrap();
    assert_eq!(
        loaded.config.worktrees_dir(&f.paths),
        f.root.join("../trees")
    );
}

#[test]
fn unknown_keys_are_rejected() {
    let f = fixture();
    write_home(&f, "[project]\nbaze = \"main\"\n");
    let err = load(&f.paths).unwrap_err();
    assert!(
        format!("{err:#}").contains("baze"),
        "the error should name the unknown key: {err:#}"
    );

    // Every table denies unknown fields, including a service variant
    // behind the `kind` tag and a whole unknown section.
    write_home(
        &f,
        "[[services]]\nkind = \"compose\"\nfile = \"c.yml\"\nincldue = [\"db\"]\n",
    );
    assert!(
        load(&f.paths).is_err(),
        "unknown service key must be rejected"
    );

    write_home(&f, "[nonsense]\nkey = 1\n");
    assert!(load(&f.paths).is_err(), "unknown section must be rejected");
}

// "unknown field `prots`, expected one of …" with "in `processes.api2`"
// on a line of its own and " — in <file>" on a third, and no hint.
#[test]
fn a_mistyped_key_is_one_line_with_the_file_and_what_was_meant() {
    let f = fixture();
    write_home(
        &f,
        "[processes.api2]\ncmd = \"node api\"\nprots = { PORT = \"api\" }\n",
    );
    let err = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(!err.contains('\n'), "{err}");
    assert!(
        err.starts_with("unknown field `prots` in `processes.api2` of "),
        "{err}"
    );
    assert!(
        err.contains(&f.paths.config_file().display().to_string()),
        "{err}"
    );
    assert!(err.contains("did you mean `ports`?"), "{err}");
    assert!(err.contains("expected one of `cmd`"), "{err}");

    // Nothing near enough: no guess, still one line.
    write_home(&f, "[processes.api2]\ncmd = \"x\"\nzzzzzzzz = 1\n");
    let err = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(
        !err.contains('\n') && !err.contains("did you mean"),
        "{err}"
    );

    // A committed layer is warned about, and the warning is one line too.
    write_home(&f, "");
    write_committed(&f, "[dev]\ncmd = \"x\"\nprots = 1\n");
    let loaded = load(&f.paths).unwrap();
    let warning = loaded.warnings.join("|");
    assert!(warning.contains("did you mean `ports`?"), "{warning}");
    assert!(!warning.contains('\n'), "{warning}");
}

#[test]
fn a_typo_is_matched_to_the_nearest_name_and_nothing_far() {
    assert_eq!(closest("prots", &["cmd", "ports", "cwd"]), Some("ports"));
    assert_eq!(
        closest("isntall", &["install", "provision"]),
        Some("install")
    );
    assert_eq!(closest("banana", &["cmd", "ports"]), None);
    assert_eq!(edit_distance("prots", "ports"), 2);
}

#[test]
fn dev_and_processes_together_are_an_error() {
    let f = fixture();
    write_home(
        &f,
        "[dev]\ncmd = \"pnpm dev\"\n\n[processes.api]\ncmd = \"node api\"\n",
    );
    let err = load(&f.paths).unwrap_err();
    assert!(
        format!("{err:#}").contains("may not both be set"),
        "unexpected error: {err:#}"
    );
}

#[test]
fn dev_is_shorthand_for_a_process_named_dev() {
    let f = fixture();
    write_home(
        &f,
        "[dev]\ncmd = \"pnpm dev\"\nports = { PORT = \"web\" }\n",
    );
    let loaded = load(&f.paths).unwrap();
    assert!(loaded.config.dev.is_none(), "[dev] is normalised away");
    let dev = loaded.config.processes.get("dev").expect("processes.dev");
    assert_eq!(dev.cmd, "pnpm dev");
    assert_eq!(dev.roles(), vec!["web".to_string()]);
}

#[test]
fn the_map_form_of_ports_is_sugar_for_a_role_plus_an_env_template() {
    let spec = PortsSpec::Map(BTreeMap::from([("PORT".to_string(), "web".to_string())]));
    assert_eq!(spec.roles(), vec!["web".to_string()]);
    assert_eq!(
        spec.env_templates(),
        BTreeMap::from([("PORT".to_string(), "{port:web}".to_string())])
    );

    let list = PortsSpec::List(vec!["web".to_string(), "api".to_string()]);
    assert_eq!(list.roles(), vec!["web".to_string(), "api".to_string()]);
    assert!(
        list.env_templates().is_empty(),
        "the list form puts the port in the command, not the environment"
    );
}

// Two variables naming one role is one port: a framework that wants both
// `PORT` and `NEXT_PUBLIC_PORT` must get the same number in each.
#[test]
fn two_env_vars_for_one_role_are_still_one_role() {
    let spec = PortsSpec::Map(BTreeMap::from([
        ("PORT".to_string(), "web".to_string()),
        ("NEXT_PUBLIC_PORT".to_string(), "web".to_string()),
    ]));
    assert_eq!(spec.roles(), vec!["web".to_string()]);
    assert_eq!(spec.env_templates().len(), 2);
}

#[test]
fn ports_accept_both_the_list_and_the_map_form() {
    let f = fixture();
    write_home(
        &f,
        "[processes.web]\ncmd = \"uv run manage.py runserver 127.0.0.1:{port:web}\"\nports = [\"web\"]\n",
    );
    let loaded = load(&f.paths).unwrap();
    let web = loaded.config.processes.get("web").unwrap();
    assert_eq!(web.ports, Some(PortsSpec::List(vec!["web".into()])));
}

#[test]
fn the_full_spec_example_round_trips() {
    let f = fixture();
    write_home(
        &f,
        r#"
[project]
base = "main"
provision = [".env", ".env.local"]
provision_mode = "copy"
install = "pnpm install --frozen-lockfile"
copy_on_write = false
clone = ["node_modules"]

[runtime]
prelude = ""
version_files = [".nvmrc"]

[dev]
cmd = "pnpm dev"
cwd = "."
ports = { PORT = "web" }
env = { NODE_ENV = "development" }
ready = { role = "web", timeout_s = 30 }

[[services]]
kind = "compose"
file = "docker-compose.yml"
include = ["redis"]
env = { REDIS_URL = "redis" }

[[services]]
kind = "native"
name = "postgres"
preset = "postgres"
port_env = "DATABASE_URL"
init = "initdb --pgdata {datadir}"
cmd = "exec postgres -D {datadir} -p {port} -k {socket_dir}"
ready = "pg_isready -h 127.0.0.1 -p {port}"

[[hooks]]
name = "migrate"
after = "services"
fingerprint = ["prisma/migrations/**"]
cmd = "pnpm prisma migrate deploy"

[[probes]]
name = "native-abi"
cmd = "node -e 'require(\"better-sqlite3\")'"
match = "NODE_MODULE_VERSION"
hint = "Rebuild native modules under the dev runtime."

[branches]
rules = [{ match = "*-beta", base = "beta" }]

[share]
provider = "cloudflared"
auth_cmd = "./scripts/dev-cookie.sh"
"#,
    );
    let loaded = load(&f.paths).unwrap();
    let c = &loaded.config;
    assert_eq!(c.project.provision_mode, ProvisionMode::Copy);
    assert!(!c.project.copy_on_write());
    assert_eq!(c.project.clone, vec!["node_modules".to_string()]);
    assert_eq!(c.runtime.version_files, vec![".nvmrc".to_string()]);
    assert_eq!(c.services.len(), 2);
    assert!(matches!(c.services[0], ServiceConfig::Compose { .. }));
    assert!(matches!(c.services[1], ServiceConfig::Native { .. }));
    assert_eq!(c.hooks[0].after, HookPoint::Services);
    assert_eq!(c.probes[0].match_, "NODE_MODULE_VERSION");
    assert_eq!(c.share.provider.as_deref(), Some("cloudflared"));
    assert_eq!(c.base_for_branch("fix/thing-beta"), Some("beta"));
    assert_eq!(c.base_for_branch("feat/other"), Some("main"));

    // Serialising and reloading must produce the same value, or `write`
    // would quietly drop fields detection put there.
    let text = toml::to_string_pretty(c).unwrap();
    let back: Config = toml::from_str(&text).unwrap();
    assert_eq!(normalize(back).unwrap(), *c);
}

// `kind = "native"` parses, validates, and is then dropped by the start
// path, so a developer read "no services configured" while looking at a
// file that configures one. It is not an error — the block is legal and

// `[isolation]` holds one key per layer, and each is wrong in the
// other's file: "this project has no services" in a machine-wide
// file says it for every project on the laptop, and "prefer native"
// in a committed file imposes one developer's machine on everyone
// who clones the repository.
#[test]
fn a_machine_wide_file_may_not_say_a_project_has_no_services() {
    let f = fixture();
    write_user(
        &f,
        "[runtime]\nprelude = \"\"\n\n[isolation]\nnone = true\n",
    );
    let loaded = load(&f.paths).unwrap();
    assert!(!loaded.config.isolation.none, "it applied to every project");
    // The rest of the file survives: one wrong key is stripped, not
    // the whole layer.
    assert_eq!(loaded.config.runtime.prelude.as_deref(), Some(""));
    assert!(
        loaded
            .warnings
            .iter()
            .any(|w| w.contains("isolation.none") && w.contains("every project")),
        "{:?}",
        loaded.warnings
    );
}

#[test]
fn a_committed_file_may_not_say_which_mechanism_this_machine_prefers() {
    let f = fixture();
    write_committed(&f, "[isolation]\nprefer = \"native\"\n");
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.isolation.preferred(), None);
    assert!(
        loaded
            .warnings
            .iter()
            .any(|w| w.contains("isolation.prefer") && w.contains("property of a machine")),
        "{:?}",
        loaded.warnings
    );

    // And the machine-wide file, which is its home, keeps it.
    write_user(&f, "[isolation]\nprefer = \"native\"\n");
    assert_eq!(
        load(&f.paths).unwrap().config.isolation.preferred(),
        Some("native")
    );
}

// Two layers that are each fine alone and cannot both apply: the
// refusal names every file present rather than picking a winner.
#[test]
fn a_committed_service_and_a_recorded_none_cannot_both_apply() {
    let f = fixture();
    write_committed(
        &f,
        "[[services]]\nkind = \"compose\"\nfile = \"docker-compose.yml\"\ninclude = []\n",
    );
    write_home(&f, "[isolation]\nnone = true\n");
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(e.contains("none = true"), "{e}");
    assert!(e.contains("cannot all apply"), "{e}");
    assert!(e.contains("pando.toml"), "it names the files: {e}");
}

#[test]
fn a_native_block_is_kept_as_written_and_no_longer_warns() {
    let f = fixture();
    write_home(
        &f,
        r#"
[[services]]
kind = "compose"
file = "docker-compose.yml"
include = ["redis"]

[[services]]
kind = "native"
name = "postgres"
preset = "postgres"
port_env = "DATABASE_URL"
"#,
    );
    let loaded = load(&f.paths).unwrap();
    assert_eq!(
        loaded.config.services.len(),
        2,
        "the block is kept exactly as written"
    );
    // It warned for as long as nothing could run it. Something can
    // now, so the warning would be the false statement.
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
}

// A native service is a role, a port, a log tab and a data directory
// under its own name, so every rule a compose service's name obeys
// applies to it too — and until now nothing checked any of them.

#[test]
fn a_native_service_may_not_take_a_role_a_process_already_owns() {
    let f = fixture();
    write_home(
        &f,
        "[processes.dev]\ncmd = \"x\"\nports = [\"postgres\"]\n\n\
             [[services]]\nkind = \"native\"\nname = \"postgres\"\n",
    );
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(e.contains("both claim the role \"postgres\""), "{e}");
}

#[test]
fn a_native_service_may_not_share_a_name_with_a_compose_service() {
    let f = fixture();
    write_home(
        &f,
        "[[services]]\nkind = \"compose\"\nfile = \"c.yml\"\ninclude = [\"postgres\"]\n\n\
             [[services]]\nkind = \"native\"\nname = \"postgres\"\n",
    );
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    // One sentence about a duplicate, not two identical halves of a
    // role collision.
    assert!(e.contains("two [[services]] entries"), "{e}");
    assert!(e.contains("\"postgres\""), "{e}");
}

#[test]
fn a_native_service_may_not_share_a_name_with_a_hook() {
    let f = fixture();
    write_home(
        &f,
        "[[services]]\nkind = \"native\"\nname = \"migrate\"\n\n\
             [[hooks]]\nname = \"migrate\"\nafter = \"services\"\ncmd = \"m\"\n",
    );
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(e.contains("logs/<worktree>/migrate.log"), "{e}");

    // macOS ignores case by default, so `Migrate` is the same log.
    write_home(
        &f,
        "[[services]]\nkind = \"native\"\nname = \"migrate\"\n\n\
             [[hooks]]\nname = \"Migrate\"\nafter = \"services\"\ncmd = \"m\"\n",
    );
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(
        e.contains("the hook \"Migrate\" and the service \"migrate\""),
        "{e}"
    );
}

// A process, a hook and a service all write `logs/<worktree>/<name>.log`.
// Only a hook and a service were held apart, so a hook named like a
// process had what it printed erased when the process spawned, and a
// process named like a service shared its log with the service.
#[test]
fn a_process_may_not_share_a_name_with_a_hook() {
    let f = fixture();
    for hook in ["api", "API"] {
        write_home(
            &f,
            &format!(
                "[processes.api]\ncmd = \"npm run dev\"\nports = [\"api\"]\n\n\
                 [[hooks]]\nname = \"{hook}\"\nafter = \"install\"\ncmd = \"npm run build\"\n"
            ),
        );
        let e = format!("{:#}", load(&f.paths).unwrap_err());
        assert!(
            e.contains(&format!("the hook \"{hook}\" and the process \"api\"")),
            "{e}"
        );
        assert!(e.contains("logs/<worktree>/api.log"), "{e}");
    }
}

#[test]
fn a_process_may_not_share_a_name_with_a_service() {
    let f = fixture();
    for service in [
        "[[services]]\nkind = \"compose\"\nfile = \"c.yml\"\ninclude = [\"db\"]\n",
        "[[services]]\nkind = \"native\"\nname = \"db\"\n",
    ] {
        write_home(
            &f,
            &format!("[processes.db]\ncmd = \"x\"\nports = [\"web\"]\n\n{service}"),
        );
        let e = format!("{:#}", load(&f.paths).unwrap_err());
        assert!(
            e.contains("the service \"db\" and the process \"db\""),
            "{e}"
        );
    }
}

#[test]
fn two_processes_whose_names_differ_only_in_case_are_refused() {
    let f = fixture();
    write_home(
        &f,
        "[processes.web]\ncmd = \"x\"\nports = [\"web\"]\n\n\
         [processes.Web]\ncmd = \"y\"\nports = [\"admin\"]\n",
    );
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(e.contains("differ only in case"), "{e}");
}

// Two services are held apart by an exact lookup, so `redis` and `Redis`
// passed, and on macOS both wrote logs/<worktree>/redis.log.
#[test]
fn two_services_whose_names_differ_only_in_case_are_refused() {
    let f = fixture();
    for (services, escape) in [
        (
            "[[services]]\nkind = \"native\"\nname = \"redis\"\n\n\
             [[services]]\nkind = \"compose\"\nfile = \"c.yml\"\ninclude = [\"Redis\"]\n",
            "drop it from `include`",
        ),
        (
            "[[services]]\nkind = \"compose\"\nfile = \"c.yml\"\ninclude = [\"redis\", \"Redis\"]\n",
            "drop it from `include`",
        ),
        (
            "[[services]]\nkind = \"native\"\nname = \"redis\"\n\n\
             [[services]]\nkind = \"native\"\nname = \"Redis\"\n",
            "rename it",
        ),
    ] {
        write_home(&f, services);
        let e = format!("{:#}", load(&f.paths).unwrap_err());
        assert!(
            e.contains("the service \"Redis\" differs only in case from the service \"redis\""),
            "{e}"
        );
        assert!(e.contains(escape), "{e}");
    }
}

#[test]
fn a_native_service_name_that_is_a_path_is_refused() {
    let f = fixture();
    write_home(&f, "[[services]]\nkind = \"native\"\nname = \"../evil\"\n");
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(e.contains("single name, not a path"), "{e}");
}

#[test]
fn a_native_env_key_may_only_point_at_the_service_the_entry_runs() {
    let f = fixture();
    write_home(
        &f,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n\
             env = { DATABASE_URL = \"pg\" }\n",
    );
    let e = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(
        e.contains("this [[services]] entry runs \"postgres\""),
        "{e}"
    );

    // And the same key pointing at itself is fine.
    write_home(
        &f,
        "[[services]]\nkind = \"native\"\nname = \"postgres\"\n\
             env = { DATABASE_URL = \"postgres\" }\n",
    );
    load(&f.paths).unwrap();
}

#[test]
fn arrays_of_tables_are_replaced_whole_not_merged() {
    let f = fixture();
    write_committed(
        &f,
        "[[services]]\nkind = \"compose\"\nfile = \"a.yml\"\ninclude = [\"postgres\"]\n\n[[services]]\nkind = \"compose\"\nfile = \"b.yml\"\n",
    );
    write_home(
        &f,
        "[[services]]\nkind = \"compose\"\nfile = \"only.yml\"\n",
    );

    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.services.len(), 1);
    assert!(matches!(
        &loaded.config.services[0],
        ServiceConfig::Compose { file, .. } if file == "only.yml"
    ));
}

#[test]
fn clone_paths_must_stay_inside_the_repository() {
    let f = fixture();
    for bad in ["/usr/lib/node_modules", "../node_modules", ""] {
        write_home(&f, &format!("[project]\nclone = [\"{bad}\"]\n"));
        assert!(
            load(&f.paths).is_err(),
            "clone path {bad:?} should be refused"
        );
    }
    write_home(
        &f,
        "[project]\nclone = [\"node_modules\", \"apps/web/node_modules\"]\n",
    );
    assert!(load(&f.paths).is_ok());
}

// Unset is on, and a config that says nothing writes nothing: the key
// appears in pando.toml only when a developer sets it.
#[test]
fn copy_on_write_is_on_until_turned_off_and_never_written_unset() {
    let f = fixture();
    write_home(&f, "[project]\ninstall = \"true\"\n");
    let loaded = load(&f.paths).unwrap();
    assert!(loaded.config.project.copy_on_write());
    let text = toml::to_string(&loaded.config).unwrap();
    assert!(!text.contains("copy_on_write"), "{text}");
    assert!(!text.contains("clone"), "{text}");
}

#[test]
fn provision_paths_must_stay_inside_the_repository() {
    let f = fixture();
    for bad in ["/etc/passwd", "../secrets/.env", ".."] {
        write_home(&f, &format!("[project]\nprovision = [\"{bad}\"]\n"));
        assert!(
            load(&f.paths).is_err(),
            "provision path {bad:?} should be refused"
        );
    }
    write_home(
        &f,
        "[project]\nprovision = [\".env\", \"apps/web/.env.local\"]\n",
    );
    assert!(load(&f.paths).is_ok());
}

#[test]
fn write_only_ever_touches_the_pando_home_copy() {
    let f = fixture();
    let mut config = Config::default();
    config.project.base = Some("main".into());
    write(&f.paths, &config).unwrap();

    assert!(f.paths.config_file().is_file());
    assert!(
        !f.root.join("pando.toml").exists(),
        "write must never create a file inside the repository"
    );
    let entries: Vec<_> = std::fs::read_dir(&f.root).unwrap().collect();
    assert!(entries.is_empty(), "the repository must be untouched");

    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config, config);
    assert_eq!(
        temp_files(&f),
        Vec::<String>::new(),
        "the temp file must not leak after the rename"
    );
}

#[test]
fn a_broken_file_is_reported_and_skipped() {
    let f = fixture();
    write_committed(&f, "this is not toml {{{");
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.warnings[0].contains("ignoring"));
}

// toml's parse error is five lines, three of them a copy of the source
// with a caret. Each warning is printed as `pando: <warning>`, so the
// committed and user layers' came out as a paragraph, and the project
// layer's left "— carrying on without it" on a line of its own. The line
// and the column are what the caret says, and they stay.
#[test]
fn a_toml_syntax_error_is_one_line_that_keeps_its_line_and_column() {
    let f = fixture();
    write_committed(&f, "x = \n[processes.dev]\ncmd = \"1\"\n");
    write_user(&f, "[processes.dev]\ncmd = \"1\"\nports = [\"web\"\n");
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.warnings.len(), 2, "{:?}", loaded.warnings);
    for (warning, at) in loaded.warnings.iter().zip(["line 1, column 5", "line 3"]) {
        assert!(!warning.contains('\n'), "{warning:?}");
        assert!(warning.contains(at), "{warning}");
        assert!(!warning.contains(" | "), "no source gutter: {warning}");
    }
    assert!(
        loaded.warnings[1].contains("unclosed array"),
        "{:?}",
        loaded.warnings
    );

    write_home(&f, "[project\nbase = \"main\"\n");
    let err = format!("{:#}", load(&f.paths).unwrap_err());
    assert!(!err.contains('\n'), "{err:?}");
    assert!(
        err.contains("line 1") && err.contains("pando.toml"),
        "{err}"
    );

    let err = set_detected(
        &f.paths,
        Layer::Project,
        &["dev"],
        "cmd",
        "x",
        Note::Answered,
    );
    let err = format!("{:#}", err.unwrap_err());
    assert!(!err.contains('\n'), "{err:?}");
    assert!(
        err.contains("not valid TOML") && err.contains("line 1"),
        "{err}"
    );
}

// A committed file is someone else's work, and often a newer pando's.
// A key this build does not know, or a value it will not accept, must
// not stop `pando ls` for everyone who pulled it — the layer is dropped
// with a warning, exactly as a file that does not even parse already is.
#[test]
fn a_committed_file_that_does_not_validate_is_dropped_with_a_warning() {
    let f = fixture();
    for bad in [
        "[dev]\ncmd = \"x\"\n\n[processes.api]\ncmd = \"y\"\n",
        "[project]\nprovision = [\"../shared/.env\"]\n",
        "[project]\nbase = \"main\"\nnope = 1\n",
    ] {
        write_committed(&f, bad);
        let loaded = load(&f.paths).unwrap_or_else(|e| panic!("{bad:?} bricked load: {e:#}"));
        assert_eq!(
            loaded.config,
            Config::default(),
            "the whole layer is dropped: {bad:?}"
        );
        assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
        assert!(
            loaded.warnings[0].contains("ignoring"),
            "{:?}",
            loaded.warnings
        );
    }
}

// The home layer is pando's own file, so a problem there is pando's bug
// or the user's edit, and still fails hard.
#[test]
fn a_home_file_that_does_not_validate_still_fails() {
    let f = fixture();
    write_home(&f, "[project]\nbase = \"main\"\nnope = 1\n");
    let err = load(&f.paths).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("nope"), "{msg}");
    assert!(
        msg.contains(&f.paths.config_file().display().to_string()),
        "the failing file should be named: {msg}"
    );
}

// Each layer is fine on its own and only the merge is not, so neither
// file explains it alone and both are named.
#[test]
fn a_conflict_that_only_appears_after_merging_names_both_files() {
    let f = fixture();
    write_committed(&f, "[dev]\ncmd = \"pnpm dev\"\n");
    write_home(&f, "[processes.api]\ncmd = \"node api\"\n");
    let err = load(&f.paths).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("may not both be set"), "{msg}");
    assert!(
        msg.contains(&f.root.join("pando.toml").display().to_string()),
        "{msg}"
    );
    assert!(
        msg.contains(&f.paths.config_file().display().to_string()),
        "{msg}"
    );
}

// ---- several processes ------------------------------------------------

/// Loads a home config and returns the error message it refused with.
fn refusal(text: &str) -> String {
    let f = fixture();
    write_home(&f, text);
    format!("{:#}", load(&f.paths).unwrap_err())
}

fn accepts(text: &str) -> Config {
    let f = fixture();
    write_home(&f, text);
    load(&f.paths).expect("this config is valid").config
}

#[test]
fn two_processes_claiming_one_role_are_refused_by_name() {
    let msg = refusal(
        "[processes.web]\ncmd = \"a\"\nports = [\"web\"]\n\n\
             [processes.api]\ncmd = \"b\"\nports = { PORT = \"web\" }\n",
    );
    assert!(
        msg.contains("\"api\""),
        "the message names both processes: {msg}"
    );
    assert!(msg.contains("\"web\""), "{msg}");
    // Both forms of `ports` own roles the same way, so the map form is
    // caught as well as the list.
    assert!(msg.contains("role"), "{msg}");
}

#[test]
fn one_process_may_reference_another_processs_role() {
    // The whole reason `{port:<role>}` exists: the web process is told
    // the port the api was given. Referencing is not owning.
    let config = accepts(
        "[processes.web]\ncmd = \"a\"\nports = [\"web\"]\n\
             env = { VITE_API_URL = \"http://localhost:{port:api}\" }\n\n\
             [processes.api]\ncmd = \"b\"\nports = [\"api\"]\n",
    );
    assert_eq!(config.processes["web"].roles(), vec!["web"]);
    assert_eq!(config.processes["api"].roles(), vec!["api"]);
}

#[test]
fn a_role_repeated_inside_one_process_is_still_one_port() {
    // `PORT` and `NEXT_PUBLIC_PORT` both meaning `web` is one port, not
    // a process colliding with itself.
    let config = accepts(
        "[processes.web]\ncmd = \"a\"\nports = { PORT = \"web\", NEXT_PUBLIC_PORT = \"web\" }\n",
    );
    assert_eq!(config.processes["web"].roles(), vec!["web"]);
}

#[test]
fn a_ready_role_a_process_does_not_own_is_refused() {
    let msg = refusal(
        "[processes.web]\ncmd = \"a\"\nports = [\"web\"]\n\n\
             [processes.api]\ncmd = \"b\"\nports = [\"api\"]\nready = { role = \"web\" }\n",
    );
    assert!(msg.contains("ready.role"), "{msg}");
    assert!(msg.contains("\"api\""), "the process is named: {msg}");
    assert!(
        msg.contains("owns api"),
        "and so is what it does own: {msg}"
    );
}

#[test]
fn a_ready_role_a_process_does_own_is_fine() {
    let config = accepts(
        "[processes.web]\ncmd = \"a\"\nports = [\"web\"]\nready = { role = \"web\", timeout_s = 90 }\n",
    );
    let ready = config.processes["web"].ready.clone().expect("a ready rule");
    assert_eq!(ready.role.as_deref(), Some("web"));
    assert_eq!(ready.timeout_s, Some(90));
}

#[test]
fn a_cwd_that_escapes_the_worktree_is_refused() {
    for cwd in ["/etc", "../sibling", "apps/../../elsewhere"] {
        let msg = refusal(&format!("[processes.web]\ncmd = \"a\"\ncwd = \"{cwd}\"\n"));
        assert!(
            msg.contains("web"),
            "the process is named for cwd {cwd:?}: {msg}"
        );
        assert!(
            msg.contains("relative") || msg.contains("escape"),
            "cwd {cwd:?} was refused for the wrong reason: {msg}"
        );
    }
    let msg = refusal("[processes.web]\ncmd = \"a\"\ncwd = \"  \"\n");
    assert!(msg.contains("empty"), "{msg}");
}

#[test]
fn a_cwd_inside_the_worktree_is_kept_as_written() {
    let config = accepts("[processes.web]\ncmd = \"a\"\ncwd = \"apps/web\"\n");
    assert_eq!(config.processes["web"].cwd.as_deref(), Some("apps/web"));
    // The worktree root itself, spelled out, is not an escape.
    let config = accepts("[dev]\ncmd = \"a\"\ncwd = \".\"\n");
    assert_eq!(config.processes["dev"].cwd.as_deref(), Some("."));
}

// Phase 2b review, finding 1. A TOML key may be any quoted string, and
// a process's name is a path component of its log file: `start` then
// creates and truncates a `.log` file wherever the name points, up to
// and including inside the repository.
#[test]
fn a_process_name_that_escapes_the_log_directory_is_refused() {
    for bad in [
        "../../../../../escaped-log",
        "../../../../../acme-shop/inside-repo",
        "apps/web",
        "/absolute",
        "..",
        ".",
        "",
        "   ",
    ] {
        let msg = refusal(&format!("[processes.\"{bad}\"]\ncmd = \"true\"\n"));
        assert!(
            msg.contains("logs/<worktree>"),
            "{bad:?} must be refused as a log path: {msg}"
        );
        if !bad.trim().is_empty() {
            assert!(
                msg.contains(&format!("{bad:?}")),
                "the refusal quotes the name: {msg}"
            );
        }
    }
}

#[test]
fn a_process_name_with_a_directory_in_it_suggests_the_name_it_meant() {
    let msg = refusal("[processes.\"apps/web\"]\ncmd = \"true\"\n");
    assert!(msg.contains("\"apps/web\""), "{msg}");
    assert!(msg.contains("try \"web\""), "{msg}");
}

// A process named `install` shares the install hook's log file, and
// `reset_log` truncates it on every start; `tunnel` and `proxy` are
// `share`'s, reserved the same way.
#[test]
fn a_process_named_after_one_of_pandos_own_logs_is_refused() {
    for reserved in crate::paths::RESERVED_LOG_SOURCES {
        let msg = refusal(&format!("[processes.{reserved}]\ncmd = \"true\"\n"));
        assert!(msg.contains("reserved"), "{msg}");
        assert!(msg.contains(reserved), "{msg}");
    }
    let config = accepts("[processes.installer]\ncmd = \"true\"\n");
    assert!(config.processes.contains_key("installer"));
}

#[test]
fn a_process_name_in_any_alphabet_is_still_fine() {
    let config = accepts("[processes.\"wörker\"]\ncmd = \"true\"\nports = []\n");
    assert!(config.processes.contains_key("wörker"));
}

// Hooks write into the same directory under the same rules, so the same
// name check applies to them — before Phase 3 gives anyone a way to
// write one.
#[test]
fn a_hook_name_that_escapes_the_log_directory_or_is_reserved_is_refused() {
    let msg = refusal(
        "[[hooks]]\nname = \"../../../../../escaped-hook\"\nafter = \"install\"\ncmd = \"true\"\n",
    );
    assert!(msg.contains("\"../../../../../escaped-hook\""), "{msg}");
    assert!(msg.contains("logs/<worktree>"), "{msg}");

    let msg = refusal("[[hooks]]\nname = \"install\"\nafter = \"install\"\ncmd = \"true\"\n");
    assert!(msg.contains("reserved"), "{msg}");

    let config = accepts("[[hooks]]\nname = \"migrate\"\nafter = \"services\"\ncmd = \"true\"\n");
    assert_eq!(config.hooks[0].name, "migrate");
}

#[test]
fn a_committed_config_that_fails_the_new_rules_is_dropped_rather_than_fatal() {
    // The Phase 1 rule, still holding for rules Phase 2b added: a file
    // the team committed may be newer, or wrong, and must not brick
    // every command.
    let f = fixture();
    write_committed(
        &f,
        "[processes.web]\ncmd = \"a\"\nports = [\"web\"]\n\n\
             [processes.api]\ncmd = \"b\"\nports = [\"web\"]\n",
    );
    let loaded = load(&f.paths).expect("a committed file is dropped, not fatal");
    assert!(loaded.config.processes.is_empty());
    assert_eq!(loaded.warnings.len(), 1, "{:?}", loaded.warnings);
    assert!(loaded.warnings[0].contains("role"), "{:?}", loaded.warnings);
}

#[test]
fn glob_match_handles_the_shapes_branch_rules_use() {
    assert!(glob_match("*-beta", "fix/thing-beta"));
    assert!(!glob_match("*-beta", "fix/betamax"));
    assert!(glob_match("release/*", "release/1.2"));
    assert!(glob_match("v?.?", "v4.1"));
    assert!(!glob_match("v?.?", "v4.11"));
    assert!(glob_match("*", "anything"));
    assert!(glob_match("exact", "exact"));
    assert!(!glob_match("exact", "exactly"));
}

#[test]
fn branch_rules_win_over_the_project_base_and_first_match_wins() {
    let mut config = Config::default();
    config.project.base = Some("main".into());
    config.branches.rules = vec![
        BranchRule {
            match_: "release/*".into(),
            base: "release".into(),
        },
        BranchRule {
            match_: "*".into(),
            base: "catch-all".into(),
        },
    ];
    assert_eq!(config.base_for_branch("release/1.2"), Some("release"));
    assert_eq!(config.base_for_branch("feat/x"), Some("catch-all"));
}

// `ports = ["my web"]` loaded, and `{port:my web}` reached the app as
// literal braces: the placeholder cannot spell a space.
#[test]
fn a_role_the_placeholder_cannot_spell_is_refused() {
    let msg = refusal("[processes.web]\ncmd = \"x --port {port:my web}\"\nports = [\"my web\"]\n");
    assert!(msg.contains("cannot"), "{msg}");
    assert!(msg.contains("\"my web\""), "{msg}");
}

// ---- [ui] ---------------------------------------------------------------

#[test]
fn the_user_layer_carries_the_theme_and_expands_its_file() {
    let f = fixture();
    write_user(
        &f,
        "[ui]\ntheme = \"gruvbox\"\ntheme_from = \"~/.config/switcher/current\"\nappearance = \"dark\"\n",
    );
    let loaded = load(&f.paths).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let settings = loaded.config.ui.theme_settings();
    assert_eq!(settings.theme.as_deref(), Some("gruvbox"));
    assert_eq!(settings.appearance.as_deref(), Some("dark"));
    let from = settings.theme_from.unwrap();
    assert!(!from.starts_with("~"), "{}", from.display());
    assert!(
        from.ends_with(".config/switcher/current"),
        "{}",
        from.display()
    );
}

#[test]
fn an_appearance_pando_does_not_know_drops_the_layer_with_the_spellings() {
    let f = fixture();
    write_user(&f, "[ui]\nappearance = \"dusk\"\n");
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.ui.appearance, None);
    assert!(
        loaded
            .warnings
            .iter()
            .any(|w| w.contains("auto, dark, light")),
        "{:?}",
        loaded.warnings
    );
}

#[test]
fn the_user_layer_carries_the_lists_order() {
    let f = fixture();
    write_user(&f, "[ui]\nsort = \"run\"\n");
    let loaded = load(&f.paths).unwrap();
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(loaded.config.ui.sort.as_deref(), Some("run"));
}

#[test]
fn an_order_pando_does_not_know_drops_the_layer_with_the_spellings() {
    let f = fixture();
    write_user(&f, "[ui]\nsort = \"oldest\"\n");
    let loaded = load(&f.paths).unwrap();
    assert_eq!(loaded.config.ui.sort, None);
    assert!(
        loaded
            .warnings
            .iter()
            .any(|w| w.contains("pr, newest, run, name")),
        "{:?}",
        loaded.warnings
    );
}

// A login is a password. A file the team shares must not carry one, and a
// machine-wide one is not where a project's login belongs; pando's own
// file for the project is.
#[test]
fn a_namespace_login_is_read_from_pandos_own_file_and_nowhere_else() {
    let f = fixture();
    let login = "[namespaced.mariadb]\nuser = \"root\"\npassword = \"hunter2\"\n";
    write_committed(&f, login);
    write_user(&f, login);
    let loaded = load(&f.paths).unwrap();
    assert!(
        loaded.config.namespaced.is_empty(),
        "{:?}",
        loaded.config.namespaced
    );
    for why in ["a file the team shares", "pando's own file"] {
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| w.contains("ignoring [namespaced]") && w.contains(why)),
            "{:?}",
            loaded.warnings
        );
    }
    assert!(
        loaded.warnings.iter().all(|w| !w.contains("hunter2")),
        "{:?}",
        loaded.warnings
    );

    write_home(&f, login);
    let loaded = load(&f.paths).unwrap();
    let mariadb = &loaded.config.namespaced["mariadb"];
    assert_eq!(mariadb.user.as_deref(), Some("root"));
    assert_eq!(mariadb.password.as_deref(), Some("hunter2"));
    assert!(!format!("{:?}", loaded.config).contains("hunter2"));
}

// A file pando prints whole is printed without a login's password,
// however the login was written, and with everything else as it was.
#[test]
fn a_printed_config_hides_every_namespace_password_and_nothing_else() {
    let text = "# my project\n[dev]\ncmd = \"next dev\"  # detected: package.json\n\n\
                [namespaced]\ncache = { user = \"u\", password = \"inline-secret\" }\n\n\
                [namespaced.db]  # answered: 2026-09-26\nuser = \"root\"\n\
                password = \"table-secret\"  # typed at a start\n";
    assert_eq!(
        hide_passwords(text),
        "# my project\n[dev]\ncmd = \"next dev\"  # detected: package.json\n\n\
         [namespaced]\ncache = { user = \"u\", password = \"(hidden)\" }\n\n\
         [namespaced.db]  # answered: 2026-09-26\nuser = \"root\"\n\
         password = \"(hidden)\"  # typed at a start\n"
    );
    assert_eq!(
        hide_passwords("namespaced.db.password = \"dotted-secret\"\n"),
        "namespaced.db.password = \"(hidden)\"\n"
    );
    // A password anywhere else is not a login pando keeps, and a file
    // with none is returned exactly as it was.
    let other = "[dev]\ncmd = \"x\"\nenv = { password = \"not-a-login\" }\n";
    assert_eq!(hide_passwords(other), other);
}

#[test]
fn a_printed_config_that_does_not_parse_hides_every_line_naming_a_password() {
    let text = "[namespaced.db\nuser = \"root\"\npassword = \"s3cret\"\n";
    assert_eq!(
        hide_passwords(text),
        "[namespaced.db\nuser = \"root\"\npassword = \"(hidden)\"\n"
    );
}
