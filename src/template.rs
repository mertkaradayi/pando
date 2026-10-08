//! `{port:web}` and friends: the one substitution language pando's config
//! has.
//!
//! Templates are how a port reaches an app without pando editing the
//! project's own files. A framework that takes its port on the command line
//! gets `{port:web}` in `cmd`; one that reads an environment variable gets
//! `{port:web}` in `env`. Either way the project stays boring.
//!
//! Rendering is deliberately strict — an unknown placeholder is a config
//! error naming it, not an empty string silently spliced into a command —
//! with two escapes so a shell command is never impossible to write:
//!
//! - `{{` and `}}` render as `{` and `}`.
//! - `${…}` is left alone, because that is a shell variable, not a
//!   placeholder. Anything else that does not look like `{name}` or
//!   `{name:arg}` — brace expansion, a JSON literal — is left alone too.
//!
//! A command the shell runs is rendered with [`render_shell`], which
//! quotes each value for where it stands in the command: a path with a
//! space, or a branch a fork named `x$(…)`, is one word of data and never
//! shell. An `env` value is not a command, and is rendered as it is.

use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::path::Path;

/// Everything a template can refer to for one process of one worktree.
pub struct Context<'a> {
    pub name: &'a str,
    pub branch: Option<&'a str>,
    pub worktree: &'a Path,
    pub root: &'a Path,
    pub project: &'a str,
    /// Role name to allocated port.
    pub ports: &'a BTreeMap<String, u16>,
    /// What a bare `{port}` means: the role this process owns. `None` when
    /// it owns none, which makes `{port}` an error naming the roles it could
    /// have used.
    pub default_role: Option<&'a str>,
    /// This process's log file, for a command that wants to write to it.
    pub log: Option<&'a Path>,
}

/// What a set of placeholder names means.
///
/// The lexer below — `{{`, `}}`, the `${…}` exemption, what counts as a
/// placeholder at all — is the one thing every template in pando shares,
/// and only the *names* differ: a process command resolves `{port:web}`
/// against a worktree's roles, a service recipe resolves `{datadir}`
/// against a directory under pando's home. Splitting the two here is what
/// keeps a recipe a template without a second, subtly different
/// substitution language growing up beside this one.
pub trait Resolver {
    /// The value for `{key}` or `{key:arg}`, or an error naming what it
    /// could have been instead.
    fn resolve(&self, key: &str, arg: Option<&str>) -> Result<String>;
}

impl Resolver for Context<'_> {
    fn resolve(&self, key: &str, arg: Option<&str>) -> Result<String> {
        resolve(key, arg, self)
    }
}

/// Substitutes every placeholder in `text`, or fails naming the first one it
/// cannot resolve.
pub fn render(text: &str, ctx: &Context<'_>) -> Result<String> {
    render_with(text, ctx)
}

/// [`render`] against any other set of names.
pub fn render_with(text: &str, resolver: &dyn Resolver) -> Result<String> {
    render_inner(text, resolver, false)
}

/// [`render`] for a command a shell runs: each value quoted for where it
/// stands — a bare word where it is safe as one and single-quoted where
/// not, escaped for `"…"` inside double quotes, and spliced as `'\''`
/// inside single quotes. So `cd {worktree}`, `cd "{worktree}"` and
/// `cd '{worktree}'` all reach the one directory, whatever its name holds.
pub fn render_shell(text: &str, ctx: &Context<'_>) -> Result<String> {
    render_inner(text, ctx, true)
}

/// Where the shell is in a command, as far as the text read so far says.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Quoting {
    Bare,
    Single,
    Double,
}

/// A value, written so the shell reads it back as itself where `quoting`
/// says it stands.
fn quoted_for(value: &str, quoting: Quoting) -> String {
    match quoting {
        Quoting::Bare => crate::process::shell_word(value),
        Quoting::Single => value.replace('\'', "'\\''"),
        Quoting::Double => {
            let mut out = String::with_capacity(value.len());
            for c in value.chars() {
                if matches!(c, '\\' | '"' | '$' | '`') {
                    out.push('\\');
                }
                out.push(c);
            }
            out
        }
    }
}

fn render_inner(text: &str, resolver: &dyn Resolver, shell: bool) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let bytes: Vec<char> = text.chars().collect();
    let mut quoting = Quoting::Bare;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == '{' && bytes.get(i + 1) == Some(&'{') {
            out.push('{');
            i += 2;
            continue;
        }
        if c == '}' && bytes.get(i + 1) == Some(&'}') {
            out.push('}');
            i += 2;
            continue;
        }
        // `${PORT}` is the shell's, not ours.
        let shell_variable = c == '{' && i > 0 && bytes[i - 1] == '$';
        if c == '{'
            && !shell_variable
            && let Some(end) = find_close(&bytes, i)
        {
            let inner: String = bytes[i + 1..end].iter().collect();
            if let Some((key, arg)) = split_placeholder(&inner) {
                let value = resolver.resolve(key, arg)?;
                match shell {
                    true => out.push_str(&quoted_for(&value, quoting)),
                    false => out.push_str(&value),
                }
                i = end + 1;
                continue;
            }
        }
        // The quoting the command's own text sets up, which the value
        // after it has to fit: a backslash outside single quotes takes the
        // next character with it.
        match (quoting, c) {
            (Quoting::Bare | Quoting::Double, '\\')
                if bytes.get(i + 1).is_some_and(|next| *next != '{') =>
            {
                out.push(c);
                out.push(bytes[i + 1]);
                i += 2;
                continue;
            }
            (Quoting::Bare, '\'') => quoting = Quoting::Single,
            (Quoting::Bare, '"') => quoting = Quoting::Double,
            (Quoting::Single, '\'') | (Quoting::Double, '"') => quoting = Quoting::Bare,
            _ => {}
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}

/// The matching `}` for the `{` at `open`, if it is on the same run of
/// non-brace characters. A `{` with no `}` is just a brace.
fn find_close(chars: &[char], open: usize) -> Option<usize> {
    chars[open + 1..]
        .iter()
        .position(|&c| c == '}' || c == '{')
        .and_then(|offset| {
            let at = open + 1 + offset;
            (chars[at] == '}').then_some(at)
        })
}

/// Splits `port:web` into `("port", Some("web"))`. `None` when the content
/// does not look like a placeholder at all, in which case the braces are
/// literal text.
fn split_placeholder(inner: &str) -> Option<(&str, Option<&str>)> {
    let (key, arg) = match inner.split_once(':') {
        Some((key, arg)) => (key, Some(arg)),
        None => (inner, None),
    };
    let key_ok = !key.is_empty()
        && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let arg_ok = arg.is_none_or(|a| {
        !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    });
    (key_ok && arg_ok).then_some((key, arg))
}

fn resolve(key: &str, arg: Option<&str>, ctx: &Context<'_>) -> Result<String> {
    match (key, arg) {
        ("port", role) => {
            let role = match role.or(ctx.default_role) {
                Some(role) => role,
                None => bail!(
                    "{{port}} needs a role: this process declares no ports, so there is nothing \
                     to substitute"
                ),
            };
            match ctx.ports.get(role) {
                Some(port) => Ok(port.to_string()),
                None => bail!(
                    "{{port:{role}}} names a role this worktree has no port for — {}",
                    known_roles(ctx)
                ),
            }
        }
        ("name", None) => Ok(ctx.name.to_string()),
        ("branch", None) => Ok(ctx.branch.unwrap_or(ctx.name).to_string()),
        ("worktree", None) => Ok(ctx.worktree.display().to_string()),
        ("root", None) => Ok(ctx.root.display().to_string()),
        ("project", None) => Ok(ctx.project.to_string()),
        ("log", None) => match ctx.log {
            Some(path) => Ok(path.display().to_string()),
            None => bail!("{{log}} is only available inside a process or hook command"),
        },
        // A known name with an argument it does not take is worth its own
        // message: `{name:web}` is a typo for `{port:web}` far more often
        // than it is a placeholder nobody implemented.
        (key, Some(arg)) if KNOWN.contains(&key) => {
            bail!("{{{key}:{arg}}} — {key} takes no argument; did you mean {{port:{arg}}}?")
        }
        (key, _) => bail!(
            "unknown placeholder {{{key}}} — pando understands {}",
            KNOWN.join(", ")
        ),
    }
}

const KNOWN: [&str; 7] = [
    "port", "name", "branch", "worktree", "root", "project", "log",
];

fn known_roles(ctx: &Context<'_>) -> String {
    if ctx.ports.is_empty() {
        return "this worktree has no ports assigned".to_string();
    }
    format!(
        "it has {}",
        ctx.ports
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn ports() -> BTreeMap<String, u16> {
        BTreeMap::from([("web".to_string(), 17_342), ("api".to_string(), 17_343)])
    }

    struct Owned {
        ports: BTreeMap<String, u16>,
        worktree: PathBuf,
        root: PathBuf,
    }

    fn owned() -> Owned {
        Owned {
            ports: ports(),
            worktree: PathBuf::from("/home/me/.pando/worktrees/feat+one"),
            root: PathBuf::from("/home/me/code/acme-shop"),
        }
    }

    fn ctx(o: &Owned) -> Context<'_> {
        Context {
            name: "feat+one",
            branch: Some("feat/one"),
            worktree: &o.worktree,
            root: &o.root,
            project: "acme-shop-3f9a2c1d",
            ports: &o.ports,
            default_role: Some("web"),
            log: None,
        }
    }

    #[test]
    fn every_placeholder_renders() {
        let o = owned();
        let c = ctx(&o);
        assert_eq!(render("{port}", &c).unwrap(), "17342");
        assert_eq!(render("{port:web}", &c).unwrap(), "17342");
        assert_eq!(render("{port:api}", &c).unwrap(), "17343");
        assert_eq!(render("{name}", &c).unwrap(), "feat+one");
        assert_eq!(render("{branch}", &c).unwrap(), "feat/one");
        assert_eq!(
            render("{worktree}", &c).unwrap(),
            "/home/me/.pando/worktrees/feat+one"
        );
        assert_eq!(render("{root}", &c).unwrap(), "/home/me/code/acme-shop");
        assert_eq!(render("{project}", &c).unwrap(), "acme-shop-3f9a2c1d");
    }

    #[test]
    fn placeholders_render_inside_a_real_command() {
        let o = owned();
        assert_eq!(
            render(
                "uv run python manage.py runserver 127.0.0.1:{port:web}",
                &ctx(&o)
            )
            .unwrap(),
            "uv run python manage.py runserver 127.0.0.1:17342"
        );
        assert_eq!(
            render("http://localhost:{port:api}/v1", &ctx(&o)).unwrap(),
            "http://localhost:17343/v1"
        );
    }

    #[test]
    fn an_unknown_placeholder_names_itself() {
        let o = owned();
        let err = render("pnpm dev --host {hostname}", &ctx(&o)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("{hostname}"), "{msg}");
        assert!(
            msg.contains("port"),
            "the message lists what is available: {msg}"
        );
    }

    #[test]
    fn an_unknown_role_names_the_roles_that_exist() {
        let o = owned();
        let err = render("dev --port {port:worker}", &ctx(&o)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("{port:worker}"), "{msg}");
        assert!(msg.contains("web") && msg.contains("api"), "{msg}");
    }

    #[test]
    fn a_bare_port_without_a_role_says_why() {
        let o = owned();
        let mut c = ctx(&o);
        c.default_role = None;
        let err = render("dev --port {port}", &c).unwrap_err();
        assert!(format!("{err:#}").contains("needs a role"));
    }

    // Without this, `PORT=3000 node server.js` style commands — and every
    // `${VAR}` in a shell one-liner — would be unwritable.
    #[test]
    fn a_shell_variable_is_left_alone() {
        let o = owned();
        let c = ctx(&o);
        assert_eq!(
            render("node -e \"console.log(process.env.PORT)\" ${EXTRA}", &c).unwrap(),
            "node -e \"console.log(process.env.PORT)\" ${EXTRA}"
        );
        assert_eq!(
            render("echo ${PORT:-3000}", &c).unwrap(),
            "echo ${PORT:-3000}"
        );
    }

    #[test]
    fn braces_that_are_not_placeholders_are_literal() {
        let o = owned();
        let c = ctx(&o);
        // Brace expansion, a JSON body, and an awk program.
        assert_eq!(
            render("cp a.{js,ts} out/", &c).unwrap(),
            "cp a.{js,ts} out/"
        );
        assert_eq!(
            render("curl -d '{\"a\": 1}' http://x", &c).unwrap(),
            "curl -d '{\"a\": 1}' http://x"
        );
        assert_eq!(render("awk '{print $1}'", &c).unwrap(), "awk '{print $1}'");
        assert_eq!(render("a { b", &c).unwrap(), "a { b");
    }

    #[test]
    fn doubled_braces_escape_to_one() {
        let o = owned();
        let c = ctx(&o);
        assert_eq!(render("{{port}}", &c).unwrap(), "{port}");
        assert_eq!(render("{{{port}}}", &c).unwrap(), "{17342}");
        assert_eq!(render("}}{{", &c).unwrap(), "}{");
    }

    #[test]
    fn a_known_name_with_an_argument_suggests_the_port_form() {
        let o = owned();
        let err = render("{name:web}", &ctx(&o)).unwrap_err();
        assert!(format!("{err:#}").contains("{port:web}"));
    }

    #[test]
    fn the_log_path_is_only_available_when_there_is_one() {
        let o = owned();
        let mut c = ctx(&o);
        assert!(render("tee {log}", &c).is_err());
        let log = PathBuf::from("/home/me/.pando/logs/feat+one/dev.log");
        c.log = Some(&log);
        assert_eq!(
            render("tee {log}", &c).unwrap(),
            "tee /home/me/.pando/logs/feat+one/dev.log"
        );
    }

    #[test]
    fn a_branch_falls_back_to_the_directory_name_when_detached() {
        let o = owned();
        let mut c = ctx(&o);
        c.branch = None;
        assert_eq!(render("{branch}", &c).unwrap(), "feat+one");
    }

    #[test]
    fn text_without_placeholders_is_returned_unchanged() {
        let o = owned();
        assert_eq!(render("pnpm dev", &ctx(&o)).unwrap(), "pnpm dev");
        assert_eq!(render("", &ctx(&o)).unwrap(), "");
    }

    /// What a POSIX shell prints for `command`.
    fn shell_says(command: &str) -> String {
        let out = crate::platform::shell::posix()
            .unwrap()
            .arg("-c")
            .arg(command)
            .output()
            .unwrap();
        assert!(out.status.success(), "{command}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    // Values a fork's branch or a home directory can hold, each read back
    // by the shell as itself, however the command writes the placeholder.
    // Spliced raw, a space split the path and `$(…)` ran.
    #[test]
    fn a_value_in_a_command_is_one_word_of_data_however_it_is_quoted() {
        let dir = tempfile::tempdir().unwrap();
        let ran = dir.path().join("ran");
        for branch in [
            "feat/plain".to_string(),
            "with space".to_string(),
            format!("x$(touch {})", ran.display()),
            "it's".to_string(),
            "say \"hi\" `now` \\ $HOME".to_string(),
        ] {
            let o = owned();
            let mut c = ctx(&o);
            c.branch = Some(&branch);
            for command in [
                "printf %s {branch}",
                "printf %s \"{branch}\"",
                "printf %s '{branch}'",
                "printf %s \"[{branch}]\"",
                "printf %s pre-{branch}-post",
            ] {
                let rendered = render_shell(command, &c).unwrap();
                let expected = match command {
                    "printf %s \"[{branch}]\"" => format!("[{branch}]"),
                    "printf %s pre-{branch}-post" => format!("pre-{branch}-post"),
                    _ => branch.clone(),
                };
                assert_eq!(shell_says(&rendered), expected, "{command} → {rendered}");
            }
        }
        assert!(!ran.exists(), "a value ran as a command");
    }

    // A plain value is left as it was, so every command that rendered
    // before renders the same — its hook's fingerprint with it.
    #[test]
    fn a_plain_value_renders_as_before_in_a_command() {
        let o = owned();
        let c = ctx(&o);
        for command in [
            "cd {worktree} && pnpm dev --port {port}",
            "cd \"{worktree}\" && echo '{branch}' {{x}} ${PORT}",
            "echo \\{port}",
        ] {
            assert_eq!(
                render_shell(command, &c).unwrap(),
                render(command, &c).unwrap(),
                "{command}"
            );
        }
    }
}
