//! Commands the developer names in the environment — `$BROWSER`,
//! `$VISUAL`, `$EDITOR` — read the way other tools read them.
//!
//! `pando open` and the TUI's `o`, `O` and `e` all go through here, so a
//! key and the command it mirrors never read the same variable two ways.

use crate::platform::{Host, desktop};

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
/// empty, the desktop's own: [`desktop::url_openers`].
pub fn browser_commands(browser: Option<&str>, url: &str, host: &Host) -> Vec<Vec<String>> {
    let entries: Vec<&str> = browser
        .unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .collect();
    if entries.is_empty() {
        return desktop::url_openers(host, url);
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
        let host = Host::default();
        assert_eq!(
            browser_commands(Some("firefox --new-window"), url, &host),
            vec![owned(&["firefox", "--new-window", url])]
        );
        assert_eq!(
            browser_commands(Some("w3m:lynx -dump %s"), url, &host),
            vec![owned(&["w3m", url]), owned(&["lynx", "-dump", url])],
            "a list, tried in order, with %s standing for the URL"
        );
        assert_eq!(
            browser_commands(Some(r#""/opt/My Browser/run" --private"#), url, &host),
            vec![owned(&["/opt/My Browser/run", "--private", url])],
            "a quoted path with a space in it"
        );
        let dir = tempfile::tempdir().unwrap();
        let spaced = dir.path().join("My Browser");
        std::fs::write(&spaced, "").unwrap();
        let spaced = spaced.to_str().unwrap();
        assert_eq!(
            browser_commands(Some(spaced), url, &host),
            vec![owned(&[spaced, url])],
            "a file as written is one program, space and all"
        );
        for (os, opener) in [
            (crate::platform::Os::MacOs, "open"),
            (crate::platform::Os::Linux, "xdg-open"),
        ] {
            let host = Host { os, wsl: None };
            for unset in [None, Some(""), Some(" : ")] {
                assert_eq!(
                    browser_commands(unset, url, &host),
                    vec![owned(&[opener, url])],
                    "{os:?} {unset:?}"
                );
            }
        }
    }
}
