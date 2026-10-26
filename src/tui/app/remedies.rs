//! CLI remedies rewritten as the TUI keys that do the same thing.
//!
//! An action's error ends with what to do next, and the actions are shared
//! with the CLI, so that advice is sometimes a flag: `pass --force`,
//! `start without --isolated`. In the TUI nobody types a flag. Each row
//! below is one phrase and the key that stands for it; a phrase the
//! actions no longer use simply never matches, so the table can only go
//! stale, never break a message.

/// Phrase, replacement. Longest first where one contains another, so the
/// specific rewrite wins over the general one.
const REMEDIES: &[(&str, &str)] = &[
    // The neutral spellings in `crate::remedy`: "confirm" is a flag on the
    // command line and a key here.
    (
        "confirm to let git discard them",
        "press F in the remove dialog to let git discard them",
    ),
    (
        "confirm to remove it with the set",
        "press F in the remove dialog to remove it with the set",
    ),
    (
        "confirm to remove it without Docker",
        "press F in the remove dialog to remove it without Docker",
    ),
    (
        "confirm to remove it anyway",
        "press y in the remove dialog to remove it anyway",
    ),
    (
        "name the branch to fork it from",
        "pick the base with tab in the new-worktree dialog",
    ),
    (
        "start it on the project's shared services instead",
        "press S to start it on the project's shared services instead",
    ),
    (
        "start it on the main checkout's services on purpose",
        "press S to start it on the main checkout's services on purpose",
    ),
    (
        "or pass --force to let git discard them",
        "or press F in the remove dialog to remove it anyway",
    ),
    (
        "pass --force to let git discard them",
        "press F in the remove dialog to remove it anyway",
    ),
    (
        "pass --force",
        "press F in the remove dialog to remove it anyway",
    ),
    ("start without --isolated", "press S to start it shared"),
    ("start it without --isolated", "press S to start it shared"),
    ("start --shared", "S (start it shared)"),
    ("start --isolated", "i (start it isolated)"),
    (
        "pass --yes to remove it anyway",
        "confirm in the remove dialog",
    ),
    ("--force", "F in the remove dialog"),
    ("--isolated", "i"),
    ("--shared", "S"),
];

/// `message` with every CLI remedy it contains said as a TUI key.
///
/// Two things are never rewritten. A command in backticks is one to type
/// in a shell — `` `git worktree remove --force` `` — and a key in the
/// middle of it would make it nonsense. And a phrase only matches whole:
/// `start --shared` is not inside `restart --shared`, nor `--force` inside
/// `--force-with-lease`.
pub fn as_tui_remedy(message: &str) -> String {
    message
        .split('`')
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 1 {
                return part.to_string();
            }
            REMEDIES
                .iter()
                .fold(part.to_string(), |text, (phrase, key)| {
                    replace_whole(&text, phrase, key)
                })
        })
        .collect::<Vec<_>>()
        .join("`")
}

/// `text` with every occurrence of `phrase` that is not part of a longer
/// word or flag replaced by `with`.
fn replace_whole(text: &str, phrase: &str, with: &str) -> String {
    let joins = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(phrase) {
        let before = if at > 0 {
            rest[..at].chars().next_back()
        } else {
            out.chars().next_back()
        };
        let after = rest[at + phrase.len()..].chars().next();
        out.push_str(&rest[..at]);
        if joins(before) || joins(after) {
            out.push_str(phrase);
        } else {
            out.push_str(with);
        }
        rest = &rest[at + phrase.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::as_tui_remedy;

    // A neutral remedy with no row here would reach the screen as
    // "confirm", which in the TUI means nothing until it names the key.
    #[test]
    fn every_neutral_remedy_is_said_as_a_key() {
        for remedy in crate::remedy::ALL {
            assert_ne!(
                as_tui_remedy(remedy.neutral),
                remedy.neutral,
                "{:?} has no TUI spelling",
                remedy.neutral
            );
        }
    }
}
