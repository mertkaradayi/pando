//! Namespaced mode against real servers, gated by `PANDO_TEST_NATIVE=1`.
//!
//! Namespaced mode writes into a server the developer owns, so what a fake
//! client cannot honestly claim is pinned here, against throwaway servers
//! this test starts itself: a MariaDB with its grant tables on, where a
//! login really is refused until the printed grant is run, a Postgres
//! with password authentication, where it is refused until the printed
//! `ALTER ROLE` is, and a Redis with a password, where emptying one slot
//! really leaves slot 0 alone.
//!
//! Everything is inside a temporary directory, on ports the kernel handed
//! out, and stopped when the test ends — never the developer's own
//! servers. Run it with:
//!
//! ```text
//! PANDO_TEST_NATIVE=1 cargo test --test integration namespaced:: -- --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pando::namespace::{Created, Login, Server};
use pando::recipes::{NamespaceRecipe, Recipes};
use tempfile::TempDir;

const APP_PASSWORD: &str = "p@ss w'rd $x";

fn enabled() -> bool {
    std::env::var("PANDO_TEST_NATIVE").as_deref() == Ok("1")
}

fn missing(binaries: &[&str]) -> Vec<String> {
    binaries
        .iter()
        .filter(|binary| {
            !Command::new("sh")
                .arg("-c")
                .arg(format!("command -v {binary}"))
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .map(|binary| binary.to_string())
        .collect()
}

fn skip(binaries: &[&str]) -> bool {
    if !enabled() {
        eprintln!("skipping: set PANDO_TEST_NATIVE=1 to run against real servers");
        return true;
    }
    let missing = missing(binaries);
    if !missing.is_empty() {
        eprintln!("skipping: this machine has no {}", missing.join(", "));
        return true;
    }
    false
}

fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn recipe(name: &str) -> NamespaceRecipe {
    Recipes::built_in()
        .get(name)
        .unwrap()
        .recipe
        .namespace
        .clone()
        .unwrap()
}

/// A server this test started, stopped when it goes out of scope.
struct Throwaway {
    dir: TempDir,
    port: u16,
    child: Child,
}

impl Drop for Throwaway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_for(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    panic!("{what} never came up");
}

// ---- MariaDB ------------------------------------------------------------------

/// A MariaDB with its grant tables on — so logins are real — holding the
/// main checkout's database `shop` and an app login that may use it and
/// nothing else, as a developer's own server would.
fn mariadb() -> Throwaway {
    // One at a time: two bootstraps at once trip over each other's
    // temporary tables ("Unknown table 'mysql.tmp_user_sys'").
    static INSTALL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("data");
    let installing = INSTALL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let init = Command::new("mariadb-install-db")
        .arg(format!("--datadir={}", data.display()))
        .arg("--auth-root-authentication-method=normal")
        .output()
        .unwrap();
    drop(installing);
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let port = free_port();
    let child = Command::new("mariadbd")
        .arg(format!("--datadir={}", data.display()))
        .arg(format!("--port={port}"))
        .arg("--bind-address=127.0.0.1")
        .arg(format!("--socket={}", dir.path().join("s.sock").display()))
        .arg(format!("--pid-file={}", dir.path().join("pid").display()))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let server = Throwaway { dir, port, child };
    wait_for("mariadb", || root_sql(&server, "SELECT 1").is_ok());
    root_sql(
        &server,
        &format!(
            "CREATE DATABASE shop; CREATE USER 'app'@'localhost' IDENTIFIED BY '{}'; \
             GRANT ALL ON shop.* TO 'app'@'localhost';",
            APP_PASSWORD.replace('\'', "''")
        ),
    )
    .unwrap();
    server
}

/// SQL as root, the server's administrator.
fn root_sql(server: &Throwaway, sql: &str) -> Result<String, String> {
    let out = Command::new("mariadb")
        .args(["--protocol=tcp", "-h", "127.0.0.1", "-P"])
        .arg(server.port.to_string())
        .args(["-u", "root", "-N", "-B", "-e", sql])
        .output()
        .unwrap();
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        false => Err(String::from_utf8_lossy(&out.stderr).to_string()),
    }
}

fn app_server<'a>(recipe: &'a NamespaceRecipe, db: &Throwaway, bin: &Path) -> Server<'a> {
    Server {
        service: "mariadb",
        recipe,
        host: "127.0.0.1".into(),
        port: db.port,
        login: Login::new(
            Some("app".into()),
            Some(APP_PASSWORD.into()),
            "the test's app login",
        ),
        bin_dir: bin.to_path_buf(),
    }
}

// Decision 4, end to end: the app's login is refused until the grant pando
// prints is run once, and then it may make and drop `shop__…` — and still
// nothing else, which is the server's own wall behind pando's guard.
#[test]
fn a_real_mariadb_makes_a_worktrees_database_once_the_printed_grant_is_run() {
    if skip(&["mariadb-install-db", "mariadbd", "mariadb"]) {
        return;
    }
    let db = mariadb();
    let recipe = recipe("mariadb");
    let bin = db.dir.path().join("bin");
    let server = app_server(&recipe, &db, &bin);
    server.ping().expect("the app login answers");

    let refused = format!("{:#}", server.create("shop__feat_x", "shop").unwrap_err());
    assert!(refused.contains("Nothing was made"), "{refused}");
    assert!(!refused.contains("p@ss"), "{refused}");
    let grant = refused
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("GRANT "))
        .unwrap_or_else(|| panic!("no grant in: {refused}"))
        .to_string();
    assert_eq!(grant, "GRANT ALL ON `shop\\_\\_%`.* TO 'app'@'localhost';");
    assert_eq!(
        root_sql(&db, "SHOW DATABASES LIKE 'shop\\_\\_%'").unwrap(),
        ""
    );

    root_sql(&db, &grant).expect("the grant pando printed runs as written");
    assert!(!server.exists("shop__feat_x").unwrap());
    assert_eq!(
        server.create("shop__feat_x", "shop").unwrap(),
        Created::Made
    );
    assert!(server.exists("shop__feat_x").unwrap());
    assert_eq!(
        server.create("shop__feat_x", "shop").unwrap(),
        Created::AlreadyThere
    );
    // The grant is the prefix and nothing else.
    let outside = format!("{:#}", server.create("other__feat_x", "shop").unwrap_err());
    assert!(
        outside.contains("ERROR 1044") || outside.contains("does not let"),
        "{outside}"
    );

    server.drop("shop__feat_x", "shop").unwrap();
    assert!(!server.exists("shop__feat_x").unwrap());
    assert_eq!(
        root_sql(&db, "SHOW DATABASES LIKE 'shop'").unwrap(),
        "shop",
        "the main database is where it was"
    );

    let mut wrong = app_server(&recipe, &db, &bin);
    wrong.login = Login::new(Some("app".into()), Some("nope".into()), "a wrong password");
    let e = format!("{:#}", wrong.ping().unwrap_err());
    assert!(
        e.contains("ERROR 1045") && e.contains("a wrong password"),
        "{e}"
    );
}

// A worktree's database is made in main's shape. A schema written for
// main's character set — here utf8mb3, where a primary key on 1024
// characters is exactly the 3072-byte limit — was run into a database made in the
// server's wider default, where the same index is 4096 bytes and the
// schema step died on error 1071.
#[test]
fn a_real_mariadb_makes_a_worktrees_database_in_mains_character_set() {
    if skip(&["mariadb-install-db", "mariadbd", "mariadb"]) {
        return;
    }
    let db = mariadb();
    root_sql(
        &db,
        "ALTER DATABASE shop CHARACTER SET utf8mb3 COLLATE utf8mb3_general_ci; \
         GRANT ALL ON `shop\\_\\_%`.* TO 'app'@'localhost';",
    )
    .unwrap();
    const SCHEMA: &str = "CREATE TABLE t (k VARCHAR(1024) NOT NULL, PRIMARY KEY (k))";
    // What the server's default would have done with that schema: the
    // failure this test is about, so the test proves something.
    root_sql(&db, "CREATE DATABASE plain_default").unwrap();
    let default = root_sql(&db, &format!("USE plain_default; {SCHEMA}")).unwrap_err();
    assert!(default.contains("1071"), "{default}");

    let recipe = recipe("mariadb");
    let bin = db.dir.path().join("bin");
    let server = app_server(&recipe, &db, &bin);
    assert_eq!(
        server.create("shop__feat_x", "shop").unwrap(),
        Created::Made
    );
    let shape = "SELECT DEFAULT_CHARACTER_SET_NAME, DEFAULT_COLLATION_NAME \
                 FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ";
    assert_eq!(
        root_sql(&db, &format!("{shape}'shop__feat_x'")).unwrap(),
        root_sql(&db, &format!("{shape}'shop'")).unwrap(),
        "made in main's shape"
    );
    root_sql(&db, &format!("USE shop__feat_x; {SCHEMA}"))
        .expect("main's schema runs into the worktree's database");

    // A main pando cannot see — here, one that is not there — leaves the
    // server's default, and the database is still made.
    assert_eq!(
        server.create("shop__feat_y", "not_there").unwrap(),
        Created::Made
    );
    assert!(server.exists("shop__feat_y").unwrap());
}

// ---- Redis --------------------------------------------------------------------

const REDIS_PASSWORD: &str = "redis p@ss";

fn redis() -> Throwaway {
    let dir = TempDir::new().unwrap();
    let port = free_port();
    let child = Command::new("redis-server")
        .args(["--port", &port.to_string(), "--bind", "127.0.0.1"])
        .args(["--dir", &dir.path().display().to_string()])
        .args(["--save", "", "--daemonize", "no"])
        .args(["--requirepass", REDIS_PASSWORD])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let server = Throwaway { dir, port, child };
    wait_for("redis", || {
        redis_cli(&server, &["ping"]).is_ok_and(|out| out == "PONG")
    });
    server
}

fn redis_cli(server: &Throwaway, args: &[&str]) -> Result<String, String> {
    let out = Command::new("redis-cli")
        .env("REDISCLI_AUTH", REDIS_PASSWORD)
        .args(["-e", "-h", "127.0.0.1", "-p", &server.port.to_string()])
        .args(args)
        .output()
        .unwrap();
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        false => Err(String::from_utf8_lossy(&out.stdout).to_string()),
    }
}

// The trap the recipe exists to avoid: emptying slot 3 leaves slot 0's
// keys, and a slot the server does not have is an error that touches
// nothing — where `redis-cli -n 16 FLUSHDB` would have emptied slot 0.
#[test]
fn a_real_redis_empties_one_slot_and_never_falls_back_to_slot_0() {
    if skip(&["redis-server", "redis-cli"]) {
        return;
    }
    let cache = redis();
    let recipe = recipe("redis");
    let bin: PathBuf = cache.dir.path().join("bin");
    let server = Server {
        service: "redis",
        recipe: &recipe,
        host: "127.0.0.1".into(),
        port: cache.port,
        login: Login::new(None, Some(REDIS_PASSWORD.into()), "the test's password"),
        bin_dir: bin.clone(),
    };
    server.ping().unwrap();
    redis_cli(&cache, &["-n", "0", "SET", "main-key", "keep"]).unwrap();
    redis_cli(&cache, &["-n", "3", "SET", "feat-key", "go"]).unwrap();
    assert_eq!(server.size(3).unwrap(), 1);
    assert_eq!(server.size(4).unwrap(), 0);

    server.drop("3", "0").unwrap();
    assert_eq!(server.size(3).unwrap(), 0);
    assert!(server.size(16).is_err(), "a slot the server does not have");
    assert!(server.drop("16", "0").is_err());
    assert_eq!(
        redis_cli(&cache, &["-n", "0", "GET", "main-key"]).unwrap(),
        "keep",
        "slot 0 is untouched"
    );

    let refused = Server {
        login: Login::none(),
        bin_dir: bin,
        ..server
    };
    let e = format!("{:#}", refused.ping().unwrap_err());
    assert!(e.contains("NOAUTH"), "{e}");
}

// ---- a whole namespaced start -----------------------------------------------------

use crate::common;

/// Stops whatever a test started through pando, even when it panics.
struct Started(pando::paths::PandoPaths);

impl Drop for Started {
    fn drop(&mut self) {
        let _ = pando::actions::stop_all(&self.0, &|_| {});
    }
}

// The start path against a real server: the worktree's own database is
// made under the grant, the schema step builds its table there and not in
// main, and a second worktree gets a database of its own beside it.
#[test]
fn a_real_namespaced_start_builds_the_worktrees_own_database_and_leaves_main_alone() {
    if skip(&["mariadb-install-db", "mariadbd", "mariadb", "python3"]) {
        return;
    }
    let db = mariadb();
    root_sql(&db, "GRANT ALL ON `shop\\_\\_%`.* TO 'app'@'localhost'").unwrap();
    root_sql(&db, "CREATE TABLE shop.main_only (x int)").unwrap();

    let dir = TempDir::new().unwrap();
    let root = common::fixture_repo(dir.path());
    std::fs::write(
        root.join(".env"),
        format!(
            "DATABASE_HOST=127.0.0.1\nDATABASE_PORT={}\nDATABASE_NAME=shop\n\
             DATABASE_USER=app\nDATABASE_PASSWORD={APP_PASSWORD}\n",
            db.port
        ),
    )
    .unwrap();
    let paths = common::paths_for(&dir.path().join("pando-home"), &root);
    std::fs::create_dir_all(paths.project_dir()).unwrap();
    std::fs::write(
        paths.config_file(),
        format!(
            "[dev]\ncmd = '''{}'''\nports = {{ PORT = \"web\" }}\n\n\
             [[services]]\nkind = \"native\"\nname = \"mariadb\"\n\
             env = {{ DATABASE_PORT = \"mariadb\" }}\n\n\
             [[hooks]]\nname = \"schema\"\nafter = \"services\"\n\
             cmd = '''export MYSQL_PWD=\"$(sed -n 's/^DATABASE_PASSWORD=//p' \"$PANDO_ROOT/.env\")\"; \
             mariadb --protocol=tcp -h 127.0.0.1 -P \"$DATABASE_PORT\" -u app \"$DATABASE_NAME\" \
             -e 'CREATE TABLE worktree_only (x int)' '''\n",
            common::listener_on_port_env()
        ),
    )
    .unwrap();
    let config = pando::config::load(&paths).unwrap().config;
    let _started = Started(paths.clone());

    for (branch, database) in [
        ("feat/one", "shop__feat_one"),
        ("feat/two", "shop__feat_two"),
    ] {
        let name = pando::actions::new(&paths, &config, branch, None, &|_| {}).unwrap();
        pando::actions::start(
            &paths,
            &config,
            &name,
            None,
            pando::actions::Mode::Namespaced,
            &|_| {},
        )
        .unwrap_or_else(|e| panic!("start {branch} namespaced: {e:#}"));
        let tables = root_sql(&db, &format!("SHOW TABLES FROM {database}")).unwrap();
        assert_eq!(tables, "worktree_only", "{database}");
        let store = pando::state::load(&paths.state_file()).unwrap();
        let record = &store.worktrees[&name];
        assert_eq!(record.namespaces[0].name, database);
        assert_eq!(record.mode, Some(pando::state::ServiceMode::Namespaced));
    }
    assert_eq!(
        root_sql(&db, "SHOW TABLES FROM shop").unwrap(),
        "main_only",
        "the main checkout's database never saw a branch's schema step"
    );
}

// The same, with a Redis beside it: each worktree gets a slot of its own,
// its schema step writes there, and slot 0 — the main checkout's — keeps
// only what the main checkout put in it.
#[test]
fn a_real_namespaced_start_gives_each_worktree_a_redis_slot_of_its_own() {
    if skip(&[
        "mariadb-install-db",
        "mariadbd",
        "mariadb",
        "redis-server",
        "redis-cli",
        "python3",
    ]) {
        return;
    }
    let db = mariadb();
    root_sql(&db, "GRANT ALL ON `shop\\_\\_%`.* TO 'app'@'localhost'").unwrap();
    let cache = redis();
    redis_cli(&cache, &["-n", "0", "SET", "main-key", "main"]).unwrap();

    let dir = TempDir::new().unwrap();
    let root = common::fixture_repo(dir.path());
    std::fs::write(
        root.join(".env"),
        format!(
            "DATABASE_HOST=127.0.0.1\nDATABASE_PORT={}\nDATABASE_NAME=shop\n\
             DATABASE_USER=app\nDATABASE_PASSWORD={APP_PASSWORD}\n\
             REDIS_HOST=127.0.0.1\nREDIS_PORT={}\nREDIS_PASSWORD={REDIS_PASSWORD}\nREDIS_DB=0\n",
            db.port, cache.port
        ),
    )
    .unwrap();
    let paths = common::paths_for(&dir.path().join("pando-home"), &root);
    std::fs::create_dir_all(paths.project_dir()).unwrap();
    std::fs::write(
        paths.config_file(),
        format!(
            "[dev]\ncmd = '''{}'''\nports = {{ PORT = \"web\" }}\n\n\
             [[services]]\nkind = \"native\"\nname = \"mariadb\"\n\
             env = {{ DATABASE_PORT = \"mariadb\" }}\n\n\
             [[services]]\nkind = \"native\"\nname = \"redis\"\n\
             env = {{ REDIS_PORT = \"redis\" }}\n\n\
             [[hooks]]\nname = \"seed\"\nafter = \"services\"\n\
             cmd = '''export REDISCLI_AUTH=\"$(sed -n 's/^REDIS_PASSWORD=//p' \"$PANDO_ROOT/.env\")\"; \
             redis-cli -e -h 127.0.0.1 -p \"$REDIS_PORT\" -n \"$REDIS_DB\" SET seeded \"$PANDO_NAME\" '''\n",
            common::listener_on_port_env()
        ),
    )
    .unwrap();
    let config = pando::config::load(&paths).unwrap().config;
    let _started = Started(paths.clone());

    for (branch, slot) in [("feat/one", "1"), ("feat/two", "2")] {
        let name = pando::actions::new(&paths, &config, branch, None, &|_| {}).unwrap();
        pando::actions::start(
            &paths,
            &config,
            &name,
            None,
            pando::actions::Mode::Namespaced,
            &|_| {},
        )
        .unwrap_or_else(|e| panic!("start {branch} namespaced: {e:#}"));
        assert_eq!(
            redis_cli(&cache, &["-n", slot, "GET", "seeded"]).unwrap(),
            name,
            "{branch}'s own slot"
        );
    }
    assert_eq!(
        redis_cli(&cache, &["-n", "0", "GET", "seeded"]).unwrap(),
        ""
    );
    assert_eq!(
        redis_cli(&cache, &["-n", "0", "GET", "main-key"]).unwrap(),
        "main"
    );

    // `rm` takes one worktree's database and slot with it, on the real
    // servers, and leaves the other worktree's and main's as they were.
    let said = std::cell::RefCell::new(Vec::<String>::new());
    pando::actions::rm(&paths, "feat+one", false, false, &|line: &str| {
        said.borrow_mut().push(line.to_string())
    })
    .unwrap();
    let said = said.into_inner();
    assert!(
        said.iter()
            .any(|l| l == "mariadb: dropped database shop__feat_one"),
        "{said:?}"
    );
    assert!(
        said.iter().any(|l| l == "redis: emptied slot 1"),
        "{said:?}"
    );
    let databases = root_sql(&db, "SHOW DATABASES LIKE 'shop%'").unwrap();
    assert_eq!(
        databases.lines().collect::<Vec<_>>(),
        vec!["shop", "shop__feat_two"]
    );
    assert_eq!(redis_cli(&cache, &["-n", "1", "DBSIZE"]).unwrap(), "0");
    assert_eq!(
        redis_cli(&cache, &["-n", "2", "GET", "seeded"]).unwrap(),
        "feat+two"
    );
    assert_eq!(
        redis_cli(&cache, &["-n", "0", "GET", "main-key"]).unwrap(),
        "main"
    );
}

// `pando check` against a real server: a project whose schema step builds
// a table is checked in a database of the check's own, made under the
// grant, and when the check has passed that database is gone again and
// main's is as it was — the proof the maintainer's first namespaced start
// was missing.
#[test]
fn a_real_namespaced_check_proves_the_schema_step_and_leaves_only_main() {
    if skip(&["mariadb-install-db", "mariadbd", "mariadb", "python3"]) {
        return;
    }
    let db = mariadb();
    root_sql(&db, "GRANT ALL ON `shop\\_\\_%`.* TO 'app'@'localhost'").unwrap();
    root_sql(&db, "CREATE TABLE shop.main_only (x int)").unwrap();

    let dir = TempDir::new().unwrap();
    let root = common::fixture_repo(dir.path());
    std::fs::write(
        root.join(".env"),
        format!(
            "DATABASE_HOST=127.0.0.1\nDATABASE_PORT={}\nDATABASE_NAME=shop\n\
             DATABASE_USER=app\nDATABASE_PASSWORD={APP_PASSWORD}\n",
            db.port
        ),
    )
    .unwrap();
    let home = dir.path().join("pando-home");
    let paths = common::paths_for(&home, &root);
    paths.ensure_home().unwrap();
    std::fs::write(home.join("config.toml"), "[runtime]\nprelude = \"\"\n").unwrap();
    // A dev server that answers every request, brace-free: `{` is pando's
    // template syntax. The schema step reads the password the way an app
    // would, from the main checkout's env file, and builds its table in
    // whichever database it is given.
    let answering = "python3 -u -c \"import http.server as h,os;\
                     C=type('C',(h.BaseHTTPRequestHandler,),dict(do_GET=lambda s:\
                     (s.send_response(200),s.end_headers())));\
                     type('S',(h.socketserver.TCPServer,),dict(allow_reuse_address=1))(('127.0.0.1',int(os.environ['PORT'])),C).serve_forever()\"";
    std::fs::write(
        paths.config_file(),
        format!(
            "[project]\ninstall = \"true\"\n\n\
             [dev]\ncmd = '''{answering}'''\nports = {{ PORT = \"web\" }}\n\n\
             [[services]]\nkind = \"native\"\nname = \"mariadb\"\n\
             env = {{ DATABASE_PORT = \"mariadb\" }}\n\n\
             [[hooks]]\nname = \"schema\"\nafter = \"services\"\n\
             cmd = '''export MYSQL_PWD=\"$(sed -n 's/^DATABASE_PASSWORD=//p' \"$PANDO_ROOT/.env\")\"; \
             mariadb --protocol=tcp -h 127.0.0.1 -P \"$DATABASE_PORT\" -u app \"$DATABASE_NAME\" \
             -e 'CREATE TABLE worktree_only (id varchar(255) PRIMARY KEY)' '''\n"
        ),
    )
    .unwrap();
    let config = pando::config::load(&paths).unwrap().config;
    let _started = Started(paths.clone());

    let said = std::cell::RefCell::new(Vec::<String>::new());
    let step = |line: &str| said.borrow_mut().push(line.to_string());
    let say = pando::actions::Narration {
        step: &step,
        detail: &step,
    };
    let checked =
        pando::actions::check(&paths, &config, pando::setup::RanBy::Program, &say).unwrap();
    let said = said.into_inner();
    assert_eq!(
        checked.record.outcome,
        pando::setup::CheckOutcome::Passed,
        "{:?}\n{said:#?}",
        checked.record
    );
    assert_eq!(checked.record.mode, pando::setup::CheckMode::Namespaced);
    assert!(
        said.iter()
            .any(|l| l == "mariadb: own database shop__pando_check, made just now"),
        "{said:#?}"
    );
    assert!(
        said.iter()
            .any(|l| l == "mariadb: dropped database shop__pando_check"),
        "{said:#?}"
    );
    assert_eq!(
        root_sql(&db, "SHOW DATABASES LIKE 'shop%'").unwrap(),
        "shop",
        "the check's database is gone, and main's is there"
    );
    assert_eq!(
        root_sql(&db, "SHOW TABLES FROM shop").unwrap(),
        "main_only",
        "main never saw the schema step"
    );
    assert!(said.iter().all(|l| !l.contains("p@ss")), "{said:#?}");
}

// ---- Postgres -----------------------------------------------------------------

const PG_ADMIN_PASSWORD: &str = "admin p@ss";

/// A Postgres with password authentication on, holding the main
/// checkout's database `shop` — LATIN1, where the server's default is
/// UTF8 — owned by an app login that may not make databases, as the
/// app's role on a developer's own server is.
fn postgres() -> Throwaway {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("data");
    let pwfile = dir.path().join("pw");
    std::fs::write(&pwfile, PG_ADMIN_PASSWORD).unwrap();
    let init = Command::new("initdb")
        .arg("-D")
        .arg(&data)
        .args(["-U", "postgres", "--auth=scram-sha-256", "-E", "UTF8"])
        .args(["--no-locale", "--pwfile"])
        .arg(&pwfile)
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let port = free_port();
    // `-k ''`: no Unix socket, so nothing but this TCP port reaches it.
    let child = Command::new("postgres")
        .arg("-D")
        .arg(&data)
        .args(["-p", &port.to_string(), "-k", ""])
        .args(["-c", "listen_addresses=127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let server = Throwaway { dir, port, child };
    wait_for("postgres", || admin_sql(&server, "SELECT 1").is_ok());
    admin_sql(
        &server,
        &format!(
            "CREATE ROLE app LOGIN PASSWORD '{}'",
            APP_PASSWORD.replace('\'', "''")
        ),
    )
    .unwrap();
    admin_sql(
        &server,
        "CREATE DATABASE shop OWNER app TEMPLATE template0 ENCODING 'LATIN1' \
         LC_COLLATE 'C' LC_CTYPE 'C'",
    )
    .unwrap();
    server
}

/// SQL as the cluster's superuser.
fn admin_sql(server: &Throwaway, sql: &str) -> Result<String, String> {
    admin_sql_in(server, "postgres", sql)
}

fn admin_sql_in(server: &Throwaway, database: &str, sql: &str) -> Result<String, String> {
    let out = Command::new("psql")
        .env("PGPASSWORD", PG_ADMIN_PASSWORD)
        .args([
            "-X",
            "-w",
            "-h",
            "127.0.0.1",
            "-p",
            &server.port.to_string(),
        ])
        .args(["-U", "postgres", "-d", database, "-v", "ON_ERROR_STOP=1"])
        .args(["-tAc", sql])
        .output()
        .unwrap();
    match out.status.success() {
        true => Ok(String::from_utf8_lossy(&out.stdout).trim().to_string()),
        false => Err(String::from_utf8_lossy(&out.stderr).to_string()),
    }
}

fn pg_server<'a>(recipe: &'a NamespaceRecipe, db: &Throwaway, bin: &Path) -> Server<'a> {
    Server {
        service: "postgres",
        ..app_server(recipe, db, bin)
    }
}

const PG_SHAPE: &str = "SELECT pg_encoding_to_char(encoding), datcollate, datctype \
                        FROM pg_database WHERE datname = ";

// Issue #9, at the engine: the app's role is refused until the printed
// `ALTER ROLE … CREATEDB` is run, the database is then made in main's
// encoding and locale, a name of 63 bytes is kept whole, and the drop
// leaves main alone. Postgres cannot grant by prefix, so the refusal says
// what CREATEDB covers, and ownership is the wall: a database the role did
// not make is refused on the server's side too.
#[test]
fn a_real_postgres_makes_a_worktrees_database_once_the_printed_alter_role_is_run() {
    if skip(&["initdb", "postgres", "psql"]) {
        return;
    }
    let db = postgres();
    let recipe = recipe("postgres");
    let bin = db.dir.path().join("bin");
    let server = pg_server(&recipe, &db, &bin);
    server.ping().expect("the app login answers");

    let refused = format!("{:#}", server.create("shop__feat_x", "shop").unwrap_err());
    assert!(refused.contains("Nothing was made"), "{refused}");
    assert!(
        refused.contains("It lets that login make databases, and drop only the ones it made."),
        "{refused}"
    );
    assert!(!refused.contains("p@ss"), "{refused}");
    let grant = refused
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ALTER ROLE "))
        .unwrap_or_else(|| panic!("no grant in: {refused}"))
        .to_string();
    assert_eq!(grant, "ALTER ROLE \"app\" CREATEDB;");
    assert_eq!(
        admin_sql(
            &db,
            r"SELECT datname FROM pg_database WHERE datname LIKE 'shop\_\_%'"
        )
        .unwrap(),
        ""
    );

    admin_sql(&db, &grant).expect("the statement pando printed runs as written");
    assert!(!server.exists("shop__feat_x").unwrap());
    assert_eq!(
        server.create("shop__feat_x", "shop").unwrap(),
        Created::Made
    );
    assert!(server.exists("shop__feat_x").unwrap());
    assert_eq!(
        server.create("shop__feat_x", "shop").unwrap(),
        Created::AlreadyThere
    );
    assert_eq!(
        admin_sql(&db, &format!("{PG_SHAPE}'shop__feat_x'")).unwrap(),
        "LATIN1|C|C",
        "made in main's shape, not the server's UTF8"
    );

    // A main pando cannot see leaves the server's default.
    assert_eq!(
        server.create("shop__feat_y", "not_there").unwrap(),
        Created::Made
    );
    assert_eq!(
        admin_sql(&db, &format!("{PG_SHAPE}'shop__feat_y'")).unwrap(),
        "UTF8|C|C"
    );

    // The longest name the recipe allows is the name the server keeps.
    let longest = format!("shop__{}", "x".repeat(recipe.max_name() - "shop__".len()));
    assert_eq!(longest.len(), 63);
    assert_eq!(server.create(&longest, "shop").unwrap(), Created::Made);
    assert!(server.exists(&longest).unwrap());
    let mut listed = server.list("shop").unwrap();
    listed.sort();
    assert_eq!(
        listed,
        vec!["shop__feat_x", "shop__feat_y", longest.as_str()]
    );

    // Ownership is the server's wall: one the role did not make stays.
    admin_sql(&db, "CREATE DATABASE shop__theirs").unwrap();
    let e = format!("{:#}", server.drop("shop__theirs", "shop").unwrap_err());
    assert!(e.contains("does not let"), "{e}");
    assert!(server.exists("shop__theirs").unwrap());

    // A session the app left open on the worktree's database — a shell,
    // a test runner — does not keep the drop from happening.
    let mut session = Command::new("psql")
        .env("PGPASSWORD", APP_PASSWORD)
        .args(["-X", "-w", "-h", "127.0.0.1", "-p", &db.port.to_string()])
        .args([
            "-U",
            "app",
            "-d",
            "shop__feat_x",
            "-c",
            "SELECT pg_sleep(60)",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_for("a session on shop__feat_x", || {
        admin_sql(
            &db,
            "SELECT count(*) FROM pg_stat_activity WHERE datname = 'shop__feat_x'",
        )
        .is_ok_and(|n| n == "1")
    });
    server.drop("shop__feat_x", "shop").unwrap();
    let _ = session.kill();
    let _ = session.wait();
    assert!(!server.exists("shop__feat_x").unwrap());
    assert_eq!(
        admin_sql(
            &db,
            "SELECT datname FROM pg_database WHERE datname = 'shop'"
        )
        .unwrap(),
        "shop",
        "the main database is where it was"
    );

    let mut wrong = pg_server(&recipe, &db, &bin);
    wrong.login = Login::new(Some("app".into()), Some("nope".into()), "a wrong password");
    let e = format!("{:#}", wrong.ping().unwrap_err());
    assert!(
        e.contains("password authentication failed") && e.contains("a wrong password"),
        "{e}"
    );
}

// Issue #9's project, end to end: no manifest at the root, the api in
// `backend/` with its Postgres as split keys in `backend/.env` —
// `POSTGRES_SERVER`, `POSTGRES_PORT`, `POSTGRES_USER`, `POSTGRES_PASSWORD`,
// `POSTGRES_DB`. A namespaced start makes the worktree's own database, the
// schema step builds its table there and not in main, and `rm` drops it.
#[test]
fn a_real_namespaced_start_gives_a_backend_dir_its_own_postgres_database() {
    if skip(&["initdb", "postgres", "psql"]) {
        return;
    }
    let db = postgres();
    admin_sql(&db, "ALTER ROLE app CREATEDB").unwrap();
    admin_sql_in(
        &db,
        "shop",
        "CREATE TABLE main_only (x int); ALTER TABLE main_only OWNER TO app",
    )
    .unwrap();

    let dir = TempDir::new().unwrap();
    let root = common::fixture_repo(dir.path());
    std::fs::create_dir_all(root.join("backend")).unwrap();
    std::fs::write(root.join("backend/app.py"), "# the api\n").unwrap();
    common::git(&root, &["add", "backend/app.py"]);
    common::git(&root, &["commit", "--quiet", "-m", "backend"]);
    std::fs::write(
        root.join("backend/.env"),
        format!(
            "POSTGRES_SERVER=127.0.0.1\nPOSTGRES_PORT={}\nPOSTGRES_USER=app\n\
             POSTGRES_PASSWORD={APP_PASSWORD}\nPOSTGRES_DB=shop\n",
            db.port
        ),
    )
    .unwrap();
    let paths = common::paths_for(&dir.path().join("pando-home"), &root);
    std::fs::create_dir_all(paths.project_dir()).unwrap();
    // The schema step reads the address and the password the way the app
    // would, from the main checkout's `backend/.env`, and builds its table
    // in whichever database POSTGRES_DB names.
    let read = |key: &str| format!(r#"$(sed -n "s/^{key}=//p" "$PANDO_ROOT/backend/.env")"#);
    std::fs::write(
        paths.config_file(),
        format!(
            r#"[dev]
cmd = '''{}'''
cwd = "backend"
ports = {{ PORT = "web" }}

[[services]]
kind = "native"
name = "postgres"
env = {{ POSTGRES_PORT = "postgres" }}

[[hooks]]
name = "schema"
after = "services"
cwd = "backend"
cmd = '''PGPASSWORD="{}" psql -X -w -h 127.0.0.1 -p "{}" -U app -d "$POSTGRES_DB" -v ON_ERROR_STOP=1 -c 'CREATE TABLE worktree_only (x int)' '''
"#,
            common::listener_on_port_env(),
            read("POSTGRES_PASSWORD"),
            read("POSTGRES_PORT"),
        ),
    )
    .unwrap();
    let config = pando::config::load(&paths).unwrap().config;
    let _started = Started(paths.clone());

    let name = pando::actions::new(&paths, &config, "feat/one", None, &|_| {}).unwrap();
    let said = std::cell::RefCell::new(Vec::<String>::new());
    pando::actions::start(
        &paths,
        &config,
        &name,
        None,
        pando::actions::Mode::Namespaced,
        &|line| said.borrow_mut().push(line.to_string()),
    )
    .unwrap_or_else(|e| panic!("start namespaced: {e:#}\n{:#?}", said.borrow()));
    let said = said.into_inner();
    assert!(
        said.iter()
            .any(|l| l == "postgres: own database shop__feat_one, made just now"),
        "{said:#?}"
    );
    assert!(said.iter().all(|l| !l.contains("p@ss")), "{said:#?}");
    let tables = "SELECT tablename FROM pg_tables WHERE schemaname = 'public'";
    assert_eq!(
        admin_sql_in(&db, "shop__feat_one", tables).unwrap(),
        "worktree_only"
    );
    assert_eq!(
        admin_sql(&db, &format!("{PG_SHAPE}'shop__feat_one'")).unwrap(),
        "LATIN1|C|C"
    );
    let store = pando::state::load(&paths.state_file()).unwrap();
    assert_eq!(store.worktrees[&name].namespaces[0].name, "shop__feat_one");

    pando::actions::rm(&paths, &name, true, true, &|_| {}).unwrap();
    assert_eq!(
        admin_sql(
            &db,
            "SELECT datname FROM pg_database WHERE datname LIKE 'shop%'"
        )
        .unwrap(),
        "shop",
        "the worktree's database is gone, and main's is there"
    );
    assert_eq!(
        admin_sql_in(&db, "shop", tables).unwrap(),
        "main_only",
        "main never saw the schema step"
    );
}
