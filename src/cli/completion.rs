//! Completing a worktree's name from the worktrees there are.
//!
//! clap_complete's scripts are static: an argument it knows nothing about
//! completes as a file name. Its dynamic engine is behind an unstable
//! feature, so this does the one dynamic thing pando needs by hand: every
//! worktree argument carries the value name [`WORKTREE`], and the script
//! clap generates is given a small function that asks `pando ls --names`
//! at completion time, wired to exactly those arguments. zsh, bash and
//! fish get it; any other shell keeps the static script.

use crate::paths::PandoPaths;
use crate::worktree;
use anyhow::Result;
use clap::CommandFactory;
use clap_complete::Shell;
use std::io::Write;

/// The value name of every argument that takes a worktree.
pub(super) const WORKTREE: &str = "WORKTREE";

/// What a script runs to list the names. Quiet, so completing outside a
/// repository offers nothing rather than printing an error mid-line.
const LIST: &str = "pando ls --names 2>/dev/null";

/// The zsh function the worktree arguments call.
const ZSH_FUNCTION: &str = "_pando_worktrees";

/// `pando ls --names`: every name a worktree answers to that a person
/// would type — its branch where it is named for one, its directory
/// otherwise — one per line, the main checkout included.
pub(super) fn names<W: Write>(paths: &PandoPaths, out: &mut W) -> Result<()> {
    let discovery = worktree::discover_all(&paths.project)?;
    let listed = discovery
        .worktrees
        .iter()
        .filter(|w| !worktree::is_check(&w.name));
    for w in std::iter::once(&discovery.main).chain(listed) {
        writeln!(out, "{}", w.display_name())?;
    }
    Ok(())
}

/// The subcommands with an argument that takes a worktree.
pub(super) fn verbs_taking_a_worktree() -> Vec<String> {
    super::Cli::command()
        .get_subcommands()
        .filter(|verb| {
            verb.get_positionals().any(|arg| {
                arg.get_value_names()
                    .is_some_and(|names| names.iter().any(|n| n.as_str() == WORKTREE))
            })
        })
        .map(|verb| verb.get_name().to_string())
        .collect()
}

/// `script`, as clap generated it for `shell`, with worktree arguments
/// completing worktree names.
pub(super) fn with_worktree_names(shell: Shell, script: &str) -> String {
    let verbs = verbs_taking_a_worktree();
    match shell {
        Shell::Zsh => zsh(script),
        Shell::Bash => bash(script, &verbs),
        Shell::Fish => fish(script, &verbs),
        _ => script.to_string(),
    }
}

/// zsh: a worktree positional is `'::name -- help:' \` — its action empty
/// because the argument's hint is `Other` — and gets the function as its
/// action; `'*::name -- help:' \` is one that takes several, `rm`'s. The
/// function is defined before `_pando`, which calls it.
fn zsh(script: &str) -> String {
    let function = format!(
        "{ZSH_FUNCTION}() {{\n    local -a names\n    names=(${{(f)\"$({LIST})\"}})\n    \
         compadd -a names\n}}\n\n"
    );
    let mut out = String::with_capacity(script.len() + function.len());
    let mut defined = false;
    for line in script.split_inclusive('\n') {
        if !defined && line.starts_with("_pando() {") {
            out.push_str(&function);
            defined = true;
        }
        let trimmed = line.trim_start();
        let worktree_arg = ["':name -- ", "'::name -- ", "'*:name -- ", "'*::name -- "]
            .iter()
            .any(|start| trimmed.starts_with(start))
            && line.trim_end().ends_with(":' \\");
        match worktree_arg {
            true => {
                let at = line.rfind(":' \\").expect("just checked");
                out.push_str(&line[..at + 1]);
                out.push_str(ZSH_FUNCTION);
                out.push_str(&line[at + 1..]);
            }
            false => out.push_str(line),
        }
    }
    out
}

/// bash: each verb's candidate list gains the names, asked for when the
/// completion runs.
fn bash(script: &str, verbs: &[String]) -> String {
    let mut out = String::with_capacity(script.len());
    let mut in_verb = false;
    for line in script.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(case) = trimmed.strip_suffix(')')
            && let Some(verb) = case.strip_prefix("pando__subcmd__")
        {
            in_verb = verbs.iter().any(|v| v == verb);
        }
        if in_verb
            && let Some(opts) = trimmed.strip_prefix("opts=\"")
            && let Some(opts) = opts.strip_suffix('"')
        {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push_str(&format!("{indent}opts=\"{opts} $({LIST})\"\n"));
            in_verb = false;
            continue;
        }
        out.push_str(line);
    }
    out
}

/// fish: one more rule, for the verbs that take a worktree, listing the
/// names instead of files.
fn fish(script: &str, verbs: &[String]) -> String {
    format!(
        "{script}complete -c pando -n \"__fish_seen_subcommand_from {}\" -f -a \"({})\"\n",
        verbs.join(" "),
        LIST
    )
}
