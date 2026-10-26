//! The way out of a refusal, said once for every front end.
//!
//! An error from `actions` reaches a person through the CLI or through the
//! TUI, and the TUI shows it verbatim. So the remedy in it is phrased as
//! what to do — "start it on the project's shared services" — never as a
//! flag, which means nothing on a screen with no command line. The CLI,
//! where a flag *is* the way to do it, rewrites each phrase into its flag
//! form with [`for_cli`] before printing, so a message there is exactly as
//! actionable as it was.
//!
//! One row per remedy: a message that needs one names the constant, never
//! a copy of its text, or the rewrite would silently stop matching.

/// One way out, in both of its spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Remedy {
    /// What an error says, whoever shows it.
    pub neutral: &'static str,
    /// The same thing as the CLI spells it.
    pub cli: &'static str,
}

impl std::fmt::Display for Remedy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.neutral)
    }
}

/// Isolation cannot happen; the shared services still can.
pub const SHARED: Remedy = Remedy {
    neutral: "start it on the project's shared services instead",
    cli: "start it without --isolated (with --shared, if it is isolated already)",
};

/// A namespaced worktree none of whose namespaces can be had any more; the
/// main checkout's services still can, asked for.
pub const SHARED_ON_PURPOSE: Remedy = Remedy {
    neutral: "start it on the main checkout's services on purpose",
    cli: "start it with --shared to run it on the main checkout's services on purpose",
};

/// `rm` of a worktree pando did not create.
pub const REMOVE_ANYWAY: Remedy = Remedy {
    neutral: "confirm to remove it anyway",
    cli: "pass --yes to remove it anyway",
};

/// `rm` of a worktree with modified or untracked files.
pub const DISCARD_CHANGES: Remedy = Remedy {
    neutral: "confirm to let git discard them",
    cli: "pass --force to let git discard them",
};

/// `rm` of a set whose directory holds more than its checkouts.
pub const REMOVE_BESIDE: Remedy = Remedy {
    neutral: "confirm to remove it with the set",
    cli: "pass --force to remove it with the set",
};

/// `rm` while Docker is down, which would strand the worktree's volumes.
pub const REMOVE_WITHOUT_DOCKER: Remedy = Remedy {
    neutral: "start Docker and remove it again, or confirm to remove it without Docker",
    cli: "start Docker and run it again, or pass --force to remove it anyway",
};

/// `new` with no default branch to fork from.
pub const NAME_A_BASE: Remedy = Remedy {
    neutral: "name the branch to fork it from",
    cli: "pass --base <branch>",
};

/// Every remedy, for [`for_cli`] and the test that holds the two
/// spellings apart.
pub const ALL: &[Remedy] = &[
    SHARED,
    SHARED_ON_PURPOSE,
    REMOVE_ANYWAY,
    DISCARD_CHANGES,
    REMOVE_BESIDE,
    REMOVE_WITHOUT_DOCKER,
    NAME_A_BASE,
];

/// `message` with every remedy in it spelled the way the CLI spells it.
pub fn for_cli(message: &str) -> String {
    ALL.iter().fold(message.to_string(), |text, remedy| {
        text.replace(remedy.neutral, remedy.cli)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_neutral_remedy_names_no_flag_and_its_cli_form_does() {
        for remedy in ALL {
            assert!(!remedy.neutral.contains("--"), "{remedy:?}");
            assert!(remedy.cli.contains("--"), "{remedy:?}");
        }
    }

    // A neutral phrase inside another would be rewritten twice, or half.
    #[test]
    fn no_neutral_remedy_contains_another() {
        for a in ALL {
            for b in ALL {
                if a != b {
                    assert!(!a.neutral.contains(b.neutral), "{a:?} / {b:?}");
                }
            }
        }
    }

    #[test]
    fn the_cli_rewrites_a_remedy_inside_a_longer_message() {
        let message = format!("pando: Docker is not running — {SHARED}");
        assert_eq!(
            for_cli(&message),
            format!("pando: Docker is not running — {}", SHARED.cli)
        );
        assert_eq!(for_cli("nothing to rewrite"), "nothing to rewrite");
    }
}
