use super::*;
use crate::state::{NamespaceKind, NamespaceRecord, State, WorktreeRecord};
use chrono::Utc;

// ---- names ------------------------------------------------------------------

fn names(main: &str, worktree: &str) -> [String; 2] {
    database_names(main, "acme-0000beef", worktree, MAX_NAME).unwrap()
}

// `pando check`'s worktree is `.pando-check`, a name no branch can have.
// Its leading dot is dropped like any other character a database name
// cannot hold, so the check's own database is a plain one under the
// prefix a grant covers, and still never main's.
#[test]
fn the_checks_database_is_a_plain_name_under_the_marker() {
    let [readable, hashed] = names("shop", crate::paths::CHECK_WORKTREE);
    assert_eq!(readable, "shop__pando_check");
    assert!(is_plain(&readable) && is_plain(&hashed), "{hashed}");
    assert!(hashed.starts_with("shop__pando_check_"), "{hashed}");
}

// A prefix is the worktree's tag with the marker after it, joined to the
// main checkout's own so the app's names still read as the project's:
// one worktree's can be told from main's and from another's by name.
#[test]
fn a_worktrees_prefix_is_its_tag_and_the_marker_after_main_s() {
    let tag = worktree_tag("acme-0000beef", "feat+x");
    assert!(
        tag.starts_with("feat_x_") && tag.len() == "feat_x_".len() + 6,
        "{tag}"
    );
    assert_eq!(
        worktree_prefix("", "acme-0000beef", "feat+x"),
        format!("{tag}__")
    );
    assert_eq!(
        worktree_prefix("shop", "acme-0000beef", "feat+x"),
        format!("shop_{tag}__")
    );
    assert_eq!(
        worktree_prefix("laravel_", "acme-0000beef", "feat+x"),
        format!("laravel_{tag}__")
    );
    // Nothing checks a prefix before it is used, so names that slug the
    // same, and the same branch in a second clone, never share one.
    for (project, worktree) in [
        ("acme-0000beef", "feat-x"),
        ("acme-0000beef", "Feat+X"),
        ("acme-1111cafe", "feat+x"),
    ] {
        assert_ne!(worktree_tag(project, worktree), tag, "{project} {worktree}");
    }
    assert_eq!(worktree_slug(crate::paths::CHECK_WORKTREE), "pando_check");
    // Never empty, so a prefix always tells the worktree apart.
    let odd = worktree_slug("+++");
    assert_eq!(odd.len(), 8, "{odd}");
    assert_eq!(odd, worktree_slug("+++"));
}

#[test]
fn a_worktrees_database_is_the_main_one_the_marker_and_its_own_name() {
    let [readable, hashed] = names("northwind_traders", "feat+x");
    assert_eq!(readable, "northwind_traders__feat_x");
    assert!(hashed.starts_with("northwind_traders__feat_x_"), "{hashed}");
    assert_eq!(hashed.len(), "northwind_traders__feat_x_".len() + 8);
    assert!(
        hashed[hashed.len() - 8..]
            .chars()
            .all(|c| c.is_ascii_hexdigit())
    );
}

#[test]
fn a_worktree_name_becomes_lowercase_letters_digits_and_single_underscores() {
    for (worktree, tail) in [
        ("feat+login", "feat_login"),
        ("Fix/Some Thing!!", "fix_some_thing"),
        ("--leading+and+trailing--", "leading_and_trailing"),
        ("a____b", "a_b"),
        ("pr-12+add-caché", "pr_12_add_cach"),
        ("v2.0", "v2_0"),
    ] {
        assert_eq!(
            names("shop", worktree)[0],
            format!("shop__{tail}"),
            "{worktree}"
        );
    }
}

// The same main database, project and worktree always name the same
// database: a restart that picked a new name would build an empty one.
#[test]
fn the_names_are_the_same_every_time_and_only_the_hashed_one_knows_the_project() {
    assert_eq!(names("shop", "feat+x"), names("shop", "feat+x"));
    let here = database_names("shop", "acme-0000beef", "feat+x", MAX_NAME).unwrap();
    let clone = database_names("shop", "acme-1111cafe", "feat+x", MAX_NAME).unwrap();
    assert_eq!(
        here[0], clone[0],
        "the readable name is the worktree's alone"
    );
    assert_ne!(here[1], clone[1], "the hashed one tells two clones apart");
}

// `feat+x` and `feat-x` read the same; the second name is what keeps them
// from sharing one database.
#[test]
fn two_worktrees_that_read_the_same_have_different_second_names() {
    let plus = names("shop", "feat+x");
    let dash = names("shop", "feat-x");
    assert_eq!(plus[0], dash[0]);
    assert_ne!(plus[1], dash[1]);
}

#[test]
fn a_name_past_the_limit_is_cut_and_hashed_so_long_names_still_differ() {
    let long = format!("feat+{}", "x".repeat(120));
    let [readable, hashed] = names("northwind_traders", &long);
    assert_eq!(readable.len(), MAX_NAME, "{readable}");
    assert_eq!(readable, hashed, "cut means hashed, both ways");
    let other = names("northwind_traders", &format!("{long}y"));
    assert_ne!(
        readable, other[0],
        "the hash is of the whole name, not the cut"
    );
    // Exactly at the limit is not past it.
    let fits = "y".repeat(MAX_NAME - "shop__".len());
    assert_eq!(names("shop", &fits)[0], format!("shop__{fits}"));
    let over = format!("{fits}y");
    assert_ne!(names("shop", &over)[0], format!("shop__{over}"));
}

#[test]
fn a_worktree_with_nothing_readable_in_its_name_is_named_by_its_hash() {
    for worktree in ["+++", "日本語", "_", ""] {
        let [readable, hashed] = names("shop", worktree);
        assert_eq!(readable, hashed, "{worktree:?}");
        assert_eq!(readable.len(), "shop__".len() + 8, "{readable}");
    }
}

#[test]
fn a_main_name_that_is_not_plain_or_leaves_no_room_is_refused() {
    for main in ["", "shop;drop", "sh`op", "a.b", "shöp", "shop db"] {
        let e = format!(
            "{:#}",
            database_names(main, "p", "feat+x", MAX_NAME).unwrap_err()
        );
        assert!(e.contains("not a plain name"), "{main:?}: {e}");
    }
    // `<main>__` plus `_`, eight hex digits and one character of the
    // worktree is the least a name can be.
    let longest = "m".repeat(MAX_NAME - MARKER.len() - 1 - 8 - 1);
    assert!(database_names(&longest, "p", "feat+x", MAX_NAME).is_ok());
    let e = format!(
        "{:#}",
        database_names(&format!("{longest}m"), "p", "feat+x", MAX_NAME).unwrap_err()
    );
    assert!(e.contains("too long"), "{e}");
}

/// A deterministic spread of awkward worktree names, so the invariants
/// below are asked of more than the examples someone thought of.
fn awkward_worktrees() -> Vec<String> {
    let pieces = [
        "feat", "+", "-", "_", "/", "Ü", "x", "LONG", "9", ".", " ", "__", "日",
    ];
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut out = vec![String::new(), "a".repeat(300)];
    for _ in 0..400 {
        let mut name = String::new();
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let len = (seed >> 58) as usize;
        for _ in 0..len {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            name.push_str(pieces[(seed >> 33) as usize % pieces.len()]);
        }
        out.push(name);
    }
    out
}

// The whole safety argument for the names, over every awkward name above
// and a spread of main names: always inside the prefix, never the main
// database, never too long, always a plain identifier — and the second
// name never equal to the first unless both are hashed.
#[test]
fn every_name_is_inside_the_prefix_never_main_and_always_fits() {
    let mains = [
        "a".to_string(),
        "shop".into(),
        "northwind_traders".into(),
        "Shop_DB".into(),
        "shop__".into(),
        "m".repeat(MAX_NAME - MARKER.len() - 1 - 8 - 1),
    ];
    for main in &mains {
        let prefix = format!("{main}{MARKER}");
        for worktree in awkward_worktrees() {
            for name in names(main, &worktree) {
                assert!(name.starts_with(&prefix), "{main} {worktree:?}: {name}");
                assert!(name.len() > prefix.len(), "{main} {worktree:?}: {name}");
                assert!(!name.eq_ignore_ascii_case(main), "{main} {worktree:?}");
                assert!(name.len() <= MAX_NAME, "{main} {worktree:?}: {name}");
                assert!(is_plain(&name), "{main} {worktree:?}: {name}");
            }
        }
    }
}

// Postgres keeps 63 bytes of a name and cuts the rest without an error,
// so a name made at MariaDB's 64 was another database than the one pando
// recorded. An engine's own limit holds every name, and none goes past
// pando's.
#[test]
fn an_engines_shorter_limit_holds_every_name_and_none_passes_pandos() {
    for worktree in awkward_worktrees() {
        for name in database_names("northwind_traders", "p", &worktree, 63).unwrap() {
            assert!(name.len() <= 63, "{worktree:?}: {name}");
            assert!(name.starts_with("northwind_traders__"), "{name}");
        }
    }
    let fits = "y".repeat(63 - "shop__".len());
    assert_eq!(
        database_names("shop", "p", &fits, 63).unwrap()[0],
        format!("shop__{fits}")
    );
    assert_eq!(
        database_names("shop", "p", &format!("{fits}y"), 63).unwrap()[0].len(),
        63
    );
    let longest = "m".repeat(63 - MARKER.len() - 1 - 8 - 1);
    assert!(database_names(&longest, "p", "x", 63).is_ok());
    assert!(database_names(&format!("{longest}m"), "p", "x", 63).is_err());
    let wide = database_names("shop", "p", &"z".repeat(200), 200).unwrap();
    assert!(wide.iter().all(|name| name.len() == MAX_NAME), "{wide:?}");
}

// ---- the guard --------------------------------------------------------------

fn database(name: &str, main: &str) -> NamespaceRecord {
    NamespaceRecord {
        service: "mariadb".into(),
        recipe: "mariadb".into(),
        kind: NamespaceKind::Database,
        host: "localhost".into(),
        port: 3306,
        name: name.into(),
        main: main.into(),
        mains: Vec::new(),
        keys: Vec::new(),
        used_at: Utc::now(),
        server: None,
    }
}

fn slot(n: &str, main: &str) -> NamespaceRecord {
    NamespaceRecord {
        service: "redis".into(),
        recipe: "redis".into(),
        kind: NamespaceKind::Slot,
        host: "127.0.0.1".into(),
        port: 6379,
        name: n.into(),
        main: main.into(),
        mains: Vec::new(),
        keys: Vec::new(),
        used_at: Utc::now(),
        server: None,
    }
}

/// A state with each worktree holding the namespaces given.
fn state(worktrees: &[(&str, Vec<NamespaceRecord>)]) -> State {
    let mut state = State::new();
    for (name, namespaces) in worktrees {
        let mut record = WorktreeRecord::new(format!("/abs/{name}"), true);
        record.namespaces = namespaces.clone();
        state.worktrees.insert(name.to_string(), record);
    }
    state
}

fn refused(state: &State, worktree: &str, ns: &NamespaceRecord, main_now: Option<&str>) -> String {
    format!(
        "{:#}",
        may_drop(state, worktree, ns, main_now.as_slice(), &[])
            .expect_err("the guard let it through")
    )
}

#[test]
fn a_database_pando_made_for_this_worktree_may_be_dropped() {
    let ns = database("northwind_traders__feat_x", "northwind_traders");
    let st = state(&[("feat+x", vec![ns.clone()])]);
    may_drop(&st, "feat+x", &ns, &["northwind_traders"], &[]).unwrap();
    may_drop(&st, "feat+x", &ns, &[], &[]).unwrap();
}

#[test]
fn a_namespace_state_does_not_record_for_this_worktree_is_never_dropped() {
    let ns = database("northwind_traders__feat_x", "northwind_traders");
    let e = refused(&state(&[("feat+x", vec![])]), "feat+x", &ns, None);
    assert!(e.contains("not recorded"), "{e}");
    // Recorded for somebody else is not recorded for this one.
    let st = state(&[("feat+y", vec![ns.clone()]), ("feat+x", vec![])]);
    let e = refused(&st, "feat+x", &ns, None);
    assert!(e.contains("not recorded"), "{e}");
    // Nor is a worktree state has never heard of.
    let e = refused(&state(&[]), "feat+x", &ns, None);
    assert!(e.contains("not recorded"), "{e}");
    // A record that differs in any field is not the one state holds.
    let mut moved = ns.clone();
    moved.port = 3307;
    let e = refused(&state(&[("feat+x", vec![ns])]), "feat+x", &moved, None);
    assert!(e.contains("not recorded"), "{e}");
}

// The one that matters most. Whatever state says, the main checkout's own
// database is never dropped — not by the name recorded beside it, and not
// by the name its env files give now, in any case.
#[test]
fn the_main_database_is_refused_whatever_state_says() {
    for (name, main, main_now) in [
        ("northwind_traders", "northwind_traders", None),
        ("NORTHWIND_TRADERS", "northwind_traders", None),
        (
            "northwind_traders",
            "northwind_traders",
            Some("northwind_traders"),
        ),
        ("shop__feat_x", "shop", Some("shop__feat_x")),
        ("shop__feat_x", "shop", Some("SHOP__FEAT_X")),
    ] {
        let ns = database(name, main);
        let st = state(&[("feat+x", vec![ns.clone()])]);
        let e = refused(&st, "feat+x", &ns, main_now);
        assert!(e.contains("main checkout's own database"), "{name}: {e}");
    }
}

#[test]
fn a_database_without_the_marker_after_the_main_name_is_refused() {
    for name in [
        "northwind_traders_feat_x",
        "other__feat_x",
        "northwind_traders__",
        "xnorthwind_traders__feat_x",
        "feat_x__northwind_traders",
    ] {
        let ns = database(name, "northwind_traders");
        let st = state(&[("feat+x", vec![ns.clone()])]);
        let e = refused(&st, "feat+x", &ns, None);
        assert!(e.contains("does not start with"), "{name}: {e}");
    }
    // In another case it is still the marker: MariaDB on macOS agrees.
    let ns = database("NORTHWIND_TRADERS__feat_x", "northwind_traders");
    let st = state(&[("feat+x", vec![ns.clone()])]);
    may_drop(&st, "feat+x", &ns, &[], &[]).unwrap();
}

#[test]
fn a_database_name_a_statement_could_be_made_to_say_something_else_with_is_refused() {
    for name in [
        "shop__x`; DROP DATABASE shop; --",
        "shop__x'",
        "shop__x y",
        "shop__x.y",
        "shop__ünï",
    ] {
        let ns = database(name, "shop");
        let st = state(&[("feat+x", vec![ns.clone()])]);
        let e = refused(&st, "feat+x", &ns, None);
        assert!(e.contains("not a plain name"), "{name}: {e}");
    }
    let long = format!("shop__{}", "x".repeat(MAX_NAME));
    let ns = database(&long, "shop");
    let e = refused(&state(&[("feat+x", vec![ns.clone()])]), "feat+x", &ns, None);
    assert!(e.contains("at most 64"), "{e}");
}

#[test]
fn a_namespace_two_worktrees_claim_is_dropped_by_neither() {
    let ns = database("shop__feat_x", "shop");
    let mut elsewhere = ns.clone();
    // `localhost` and `127.0.0.1` are one server, and case is no
    // difference on MariaDB's side.
    elsewhere.host = "127.0.0.1".into();
    elsewhere.name = "SHOP__FEAT_X".into();
    let st = state(&[("feat+x", vec![ns.clone()]), ("feat-x", vec![elsewhere])]);
    let e = refused(&st, "feat+x", &ns, None);
    assert!(e.contains("recorded for feat-x as well"), "{e}");

    // The same name on another server is another database.
    let mut other_server = ns.clone();
    other_server.port = 3307;
    let st = state(&[("feat+x", vec![ns.clone()]), ("feat-x", vec![other_server])]);
    may_drop(&st, "feat+x", &ns, &[], &[]).unwrap();
}

#[test]
fn a_slot_pando_allocated_may_be_emptied_and_zero_and_mains_never() {
    let ns = slot("3", "0");
    let st = state(&[("feat+x", vec![ns.clone()])]);
    may_drop(&st, "feat+x", &ns, &["0"], &[]).unwrap();

    for (n, main, main_now, says) in [
        ("0", "0", None, "slot 0"),
        ("0", "5", None, "slot 0"),
        ("3", "3", None, "main checkout's own slot"),
        ("3", "0", Some("3"), "main checkout's own slot"),
        ("3", "0", Some(" 3 "), "main checkout's own slot"),
        ("x", "0", None, "not a slot number"),
        ("-1", "0", None, "not a slot number"),
        ("3; FLUSHALL", "0", None, "not a slot number"),
    ] {
        let ns = slot(n, main);
        let st = state(&[("feat+x", vec![ns.clone()])]);
        let e = refused(&st, "feat+x", &ns, main_now);
        assert!(e.contains(says), "{n} (main {main}, now {main_now:?}): {e}");
    }

    // Every slot the main checkout's env files name is its own, not only
    // the one it is known by: a queue's beside a cache's.
    let ns = slot("1", "0");
    let st = state(&[("feat+x", vec![ns.clone()])]);
    let e = format!(
        "{:#}",
        may_drop(&st, "feat+x", &ns, &["0", "1"], &[]).unwrap_err()
    );
    assert!(e.contains("main checkout's own slot"), "{e}");
}

#[test]
fn a_slot_another_worktree_holds_on_the_same_server_is_never_emptied() {
    let ns = slot("3", "0");
    let st = state(&[
        ("feat+x", vec![ns.clone()]),
        ("feat+y", vec![slot("3", "0")]),
    ]);
    let e = refused(&st, "feat+x", &ns, None);
    assert!(e.contains("feat+y"), "{e}");

    let mut other_server = slot("3", "0");
    other_server.port = 6380;
    let st = state(&[("feat+x", vec![ns.clone()]), ("feat+y", vec![other_server])]);
    may_drop(&st, "feat+x", &ns, &[], &[]).unwrap();
}

// A Redis on a port is the machine's: a slot a worktree of another project
// records, or its main checkout uses, is never emptied from this one.
#[test]
fn a_slot_another_project_records_is_never_emptied() {
    let ns = slot("3", "0");
    let st = state(&[("feat+x", vec![ns.clone()])]);
    let theirs = |namespaces: Vec<NamespaceRecord>| {
        vec![(
            "shop-1a2b3c4d".to_string(),
            Ok(state(&[("feat+y", namespaces)])),
        )]
    };
    let e = format!(
        "{:#}",
        may_drop(&st, "feat+x", &ns, &[], &theirs(vec![slot("3", "0")])).unwrap_err()
    );
    assert!(
        e.contains("recorded for feat+y of project shop-1a2b3c4d"),
        "{e}"
    );
    let e = format!(
        "{:#}",
        may_drop(&st, "feat+x", &ns, &[], &theirs(vec![slot("5", "3")])).unwrap_err()
    );
    assert!(
        e.contains("main checkout's own in project shop-1a2b3c4d"),
        "{e}"
    );

    let mut other_server = slot("3", "3");
    other_server.port = 6380;
    may_drop(&st, "feat+x", &ns, &[], &theirs(vec![other_server])).unwrap();
}

// A project whose state cannot be read may record the same one: nothing is
// dropped or emptied until it can be, and the refusal says which and why.
#[test]
fn nothing_is_dropped_while_another_projects_state_cannot_be_read() {
    for ns in [slot("3", "0"), database("shop__feat_x", "shop")] {
        let st = state(&[("feat+x", vec![ns.clone()])]);
        let unreadable = vec![(
            "shop-1a2b3c4d".to_string(),
            Err("parse state file /home/projects/shop-1a2b3c4d/state.json".to_string()),
        )];
        let e = format!(
            "{:#}",
            may_drop(&st, "feat+x", &ns, &[], &unreadable).unwrap_err()
        );
        assert!(
            e.contains("project shop-1a2b3c4d")
                && e.contains("could not be read")
                && e.contains("shop-1a2b3c4d/state.json"),
            "{e}"
        );
    }
}

#[test]
fn a_database_and_a_slot_of_the_same_name_are_not_the_same_namespace() {
    let mut as_slot = slot("3", "0");
    as_slot.port = 3306;
    as_slot.host = "localhost".into();
    let as_database = database("3", "0");
    assert!(!same_namespace(&as_slot, &as_database));
}

// The names and the guard, held to each other: every name pando would give
// a worktree is one the guard lets it drop, and the main database — in
// any case, recorded under any worktree — is one it never does.
#[test]
fn every_name_pando_gives_passes_the_guard_and_the_main_database_never_does() {
    for main in ["shop", "northwind_traders", "Shop_DB"] {
        for worktree in awkward_worktrees() {
            for name in names(main, &worktree) {
                let ns = database(&name, main);
                let st = state(&[("w", vec![ns.clone()])]);
                may_drop(&st, "w", &ns, &[main], &[])
                    .unwrap_or_else(|e| panic!("{main} {worktree:?}: {name} refused: {e:#}"));
            }
        }
        for spelled in [main.to_string(), main.to_uppercase(), main.to_lowercase()] {
            let ns = database(&spelled, main);
            let st = state(&[("w", vec![ns.clone()])]);
            assert!(may_drop(&st, "w", &ns, &[main], &[]).is_err(), "{spelled}");
            assert!(may_drop(&st, "w", &ns, &[], &[]).is_err(), "{spelled}");
        }
    }
}

// ---- the login ----------------------------------------------------------------

fn main_checkout(env: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), env).unwrap();
    dir
}

/// The env files at `root` alone, as a project with nothing below it has.
fn at(root: &std::path::Path) -> crate::services::EnvFiles {
    crate::services::EnvFiles::read(root, &[])
}

fn keys(keys: &[&str]) -> Vec<String> {
    keys.iter().map(|k| k.to_string()).collect()
}

#[test]
fn a_login_is_read_from_the_keys_beside_the_services_address() {
    let root = main_checkout(
        "DATABASE_HOST=localhost\nDATABASE_PORT=3306\nDATABASE_NAME=shop\n\
         DATABASE_USER=shop_user\nDATABASE_PASSWORD=s3cr3t:w0rd\n",
    );
    let login = login_from_env_files(&at(root.path()), &keys(&["DATABASE_PORT"]))
        .unwrap()
        .unwrap();
    assert_eq!(login.user.as_deref(), Some("shop_user"));
    assert_eq!(
        login.env(Some("MYSQL_PWD")),
        vec![("MYSQL_PWD".to_string(), "s3cr3t:w0rd".to_string())]
    );
    assert!(
        login.from.contains("DATABASE_USER and DATABASE_PASSWORD"),
        "{}",
        login.from
    );
    // The other spellings an app uses.
    let root = main_checkout("DB_PORT=3306\nDB_USERNAME=u\nDB_PASS=p\n");
    let login = login_from_env_files(&at(root.path()), &keys(&["DB_PORT"]))
        .unwrap()
        .unwrap();
    assert_eq!(login.user.as_deref(), Some("u"));
    assert_eq!(
        login.env(Some("X")),
        vec![("X".to_string(), "p".to_string())]
    );
}

#[test]
fn a_login_in_a_url_is_read_with_its_escapes_undone() {
    let root =
        main_checkout("DATABASE_URL=mysql://shop%40corp:p%40ss%3Aw0rd@localhost:3306/shop\n");
    let login = login_from_env_files(&at(root.path()), &keys(&["DATABASE_URL"]))
        .unwrap()
        .unwrap();
    assert_eq!(login.user.as_deref(), Some("shop@corp"));
    assert_eq!(
        login.env(Some("MYSQL_PWD")),
        vec![("MYSQL_PWD".to_string(), "p@ss:w0rd".to_string())]
    );
    assert!(login.from.contains("DATABASE_URL"), "{}", login.from);
    // A password alone, the way a development Redis is protected.
    let root = main_checkout("REDIS_URL=redis://:only-a-password@localhost:6379/0\n");
    let login = login_from_env_files(&at(root.path()), &keys(&["REDIS_URL"]))
        .unwrap()
        .unwrap();
    assert_eq!(login.user, None);
    assert!(login.has_password());
}

#[test]
fn a_url_with_no_login_in_it_and_no_keys_beside_it_is_no_login() {
    let root = main_checkout("DATABASE_URL=mysql://localhost:3306/shop\nREDIS_PORT=6379\n");
    assert_eq!(
        login_from_env_files(&at(root.path()), &keys(&["DATABASE_URL"])),
        Ok(None)
    );
    assert_eq!(
        login_from_env_files(&at(root.path()), &keys(&["REDIS_PORT"])),
        Ok(None)
    );
    assert_eq!(login_from_env_files(&at(root.path()), &keys(&[])), Ok(None));
    // Empty values say nothing either.
    let root = main_checkout("DATABASE_PORT=3306\nDATABASE_USER=\nDATABASE_PASSWORD=\n");
    assert_eq!(
        login_from_env_files(&at(root.path()), &keys(&["DATABASE_PORT"])),
        Ok(None)
    );
}

#[test]
fn the_login_written_for_pando_is_used_when_the_env_files_have_none_that_will_do() {
    let file = std::path::Path::new("/home/.pando/projects/p/pando.toml");
    let mut config = crate::config::Config::default();
    config.namespaced.insert(
        "mariadb".into(),
        crate::config::LoginConfig {
            user: Some("root".into()),
            password: Some("hunter2".into()),
            ..Default::default()
        },
    );
    // Nothing in the env files: the one written down.
    let root = main_checkout("DATABASE_PORT=3306\n");
    let login = find_login(
        &at(root.path()),
        &config,
        "mariadb",
        &keys(&["DATABASE_PORT"]),
        true,
        file,
    )
    .unwrap()
    .unwrap();
    assert_eq!(login.user.as_deref(), Some("root"));
    assert!(
        login.from.contains("[namespaced.mariadb]"),
        "{}",
        login.from
    );
    // A password with no user will not do for an engine that logs in as
    // somebody, so the one written down wins over it…
    let root = main_checkout("DATABASE_PORT=3306\nDATABASE_PASSWORD=x\n");
    let login = find_login(
        &at(root.path()),
        &config,
        "mariadb",
        &keys(&["DATABASE_PORT"]),
        true,
        file,
    )
    .unwrap()
    .unwrap();
    assert_eq!(login.user.as_deref(), Some("root"));
    // …and does for one that does not.
    let login = find_login(
        &at(root.path()),
        &config,
        "redis",
        &keys(&["DATABASE_PORT"]),
        false,
        file,
    )
    .unwrap()
    .unwrap();
    assert_eq!(login.user, None);
    // The main checkout's own login, when it has one, beats pando's.
    let root = main_checkout("DATABASE_PORT=3306\nDATABASE_USER=app\n");
    let login = find_login(
        &at(root.path()),
        &config,
        "mariadb",
        &keys(&["DATABASE_PORT"]),
        true,
        file,
    )
    .unwrap()
    .unwrap();
    assert_eq!(login.user.as_deref(), Some("app"));
    // Nothing anywhere is nothing.
    let root = main_checkout("DATABASE_PORT=3306\n");
    let none = crate::config::Config::default();
    assert!(
        find_login(
            &at(root.path()),
            &none,
            "mariadb",
            &keys(&["DATABASE_PORT"]),
            true,
            file
        )
        .unwrap()
        .is_none()
    );
}

// Tried as written, the login was a user called `${DB_USER}`: the one
// written down for pando is used instead, and with none, the variable is
// named.
#[test]
fn a_login_that_holds_a_reference_nothing_sets_is_not_tried() {
    let file = std::path::Path::new("/home/.pando/projects/p/pando.toml");
    let root = main_checkout(
        "DATABASE_PORT=3306\nDATABASE_USER=${PANDO_TEST_UNSET_USER}\nDATABASE_PASSWORD=pw\n",
    );
    let keys = keys(&["DATABASE_PORT"]);
    let e = login_from_env_files(&at(root.path()), &keys).unwrap_err();
    assert_eq!(e.key, "DATABASE_USER");
    assert_eq!(e.reference, "${PANDO_TEST_UNSET_USER}");
    let none = crate::config::Config::default();
    assert_eq!(
        find_login(&at(root.path()), &none, "mariadb", &keys, true, file).unwrap_err(),
        e
    );
    let mut config = crate::config::Config::default();
    config.namespaced.insert(
        "mariadb".into(),
        crate::config::LoginConfig {
            user: Some("root".into()),
            password: None,
            ..Default::default()
        },
    );
    let login = find_login(&at(root.path()), &config, "mariadb", &keys, true, file)
        .unwrap()
        .unwrap();
    assert_eq!(login.user.as_deref(), Some("root"));
}

// A bare `$DB_USER` is a variable, set perhaps in a file pando does not
// read; tried as written, the login was a user called `$DB_USER`.
#[test]
fn a_login_that_holds_a_bare_variable_nothing_sets_is_not_tried() {
    let root = main_checkout(
        "DATABASE_PORT=3306\nDATABASE_USER=$PANDO_TEST_UNSET_USER\nDATABASE_PASSWORD=pw\n",
    );
    let e = login_from_env_files(&at(root.path()), &keys(&["DATABASE_PORT"])).unwrap_err();
    assert_eq!(e.key, "DATABASE_USER");
    assert_eq!(e.reference, "$PANDO_TEST_UNSET_USER");
}

// A password with a `$` in it stopped a namespaced start, taken for a
// variable nothing sets. The app's loader reads it as written; so does
// the login.
#[test]
fn a_password_with_a_bare_dollar_is_the_login_as_written() {
    let root = main_checkout(
        "DATABASE_PORT=3306\nDATABASE_USER=app\nDATABASE_PASSWORD=pa$pando_test_unset_word\n",
    );
    let login = login_from_env_files(&at(root.path()), &keys(&["DATABASE_PORT"]))
        .unwrap()
        .unwrap();
    assert_eq!(login.user.as_deref(), Some("app"));
    assert_eq!(
        login.env(Some("MYSQL_PWD")),
        vec![(
            "MYSQL_PWD".to_string(),
            "pa$pando_test_unset_word".to_string()
        )]
    );
}

// A config and a login both end up in error messages and `{:?}`s; neither
// may carry the password there.
#[test]
fn a_password_is_never_in_what_a_login_or_its_config_prints() {
    let login = Login::new(Some("app".into()), Some("hunter2".into()), "somewhere");
    let shown = format!("{login:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    assert!(shown.contains("hidden") && shown.contains("app"), "{shown}");
    let config = crate::config::LoginConfig {
        user: Some("app".into()),
        password: Some("hunter2".into()),
        ..Default::default()
    };
    let shown = format!("{config:?}");
    assert!(!shown.contains("hunter2"), "{shown}");
    // And the only way out is the environment of the command it is for.
    assert_eq!(login.env(None), Vec::new());
    assert_eq!(Login::none().env(Some("MYSQL_PWD")), Vec::new());
}

// ---- the engine, against a fake client -------------------------------------------

/// A pando `bin` with a fake client in it that records what it was run
/// with — its arguments and the password variable — and behaves as the
/// files in its directory say.
struct FakeClient {
    dir: tempfile::TempDir,
}

impl FakeClient {
    fn new(name: &str, password_env: &str, body: &str) -> FakeClient {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().display().to_string();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/bin/sh\nstate='{state}'\nprintf '%s\\n' \"$*\" >> \"$state/argv\"\n\
             printf '%s\\n' \"${{{password_env}-unset}}\" >> \"$state/env\"\n\
             for last; do :; done\n{body}"
        );
        std::fs::write(bin.join(name), script).unwrap();
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        FakeClient { dir }
    }

    fn bin(&self) -> std::path::PathBuf {
        self.dir.path().join("bin")
    }

    fn touch(&self, file: &str) {
        std::fs::write(self.dir.path().join(file), "").unwrap();
    }

    fn write(&self, file: &str, text: &str) {
        std::fs::write(self.dir.path().join(file), text).unwrap();
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(file)).unwrap_or_default()
    }
}

const FAKE_MARIADB: &str = r#"case "$*" in
  *"SELECT 1"*) echo 1 ;;
  *"CURRENT_USER()"*) echo "app@localhost" ;;
  *"CREATE DATABASE"*)
    if [ -f "$state/deny" ]; then
      echo "ERROR 1044 (42000) at line 1: Access denied for user 'app'@'localhost' to database 'shop__feat_x'" >&2; exit 1
    fi
    if [ -f "$state/exists" ]; then
      echo "ERROR 1007 (HY000) at line 1: Can't create database 'shop__feat_x'; database exists" >&2; exit 1
    fi
    touch "$state/exists" ;;
  *"SCHEMATA"*) if [ -f "$state/exists" ]; then echo "SHOP__FEAT_X"; fi ;;
  *"DROP DATABASE"*) rm -f "$state/exists" ;;
  *) echo "unexpected: $*" >&2; exit 9 ;;
esac
"#;

const PASSWORD: &str = "hunter2 'quoted' $HOME";

fn recipe_namespace(name: &str) -> crate::recipes::NamespaceRecipe {
    crate::recipes::Recipes::built_in()
        .get(name)
        .unwrap()
        .recipe
        .namespace
        .clone()
        .unwrap()
}

fn server<'a>(
    recipe: &'a crate::recipes::NamespaceRecipe,
    fake: &FakeClient,
    service: &'a str,
) -> Server<'a> {
    Server {
        service,
        recipe,
        host: "localhost".into(),
        port: 3306,
        login: Login::new(Some("app".into()), Some(PASSWORD.into()), "the test's env"),
        bin_dir: fake.bin(),
        runner: Runner::Host,
    }
}

// ---- the client in the container ---------------------------------------------

/// A client no machine has, so only the container's can answer.
const INSIDE_CLIENT: &str = "pando-test-sql";

const FAKE_SQL: &str = r#"case "$*" in
  *" ping") echo ok ;;
  *" exists "*) if [ -f "$state/exists" ]; then echo "$last"; fi ;;
  *" create "*) if [ -f "$state/exists" ]; then echo "exists" >&2; exit 1; fi; touch "$state/exists" ;;
  *" drop "*) rm -f "$state/exists" ;;
  *) echo "unexpected: $*" >&2; exit 9 ;;
esac
"#;

/// A `[namespace]` whose client is [`INSIDE_CLIENT`].
fn inside_recipe() -> crate::recipes::NamespaceRecipe {
    let text = format!(
        "kind = \"service\"\nname = \"inside\"\n\n[namespace]\nkind = \"database\"\n\
         binaries = [\"{c}\"]\npassword_env = \"TEST_PWD\"\n\
         ping = \"{c} -h {{host}} -p {{port}} ping\"\n\
         exists = \"{c} -h {{host}} -p {{port}} exists {{namespace}}\"\n\
         create = \"{c} -h {{host}} -p {{port}} create {{namespace}}\"\n\
         drop = \"{c} -h {{host}} -p {{port}} drop {{namespace}}\"\n",
        c = INSIDE_CLIENT
    );
    crate::recipes::parse(&text).unwrap().namespace.unwrap()
}

/// The fake client moved out of the host's `bin` into the container's,
/// and a fake `docker` in the host's that publishes `published` from a
/// container `c0ffee` listening on 5432 inside — or no container at all.
fn containerised(published: Option<u16>) -> FakeClient {
    use std::os::unix::fs::PermissionsExt;
    let fake = FakeClient::new(INSIDE_CLIENT, "TEST_PWD", FAKE_SQL);
    let inside = fake.dir.path().join("inside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::rename(fake.bin().join(INSIDE_CLIENT), inside.join(INSIDE_CLIENT)).unwrap();
    let state = fake.dir.path().display();
    // Docker's `--filter publish=` matches the port inside the container,
    // 5432 here, never the one published on the host: a stand-in that
    // ignored it hid a lookup by the host's port that finds nothing.
    let ps = match published {
        Some(_) => {
            "f=$(printf '%s\\n' \"$*\" | sed -n 's/.*publish=\\([0-9]*\\).*/\\1/p'); \
             if [ -z \"$f\" ] || [ \"$f\" = 5432 ]; then echo c0ffee; fi"
        }
        None => ":",
    };
    let port = published.unwrap_or(0);
    let docker = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{state}/docker'\n\
         case \"$1\" in\n\
           ps) {ps} ;;\n\
           port) echo \"5432/tcp -> 0.0.0.0:{port}\"; echo \"5432/tcp -> [::]:{port}\" ;;\n\
           exec) shift; while [ \"$1\" = -e ]; do shift 2; done; shift; shift; shift;\n\
             PATH='{state}/inside':\"$PATH\" exec sh -c \"$1\" ;;\n\
         esac\n"
    );
    std::fs::write(fake.bin().join("docker"), docker).unwrap();
    std::fs::set_permissions(
        fake.bin().join("docker"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fake
}

// A database in Docker leaves the host with no client, and the start
// stopped saying what to install. The container that publishes the
// server's port has its own: the commands run there, at its loopback and
// the port inside, with the password passed by name and never written.
#[test]
fn a_server_in_a_container_is_reached_through_the_client_its_image_ships() {
    let fake = containerised(Some(15432));
    let recipe = inside_recipe();
    let db = Server {
        port: 15432,
        ..server(&recipe, &fake, "db")
    }
    .reach();
    assert_eq!(
        db.runner,
        Runner::Container {
            id: "c0ffee".into(),
            port: 5432
        }
    );
    db.ping().unwrap();
    assert_eq!(db.create("shop__feat_x", "shop").unwrap(), Created::Made);
    assert!(db.exists("shop__feat_x").unwrap());
    db.drop("shop__feat_x", "shop").unwrap();
    assert!(!db.exists("shop__feat_x").unwrap());

    let argv = fake.read("argv");
    assert!(
        argv.contains("-h 127.0.0.1 -p 5432 create shop__feat_x"),
        "{argv}"
    );
    assert!(
        fake.read("env").lines().all(|line| line == PASSWORD),
        "the client inside read the password: {}",
        fake.read("env")
    );
    let docker = fake.read("docker");
    assert!(docker.contains("exec -e TEST_PWD c0ffee sh -c"), "{docker}");
    assert!(!docker.contains("hunter2"), "{docker}");
    let by_hand = db.by_hand("shop__feat_x").unwrap();
    assert!(
        by_hand.starts_with("docker exec -e 'TEST_PWD' 'c0ffee' sh -c "),
        "{by_hand}"
    );
    assert!(!by_hand.contains("hunter2"), "{by_hand}");
}

// The container is the server only when it alone publishes the port where
// this machine's loopback reaches it: two of them, or one published on
// another address, is a choice pando does not make.
#[test]
fn a_container_is_taken_for_the_server_only_when_it_alone_publishes_the_port_here() {
    let pick = |listing: &str| super::engine::container_publishing(listing, 15432);
    assert_eq!(
        pick("--\nc0ffee\n5432/tcp -> 0.0.0.0:15432\n5432/tcp -> [::]:15432\n"),
        Some(("c0ffee".into(), 5432))
    );
    assert_eq!(
        pick("--\nc0ffee\n5432/tcp -> 127.0.0.1:15432\n"),
        Some(("c0ffee".into(), 5432))
    );
    assert_eq!(
        pick("--\nc0ffee\n5432/tcp -> 0.0.0.0:15432\nbeef\n5432/tcp -> 0.0.0.0:15432\n"),
        None
    );
    assert_eq!(pick("--\nc0ffee\n5432/tcp -> 192.168.1.5:15432\n"), None);
    assert_eq!(pick("--\nc0ffee\n5432/tcp -> 0.0.0.0:25432\n"), None);
}

// A server the app reaches somewhere else, or a native one listening on
// the same port beside a container, is never mistaken for the container.
#[test]
fn a_remote_server_or_a_native_listener_keeps_the_commands_here() {
    let fake = containerised(Some(15432));
    let recipe = inside_recipe();
    let remote = Server {
        port: 15432,
        host: "db.internal".into(),
        ..server(&recipe, &fake, "db")
    }
    .reach();
    assert_eq!(remote.runner, Runner::Host);

    use std::os::unix::fs::PermissionsExt;
    let lsof = fake.bin().join("lsof");
    std::fs::write(&lsof, "#!/bin/sh\necho p1\necho cpostgres\n").unwrap();
    std::fs::set_permissions(&lsof, std::fs::Permissions::from_mode(0o755)).unwrap();
    let native = Server {
        port: 15432,
        ..server(&recipe, &fake, "db")
    }
    .reach();
    assert_eq!(native.runner, Runner::Host);
}

// No container publishes the port: the server stays reached from here,
// and the start says what to install, as before.
#[test]
fn with_no_container_the_missing_client_is_still_named() {
    let fake = containerised(None);
    let recipe = inside_recipe();
    let db = Server {
        port: 15432,
        ..server(&recipe, &fake, "db")
    }
    .reach();
    assert_eq!(db.runner, Runner::Host);
    let e = format!("{:#}", db.ping().unwrap_err());
    assert!(
        e.contains(INSIDE_CLIENT) && e.contains("not on PATH"),
        "{e}"
    );
}

// The whole round, and the password in exactly one place: the variable the
// client reads. Not an argument of the client, and not in the script pando
// hands `bash -lc`, which anyone on the machine can read in `ps`.
// `postgres://u:p@[::1]:5432/shop` names its host `[::1]`, and a client
// told `-h '[::1]'` cannot resolve it: the address goes without brackets.
#[test]
fn an_ipv6_host_reaches_the_client_without_its_brackets() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    let recipe = recipe_namespace("mariadb");
    let db = Server {
        host: "[::1]".into(),
        ..server(&recipe, &fake, "mariadb")
    };
    db.ping().unwrap();
    let argv = fake.read("argv");
    assert!(argv.contains("::1"), "{argv}");
    assert!(!argv.contains("[::1]"), "{argv}");
}

#[test]
fn a_database_is_made_found_and_dropped_with_the_password_in_the_environment_alone() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    let recipe = recipe_namespace("mariadb");
    let db = server(&recipe, &fake, "mariadb");
    db.ping().unwrap();
    assert!(!db.exists("shop__feat_x").unwrap());
    assert_eq!(db.create("shop__feat_x", "shop").unwrap(), Created::Made);
    assert!(
        db.exists("shop__feat_x").unwrap(),
        "found without case, as MariaDB compares"
    );
    assert_eq!(
        db.create("shop__feat_x", "shop").unwrap(),
        Created::AlreadyThere,
        "one already there is not one pando made"
    );
    db.drop("shop__feat_x", "shop").unwrap();
    assert!(!db.exists("shop__feat_x").unwrap());

    let argv = fake.read("argv");
    assert!(argv.contains("-h localhost -P 3306 -u app"), "{argv}");
    assert!(argv.contains("CREATE DATABASE `shop__feat_x`"), "{argv}");
    // Made in main's shape: its character set and collation, looked up by
    // its name.
    assert!(
        argv.contains("WHERE SCHEMA_NAME = 'shop'")
            && argv.contains("CHARACTER SET ', DEFAULT_CHARACTER_SET_NAME")
            && argv.contains("' COLLATE ', DEFAULT_COLLATION_NAME"),
        "{argv}"
    );
    assert!(
        !argv.contains("hunter2"),
        "the password reached the client's arguments: {argv}"
    );
    // Nor the script `bash -lc` runs, which is the other command line `ps`
    // shows while it runs — for every command the recipe has.
    for command in [
        Some(recipe.ping.as_str()),
        recipe.exists.as_deref(),
        Some(recipe.drop.as_str()),
        recipe.account.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let script = db.script(command, Some("shop__feat_x")).unwrap();
        assert!(script.contains("mariadb"), "{script}");
        assert!(!script.contains("hunter2"), "{script}");
    }
    let create = db.create_script("shop__feat_x", "shop").unwrap();
    assert!(create.contains("mariadb"), "{create}");
    assert!(!create.contains("hunter2"), "{create}");
    let env = fake.read("env");
    assert!(env.lines().all(|line| line == PASSWORD), "{env}");
}

// Decision 4: a login that may not make the namespace stops the start with
// nothing made, and the refusal is the one statement that fixes it —
// scoped to the main database's prefix, for the account the server sees.
#[test]
fn a_login_that_may_not_make_a_database_is_refused_with_the_grant_that_lets_it() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    fake.touch("deny");
    let recipe = recipe_namespace("mariadb");
    let db = server(&recipe, &fake, "mariadb");
    let e = format!("{:#}", db.create("shop__feat_x", "shop").unwrap_err());
    assert!(
        e.contains("GRANT ALL ON `shop\\_\\_%`.* TO 'app'@'localhost';"),
        "{e}"
    );
    assert!(e.contains("Nothing was made"), "{e}");
    assert!(e.contains("the test's env"), "it says whose login: {e}");
    assert!(!e.contains("hunter2"), "{e}");
    assert!(!fake.dir.path().join("exists").exists());
}

// The last line before the statement runs holds its own: a name that is
// not a worktree's is refused before anything is asked of the server.
#[test]
fn the_engine_refuses_to_drop_anything_that_is_not_a_namespace() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    let recipe = recipe_namespace("mariadb");
    let db = server(&recipe, &fake, "mariadb");
    for name in ["shop", "shop`; DROP DATABASE shop; --", "", "a b__c"] {
        assert!(db.drop(name, "shop").is_err(), "{name:?}");
        assert!(db.exists(name).is_err() || name == "shop", "{name:?}");
    }
    assert!(
        !fake.read("argv").contains("DROP"),
        "a drop reached the server: {}",
        fake.read("argv")
    );
}

// The main database's name comes from an env file, and the listing put it
// in a command unchecked: `$(…)` in it ran in the shell before the client
// was ever started. It is refused like any name that is not plain, and
// nothing runs.
#[test]
fn a_main_name_that_is_not_plain_is_never_put_in_a_listing() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    let recipe = recipe_namespace("mariadb");
    let db = server(&recipe, &fake, "mariadb");
    let ran = fake.dir.path().join("ran");
    let main = format!("shop$(touch {})", ran.display());
    let e = format!("{:#}", db.list(&main).unwrap_err());
    assert!(e.contains("not a plain name"), "{e}");
    assert!(!ran.exists(), "the shell ran what the env file said");
    assert_eq!(fake.read("argv"), "", "the client was run");
    assert!(db.grant(&main).is_none(), "and no grant is printed for it");
    assert!(db.grant("shop").is_some());
}

// A name the server lists can be anything its owner typed. Only one the
// engine would drop itself is printed as a drop: the command printed for
// `shop__a`; DROP DATABASE `shop` dropped the main database with it.
#[test]
fn a_drop_by_hand_is_printed_only_for_a_name_the_engine_would_drop() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    let recipe = recipe_namespace("mariadb");
    let db = server(&recipe, &fake, "mariadb");
    assert!(
        db.by_hand("shop__feat_x")
            .is_some_and(|c| c.contains("DROP DATABASE IF EXISTS `shop__feat_x`")),
        "{:?}",
        db.by_hand("shop__feat_x")
    );
    for name in ["shop__a`; DROP DATABASE `shop", "shop__a'b", "shop", ""] {
        assert_eq!(db.by_hand(name), None, "{name:?}");
    }

    let fake = FakeClient::new("redis-cli", "REDISCLI_AUTH", FAKE_REDIS);
    let recipe = recipe_namespace("redis");
    let cache = server(&recipe, &fake, "redis");
    assert!(cache.by_hand("3").is_some());
    for name in ["0", "3; FLUSHALL", "x"] {
        assert_eq!(cache.by_hand(name), None, "{name:?}");
    }
}

const FAKE_REDIS: &str = r#"case "$*" in
  *" ping") echo PONG ;;
  *DBSIZE*)
    if [ "$last" -gt 15 ]; then echo "ERR DB index is out of range" >&2; exit 1; fi
    cat "$state/size-$last" 2>/dev/null || echo 0 ;;
  *FLUSHDB*) echo "$last" >> "$state/flushed"; echo OK ;;
  *) echo "unexpected: $*" >&2; exit 9 ;;
esac
"#;

#[test]
fn a_slot_is_sized_and_emptied_by_its_own_number_and_never_by_a_fallback() {
    let fake = FakeClient::new("redis-cli", "REDISCLI_AUTH", FAKE_REDIS);
    let recipe = recipe_namespace("redis");
    let mut cache = server(&recipe, &fake, "redis");
    cache.port = 6379;
    cache.login = Login::new(None, Some(PASSWORD.into()), "the test's env");
    cache.ping().unwrap();
    fake.write("size-3", "2\n");
    fake.write("size-4", "7\n");
    assert_eq!(cache.size(3).unwrap(), 2);
    assert_eq!(cache.size(4).unwrap(), 7);
    assert_eq!(cache.size(5).unwrap(), 0);
    // A number wrapped in a client's own decoration is not the contract:
    // the recipe prints a bare one, and anything else is no answer.
    fake.write("size-6", "(integer) 7\n");
    assert!(cache.size(6).is_err());
    let e = format!("{:#}", cache.size(16).unwrap_err());
    assert!(e.contains("out of range"), "{e}");

    cache.drop("3", "0").unwrap();
    for name in ["0", "x", "-1", "3 4", ""] {
        assert!(cache.drop(name, "0").is_err(), "{name:?}");
    }
    assert_eq!(fake.read("flushed"), "3\n", "only slot 3 was ever emptied");
    let argv = fake.read("argv");
    assert!(argv.contains("-e -h localhost -p 6379 EVAL"), "{argv}");
    assert!(!argv.contains(" -n "), "{argv}");
    assert!(!argv.contains("hunter2"), "{argv}");
    for command in [
        recipe.ping.as_str(),
        &recipe.drop,
        recipe.size.as_deref().unwrap(),
    ] {
        assert!(
            !cache
                .script(command, Some("3"))
                .unwrap()
                .contains("hunter2")
        );
    }
}

// A slot has no grant to print and no `<main>__` to scope one to: its
// refusal names the slot, says whose connection, and ends each sentence.
#[test]
fn a_slot_the_server_will_not_let_pando_empty_is_refused_in_sentences_about_the_slot() {
    let fake = FakeClient::new(
        "redis-cli",
        "REDISCLI_AUTH",
        "echo \"(error) NOPERM this user has no permissions to run the 'flushdb' command\"; \
         exit 1\n",
    );
    let recipe = recipe_namespace("redis");
    let mut cache = server(&recipe, &fake, "redis");
    cache.port = 6379;
    cache.login = Login::none();
    let e = format!("{:#}", cache.drop("3", "0").unwrap_err());
    assert_eq!(
        e,
        "redis on localhost:6379 does not let a connection with no login empty slot 3 — give \
         it the right to empty slot 3 on that server. Nothing was dropped."
    );

    cache.login = Login::new(Some("app".into()), Some(PASSWORD.into()), "the test's env");
    let e = format!("{:#}", cache.drop("3", "0").unwrap_err());
    assert!(
        e.starts_with("redis on localhost:6379 does not let the login from the test's env"),
        "{e}"
    );
    assert!(
        !e.contains("databases named") && !e.contains("hunter2"),
        "{e}"
    );
}

// A database recipe with no grant to print still ends its sentences, and
// still says the right is to `<main>__…` alone.
#[test]
fn a_database_refusal_with_no_grant_to_print_says_the_right_it_needs() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    fake.touch("deny");
    let mut recipe = recipe_namespace("mariadb");
    recipe.grant = None;
    let db = server(&recipe, &fake, "mariadb");
    let e = format!("{:#}", db.create("shop__feat_x", "shop").unwrap_err());
    assert_eq!(
        e,
        "mariadb on localhost:3306 does not let the login from the test's env make or drop \
         shop__feat_x — give it the right to make and drop databases named shop__… on that \
         server. Nothing was made."
    );
}

#[test]
fn a_missing_client_is_named_before_anything_is_asked() {
    let fake = FakeClient::new("not-mariadb", "MYSQL_PWD", "exit 0\n");
    let mut recipe = recipe_namespace("mariadb");
    recipe.binaries = vec!["pando-test-no-such-client".into()];
    let db = server(&recipe, &fake, "mariadb");
    let e = format!("{:#}", db.ping().unwrap_err());
    assert!(e.contains("pando-test-no-such-client"), "{e}");
    assert!(e.contains("not on PATH"), "{e}");
    // The server in Docker leaves the host no client: what to install.
    assert!(
        e.contains("install it (brew install mariadb, or your distribution's mariadb-client)"),
        "{e}"
    );
}

#[test]
fn a_prefix_is_an_sql_pattern_matching_only_itself_and_what_follows() {
    assert_eq!(
        prefix_like("northwind_traders"),
        "northwind\\_traders\\_\\_%"
    );
    assert_eq!(prefix_like("a%b"), "a\\%b\\_\\_%");
    assert_eq!(prefix_like("shop"), "shop\\_\\_%");
}

// A main database whose name is not plain is not put in a command: the
// lookup of its shape finds nothing, and the server's default is used.
#[test]
fn a_main_that_is_not_a_plain_name_stays_out_of_the_create() {
    let fake = FakeClient::new("mariadb", "MYSQL_PWD", FAKE_MARIADB);
    let recipe = recipe_namespace("mariadb");
    let db = server(&recipe, &fake, "mariadb");
    assert_eq!(db.create("shop__feat_x", "sh'op").unwrap(), Created::Made);
    let argv = fake.read("argv");
    assert!(argv.contains("WHERE SCHEMA_NAME = ''"), "{argv}");
    assert!(!argv.contains("sh'op"), "{argv}");
}
