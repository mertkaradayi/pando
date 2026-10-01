//! Commands the developer names in the environment — `$BROWSER`,
//! `$VISUAL`, `$EDITOR` — read the way other tools read them.
//!
//! `pando open` and the TUI's `o`, `O` and `e` all go through here, so a
//! key and the command it mirrors never read the same variable two ways.

/// A command with a quote that is never closed, or a `\` with nothing
/// after it: there is no telling where its words end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unclosed;

/// Splits `command` into words the way a POSIX shell does, expanding
/// nothing: whitespace separates words, `'…'` keeps everything in it,
/// `"…"` keeps everything but lets `\` escape `"`, `\`, `$` and `` ` ``,
/// and a `\` outside quotes keeps the character after it. So
/// `"/Applications/My Editor.app/…" -w` is a program with a space in its
/// path, and one flag.
pub fn words(command: &str) -> Result<Vec<String>, Unclosed> {
    let mut out = Vec::new();
    let mut word = String::new();
    // A word can be empty and still be one (`''`), so "is there a word"
    // is not the same as "is `word` non-empty".
    let mut in_word = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next().ok_or(Unclosed)? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next().ok_or(Unclosed)? {
                        '"' => break,
                        '\\' => match chars.next().ok_or(Unclosed)? {
                            c @ ('"' | '\\' | '$' | '`') => word.push(c),
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next().ok_or(Unclosed)?);
            }
            c if c.is_whitespace() => {
                if in_word {
                    out.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        out.push(word);
    }
    Ok(out)
}

/// The commands that open `url`, in the order to try them.
///
/// `$BROWSER` is a `:`-separated list of browsers, tried until one works,
/// each a command with arguments — `firefox --new-window` — where `%s`
/// stands for the URL, which goes last when there is no `%s`. An entry
/// that is a file as written is one program, even with a space in its
/// path; otherwise it is split into [`words`], and one that cannot be is
/// split on whitespace, as it was before quotes were read. Unset or
/// empty, the desktop's own opener: [`desktop_openers`].
pub fn browser_commands(browser: Option<&str>, url: &str) -> Vec<Vec<String>> {
    let entries: Vec<&str> = browser
        .unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    if entries.is_empty() {
        return desktop_openers(url, crate::wsl::Wsl::here().is_some());
    }
    entries
        .into_iter()
        .map(|entry| {
            let mut words: Vec<String> = if std::path::Path::new(entry).is_file() {
                vec![entry.to_string()]
            } else {
                words(entry).unwrap_or_else(|Unclosed| {
                    entry.split_whitespace().map(str::to_string).collect()
                })
            };
            if words.iter().any(|word| word.contains("%s")) {
                for word in &mut words {
                    *word = word.replace("%s", url);
                }
            } else {
                words.push(url.to_string());
            }
            words
        })
        .collect()
}

/// The desktop's own opener for `url`: `open` on macOS, `xdg-open`
/// elsewhere.
///
/// WSL has no desktop of its own, and Ubuntu on WSL ships no `xdg-open`,
/// so there the browser is Windows': `wslview` when wslu is installed,
/// then Windows' URL handler through interop, then `xdg-open` for a
/// distro that has one set up. The handler is `rundll32.exe
/// url.dll,FileProtocolHandler` because neither obvious one fits:
/// `explorer.exe` exits 1 when it has opened the page, so it would read as
/// a failure, and `cmd.exe /c start` reads an `&` in the URL as the end of
/// its command.
fn desktop_openers(url: &str, wsl: bool) -> Vec<Vec<String>> {
    let command = |words: &[&str]| {
        words
            .iter()
            .map(|word| word.to_string())
            .chain([url.to_string()])
            .collect::<Vec<String>>()
    };
    if cfg!(target_os = "macos") {
        return vec![command(&["open"])];
    }
    match wsl {
        true => vec![
            command(&["wslview"]),
            command(&["rundll32.exe", "url.dll,FileProtocolHandler"]),
            command(&["xdg-open"]),
        ],
        false => vec![command(&["xdg-open"])],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn words_are_split_the_way_a_shell_splits_them() {
        assert_eq!(words("code -w"), Ok(owned(&["code", "-w"])));
        assert_eq!(words("  nvim  "), Ok(owned(&["nvim"])));
        assert_eq!(words(""), Ok(vec![]));
        assert_eq!(
            words(r#""/Applications/My Editor.app/bin/edit" -w"#),
            Ok(owned(&["/Applications/My Editor.app/bin/edit", "-w"]))
        );
        assert_eq!(
            words("'/opt/my editor/ed' --wait"),
            Ok(owned(&["/opt/my editor/ed", "--wait"]))
        );
        assert_eq!(
            words(r"/opt/my\ editor/ed"),
            Ok(owned(&["/opt/my editor/ed"]))
        );
        assert_eq!(
            words(r#"a"b c"d 'e'f"#),
            Ok(owned(&["ab cd", "ef"])),
            "quotes join onto the word they touch"
        );
        assert_eq!(words(r#""say \"hi\" \n""#), Ok(owned(&[r#"say "hi" \n"#])));
        assert_eq!(words("x '' y"), Ok(owned(&["x", "", "y"])));
    }

    #[test]
    fn a_command_whose_words_never_end_is_unclosed() {
        for command in [r#""/opt/my editor"#, "'open", r"trailing\", r#""a\"#] {
            assert_eq!(words(command), Err(Unclosed), "{command}");
        }
    }

    // `BROWSER="firefox --new-window"` was once run as a program of that
    // whole name: "No such file or directory", for a setting every other
    // tool reads.
    #[test]
    fn browser_is_a_list_of_commands_with_arguments() {
        let url = "http://localhost:3000";
        assert_eq!(
            browser_commands(Some("firefox --new-window"), url),
            vec![owned(&["firefox", "--new-window", url])]
        );
        assert_eq!(
            browser_commands(Some("w3m:lynx -dump %s"), url),
            vec![owned(&["w3m", url]), owned(&["lynx", "-dump", url])],
            "a list, tried in order, with %s standing for the URL"
        );
        assert_eq!(
            browser_commands(Some(r#""/opt/My Browser/run" --private"#), url),
            vec![owned(&["/opt/My Browser/run", "--private", url])],
            "a quoted path with a space in it"
        );
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("My Browser");
        std::fs::write(&spaced, "").unwrap();
        let spaced = spaced.to_str().unwrap();
        assert_eq!(
            browser_commands(Some(spaced), url),
            vec![owned(&[spaced, url])],
            "a file as written is one program, space and all"
        );
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        for unset in [None, Some(""), Some(" : ")] {
            assert_eq!(
                browser_commands(unset, url),
                vec![owned(&[opener, url])],
                "{unset:?}"
            );
        }
    }

    // Ubuntu on WSL ships no `xdg-open`, and `pando open` said it could
    // not run it: the browser there is Windows'. An `&` in the URL stays
    // in its one argument, which `cmd.exe /c start` would have cut at.
    #[test]
    fn under_wsl_the_browser_is_windows() {
        let url = "http://localhost:3000/?a=1&b=2";
        if cfg!(target_os = "macos") {
            assert_eq!(desktop_openers(url, true), vec![owned(&["open", url])]);
            return;
        }
        assert_eq!(
            desktop_openers(url, true),
            vec![
                owned(&["wslview", url]),
                owned(&["rundll32.exe", "url.dll,FileProtocolHandler", url]),
                owned(&["xdg-open", url]),
            ]
        );
        assert_eq!(desktop_openers(url, false), vec![owned(&["xdg-open", url])]);
    }
}
