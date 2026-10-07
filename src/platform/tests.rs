//! The boundary: nothing outside this layer talks to the OS, and this layer
//! imports nothing above it. Then what [`Host`] promises.

use std::path::{Path, PathBuf};

use super::*;

/// Where [`Host::here`] may be read: where pando meets the outside. Every
/// module below them is handed a `&Host`.
const HOST_EDGES: &[&str] = &[
    "actions/runtime.rs",
    "tui/app/launch.rs",
    "cli/open.rs",
    "theme/select.rs",
];

fn src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src/`, as its path below `src/` with `/`.
fn rust_files() -> Vec<String> {
    let root = src();
    let mut out = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let relative = path.strip_prefix(&root).unwrap();
                let parts: Vec<_> = relative
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect();
                out.push(parts.join("/"));
            }
        }
    }
    out.sort();
    out
}

/// The lines of `text` that are production code, numbered from 1: none of a
/// `tests.rs` or of `testutil.rs`, nothing from an inline test module on,
/// and no comment, so a doc may name what the code may not use.
fn production_lines<'t>(file: &str, text: &'t str) -> Vec<(usize, &'t str)> {
    let name = file.rsplit('/').next().unwrap_or(file);
    if name == "tests.rs" || name == "testutil.rs" {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if (trimmed.starts_with("mod tests") || trimmed.starts_with("pub(crate) mod tests"))
            && trimmed.ends_with('{')
        {
            break;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        out.push((index + 1, line));
    }
    out
}

/// The production lines joined where one item spans several, numbered by
/// its first: a `use` until its `;`, and a `cfg` until its parentheses
/// close, so a grouped import or a split `cfg(any(…))` is read whole.
fn statements(lines: &[(usize, &str)]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for &(number, line) in lines {
        let (first, mut text) = open.take().unwrap_or((number, String::new()));
        text.push_str(line.trim());
        text.push(' ');
        match unfinished(&text) {
            true => open = Some((first, text)),
            false => out.push((first, text)),
        }
    }
    out.extend(open);
    out
}

/// Whether `text` is a `use` without its `;` yet, or a `cfg` whose
/// parentheses are still open.
fn unfinished(text: &str) -> bool {
    let mut words = text.split_whitespace();
    let is_use = match words.next() {
        Some("use") => true,
        Some(word) if word.starts_with("pub") => words.next() == Some("use"),
        _ => false,
    };
    if is_use {
        return !text.contains(';');
    }
    let Some(at) = ["cfg(", "cfg!(", "cfg_attr("]
        .iter()
        .filter_map(|cfg| text.find(cfg))
        .min()
    else {
        return false;
    };
    let depth: i32 = text[at..]
        .chars()
        .map(|c| match c {
            '(' => 1,
            ')' => -1,
            _ => 0,
        })
        .sum();
    depth > 0
}

/// Whether `line` names `path` as a path of its own: `nix::` is the crate,
/// not the end of `std::os::unix::`, and `::libc::` is the crate too,
/// where `foo::libc::` is a module of `foo`.
fn names(line: &str, path: &str) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(path).any(|(at, _)| {
        let before = &line[..at];
        match before.strip_suffix("::") {
            Some(rest) => !rest.chars().next_back().is_some_and(ident),
            None => !before
                .chars()
                .next_back()
                .is_some_and(|c| ident(c) || c == ':'),
        }
    })
}

/// What in one production statement outside this layer talks to the OS.
fn os_talk(file: &str, line: &str) -> Option<&'static str> {
    const PATHS: &[&str] = &["std::os::", "nix::", "libc::", "windows_sys::"];
    const CALLS: &[&str] = &[
        "extern \"",
        "var_os(\"HOME\")",
        "var(\"HOME\")",
        "Command::new(\"bash\")",
        "Command::new(\"sh\")",
        "\"/bin/sh\"",
        "\"/bin/bash\"",
        "Group::from_raw",
        ".as_raw()",
    ];
    if let Some(path) = PATHS.iter().find(|path| names(line, path)) {
        return Some(path);
    }
    // `use std::{fs, os::unix::…}`: `std::os` with a group between.
    if let Some(at) = line.find("std::{")
        && names(&line[at..], "os::")
    {
        return Some("std::os::");
    }
    if let Some(call) = CALLS.iter().find(|call| line.contains(*call)) {
        return Some(call);
    }
    let conditional = ["cfg(", "cfg!(", "cfg_attr("]
        .iter()
        .any(|cfg| line.contains(cfg));
    if conditional
        && ["unix", "windows", "target_"]
            .iter()
            .any(|os| line.contains(os))
    {
        return Some("a cfg on the OS");
    }
    if line.contains("Host::here()") && !HOST_EDGES.contains(&file) {
        return Some("Host::here() below the edges");
    }
    None
}

/// Best effort: it reads lines, not Rust. It sees a path to the OS however
/// it is spelt (`::libc::`, `std::{fs, os::…}`), a `use` or a `cfg` split
/// over lines, any foreign ABI, and the shells by name; not a renamed
/// import, or a shell's name held in a const. What it misses, CI's
/// Windows build catches: a Unix API named outside a `cfg` does not
/// compile there. `tests/` and `examples/` are not read, since test code
/// stays Unix-only until a native port.
#[test]
fn only_the_platform_layer_talks_to_the_os() {
    let mut offending = Vec::new();
    for file in rust_files() {
        if file.starts_with("platform/") {
            continue;
        }
        let text = std::fs::read_to_string(src().join(&file)).unwrap();
        for (number, statement) in statements(&production_lines(&file, &text)) {
            if let Some(what) = os_talk(&file, &statement) {
                offending.push(format!("src/{file}:{number}: {what}"));
            }
        }
    }
    assert!(
        offending.is_empty(),
        "only src/platform talks to the OS; move these into it: {offending:#?}"
    );
}

/// What the boundary finds in `code`, as a file outside this layer.
fn talk_in(code: &str) -> Vec<&'static str> {
    let lines: Vec<(usize, &str)> = code
        .lines()
        .enumerate()
        .map(|(index, line)| (index + 1, line))
        .collect();
    statements(&lines)
        .iter()
        .filter_map(|(_, statement)| os_talk("cli/x.rs", statement))
        .collect()
}

// Each of these slipped past the first version of the boundary.
#[test]
fn the_boundary_sees_a_path_however_it_is_spelt_and_split() {
    for code in [
        "let pid = unsafe { ::libc::getpid() };",
        "use ::std::os::unix::fs::PermissionsExt;",
        "use std::{fs, os::unix::fs::PermissionsExt};",
        "use std::{\n    fs,\n    os::unix::fs::PermissionsExt,\n};",
        "#[cfg(any(\n    target_os = \"linux\",\n    target_os = \"macos\",\n))]\nfn f() {}",
        "unsafe extern \"system\" {\n    fn GetTickCount() -> u32;\n}",
        "Command::new(\"/bin/bash\")",
    ] {
        assert!(!talk_in(code).is_empty(), "not seen:\n{code}");
    }
    for code in [
        "use crate::platform::files;",
        "use std::{fs, io::Write};",
        "let unix_time = 0;",
        "let parsed = toml::libc::Value::new();",
        "#[cfg(test)]\nfn f() {}",
        "#[cfg(any(\n    test,\n    feature = \"extra\",\n))]\nfn f() {}",
    ] {
        assert!(talk_in(code).is_empty(), "seen in:\n{code}");
    }
}

// The Windows backends are held to compiling by a CI job that builds the
// library and the binary on Windows: without it they rot unseen, since no
// test here runs there.
#[test]
fn ci_builds_the_windows_backends() {
    let workflow = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/ci.yml"),
    )
    .unwrap();
    let job = workflow
        .split_once("\n  windows:\n")
        .map(|(_, rest)| rest)
        .expect("ci.yml has a windows job");
    assert!(job.contains("runs-on: windows-latest"), "{job}");
    assert!(
        job.contains("cargo clippy --locked --lib --bins -- -D warnings"),
        "{job}"
    );
}

// What a Unix build talks to the OS with is a dependency of Unix builds
// alone, and of src/platform alone: a crate the rest of pando could reach
// for on any OS is how the boundary would erode without a line of code
// naming it here.
#[test]
fn the_os_crates_are_dependencies_of_a_unix_build_only() {
    let manifest: toml::Table =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .unwrap()
            .parse()
            .unwrap();
    let everywhere = manifest["dependencies"].as_table().unwrap();
    let unix = manifest["target"]["cfg(unix)"]["dependencies"]
        .as_table()
        .expect("a [target.'cfg(unix)'.dependencies] table");
    for os_crate in ["libc", "nix"] {
        assert!(
            !everywhere.contains_key(os_crate),
            "{os_crate} is in [dependencies]"
        );
        assert!(unix.contains_key(os_crate), "{os_crate} is a Unix build's");
    }
}

#[test]
fn the_platform_layer_imports_nothing_above_it() {
    let mut above = Vec::new();
    for file in rust_files() {
        if !file.starts_with("platform/") {
            continue;
        }
        let text = std::fs::read_to_string(src().join(&file)).unwrap();
        for (number, line) in production_lines(&file, &text) {
            for (at, _) in line.match_indices("crate::") {
                let rest = &line[at + "crate::".len()..];
                if !rest.starts_with("platform::") && !rest.starts_with("testutil::") {
                    above.push(format!("src/{file}:{number}: {}", line.trim()));
                }
            }
        }
    }
    assert!(
        above.is_empty(),
        "src/platform sits below everything else: {above:#?}"
    );
}

#[test]
fn only_a_native_windows_build_refuses_to_run() {
    assert_eq!(unsupported().is_some(), Os::HERE == Os::Windows);
}

#[test]
fn no_test_sees_the_machine_it_runs_on() {
    assert_eq!(Host::here(), &Host::default());
    assert_eq!(Host::here().os, Os::HERE);
    assert_eq!(Host::here().wsl, None, "not even a test run in WSL");
}

// ---- wsl -------------------------------------------------------------

fn wsl_at(release: &str, mounts: &str) -> Option<Wsl> {
    let system = tempfile::tempdir().unwrap();
    crate::testutil::wsl_system(system.path(), release, mounts);
    Wsl::at(system.path())
}

fn drive(wsl: &Wsl, path: &str) -> Option<PathBuf> {
    wsl.drive_of(Path::new(path)).map(Path::to_path_buf)
}

#[test]
fn wsl_is_read_from_the_kernel_release_and_its_drives_from_the_mounts() {
    use crate::testutil::{WSL_MOUNTS, WSL_RELEASE};
    let second = "D:\\134 /mnt/d 9p rw,noatime,aname=drvfs;path=D:\\;uid=1000 0 0\n";
    let wsl2 = wsl_at(WSL_RELEASE, &format!("{WSL_MOUNTS}{second}")).expect("a WSL 2 kernel");
    assert_eq!(drive(&wsl2, "/mnt/c/app"), Some(PathBuf::from("/mnt/c")));
    assert_eq!(drive(&wsl2, "/mnt/d/app"), Some(PathBuf::from("/mnt/d")));
    assert_eq!(
        drive(&wsl2, "/usr/lib/wsl/drivers/x"),
        None,
        "the drivers mount is 9p but not a drive"
    );
    assert!(wsl_at("4.4.0-19041-Microsoft", "").is_some(), "WSL 1");
    assert_eq!(wsl_at("6.8.0-45-generic", WSL_MOUNTS), None);
    let nothing = tempfile::tempdir().unwrap();
    assert_eq!(Wsl::at(nothing.path()), None, "macOS has no /proc");
}

#[test]
fn a_host_read_under_a_root_carries_the_wsl_found_there() {
    let system = tempfile::tempdir().unwrap();
    assert_eq!(Host::at(system.path()), Host::default());
    let wsl = crate::testutil::WSL_RELEASE;
    crate::testutil::wsl_system(system.path(), wsl, crate::testutil::WSL_MOUNTS);
    let host = Host::at(system.path());
    assert_eq!(host.os, Os::HERE);
    assert!(host.wsl.is_some());
}

#[test]
fn a_path_is_on_the_drive_whose_mount_it_is_under() {
    let mounts = "C:\\134 /mnt/c 9p aname=drvfs 0 0\nD:\\134 /mnt/c/Data 9p aname=drvfs 0 0\n";
    let wsl = wsl_at(crate::testutil::WSL_RELEASE, mounts).unwrap();
    assert_eq!(drive(&wsl, "/mnt/c/Users/me/app"), Some("/mnt/c".into()));
    assert_eq!(
        drive(&wsl, "/mnt/c/Data/app"),
        Some("/mnt/c/Data".into()),
        "the deepest mount"
    );
    assert_eq!(
        drive(&wsl, "/mnt/cache/app"),
        None,
        "a prefix of the name is not the mount"
    );
    assert_eq!(drive(&wsl, "/home/me/app"), None);
}

#[test]
fn wsl_1_mounts_drvfs_itself_and_a_mount_point_may_be_escaped() {
    let mounts = "C: /mnt/c drvfs rw,noatime 0 0\n\
                  E: /mnt/my\\040drive drvfs rw 0 0\n\
                  F: /mnt/a\\134b\\011c drvfs rw 0 0\n\
                  G: /mnt/trailing\\04 drvfs rw 0 0\n";
    let wsl = wsl_at("4.4.0-19041-Microsoft", mounts).unwrap();
    assert_eq!(drive(&wsl, "/mnt/c/x"), Some("/mnt/c".into()));
    assert_eq!(drive(&wsl, "/mnt/my drive/x"), Some("/mnt/my drive".into()));
    assert_eq!(drive(&wsl, "/mnt/a\\b\tc/x"), Some("/mnt/a\\b\tc".into()));
    assert_eq!(
        drive(&wsl, "/mnt/trailing\\04/x"),
        Some("/mnt/trailing\\04".into()),
        "too short to be an escape"
    );
}

// ---- boot ------------------------------------------------------------

#[test]
fn this_boot_has_an_id_that_stays_the_same_and_matches_itself() {
    let Some(now) = boot::now() else {
        return; // A sandbox with no /proc or sysctl cannot say.
    };
    assert!(!now.id.is_empty());
    assert_eq!(boot::now(), Some(now), "read once");
    assert!(boot::same(now, now));
    let earlier = boot::Boot {
        id: "an-earlier-boot",
        ..now
    };
    assert!(!boot::same(earlier, now));
}

#[test]
fn a_boot_that_does_not_say_when_its_pids_began_is_judged_on_its_id_alone() {
    let boot = |id, pids_since| boot::Boot { id, pids_since };
    assert!(
        !boot::same(boot("kernel-a", Some(100)), boot("kernel-a", Some(250))),
        "init restarted"
    );
    assert!(boot::same(
        boot("kernel-a", Some(100)),
        boot("kernel-a", Some(100))
    ));
    assert!(
        boot::same(boot("kernel-a", None), boot("kernel-a", Some(250))),
        "written before init's start time was recorded"
    );
    assert!(
        boot::same(boot("kernel-a", Some(100)), boot("kernel-a", None)),
        "a machine where /proc/1 cannot be read now"
    );
    assert!(!boot::same(
        boot("kernel-a", None),
        boot("kernel-b", Some(250))
    ));
    assert!(!boot::same(
        boot("kernel-a", Some(100)),
        boot("kernel-b", Some(100))
    ));
    assert!(boot::same(boot("kernel-a", None), boot("kernel-a", None)));
}

#[test]
fn inits_start_time_is_field_22_counted_after_the_command_name() {
    let stat = "1 (sys temd) x) S 0 1 1 0 -1 4194560 100 200 3 4 5 6 7 8 20 0 1 0 4242 1000 50";
    assert_eq!(
        boot::parse_start_time(stat),
        Some(4242),
        "a name with ) and a space"
    );
    assert_eq!(
        boot::parse_start_time("1 (init) S 0 1 1"),
        None,
        "cut short"
    );
    assert_eq!(boot::parse_start_time("no command name"), None);
}

#[cfg(target_os = "linux")]
#[test]
fn a_linux_boot_is_the_kernels_with_inits_start_time() {
    let Some(now) = boot::now() else {
        return; // A sandbox with no /proc cannot say.
    };
    let kernel = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    assert_eq!(now.id, kernel.trim());
    let init = std::fs::read_to_string("/proc/1/stat")
        .ok()
        .and_then(|stat| boot::parse_start_time(&stat));
    assert_eq!(now.pids_since, init);
}

// ---- files -----------------------------------------------------------

#[test]
fn a_lock_held_is_refused_to_a_second_asker_until_it_is_let_go() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.lock");
    let open = || {
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .unwrap()
    };
    let first = open();
    files::lock_exclusive(&first).unwrap();
    assert!(!files::try_lock_exclusive(&open()).unwrap(), "held");
    drop(first);
    assert!(files::try_lock_exclusive(&open()).unwrap(), "let go");
}

#[test]
fn a_new_file_has_exactly_the_bits_asked_for_whatever_the_umask() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("private");
    files::create_new(&path, 0o600).unwrap();
    let meta = std::fs::metadata(&path).unwrap();
    if let Some(bits) = files::permission_bits(&meta) {
        assert_eq!(bits, 0o600);
    }
    assert!(files::create_new(&path, 0o600).is_err(), "never over one");
    assert!(files::owned_by_current_user(&meta));
}

#[test]
fn a_file_is_executable_once_the_os_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tool");
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    if files::permission_bits(&std::fs::metadata(&path).unwrap()).is_some() {
        files::set_permission_bits(&path, 0o644).unwrap();
        assert!(!files::is_executable(&path));
        files::set_permission_bits(&path, 0o755).unwrap();
    }
    assert!(files::is_executable(&path));
    assert!(!files::is_executable(dir.path()), "a directory is not run");
    assert!(!files::is_executable(&dir.path().join("missing")));
}

#[test]
fn a_file_put_in_anothers_place_is_another_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("log");
    std::fs::write(&path, "a").unwrap();
    let id = |p: &Path| files::FileId::of(&std::fs::metadata(p).unwrap());
    let first = id(&path);
    std::fs::write(&path, "ab").unwrap();
    assert_eq!(id(&path), first, "written to, the same file");
    let other = dir.path().join("other");
    std::fs::write(&other, "c").unwrap();
    std::fs::rename(&other, &path).unwrap();
    if first.is_some() {
        assert_ne!(id(&path), first, "replaced, another");
    }
}

#[test]
fn a_link_points_at_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, "t").unwrap();
    let link = dir.path().join("link");
    files::symlink(&target, &link).unwrap();
    assert_eq!(std::fs::read_link(&link).unwrap(), target);
}

// Windows makes a link to a directory and a link to a file differently,
// so it asks what the target is: a relative one from the link's own
// directory, where the link will look, not from pando's.
#[test]
fn a_relative_target_is_seen_from_the_links_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a/shared")).unwrap();
    let link = dir.path().join("a/link");
    let seen = files::as_seen_from(&link, Path::new("shared"));
    assert_eq!(seen, dir.path().join("a/shared"));
    assert!(seen.is_dir());
    let absolute = dir.path().join("elsewhere");
    assert_eq!(files::as_seen_from(&link, &absolute), absolute);
}

// ---- desktop ---------------------------------------------------------

fn on(os: Os) -> Host {
    Host { os, wsl: None }
}

#[test]
fn every_desktop_has_one_row() {
    for desktop in [
        desktop::Desktop::MacOs,
        desktop::Desktop::Linux,
        desktop::Desktop::Windows,
        desktop::Desktop::Wsl,
    ] {
        let rows = desktop::DESKTOPS
            .iter()
            .filter(|row| row.desktop == desktop)
            .count();
        assert_eq!(rows, 1, "{desktop:?}");
    }
    for os in [Os::MacOs, Os::Linux, Os::Windows] {
        assert_eq!(desktop::row(&on(os)).desktop, desktop::Desktop::of(&on(os)));
    }
}

fn wsl_host() -> Host {
    let system = tempfile::tempdir().unwrap();
    crate::testutil::wsl_system(
        system.path(),
        crate::testutil::WSL_RELEASE,
        crate::testutil::WSL_MOUNTS,
    );
    Host::at(system.path())
}

// Ubuntu on WSL ships no `xdg-open`, and `pando open` said it could not
// run it: the browser there is Windows'. clip.exe through interop reads
// the console's code page, so it is handed ASCII only.
#[test]
fn under_wsl_the_browser_and_the_clipboard_are_windows() {
    let wsl = wsl_host();
    assert_eq!(desktop::Desktop::of(&wsl), desktop::Desktop::Wsl);
    let url = "http://localhost:3000/?a=1&b=2";
    let words = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    assert_eq!(
        desktop::url_openers(&wsl, url),
        vec![
            words(&["wslview", url]),
            words(&["rundll32.exe", "url.dll,FileProtocolHandler", url]),
            words(&[
                "/mnt/c/Windows/System32/rundll32.exe",
                "url.dll,FileProtocolHandler",
                url
            ]),
            words(&["xdg-open", url]),
        ]
    );
    assert_eq!(desktop::clipboard(&wsl, url), Some("clip.exe"));
    assert_eq!(desktop::clipboard(&wsl, "/home/me/çalışma"), None);
    assert!(!desktop::starts_simulators(&wsl));
    assert_eq!(desktop::is_dark(&wsl), None);
    assert_eq!(desktop::fallback_shell(&wsl), "/bin/sh");
}

// Windows' URL handler takes the URL as one argument; `cmd /c start`
// would have cut it at the `&`. clip.exe reads the console code page, so
// it is handed ASCII only.
#[test]
fn windows_opens_with_its_url_handler_and_copies_ascii_with_clip() {
    let windows = on(Os::Windows);
    let url = "http://localhost:3000/?a=1&b=2";
    assert_eq!(
        desktop::url_openers(&windows, url),
        vec![vec![
            "rundll32.exe".to_string(),
            "url.dll,FileProtocolHandler".into(),
            url.into()
        ]]
    );
    assert_eq!(desktop::clipboard(&windows, url), Some("clip.exe"));
    assert_eq!(desktop::clipboard(&windows, "C:\\Users\\çalışma"), None);
    assert!(!desktop::starts_simulators(&windows));
}

// The URL is one argument, given last: an `&` in it is never a shell's.
#[test]
fn a_url_is_opened_with_the_desktops_own_program_as_its_last_argument() {
    let url = "http://localhost:3000/?a=1&b=2";
    let opened = |os| desktop::url_openers(&on(os), url);
    assert_eq!(
        opened(Os::MacOs),
        vec![vec!["open".to_string(), url.into()]]
    );
    assert_eq!(
        opened(Os::Linux),
        vec![vec!["xdg-open".to_string(), url.into()]]
    );
}

#[test]
fn macos_copies_any_text_and_linux_leaves_it_to_osc_52() {
    for text in ["/tmp/x", "/home/me/çalışma"] {
        assert_eq!(desktop::clipboard(&on(Os::MacOs), text), Some("pbcopy"));
        assert_eq!(desktop::clipboard(&on(Os::Linux), text), None);
    }
}

#[test]
fn only_macos_starts_a_simulator_and_says_whether_it_is_dark() {
    assert!(desktop::starts_simulators(&on(Os::MacOs)));
    assert!(!desktop::starts_simulators(&on(Os::Linux)));
    assert!(desktop::row(&on(Os::MacOs)).dark_mode.is_some());
    assert_eq!(desktop::is_dark(&on(Os::Linux)), None, "nothing to ask");
    for os in [Os::MacOs, Os::Linux] {
        assert_eq!(desktop::fallback_shell(&on(os)), "/bin/sh");
    }
}
