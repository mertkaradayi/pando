//! Human text on a terminal: colour when a person is reading, and `~` for
//! the home directory.
//!
//! The CLI's human output and `doctor`'s report both go through a
//! [`Style`], decided once per run from the stream they print to. Colour is
//! the terminal's own eight ANSI colours, so it follows whatever palette
//! the terminal has; it is never on for a pipe or a file, and never when
//! `NO_COLOR` is set (<https://no-color.org>) or `TERM` is `dumb`. The
//! plain style is what every test reads, so none of them has to strip
//! escapes.

use std::path::{Path, PathBuf};

/// The handful of looks human output uses. Each one says what it is *for*
/// rather than which colour it is, so the meaning stays in one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Paint {
    /// Section titles and table headers.
    Heading,
    /// Something up and serving.
    Good,
    /// Something on its way, or worth a look: starting, dirty, a note.
    Warn,
    /// Something broken: failed, gone, a problem.
    Bad,
    /// Filler that should recede: a dash for "nothing", a stopped worktree.
    Faint,
    /// A URL, which a person is here to click.
    Link,
}

impl Paint {
    fn code(self) -> &'static str {
        match self {
            Paint::Heading => "1",
            Paint::Good => "32",
            Paint::Warn => "33",
            Paint::Bad => "31",
            Paint::Faint => "2",
            Paint::Link => "36",
        }
    }
}

/// How human text is dressed for the stream it goes to.
#[derive(Debug, Clone, Default)]
pub struct Style {
    color: bool,
    home: Option<PathBuf>,
    /// Where `home` resolves to, when a symlink makes that another path.
    real_home: Option<PathBuf>,
}

impl Style {
    /// No colour and no `~`: exactly the text, for tests and for anything
    /// that is not a terminal.
    pub fn plain() -> Self {
        Self::default()
    }

    /// The style for stdout: colour only when it is a terminal and
    /// nothing asked for none, `~` always.
    pub fn for_stdout() -> Self {
        use std::io::IsTerminal;
        Self::decide(std::io::stdout().is_terminal())
    }

    /// The same decision for stderr, which is where pando narrates.
    pub fn for_stderr() -> Self {
        use std::io::IsTerminal;
        Self::decide(std::io::stderr().is_terminal())
    }

    fn decide(is_terminal: bool) -> Self {
        Self::decide_from(
            is_terminal,
            std::env::var("NO_COLOR").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// The decision itself, with the environment passed in.
    fn decide_from(is_terminal: bool, no_color: Option<&str>, term: Option<&str>) -> Self {
        // NO_COLOR counts when it is set to anything but the empty string.
        let no_color = no_color.is_some_and(|v| !v.is_empty());
        let dumb = term == Some("dumb");
        Self::with(
            is_terminal && !no_color && !dumb,
            crate::platform::dirs::home().filter(|h| h.is_absolute() && h != Path::new("/")),
        )
    }

    /// Colour forced on or off, with `home` as the directory `~` stands
    /// for.
    pub fn with(color: bool, home: Option<PathBuf>) -> Self {
        let real_home = home.as_deref().and_then(real_home);
        Self {
            color,
            home,
            real_home,
        }
    }

    pub fn color(&self) -> bool {
        self.color
    }

    /// `text` in `paint`, or `text` itself when colour is off.
    pub fn paint(&self, text: &str, paint: Paint) -> String {
        if !self.color || text.is_empty() {
            return text.to_string();
        }
        format!("\x1b[{}m{text}\x1b[0m", paint.code())
    }

    /// A path with the home directory written as `~`, which is how a
    /// person reads and types it. Anything outside home is left alone.
    pub fn tilde(&self, path: &str) -> String {
        [&self.home, &self.real_home]
            .into_iter()
            .flatten()
            .find_map(|home| tilde_under(path, home))
            .unwrap_or_else(|| path.to_string())
    }
}

/// `path` with `home` written as `~`, when it is under it.
fn tilde_under(path: &str, home: &Path) -> Option<String> {
    let home = home.to_string_lossy();
    let home = home.trim_end_matches('/');
    match path.strip_prefix(home) {
        Some("") => Some("~".to_string()),
        Some(rest) if rest.starts_with('/') => Some(format!("~{rest}")),
        _ => None,
    }
}

/// Where `home` resolves to, when that is another path: a home reached
/// through a symlink (`/tmp` is `/private/tmp` on macOS), which a path
/// read from the filesystem is written under. Resolved once per home and
/// kept, so a screen that shortens paths every frame asks the disk once.
pub fn real_home(home: &Path) -> Option<PathBuf> {
    static SEEN: std::sync::Mutex<Option<(PathBuf, Option<PathBuf>)>> = std::sync::Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, real)) = seen.as_ref()
        && at == home
    {
        return real.clone();
    }
    let real = std::fs::canonicalize(home).ok().filter(|real| real != home);
    *seen = Some((home.to_path_buf(), real.clone()));
    real
}

/// How many columns `text` takes once any colour in it is ignored.
pub fn visible_width(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            // CSI: ESC [ … final byte in @..~
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) && c != '[' {
                    break;
                }
            }
            continue;
        }
        width += char_width(c);
    }
    width
}

/// How many terminal columns `c` takes: two for a wide CJK character or
/// most emoji, none for a combining mark or a control character.
fn char_width(c: char) -> usize {
    unicode_width::UnicodeWidthChar::width(c).unwrap_or(0)
}

/// How many columns uncoloured `text` takes.
pub fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// `text` cut to at most `max` columns at its end, behind a `…`, so a row
/// with wide characters in it still fits the terminal it was cut for.
pub fn ellipsize_end(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut kept = String::new();
    let mut used = 0;
    for c in text.chars() {
        if used + char_width(c) > max - 1 {
            break;
        }
        used += char_width(c);
        kept.push(c);
    }
    format!("{kept}…")
}

/// `text` cut to at most `max` columns by taking out its middle, so both
/// the start and the end survive: two long branch names that share a
/// prefix still read as two different names.
pub fn ellipsize_middle(text: &str, max: usize) -> String {
    if text_width(text) <= max {
        return text.to_string();
    }
    if max <= 1 {
        return "…".chars().take(max).collect();
    }
    // Budgets in columns, so a wide character is never cut in half and
    // never pushes the result past `max`.
    let keep = max - 1;
    let tail_room = keep / 2;
    let head_room = keep - tail_room;
    let mut start = String::new();
    let mut used = 0;
    for c in text.chars() {
        if used + char_width(c) > head_room {
            break;
        }
        used += char_width(c);
        start.push(c);
    }
    let mut end: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        if used + char_width(c) > tail_room {
            break;
        }
        used += char_width(c);
        end.push(c);
    }
    let end: String = end.into_iter().rev().collect();
    format!("{start}…{end}")
}

/// `text` cut to at most `max` columns so that it still reads differently
/// from each of `others`.
///
/// Branch names share prefixes (`feature/very-long-name-1`,
/// `feature/very-long-name-2`), and a cut at the end — or in the middle —
/// can leave every row identical. So the cut keeps the stretch where this
/// name first departs from the one it shares most with: when that point is
/// past the first half of what fits, the shared prefix goes (`…name-1`);
/// otherwise it is [`ellipsize_middle`].
pub fn ellipsize_distinct(text: &str, others: &[String], max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if text_width(text) <= max {
        return text.to_string();
    }
    // The window below counts characters, which is columns for the
    // branch names this is written for; a name with wide characters in it
    // is cut by columns instead, so it can never overrun its cell.
    if chars.iter().any(|&c| char_width(c) != 1) {
        return ellipsize_middle(text, max);
    }
    let shared = others
        .iter()
        .filter(|o| o.as_str() != text)
        .map(|o| {
            o.chars()
                .zip(chars.iter())
                .take_while(|(a, b)| a == *b)
                .count()
        })
        .max()
        .unwrap_or(0);
    if max < 4 || shared < max / 2 {
        return ellipsize_middle(text, max);
    }
    // A little of what came before the difference, for context.
    let context = (max / 4).min(shared);
    let start = (shared - context).min(chars.len() - (max - 1));
    let window: String = if start + (max - 1) >= chars.len() {
        chars[start..].iter().collect()
    } else {
        // Cut at both ends: one `…` in front and one behind.
        let mut w: String = chars[start..start + max - 2].iter().collect();
        w.push('…');
        w
    };
    format!("…{window}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Cut by columns: a wide character is never split and never pushes
    // the result past the budget.
    #[test]
    fn wide_characters_are_cut_by_the_columns_they_take() {
        assert_eq!(text_width("日本語"), 6);
        for max in 1..12 {
            let cut = ellipsize_middle("feat/日本語のブランチ名", max);
            assert!(text_width(&cut) <= max, "{cut:?} at {max}");
            let names = vec![
                "feat/日本語のブランチ名".to_string(),
                "feat/日本語のもう一つ".to_string(),
            ];
            let cut = ellipsize_distinct(&names[0], &names, max);
            assert!(text_width(&cut) <= max, "{cut:?} at {max}");
            let cut = ellipsize_end("  api  failed    ポートが使われています", max);
            assert!(text_width(&cut) <= max, "{cut:?} at {max}");
        }
        assert_eq!(visible_width("\x1b[31m日本\x1b[0m"), 4);
    }

    #[test]
    fn ellipsize_end_keeps_short_strings_and_truncates_long_ones() {
        assert_eq!(ellipsize_end("short", 10), "short");
        assert_eq!(ellipsize_end("abcdefghij", 5), "abcd…");
        assert_eq!(ellipsize_end("日本語", 4), "日…");
        assert_eq!(ellipsize_end("abc", 0), "");
    }

    #[test]
    fn plain_text_carries_no_escapes() {
        let style = Style::plain();
        assert_eq!(style.paint("running", Paint::Good), "running");
    }

    #[test]
    fn colour_wraps_and_the_width_ignores_it() {
        let style = Style::with(true, None);
        let painted = style.paint("running", Paint::Good);
        assert_ne!(painted, "running");
        assert_eq!(visible_width(&painted), 7);
    }

    #[test]
    fn no_color_turns_colour_off_even_on_a_terminal() {
        assert!(Style::decide_from(true, None, Some("xterm-256color")).color());
        assert!(!Style::decide_from(true, Some("1"), None).color());
        assert!(
            Style::decide_from(true, Some(""), None).color(),
            "an empty NO_COLOR is unset, per the convention"
        );
        assert!(!Style::decide_from(true, None, Some("dumb")).color());
        assert!(
            !Style::decide_from(false, None, None).color(),
            "never for a pipe"
        );
    }

    #[test]
    fn home_is_written_as_a_tilde_and_only_home() {
        let style = Style::with(false, Some(PathBuf::from("/Users/me")));
        assert_eq!(style.tilde("/Users/me/.pando/x"), "~/.pando/x");
        assert_eq!(style.tilde("/Users/me"), "~");
        assert_eq!(style.tilde("/Users/meg/x"), "/Users/meg/x");
        assert_eq!(style.tilde("/tmp/x"), "/tmp/x");
    }

    #[cfg(unix)]
    #[test]
    fn a_home_reached_through_a_symlink_shortens_its_resolved_paths_too() {
        let dir = tempfile::tempdir().unwrap();
        let real = std::fs::canonicalize(dir.path()).unwrap().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let style = Style::with(false, Some(link.clone()));
        let under = |p: &Path| style.tilde(&p.display().to_string());
        assert_eq!(under(&real.join("code")), "~/code");
        assert_eq!(under(&real), "~");
        assert_eq!(under(&link.join("code")), "~/code");
        assert_eq!(under(dir.path()), dir.path().display().to_string());
    }

    #[test]
    fn the_middle_goes_so_both_ends_survive() {
        let a = ellipsize_middle("feature+very-long-branch-name-number-1", 20);
        let b = ellipsize_middle("feature+very-long-branch-name-number-2", 20);
        assert_eq!(a.chars().count(), 20);
        assert_ne!(a, b, "the distinguishing end is kept");
        assert!(a.starts_with("feature+") && a.ends_with("-1"), "{a}");
        assert_eq!(ellipsize_middle("short", 20), "short");
    }

    #[test]
    fn a_shared_prefix_is_what_goes_when_names_differ_late() {
        let names = vec![
            "feature/very-long-branch-name-number-1-with-extra-words".to_string(),
            "feature/very-long-branch-name-number-2-with-extra-words".to_string(),
            "feat/a".to_string(),
        ];
        let cut: Vec<String> = names
            .iter()
            .map(|n| ellipsize_distinct(n, &names, 28))
            .collect();
        assert_ne!(cut[0], cut[1], "{cut:?}");
        assert!(cut[0].contains("number-1"), "{cut:?}");
        assert!(cut[1].contains("number-2"), "{cut:?}");
        for c in &cut[..2] {
            assert!(c.chars().count() <= 28, "{c}");
        }
        assert_eq!(cut[2], "feat/a", "a name that fits is left alone");
        // Differing early: the ordinary middle cut.
        let early = vec!["alpha-one-two-three-four".to_string(), "beta".to_string()];
        assert_eq!(
            ellipsize_distinct(&early[0], &early, 10),
            ellipsize_middle(&early[0], 10)
        );
    }
}
