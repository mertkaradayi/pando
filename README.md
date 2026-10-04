<h1 align="center">pando, the worktree tool</h1>

<p align="center"><b>One repo. Every branch alive.</b></p>

<p align="center">
  <img src="assets/pando.svg" width="760" alt="The pando setup screen: the PANDO wordmark in gold over a grove of aspens whose stems share one root system. The leaves quake, and the roots light up green when the setup check passes.">
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: AGPL-3.0-only" src="https://img.shields.io/badge/license-AGPL--3.0--only-ebc34b?style=flat-square"></a>
  <a href="https://github.com/mertkaradayi/pando/actions/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/github/actions/workflow/status/mertkaradayi/pando/ci.yml?branch=main&style=flat-square&label=ci"></a>
  <img alt="Rust 2024 edition" src="https://img.shields.io/badge/rust-2024_edition-e6963c?style=flat-square&logo=rust">
  <img alt="Platform: macOS and Linux" src="https://img.shields.io/badge/platform-macOS_|_Linux-6e9beb?style=flat-square">
  <img alt="Version 0.8.0, pre-release" src="https://img.shields.io/badge/version-0.8.0_pre--release-b482e6?style=flat-square">
  <a href="CONTRIBUTING.md"><img alt="PRs welcome" src="https://img.shields.io/badge/PRs-welcome-50c878?style=flat-square"></a>
  <a href="https://github.com/mertkaradayi/pando/stargazers"><img alt="GitHub stars" src="https://img.shields.io/github/stars/mertkaradayi/pando?style=flat-square&logo=github&color=ebc34b"></a>
</p>

<p align="center">
  <a href="https://mertkaradayi.github.io/pando/"><b>Website</b></a> ·
  <a href="#get-started">Get started</a> ·
  <a href="#your-first-run">First run</a> ·
  <a href="#commands">Commands</a> ·
  <a href="#shared-namespaced-isolated">Modes</a> ·
  <a href="#for-agents">For agents</a> ·
  <a href="#contributing">Contributing</a> ·
  <a href="#status">Status</a>
</p>

> Pando is a forest that is one tree. Your repo is too. `pando` checks out every
> branch you are working on side by side, gives each one a running dev server,
> its own database, and a shareable URL, and never writes a byte into your repo.

## Get started

**1. Install pando.** On macOS or Linux, no Rust needed:

```bash
brew install mertkaradayi/tap/pando
```

No Homebrew? This one line does the same:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/mertkaradayi/pando/releases/latest/download/pando-cli-installer.sh | sh
```

[Install](#install) has the details and building from source.

**2. Open a terminal in a project you work on.** pando works on the git
repository you run it in: the one with the dev server you wish you could
run on two branches at once. Nothing else needs to be set up first.

**3. Run it:**

```bash
pando
```

The first time, pando opens its setup screen. Press `a` to copy a
one-line prompt, and paste it into Claude Code or Codex, opened in the same
project: your agent reads the project, answers pando's questions, and
proves the setup in a throwaway worktree. The screen turns green when it
passes. No agent? Press enter, and pando tries its own guess. Esc skips
the setup altogether. [Your first run](#your-first-run) walks through it.

**4. Work on a second branch next to the first:**

```bash
pando new feat/checkout       # its own checkout, dependencies installed
pando start feat/checkout     # its own dev server, on ports of its own
pando open feat/checkout      # in your browser
```

Your main checkout is untouched and still runs as before. In the TUI, the
same steps are `n`, then enter. `pando rm feat/checkout` stops it and
removes the checkout and its data.

**5. See what else your project needs:**

```bash
pando doctor
```

It lists every tool this project will have pando run, which ones are
installed, and the command that installs each one that is missing. Only
`git` is required; the rest is needed only for the feature that uses it:
[What else pando uses](#what-else-pando-uses).

**[mertkaradayi.github.io/pando](https://mertkaradayi.github.io/pando/)** tells the story in
pictures and has a copy of the TUI you can drive with your keyboard, in the
browser, before installing anything.

Created by [Mert Karadayi](https://github.com/mertkaradayi).

## Why

You are halfway through `feat/checkout`. A review lands on `fix/login-loop`.
Someone asks for a link to `feat/search`, and an agent in another terminal
wants a branch of its own to break things in.

Without pando, that is a stash, a checkout, a reinstall, a restart, a
reseeded database, and a port that is already taken, three times over.
With pando, every branch is its own checkout, already running:

```text
                             your repository
             one object store, one history, one root system
                                    ┃
    ┏━━━━━━━━━━━━━━━━━━━━┳━━━━━━━━━━┻━━━━━━━━━┳━━━━━━━━━━━━━━━━━━━━┓
    ┃                    ┃                    ┃                    ┃
  ⌂ main               ● feat/checkout      ● fix/login-loop     ○ feat/search
    your checkout,       web  :22809          web  :22815          stopped, one
    your servers         api  :22808          api  :22814          keypress from
                         shared database      its own postgres     running
                         ◈ public URL         in a container
```

Each stem is a [git worktree](https://git-scm.com/docs/git-worktree) with its
own processes, ports, dependencies and logs. What it runs on is your choice
per branch: your main checkout's database, a database of its own inside your
server, or private copies of every service. All of it on one terminal screen,
for people and for the coding agents working beside them.

## Sixty seconds

`pando` with no arguments is the TUI. Everything it does is a command too;
this is a real run, trimmed a little, on one of the fixture repositories
pando's tests are built on — a workspace with a web app, an API and a
Postgres in its compose file:

```console
$ pando new feat/checkout
pando: checking out feat/checkout
pando: provisioning
pando: installing
created feat+checkout at ~/.pando/projects/mono-web-api-3cb31588/worktrees/feat+checkout

$ pando start feat/checkout
pando: using postgres as this project's services (detected: docker-compose.yml, postgres:16 → DATABASE_URL)
pando: starting api
pando: starting web
started feat+checkout — http://localhost:22809

$ pando ls
NAME            STATUS   URL                     PORTS                GIT
main            stopped  -                       -                    clean main checkout
fix/login-loop  stopped  -                       -                    clean
feat/checkout   running  http://localhost:22809  api:22808 web:22809  clean
```

Nothing was configured by hand: pando read the lockfile, the dev scripts, the
env example and the compose file, and chose. `pando doctor` says what it
detected and from where.

## The story

Pando is a single quaking aspen in Fishlake National Forest, Utah. It looks like
a forest. It is one organism.

- About 47,000 stems, all genetically identical, sharing one root system
- About 43 hectares, roughly 106 acres
- About 6,000 metric tons, the heaviest known living thing
- A root system between 9,000 and 16,000 years old
- Each stem lives 100 to 130 years and is replaced from the roots
- Named in 1993; the Latin means "I spread"

Your repository works the same way.

| Pando | Your repo |
|---|---|
| The root system | The repo. One object store, one history, never touched. |
| Each stem | A worktree. A complete tree above ground, with its own running app and its own database. |
| Stems come and go | Branches come and go. The root persists. |
| "I spread" | Spread your branches out and let every one of them live. |

## What it does

- Lists, creates, and removes git worktrees for the repo you are in
- Keeps them light on disk: where the filesystem can clone (APFS on macOS;
  btrfs, XFS on Linux), a new worktree's files are copy-on-write clones of
  your main checkout's, and git writes only what the branch changed;
  `clone = ["node_modules"]` does the same for dependencies before the
  install
- Starts each worktree's dev server on its own ports, detached, with logs
- Optionally gives each worktree private copies of its services: Postgres,
  Redis, MariaDB, whatever your compose file declares — or, on a machine
  that would rather not run Docker, the same databases natively from a
  recipe you can override
- Or, experimentally, a database and a Redis slot of its own inside the
  servers your main checkout already runs: no server to start, nothing to
  wait for, and main's data untouched
- Shares any running worktree through a public tunnel URL
- Turns an open pull request into a running worktree: pick it from the
  list, press enter — a fork's too
- Shows all of it on one terminal screen, with a real log viewer

Built for people, and for agents, who work on several branches at once.

## The promise

pando never writes into your repository. Not a config file, not a gitignore
line, not a lockfile change. Everything it learns and everything it runs lives
under `~/.pando`:

```text
~/.pando/
├── config.toml                  your choices: theme, appearance
├── recipes/  themes/            your own native-service recipes and colour themes
└── projects/<project>-<id>/
    ├── pando.toml               the whole configuration for one project
    ├── worktrees/feat+checkout/ a stem: the branch's own checkout
    ├── logs/feat+checkout/      every process's log, for the log viewer
    ├── data/feat+checkout/      its private services' data, when isolated
    └── state.json               what runs where, on which ports
```

One mode writes somewhere that is yours all the same: a namespaced
worktree gets a database of its own in your own database server. That is
opt-in, named after your main database so it can never be it, limited by a
grant you give once to that prefix and nothing else, and dropped only by
`rm`, only when pando's own records say pando made it. See
[Shared, namespaced, isolated](#shared-namespaced-isolated).

## Your first run

```bash
cd your-project
pando
```

The first time, pando opens its setup screen instead of the list, on a
grove of its own: Pando, the aspen that is one tree with 47,000 stems,
drawn in dithered blocks, its leaves quaking. Each project gets its own
grove, grown from its name:

```text
        ██████████    ██████    ██      ██  ████████      ██████
        ██░▒░▒░▒██░ ██ ▒░▒░▒██  ████    ██░ ██░▒░▒░▒██  ██ ▒░▒░▒██
        ██████████▒ ██████████▒ ██▒░██  ██▒ ██▒     ██▒ ██▒     ██▒
        ██░▒░▒░▒░▒░ ██░▒░▒░▒██░ ██░  ▒████░ ██░     ██░ ██░     ██░
        ██▒         ██▒     ██▒ ██▒    ░██▒ ████████ ░▒  ░██████ ░▒
         ▒░          ▒░      ▒░  ▒░      ▒░  ▒░▒░▒░▒░      ▒░▒░▒░

            ░         ▓▒▓▒▒   ░░░        ▒▓▒
         ░▓███▒  ▒▒▒ ▒████▓░ ░▓█▓   ░    ███   ▓▒▒       ▒▒▓▒░  ▒█▒░
     ░▒░ ░▒▓█▓▓ ░▓██▓░████▓░ ▒███░▒▓█▓▒ ▒███▒ ▒███▒   ░  ▓███▓ ▒███▒
    ░███░   █   ░▓██▒  ░█░   ░▒█▓░▓███▒  ▒▓▒  ░███░ ░███▒▓████ ▓███▓
    ░▓█▓░   ▓    ░█░    ▓      █  ▓██▓▒   █    ░█▒  ▒███▓ ▒█▒░ ▒██▒
      ▓     ▓     █     █      █    █     ▓     █     █    ▓     █
      █     █     █     █      █    ▓     █     ▓     █    █     █
  ░░░░█░░░░░█░░░░░█░░░░░█░░░░░░█░░░░█░░░░░█░░░░░█░░░░░█░░░░█░░░░░█░░░░░░░░
      ┃     ┃     ┃     ┃      ┃    ┃     ┃     ┃     ┃    ┃     ┃
  ╺━━━┻━━━━━┻━━━━━┻━━━━━┻━━━━━━┻━━━━┻━━━━━┻━━━━━┻━━━━━┻━━━━┻━━━━━┻━━━━━━━╸
      main  feat/checkout      fix/login-loop   feat/search
```

The roots stay dark until the setup passes; then they light up. Every
project is a little different, so the surest start is to let your own
coding agent look at it. Press `a` to copy this one line, and paste it
into Claude Code or Codex, opened in the project:

```
Set up pando here: run `pando init --agent` and follow what it says.
```

`pando init --agent` prints the job for this project and this version of
pando: what pando already sees, the questions only the project can
answer, and the steps. The agent answers through `pando init --answers -`
on stdin, never a file in your repository, and proves the answers with
`pando check`: a throwaway worktree of the commit a new branch would fork
from, installed, started, its page asked for, and removed again, with no
branch and nothing left behind. When it passes, the agent says so, and
the setup screen turns green by itself: you're ready. The agent then
offers to remember how to run this project's worktrees with pando, in its
own memory and never in your repository, and saves it only if you say
yes, so a later session starts, stops and reads them through pando rather
than by hand. From then on,
`pando` opens the list.

No agent? Press enter on the setup screen and pando tries its own guess,
tested by the same `pando check`. Esc skips the setup altogether and goes
straight to the list; nothing is ever gated on it.

`pando new` and `pando start` work on a project that was never set up
too. Nothing is asked where pando can tell: it reads the repository —
the lockfile, the dev script, the env example, the version file — and
takes its own first choice for anything it has one for, printing each as
it goes with the file it wrote it to:

```
pando: process list: using "npm run dev" (package.json scripts.dev, which starts the
       workspace's apps itself; API_PORT and WEB_PORT in the env example), pando's first
       choice, over 1 other option — change it in ~/.pando/projects/<id>/pando.toml
```

That file is the whole configuration; edit any line, or delete one and
run `pando init`, which puts every open question to you instead of taking
a default. `pando doctor` says what was detected and from where, and
`pando check` tests the setup again at any time. A question pando has no
option for at all is still asked, and so is the one real choice
isolation brings — which services to run private copies of.

## Commands

```
pando                 open the TUI for the repo you are in
pando new <branch>    create a worktree and branch from the default base
pando start <name>    start its dev server; --isolated for private services,
                      --namespaced (experimental) for its own database in yours
pando stop [name]     stop one worktree, or the main checkout; --all for every one
pando restart <name>  stop and start again, keeping the ports
pando ls              list the main checkout and the worktrees: status, URL,
                      ports, git; -l for paths
pando rm <name>       stop everything, remove the worktree, wipe its data
pando share <name>    expose it at a public URL
pando unshare <name>  take the public URL down
pando open <name>     open its URL in the browser; --public for the shared one,
                      --app for its app on a simulator or device
pando logs <name>     tail its logs; --json for machines
pando status          what runs where, per process; --json
pando path <name>     print the worktree's path
pando init            answer every setup question now instead of as you go
pando check           test the setup in a throwaway worktree, then remove it
pando doctor          explain what was detected, why, and what is missing
pando signals         dump detection signals as JSON, for humans or agents
pando completions     print a completion script for bash, zsh, fish…
```

A worktree is named by its branch (`feat/login`) or by its directory
(`feat+login`). Inside a worktree, `start`, `stop`, `restart`, `logs`,
`open`, `share` and `unshare` need no name.

The main checkout runs too, first in every list, named the same way —
`pando start main` — or with no name from inside it (but for `stop`,
which there stops everything). It is yours and set up by you, so pando
runs only its processes, on ports it allocates, and nothing else: no
install, no hooks, and always on the project's own services.

On a terminal, `start` and `restart` wait until every process answers,
and when one does not they print the last lines of its log and why.
From a script they return once everything is spawned; `--wait` and
`--no-wait` choose either way.

### In the TUI

```
⏎        pick its mode — shared, namespaced (experimental), isolated —
         and start it, or switch it when it runs
s i S    start it: as last time, isolated, or on the shared services
l        open the log viewer
x X      stop it, or stop everything
r P      restart it, or only the selected process
o O      open its URL (or its app, where it serves no page), or the public one
c C y    copy its local URL, its public URL, its path
t        share it publicly, or stop sharing
! e      a shell in it, or open it in your editor
n d      new worktree, remove one (never the main checkout, ⌂)
p        open pull requests: ⏎ makes a worktree for one
a v      copy the setup prompt, or test the setup (pando check)
T        colour themes, previewed live
m ?      what pando said in full, and every key
```

<details>
<summary><b>More on the TUI</b>: the chooser, confirmations, the list, pull requests, the log viewer</summary>

<br>

Enter opens the mode chooser on every worktree. On a stopped one the
mode it last ran in is under the cursor and marked `last used`, so
enter, enter is still one quick start; one never started has shared
there. On a running one the mode it runs in is marked `running`, and
choosing another switches it — every process restarts on the other
services, and the chooser was the asking. The logs are `l`.

A key that would interrupt a running worktree asks for a second press:
`r r` restarts it, `x x` stops it, `P P` restarts one process, and `i`
or `S` twice restarts it in the mode it already runs in. Esc takes the
first press back. Moving a running worktree between modes with `i` or
`S`, sharing it, and removing it ask in a dialog instead. On a stopped
worktree nothing asks.

The list is a table with a header row: `branch`, then `changes`
(`uncommitted` when there are uncommitted changes), `port`, `public` (`◈`
while it is shared), `mode` (`isolated` when it runs private copies of
the services, `namespaced` when it runs on a database and a slot of its
own in the main checkout's servers, each in its own colour), `git`
(commits ahead of and behind the base branch), `PR` and `status`
(`failed`, or what is being done to it). A column shows only when some
row has something in it. The glyph before the branch says whether it
runs: `●` running, `◌` starting, `✗` failed, `○` stopped. The full URL
and the rest are in the detail pane beside it.

The main checkout is always the first row, and the worktrees with
something up come right after it: a start moves a row up and a stop puts
it back. `,` changes the order within each, which the list's title
names: by pull request, the highest number first (the default); newest
first; last run first; or by name.
The choice is saved as `[ui] sort` in `~/.pando/config.toml`, as `pr`,
`newest`, `run` or `name`.

`p` lists the repository's open pull requests through the GitHub CLI
(`gh`, signed in); typing narrows them by number, title, branch or
author. Enter checks out the pull request's branch in a new worktree, or
goes to the worktree it already has. A pull request from a fork has no
branch on `origin`, so pando fetches it from `pull/<number>/head` into a
branch of its own, `pr-<number>/<branch>`. One whose branch cannot be
found on `origin` is refused rather than started as an empty branch of
the same name.

The log viewer has a tab per log, led by an `all` tab that merges every
process's when there are several. `1`–`9` switch tabs, `/` searches, `f`
filters by level, and `e`/`E` jump between errors.

</details>

### Shared, namespaced, isolated

A worktree's code, processes, dependencies, ports and logs are its own
whatever it runs on. Its mode decides what happens to its data:

| Mode | Its data lives in | Start it with |
|---|---|---|
| **shared** | the main checkout's database and cache, data and all. The default, and the way back from the other two | `start --shared`, or `S` |
| **namespaced** *(experimental)* | the main checkout's servers, with a database and a Redis slot of the worktree's own in them: `shop__feat_x` beside `shop` in Postgres or MariaDB, slot 3 beside slot 0 | `start --namespaced`, or the chooser on enter |
| **isolated** | servers of its own on ports of its own: containers from your compose file, or native engines from a recipe | `start --isolated`, or `i` |

A namespaced database is built by the branch's own schema step, never
copied from main's, and both it and the slot are kept through a switch to
another mode until `rm` drops them.

<details>
<summary><b>How a namespaced start stays careful</b> in a server you own</summary>

<br>

- It logs in as your app does, with the user and password beside the
  address in the main checkout's `.env` — the root's, or the one in the
  directory a process runs in, such as `backend/.env`. Where there are none it asks
  once, keeps the answer in pando's own config for the project (mode
  0600), and hands it to the database client in its environment, never
  on a command line.
- That login has to be allowed to make databases named after the main
  one. The first start that is not allowed stops with nothing made and
  prints the statement to run once, as an administrator. On MariaDB and
  MySQL it covers those names and nothing else:

  ```sql
  GRANT ALL ON `shop\_\_%`.* TO 'app'@'localhost';
  ```

  Postgres cannot grant by name, so there it is `ALTER ROLE "app"
  CREATEDB;`, and the role can drop only the databases it owns, which
  are the ones it made. The official image's `POSTGRES_USER` needs
  neither.
- The database is made in main's character set or encoding and locale,
  so a schema written for main runs into it the same way.
- A Redis slot is given out only where the app reads a slot setting
  (`REDIS_DB` beside its address, or the path of its URL), and only while the slot is empty:
  keys pando did not put there are somebody else's. When all fifteen are
  held, the start asks which stopped worktree gives its slot up.
- `rm` drops only what pando's records say it made, on the server it made
  it on, and never the main checkout's database or slot whatever a record
  says. `doctor` lists a database named for a worktree that no record
  holds, with the command that drops it, and never drops it itself.

- When the app names its database or slot under a key pando does not
  guess — one `REDIS_DB` that three Redis roles share — name it in
  `pando.toml`; it is no secret, so the committed file may carry it:

  ```toml
  [namespaced.redis]
  db_env = ["REDIS_DB"]
  ```

Postgres, MariaDB and MySQL databases and Redis slots are what it makes
today, in a native server or in a container, Postgres images with an
extension built in (pgvector, PostGIS, TimescaleDB) included. Every other
service stays shared, and a namespaced start says so for each. It needs
only the engine's client on the host: `psql`, `mariadb` or `redis-cli`.
What a namespace is on an engine is a recipe's `[namespace]` table, so
another engine is a recipe rather than a release.

</details>

### Themes

`T` in the TUI lists the colour themes, each with a swatch of its
colours; moving through them repaints the screen, enter keeps one and
esc puts the old one back. The built-ins are pando's own, Catppuccin,
Flexoki, GitHub (default, dimmed, high contrast, colorblind), Gruvbox,
Kanagawa, Monokai Pro, One Dark, Rosé Pine, Tokyo Night, VS Code and
Zenwritten, each with a dark and a light half that follows the system.

<details>
<summary><b>Configuring and writing themes</b></summary>

<br>

A choice is saved in `~/.pando/config.toml`:

```toml
[ui]
theme = "catppuccin"
# appearance = "dark"            # or "light"; "auto" follows the system
# theme_from = "~/.config/theme-switcher/current"
# sort = "run"                   # the list's order: "pr", "newest", "run" or "name"
```

`theme_from` names a file whose first line is a theme's name, such as
the state file a terminal theme switcher keeps. pando follows it while
it runs, so switching the terminal's theme switches pando's with it.
`PANDO_THEME` and `PANDO_APPEARANCE` override both for one run.

A theme is a small TOML file: a background, a foreground and seven
accents for each half, and every other colour is derived from them. One
in `~/.pando/themes/<name>.toml` is listed beside the built-ins, and
replaces the built-in of the same name.

</details>

## For agents

pando publishes what it knows as JSON so a program can read it instead of
parsing English, and answers come back through one validated write path.

- [`agent/json.md`](agent/json.md) — every machine-readable shape,
  versioned, with the exit codes. `3` means pando has a question and the
  question is on stderr.
- [`agent/brief.md`](agent/brief.md) — the procedure for turning that
  evidence into answers: read before asking, write only through
  `pando init --answers`, never a byte in the repository.
- [`agent/`](agent/README.md) — a Claude Code plugin and Codex skills, both
  thin over that one brief.

The binary carries the brief and the shapes too: `pando init --agent`
prints the setup job for the project you are in, and
`pando init --agent --reference brief` or `--reference json` prints either
document whole.

## Install

pando runs on macOS (Apple silicon and Intel) and Linux (x86_64 and
arm64). Each release ships a ready-made binary for all four, so none of
these needs Rust.

**Homebrew**, on macOS or Linux:

```bash
brew install mertkaradayi/tap/pando        # later: brew upgrade pando
```

**The install script**, with nothing else installed:

```bash
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/mertkaradayi/pando/releases/latest/download/pando-cli-installer.sh | sh
```

It puts `pando` in `~/.local/bin` and adds that directory to your PATH in
your shell's startup files; to leave those alone, run the script with
`PANDO_CLI_NO_MODIFY_PATH=1`. Every download has a checksum and a GitHub
attestation, so `gh attestation verify <file> -R mertkaradayi/pando`
proves it was built here. Rerun the line to update.

**From source**, with Rust 1.88 or newer. With no Rust on the machine,
rustup installs the current stable toolchain, and a new terminal then has
`cargo`:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
cargo install --locked --git https://github.com/mertkaradayi/pando pando-cli
```

`--locked` builds with the exact dependency versions CI tested; without
it cargo may pick newer ones that need a newer Rust. On a new Mac, `git`
comes with the Xcode Command Line Tools (`xcode-select --install`).

`pando --version` says which version you have, and `pando completions zsh`
(or bash, fish…) prints a completion script.

### What else pando uses

pando itself is one binary. The tools below are needed only for the
feature beside them, and only when your project uses that feature.
`pando doctor` checks each one and prints the command for any that is
missing; pando never installs anything on your machine by itself.

| Tool | Needed for | macOS | Linux |
|---|---|---|---|
| `git` | everything | `xcode-select --install` | your distribution's `git` |
| `cloudflared` | `pando share`, a public URL for a worktree | `brew install cloudflared` | [Cloudflare's package](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/) |
| `gh` | the TUI's pull request picker (`p`) | `brew install gh` | [the GitHub CLI's package](https://github.com/cli/cli#installation) |
| Docker with Compose | `start --isolated`, when the project's services are in a compose file | Docker Desktop or OrbStack | Docker Engine and its compose plugin |
| `redis-server`, `redis-cli` | a private Redis without Docker; `redis-cli` alone for `--namespaced` | `brew install redis` | `redis-server`, `redis-tools` |
| `mariadbd`, `mariadb` | a private MariaDB without Docker; the `mariadb` client alone for `--namespaced` | `brew install mariadb` | `mariadb-server`, `mariadb-client` |
| `postgres`, `psql` | a private Postgres without Docker; the `psql` client alone for `--namespaced` | `brew install postgresql@16`, or `brew install libpq` for `psql` alone (keg-only: put `$(brew --prefix libpq)/bin` on PATH) | `postgresql`, `postgresql-client` |
| `mongod`, `mongosh` | a private MongoDB without Docker | `brew install mongodb/brew/mongodb-community` | [MongoDB's package](https://www.mongodb.com/docs/manual/administration/install-on-linux/) |

The usual pair on a Mac, for sharing and pull requests:

```bash
brew install cloudflared gh
```

Your project's own toolchain (Node and its package manager, Python,
Go…) is whatever you already run it with. pando finds it through your
login shell and never replaces it; `pando doctor` names any it cannot
find, with how to get it.

## Contributing

pando is small enough to read and strict about how it grows, which makes
it a good project to contribute to. Most of what it knows about the
ecosystem is data, so many useful changes are a single row or a single
TOML file:

| You know… | Your contribution is | Where |
|---|---|---|
| a database or cache pando should run natively | a service recipe | `src/recipes/builtin/*.toml` |
| a framework pando misreads | a detection rule | `src/catalog/frameworks.rs` |
| a package manager or lockfile | a row | `src/catalog/package_managers.rs` |
| a language or version manager | a row | `src/runtime/languages.rs` |
| a colour scheme you love | a theme | `src/theme/builtin/*.toml` |
| a project shape pando breaks on | a fixture and a failing test | `tests/common/mod.rs` |
| a clearer way to explain it | a change to the website: plain HTML, no build step | `site/` |

Try pando without pointing it at anything real:

```bash
scripts/fixture-repo.sh --list                  # the fixture shapes the tests use
scripts/fixture-repo.sh mono-web-api --listener # build one, and print how to run pando in it
```

Every change passes `cargo test`, `cargo clippy --all-targets -- -D warnings`
and `cargo fmt --check`. [CONTRIBUTING.md](CONTRIBUTING.md) has the map of
the code, the testing rules, and how a pull request goes. Bugs and ideas go
in [issues](https://github.com/mertkaradayi/pando/issues); security problems
go through [SECURITY.md](SECURITY.md), never a public issue. Everyone
taking part follows the [code of conduct](CODE_OF_CONDUCT.md).

## Status

Version 0.8.0, released as binaries for macOS and Linux. Every command above is
implemented and covered by tests, in this order: worktrees and their
lifecycle; detached dev servers with their own ports, logs and readiness;
several processes per worktree; the log viewer; private per-worktree
services from the project's own compose file; public tunnel URLs; `init`,
`doctor` and `signals`; native service recipes for machines without
Docker; the JSON contract an agent reads; a worktree from any open
pull request in the TUI; and, experimentally, namespaced worktrees — a
database and a Redis slot of their own in the main checkout's servers,
tested against throwaway MariaDB and Redis servers the tests start
themselves. 0.4.0 adds no command: it is all of that after a review of
the whole project and the fixes it found, with a test suite that reads
no developer's shell profile and needs none of their tools. 0.5.0 adds
the guided first run: the first `pando` in a project with nothing to run
hands its setup to the developer's own coding agent, `pando init --agent`
prints that job, and `pando check` proves the result in a throwaway
worktree it removes again. 0.5.1 is the first with binaries: Homebrew
or one install script, no Rust needed. 0.6.0 reads a repository whose
apps sit in directories of their own below a root with no manifest,
recognises Expo apps, and lets an agent describe several processes, so
the first run on a polyglot monorepo ends with every app running.
0.6.1 fixes what the first real Expo project found: Metro keeps its
reloads, is never taken for a web page, and opens in the app's own
development build. 0.6.2 opens that app from `pando open` and the TUI's
`o`, on the simulator or an Android device, gives the build commands
Expo accepts, and says when the build installed on the simulator is for
another SDK. 0.6.3 lets `b` sort the TUI's list and keeps what runs at
its top. 0.7.0 adds the TUI's git menu, `space g`: fetch, pull, rebase
or merge a row, the commands shown first and a conflict aborted; space
is the list's leader, so sorting moved to `,`. 0.8.0 checks a new worktree
out by copy-on-write where the disk can clone, so it costs only the files
its branch changed, and `clone` does the same for dependencies before
the install. What changed in each version is in the [changelog](CHANGELOG.md).

macOS is what it is developed on. CI runs the whole test suite on macOS
and on Linux for every change, and both pass — but the suite runs on
fixtures, and nobody has yet used pando on Linux for real work.

What that does not mean: there is no crate on crates.io yet, and almost
every worktree pando has created has been inside a generated fixture
repository. It has been pointed at exactly one
real project, which found three bugs in an afternoon — a Makefile target
read down to its first line, a failure that left an empty log and no
explanation, and a backgrounded server reported as dead. All three are
fixed, and the count is the point: a tool this heavily tested against
situations it invented still breaks on first contact with one it did
not. If pando breaks on yours, that is the most useful issue you can open.

Next, roughly in order: a published crate, a JSON schema for
`pando.toml`, and a recipe directory with its own contribution guide.

## Star history

<a href="https://star-history.com/#mertkaradayi/pando&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=mertkaradayi/pando&type=Date&theme=dark">
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=mertkaradayi/pando&type=Date">
    <img alt="Star history of mertkaradayi/pando over time" src="https://api.star-history.com/svg?repos=mertkaradayi/pando&type=Date">
  </picture>
</a>

## License

pando is free software under the [GNU Affero General Public License v3.0
only](LICENSE) (`AGPL-3.0-only`). You may use, study, change and share it.
If you distribute pando, or a program built from it, or let people use a
modified version over a network, you must offer them its complete source
under the same licence. Contributions are accepted under the same terms.

<p align="center"><sub>One root system. Forty-seven thousand stems. Every branch alive.</sub></p>
