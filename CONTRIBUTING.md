# Contributing to pando

Thank you for wanting to help a forest grow. This guide covers how to
build pando, how its code is laid out, the rules its tests follow, and
how a change becomes a commit on `main`.

By taking part you agree to the [code of conduct](CODE_OF_CONDUCT.md).
Security problems go through [SECURITY.md](SECURITY.md), never a public
issue.

## Where to start

- **Found a bug?** Open an issue with `pando --version`, your OS, what you
  ran, and the output of `pando doctor` from the project it happened in.
  If pando misread your project, `pando signals` shows what it saw; attach
  it after removing anything private.
- **Pando broke on your project's shape?** That is the most valuable
  report there is. pando's main validation is a corpus of generated
  fixture repositories, and the first real project it met broke it in
  three ways no fixture had. Describe the shape (a monorepo with two apps
  and a native database, a Makefile that starts a backgrounded server…),
  not your code.
- **Want to add something?** Many of the most useful changes are a single
  row or a single TOML file. See [Adding to what pando knows](#adding-to-what-pando-knows).
- **Bigger idea?** Open an issue first, so the design can be agreed on
  before you write it.

## Building and testing

You need Rust 1.88 or newer (`rust-version` in `Cargo.toml`; rustup.rs
installs it) and `git`. Nothing else: the default test suite brings stand-ins for every
other tool it drives.

```bash
cargo build                 # target/debug/pando
cargo test                  # the whole suite
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

All three checks must be clean before every commit, and CI runs them on
every pull request.

On Windows, build and test inside WSL 2, on a clone in WSL's own
filesystem: pando does not build natively there yet. Avoid `/tmp` for
anything you mean to keep. A distro stops when nothing is running in it,
and with systemd on, its `/tmp` is emptied at the next start.

The integration tests are modules of one test binary,
`tests/integration.rs`. Run one file's tests with its module name:

```bash
cargo test --test integration cli::
```

Some tests reach real tools and are off unless you turn them on. They
start everything they need themselves, in temporary directories, and
never touch a server of yours:

| Set | To run | Needs |
|---|---|---|
| `PANDO_TEST_DOCKER=1` | the tests against the real Docker | a running Docker daemon |
| `PANDO_TEST_NATIVE=1` | the real database engines, and namespaced mode against throwaway MariaDB and Redis | the engines installed; skips each one that is missing |
| `PANDO_TEST_CLOUDFLARED=1` | one real Cloudflare quick tunnel | `cloudflared`, and the network |

Two things to know about running tests:

- **Never run two `cargo test`s at once.** A few tests measure readiness
  and timeouts; under a parallel build they fail for reasons that have
  nothing to do with your change. If a readiness or timeout test fails,
  rerun it alone before concluding anything.
- **A slow test is a bug.** A test that has to wait out a production
  timeout shortens it through a seam, the way `services::with_probe_timeout`
  does, rather than adding seconds to every run.

## Trying pando by hand

Point pando only at a fixture while you develop, never at a real
repository you care about. `scripts/fixture-repo.sh` builds the same
repositories the tests use:

```bash
scripts/fixture-repo.sh --list
scripts/fixture-repo.sh mono-web-api --listener
```

It prints the fixture's path and the `PANDO_HOME` to use with it, so
nothing is written to your own `~/.pando` either. `--listener` gives the
fixture a small Python server as its dev process, so `start` has
something real to start without installing a framework.

## How the code is laid out

[`src/lib.rs`](src/lib.rs) is the map: the dependency direction between
modules, inner to outer with no upward imports, and a table of where to
add each kind of thing. Read it first. Three rules hold the shape:

- **One fact, one row.** What pando knows about the ecosystem is data:
  package managers, frameworks and service images in `src/catalog/`,
  languages and version managers in `src/runtime/languages.rs`, native
  services in `src/recipes/builtin/*.toml`. Never add a second list of
  the same fact. Where two lists must differ in order, keep both and add
  a test holding them to one set.
- **A module with more than one concern is a directory.** Its `mod.rs`
  holds the module doc and `pub use` re-exports, so callers never name a
  file; each file below it is one concern with a `//!` line; tests go in
  `tests.rs`. Items shared between sibling files are `pub(super)`, no
  wider.
- **Contracts have tests.** A JSON shape `agent/json.md` documents, a
  setup slot name, a CLI verb, a TUI key: each is held to the code by a
  test. Adding one without its test is not done.

## The two promises

Every change keeps these, and the tests enforce both:

1. **pando never writes into the repository.** Not a config file, not a
   gitignore line, not a lockfile. Everything lives under `~/.pando`;
   `src/paths.rs` is the only place a path to write is made, and nothing
   there points inside a repository. The invariant test holds it.
2. **pando never touches data it did not make.** Namespaced mode writes
   into the developer's own database server, so every drop and flush goes
   through `namespace::may_drop`, and only what pando's records say it made
   is ever removed.

## The testing rules

- **Mutating commands run only against generated fixtures.** `new`,
  `start`, `stop`, `rm`, `share`, `init` and `check` run in tests against
  repositories the tests create in temporary directories.
- **Tests are hermetic.** None reads the developer's shell profile or
  depends on a tool they installed. Every login shell a test starts gets
  an empty `HOME`, and a fixture that runs a package manager or a runtime
  gets a stand-in (`common::fake_pnpm`, `common::fake_node`, the fake
  `docker` and `cloudflared`). A test that passes only with a real tool,
  or only because a shell is slow, is not done.
- **Nothing under test reaches GitHub or a real database server.** The
  TUI's `gh` workers return early under `cfg!(test)`; `gh` parsing is
  tested on fixed JSON.
- **A fixture that breaks pando is a gift.** Add the shape to
  `tests/common/mod.rs` with a test that fails without your fix. A corpus
  of tidy shapes proves very little.

## Adding to what pando knows

| To add | Edit |
|---|---|
| a package manager or lockfile | a row in `src/catalog/package_managers.rs` |
| a framework | a row in `RULES` in `src/catalog/frameworks.rs` |
| a service image a compose file uses | a row in `IMAGES` in `src/catalog/images.rs` |
| a language or version manager | `src/runtime/languages.rs` |
| a native service | a TOML file in `src/recipes/builtin/`, and a row in `recipes::BUILT_IN` |
| a colour theme | a TOML file in `src/theme/builtin/`, and a row in `theme::BUILT_IN` |
| a CLI verb | `src/cli/mod.rs`, its output under `src/cli/`, its behaviour in `src/actions/`, and the README's command list, which a test holds to clap |
| a TUI key | `src/tui/app/`, with its row in `src/tui/app/keymap.rs`, which a test holds to the handler |

A native service recipe is the same format as one a user drops into
`~/.pando/recipes/`: read `src/recipes/builtin/postgres.toml` and
`redis.toml` before writing a new one. A theme is a background, a
foreground and seven accents for each of a dark and a light half; every
other colour is derived.

## Commits and pull requests

- **One logical change per commit**, in the
  [Conventional Commits](https://www.conventionalcommits.org) style:
  `feat(tui): …`, `fix(namespace): …`, `test(detect): …`, `docs: …`.
  The subject says what changed for the person using pando.
- **Each commit passes** `cargo test`, `cargo clippy --all-targets -- -D warnings`
  and `cargo fmt --check` on its own.
- **Say why.** A commit body or pull request description explains the
  problem and the choice made, not only the diff. Comments in the code do
  the same: they explain why a thing is the way it is.
- **Keep pull requests focused.** Two unrelated fixes are two pull requests.
- **Documentation moves with the code.** A new command, flag or key is in
  the README and in `--help` in the same change.

The README's hero image, `assets/pando.svg`, is drawn by the same code as
the setup screen. After changing anything in `src/art/`, regenerate it
with `cargo run --example readme-art`.

## Licence of contributions

pando is licensed under the [GNU Affero General Public License v3.0
only](LICENSE). By submitting a contribution you agree that it is your
own work, or that you have the right to submit it, and that it is
licensed under the same terms.
