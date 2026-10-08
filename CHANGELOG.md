# Changelog

Every version of pando, newest first. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and pando uses
[semantic versioning](https://semver.org). Before 1.0, a minor version
may change behaviour.

## Unreleased

### Fixed

- The git menu no longer loses an ignored file such as `.env`. A pull,
  rebase or merge onto a base that has started tracking that path used
  to replace it, and the abort after a conflict deleted it. Such a move
  is now refused before anything runs, naming the files.
- The git menu moves a branch onto the base it was made from. That is
  the branch `new --base` forked it from, or, for a pull request, the
  branch the pull request targets. It used to rebase onto the
  configured base: a branch cut from `release` and rebased onto `main`
  took every commit of `release` with it.
- The git menu refuses a move when `git status` does not answer, rather
  than reading the tree as clean. Its fetches get five minutes, not
  thirty seconds.
- A fork's pull request is fetched from the remote of the repository it
  was opened against, such as `upstream`. Fetching from `origin`, a
  contributor's own fork, could check out a different pull request with
  the same number.
- `pando rm` drops nothing in namespaced mode while it cannot read what
  the main checkout's env files name today. Before, an unresolved value,
  a renamed service or a moved port meant it decided on the record alone.
- Namespaced mode finds a database container whose port on the host
  differs from the port inside it (`5433:5432`). Docker's `--filter
  publish=` matches the inside port, so pando used to find nothing.
- The share proxy logs in only requests for the tunnel's own host. A web
  page or another process reaching its loopback port directly is
  answered `421` and never gets the session cookie.
- A shared event stream or long poll that is quiet for more than a
  minute is no longer cut off. The proxy's upstream deadline now ends at
  the first byte of the answer. The header deadline now covers the whole
  header block, not each read.
- A Ctrl-C during `pando check`'s install or start records the check as
  interrupted, not as failing settings.
- A Ctrl-C while `new` clones files copy-on-write now stops it before
  git writes the rest of the checkout and before the `post-checkout`
  hook runs.
- A port held by a listener on `[::]` with `IPV6_V6ONLY` is no longer
  handed out as free. uvicorn's `--host ::` and nginx's `listen [::]`
  listen like that.
- A process whose whole group had already exited when it was seen to
  fail is never signalled later. That pid may by then belong to
  something else.
- The state file is synced to disk before it replaces the old one, so a
  crash right after a save cannot leave it empty.
- `pando update` downloads the install script into a new file only you
  can write, not to a predictable path in the shared temporary
  directory. Pre-release tags are ordered as semver orders them
  (`rc.10` after `rc.9`).
- The TUI saves the list's order and the theme when `ui` in the config
  is an inline or dotted table. It used to report success and write
  nothing.
- A workspace member excluded with a negated glob, such as
  `!apps/legacy`, is no longer proposed as an app.

## 0.10.0 — 2026-10-08

### Added

- pando runs on Windows inside WSL 2, as the Linux binary, and knows
  when it is there:
  - `pando open` and the TUI's `o` open Windows' browser: `wslview` when
    wslu is installed, otherwise Windows' own URL handler, found at
    `/mnt/c` too when Windows' PATH is kept out of WSL. They used to
    say they could not run `xdg-open`, which Ubuntu on WSL does not ship.
  - `pando doctor` treats a tool found on a Windows drive as seriously as
    a missing one. WSL puts Windows' PATH after Linux's, so with no Linux
    npm the shell finds the script Node's Windows installer leaves, and
    with no Linux docker the one Docker Desktop leaves. The fix says to
    install the tool inside WSL, or to turn on Docker Desktop's WSL
    integration.
  - It notes a repository, or a worktrees directory, on a Windows drive
    (`/mnt/c`). git is several times slower there, and an edit sends no
    file event, so dev servers do not reload.
  - When the Docker daemon does not answer, the fix names Docker
    Desktop's WSL integration, and the distro's own docker service for
    Docker installed inside it.
  - The TUI's copy keys fall back to `clip.exe` for a terminal that
    ignores OSC 52, for ASCII text, which `clip.exe` cannot mangle.

### Changed

- Everything pando asks of the operating system is behind one layer,
  `src/platform`: process groups, signals, locks and permission bits, the
  shell a command runs in, the boot, copy-on-write, and what the desktop
  opens and copies with. Nothing else names an OS, and a test holds that.
  On macOS and Linux one thing changes: `pando update`'s install script,
  and `share`'s check that `cloudflared` is on PATH, run in `/bin/sh`
  rather than the first `sh` on PATH, as `open`'s commands already did.
- A native Windows build compiles, and every command but `completions`
  stops at once with one line that says to run pando inside WSL 2: its
  Windows backends are not built yet. CI builds it on Windows so they
  keep compiling.

### Fixed

- A WSL 2 distro, or a container, that restarted under a kernel that
  kept running left pando trusting the pids it had recorded before. The
  kernel's boot id stays the same through such a restart, so `status`
  could call a dead dev server running, and `stop` could signal another
  process's group. On Linux the boot pando records now includes when
  init started, which changes with every such restart. Worktrees that
  are running when pando is upgraded are kept.
- The test suite passes with a git older than 2.41, such as Ubuntu
  22.04's, and on a WSL machine whose Windows side has npm or Docker
  Desktop on PATH.

## 0.9.0 — 2026-10-05

### Added

- Namespaced mode for engines whose namespaces are the app's own: an
  Elasticsearch or OpenSearch index, a Kafka or Redpanda topic, a
  Meilisearch or Typesense index, a Memcached or Redis key. Where the app reads a prefix beside the
  address (`ELASTICSEARCH_INDEX_PREFIX`, `REDIS_PREFIX`), a namespaced
  worktree is told one of its own — main's, then the worktree's name and
  a short hash of it and the project, `feat_x_1f3c4a__` — and the
  service is its data. A prefix is told, never made: nothing is recorded
  or dropped, and `rm` says what it leaves. Every process of a
  namespaced worktree is told `PANDO_NAMESPACE`, the same name.
- A database in a container is set up with nobody asked: where the
  app's login may not make the worktree's database, pando runs the
  grant it used to print as the container's own administrator, read
  from its environment (`POSTGRES_USER`/`POSTGRES_PASSWORD`,
  `MARIADB_ROOT_PASSWORD`, `MYSQL_ROOT_PASSWORD`), and where the app's
  env files carry no login, that administrator is the login, the one
  `rm` then drops as. Only a container that alone publishes the port on
  this machine's loopback, with nothing else listening there, is taken
  for the server, and an administrator is used only once it answers. A recipe says where with
  `[namespace.container_admin]` and `run_sql`.
- The setup brief's first run answers `namespaced` when `init` says a
  service would stay on main's data.
- A server in a container is reached through the client its image
  ships: when the host has no `psql`, `mariadb` or `redis-cli` and a
  container publishes the server's port, the recipe's commands run in it
  through `docker exec`, the password passed by name.
- `namespaced`, the twelfth answer `init --answers` takes: per service,
  the recipe its engine is, the keys its app names its database or slot
  by, and the keys it reads a prefix from, written to
  `[namespaced.<service>]` in pando's own file beside any login. Only a
  program answers it; nothing asks a person. `signals` lists what a
  namespaced start would do with every service, an undeclared compose
  service included, and why one stays shared.
- `[namespaced.<service>] recipe` and `prefix_env`, beside `db_env`; none
  is a secret, so a committed `pando.toml` may carry them.
- A recipe may say only how a server somebody else runs is namespaced: a
  `[namespace]` or `[prefix]` with no `[service]`, for an engine only a
  compose file runs.

### Changed

- Where an app names its database or slot is a recipe's
  `[namespace.address]` — a URL's path and query parameters, and the
  keys beside the address — rather than code; a recipe without one means
  its kind's conventions, as before. Every address key's own slot key is
  now found: two Redis roles with their own `_DB` keys both move to the
  worktree's slot, where the second went on naming main's.
- A slot's `size` prints a bare number; Redis's recipe passes `--raw`.

## 0.8.3 — 2026-10-05

### Added

- `clone` is the eleventh question: detection proposes the gitignored
  `node_modules` the main checkout has, the root's and each app's, so a
  new worktree clones them copy-on-write before its install instead of
  installing from nothing. Decided, so `new` takes it in a project set up
  before it and says so; not offered when the install deletes the tree
  first (`npm ci`). `--answers` takes it as a list, and `null` writes
  `clone = []`. The setup brief and `agent/json.md` describe it, and
  `signals` publishes `dependency_dirs`.

### Changed

- A setup run by the developer's agent asks them nothing, start to end.
  The brief has the agent decide every question pando's rules leave
  open — the apps to run (all of them), the runtime line `doctor`
  lists, a worktree's `.env` seeded from the example when there is no
  other, the main checkout's branch as the base when a check fails on
  it — and end with a report of what it set, why, and how to change it.
  Saving the run instructions in the agent's memory is offered in that
  report and done only when the developer says to.

## 0.8.2 — 2026-10-05

### Added

- `pando update`: updates pando to the latest release the way it was
  installed — `brew upgrade` for Homebrew's, `cargo install` at the
  release's tag for cargo's, and the install script, into the directory
  the binary is in and with no change to shell files, for any other. A
  build made in a checkout is updated there, so pando says how and runs
  nothing. `--check` says whether a newer release is out and what would
  install it.

## 0.8.1 — 2026-10-05

### Added

- Namespaced mode for Postgres ([#9]): `start --namespaced` gives a
  worktree a database of its own in the main checkout's Postgres,
  `shop__feat_x` beside `shop`, made in main's encoding and locale, built
  by the branch's schema step and dropped by `rm`. A login that may not
  make databases stops the start with nothing made and the `ALTER ROLE …
  CREATEDB` to run once. Compose images that are Postgres or Redis under
  another name — pgvector, PostGIS, TimescaleDB, Redis Stack — get
  namespaces as the engine they are.
- `[namespaced.<service>] db_env`: the keys that name the app's database
  or slot when pando cannot find one beside the address — one `REDIS_DB`
  shared by several Redis roles. No secret, so a committed `pando.toml`
  may carry it; a login stays in pando's own file.

### Fixed

- Namespaced mode reads the env files where the processes run, not only
  the root's: a project whose api keeps its database in `backend/.env`
  was left shared with "no port". A `_SERVER` key beside the address is
  its host, as FastAPI's template names it.
- A missing database client is named with how to install the client
  alone, since a server in Docker leaves the host with none.

## 0.8.0 — 2026-10-05

### Added

- `new` checks a worktree out by copy-on-write where the filesystem can
  clone (APFS on macOS; btrfs, XFS and bcachefs on Linux): the main
  checkout's files are cloned in, sharing their blocks, and git writes
  only the files that differ from the branch. The files, modes and git
  state are the ones git's own checkout makes, and git's `reset --hard`
  makes them; a file whose checkout converts it (CRLF endings, a filter,
  `ident`) is always written by git, and a `post-checkout` hook runs as
  it does after `git worktree add`. On a measured web monorepo that is
  about 285 MB of tracked files a worktree no longer stores. A
  filesystem that cannot clone, `core.autocrlf`, and a sparse main
  checkout keep git's own checkout; `[project] copy_on_write = false`
  turns it off. `du` still counts each worktree whole: the saving shows
  as free space. A `new` stopped by Ctrl-C or a `kill` during the
  checkout removes what it made, as `git worktree add` does. A pando
  older than this one refuses `copy_on_write` and `clone` in pando.toml
  as unknown keys, so going back means removing them.
- `[project] clone` lists gitignored paths `new` clones from the main
  checkout before the install, such as `node_modules`, so the install
  only fixes what differs: on the same monorepo `npm install` then
  rewrote 6 of 19,701 files and took about a second. Never a full copy.
  Each path must be gitignored, like `provision`'s. Not cloned, each
  with a line: a path the install deletes first (`npm ci` and
  `node_modules`), a Python virtualenv, and a tree with a link out of
  the worktree. `pando check` never clones, so a check still proves the
  install works from nothing.

### Fixed

- A `new` whose `git worktree add` failed left the branch it made, and,
  when only the `post-checkout` hook failed, the worktree too, with no
  record of either. Both are removed now, as for any other refused
  `new`.

## 0.7.0 — 2026-10-04

### Added

- A development build names its branch: `pando --version` adds
  `(my-feature@1a2b3c4)` after the version, and the TUI's header shows `⎇
  my-feature@1a2b3c4`, so a branch tried on a real project never passes
  for the released pando. `scripts/dev` sets it (`PANDO_BUILD_LABEL`); a
  release reads as before.
- `scripts/dev`, for contributors: each branch in a worktree of its own
  with its own build, `use` to make it the `pando` on your PATH and back,
  `try` for its TUI in a throwaway fixture. See CONTRIBUTING.md.
- The TUI's git menu, `space g` ([#7]): space is the list's leader, as
  in neovim, and the footer lists what may follow it. On any row, where its branch stands
  against its base and its own upstream, and what can be done about it
  — fetch, pull (a fast-forward, never a merge), rebase onto the base,
  merge the base in, or abort a rebase or merge left half-done — each
  saying what it would do there, or why it will not. Picking one shows
  the exact git commands first; enter runs them, and the row reads
  `rebasing` while it does. A rebase or merge that stops on a conflict
  is aborted, so nothing changes, and the menu names the files and
  offers `!` to do it by hand. Nothing moves a checkout with uncommitted
  changes, the main checkout is only ever fast-forwarded, and nothing
  is ever pushed. When a running worktree's branch moved, `r` in the
  menu restarts it. A rebase or merge stopped in a shell shows on its
  row and in the detail pane.
- `scripts/fixture-repo.sh <kind> --with-origin --drift`: a fixture
  whose origin has moved on and whose worktrees rebase cleanly, conflict,
  or have uncommitted work, for trying the git menu by hand.

### Changed

- Sorting the TUI's list moved from `b` to `,`, yazi's sort key: space
  is now the list's leader, as in neovim, and `g` and `G` keep their vim
  meaning.

## 0.6.3 — 2026-10-04

### Added

- The TUI's list can be sorted: `b` cycles it by pull request, the
  highest number first (the default); newest first; last run first; and
  by name. The list's title names the order, the main checkout stays the
  first row, and the cursor stays on its worktree. The choice is saved
  as `[ui] sort` in `~/.pando/config.toml`. A worktree's last start is
  kept in its state record through a stop, so last run first still
  knows after a stop when a worktree last ran.
- The TUI's list puts the worktrees with something up right after the
  main checkout, in every order, so what runs is never scrolled out of
  sight. A start moves the row up and a stop puts it back, the cursor
  going with it; below them, the rest keep the order `b` chose.

### Fixed

- The TUI reads the pull requests the last fetch cached, so a relaunch
  shows their chips on its first frame rather than once `gh` answers.
  The cache was written for this and never read.

## 0.6.2 — 2026-09-30

### Added

- `pando doctor` says how to get each tool it did not find: a line
  under it, `install it with: brew install cloudflared … — pando never
  will`, and the same command in the finding's fix. `doctor --json`
  carries it as `tools[].install`. It covers git, Docker, cloudflared,
  gh and every package manager pando knows; a native engine already had
  its recipe's. pando still installs nothing.
- `pando doctor` looks for `gh`, which the TUI's pull request picker
  needs. The README said it did; it never had.
- The README's Get started is five steps that work as written, and
  Install lists every tool pando can use, what needs it, and how to get
  it on macOS and on Linux.
- `pando open` and the TUI's `o` open an Expo app rather than printing
  how ([#5]): on the booted iOS simulator, else on a connected Android
  device or emulator (`adb reverse`, then the link), else, on a Mac, in
  a simulator pando starts (Simulator.app, or DeviceHub.app from Xcode
  27) and waits for. What runs is the command `status` prints. `pando
  open --app` opens the app of a worktree that also serves a page, a
  backend beside a mobile app. A link whose scheme is not known is never
  run.
- `pando status` gives the Android command beside the simulator's
  (`app.android` in `--json`), and says when the development build on
  the booted simulator was made for another Expo SDK than the
  worktree's, with the build that replaces it (`app.installed`) ([#5]).
  `open` gives that build instead of opening an app that would crash.
  The simulator is read from its disk; nothing is booted or run.
- The development build's scheme comes from a `slug: "…"` literal in
  `app.config.*` when there is no `app.json`, and from the app's build
  on a booted simulator when the config computes it ([#5]). pando never
  runs the config.
- Everything `pando check` runs, its processes, hooks and install, gets
  `PANDO_CHECK=1`, so a process can tell a check from a real start
  ([#5]).
- `pando start` gives a worktree pando made a `provision` file it lacks,
  never over one it has ([#5]). A worktree pando did not make is never
  written to: `start` and `doctor` name the file it lacks and the `cp`
  or `ln -s` that supplies it, one command for all the worktrees that
  lack the same file.
- `pando doctor` names a process with no `ports` that runs a server
  taking its port from a variable or a flag, such as Metro, which then
  binds its default port in every worktree ([#5]). Its fix is the
  command or the line that gives it one.

### Changed

- A process that is still running at its readiness deadline, with
  nothing in its log to explain it and no other port open, is told how
  to wait longer: its failure ends with the `ready = { timeout_s = N }`
  to put in its table, at twice the wait it had. A cold build, a JVM,
  a server that waits for its database or a slow name lookup is only
  slow, and the timeout never said what to change.

### Fixed

- The build `pando status` gives for a branch that changes native code
  is `npx expo run:ios --port <metro port>`, and
  `npx expo run:android --port <metro port>` beside it ([#5]). Expo
  refused the old one: `--port` and `--no-bundler` do not go together.
  With `--port` alone it reuses the worktree's running Metro.
  `status --json` keeps `native.build` and adds `native.builds`.
- `pando doctor`'s note on a value pando detected and would not detect
  now gives a command that fixes it, `echo '{"port_env":"RCT_METRO_PORT"}'
  | pando init --answers - --replace` say, and says "delete that line"
  only where that asks the question again ([#5]). Deleting `dev.ports`
  beside a `dev.cmd` asked nothing, and Metro ran on 8081 everywhere.

## 0.6.1 — 2026-09-29

### Added

- A process can say `page = false`: no browser opens its port, so it is
  never the worktree's URL ([#5]). Expo's Metro is one without saying
  it; `page = true` says otherwise, for `expo start --web`. A worktree
  with no page has no URL in `status`, and `pando open` and the TUI's
  `o` give the command that opens its app instead of a browser at
  Metro's root. `share` still publishes its port. A lone
  `RCT_METRO_PORT` answer now names its role `metro`, not `web`.
- `pando status` says when a worktree's branch changes an Expo app's
  native code against its base ([#5]): files under `ios/` or `android/`
  anywhere in the app, or `app.json`/`app.config.*`. It names the
  files and gives `npx expo run:ios --no-bundler --port <metro port>`,
  which builds that worktree its own development build. `status --json`
  carries it as the app's `native`.

### Changed

- The Expo rule no longer proposes `CI=1` ([#5]). Under it Metro turns
  off its reloads and file watching, and with no terminal Expo waits on
  no keypress anyway. `doctor` notes a process that still sets it.

### Fixed

- `provision` never offers a packaged build or a log ([#5]): `.apk`,
  `.aab`, `.ipa`, `.app`, `.xcarchive`, `.dSYM`, `.dmg`, `.msi`,
  `.AppImage` and `.log`, whatever the file is called. A local
  `eas build` leaves its packages in the checkout.
- An `--answers` value for a slot a rule decided wins over the rule's
  choice ([#5]). `pando init --answers -` used to keep the guessed
  `provision` and report the answer as unused.
- The development build link uses the scheme `expo-dev-client`
  registers, `exp+` and `expo.slug` lowercased, and not `expo.scheme`
  ([#5]). pando fills it in from the worktree's `app.json`. When the app
  depends on `expo-dev-client`, `status` and the TUI give the command
  that opens its development build instead of Expo Go, and
  `status --json` says which in the app's new `client` field. The
  simulator command quotes its URL.

## 0.6.0 — 2026-09-29

### Added

- **Apps below a root with no manifest** ([#4], [#5]). A repository
  whose root has no manifest, lockfile or marker is read one level
  down, and also under `apps/*` and `packages/*`. pando proposes each
  app's frozen install in its own directory, e.g.
  `(cd backend && uv sync --frozen) && (cd frontend && npm ci)`, and a
  `[processes.<app>]` with a `cwd` for every app with a dev script.
  It also proposes the apps' gitignored env files for `provision`,
  their version files, and their env examples. `pando signals` lists
  the directories as `app_dirs`, and a compose file one directory
  down, such as `docker/compose.yml`.
- **Expo** is a framework rule ([#5]). An app whose `start` script
  runs `expo start` is proposed with `RCT_METRO_PORT` (Expo ignores
  `PORT`), `CI=1` so it never waits on a key, and 90 seconds to get
  ready. A workspace app with only a `start` or `serve` script that
  runs a framework's server is an app too. An app's own
  `.env.example`, such as `EXPO_PUBLIC_API_URL=http://127.0.0.1:3000`,
  gives it `{port:<role>}` env for the app it points at.
- `--answers` takes `processes` as an object of process tables (`cmd`,
  `cwd`, `ports`, `env`, `ready`), checked as a detected option is, so
  an agent can describe a multi-process app ([#4]).
- A `base` question, asked only where origin/HEAD is at least 100
  commits and 30 days behind the main checkout's branch, which it
  offers first. `pando check --base <branch>` tests another base for
  one run, and `doctor` notes a far-behind origin/HEAD ([#4]).
- A check that fails because the tested commit lacks a file the main
  checkout's branch has, such as a lockfile, fails with `kind: "base"`
  and names both refs, instead of asking for other settings ([#4]).
  A `check --base` of another branch is a probe: it never replaces the
  last result at the project's own base, or its logs. With the `base` question still
  open, `pando check` asks it instead of testing origin/HEAD.
- `pando status` gives the command that opens an Expo app on the iOS
  simulator (`xcrun simctl openurl booted exp://127.0.0.1:<port>`), and
  `status --json` carries it as `app`, with the development build's
  link beside it ([#5]).
- When `bash -lc` resolves the wrong runtime version, the prelude
  question also offers a line that puts a matching binary first on PATH,
  from where your own shell finds it or a well-known place such as
  Homebrew's: one line per binary, the narrowest directory first, and
  never a directory named for a version the pin rejects. Such a line is
  offered and never taken for you, because it changes every project on
  the machine. `doctor` names the file a prelude goes in and the ways to
  set it.
- `doctor` notes a queue worker (ARQ, Celery, RQ, Sidekiq, BullMQ)
  whose Redis every worktree shares: a job queued in one worktree can
  run on another's code ([#4]).
- The agent brief documents the `[processes]` table, `{port:<role>}`
  in `env`, and that a phone needs the machine's LAN address ([#4],
  [#5]). For a project with an Expo process, the setup job and the
  memory block also tell the developer about the LAN address.

### Changed

- A project that would run nothing is an open question: `init --agent`
  lists the dev command, and `init --yes` exits 3 instead of writing a
  config that starts nothing ([#4]).
- An app directory that nothing starts, such as a Python API with no
  dev script beside a frontend with one, keeps `processes` open:
  `init --yes` exits 3 and names the directory, so a first run cannot
  pass while leaving the app out ([#4]).
- An app's default port includes the port its own env files state, so
  a sibling's URL to it (`EXPO_PUBLIC_API_BASE_URL=http://127.0.0.1:8787`)
  is given the worktree's port ([#5]).
- A shared service's port is also read from the env files beside the
  apps (`backend/.env`), and the job and the check name the file it
  came from ([#4]).
- An answer for a question that already has one is refused with exit
  2 and a pointer to `--replace`. Before, it was dropped and the run
  exited 0. An answers run that stops on the next question says what it
  wrote first.
- A failed install in a check is said once, with its exit status.
- `doctor` notes a compose service named like a process role only for a
  compose file pando takes services from, and its fix renames the role,
  never the committed compose file.
- The Claude Code and Codex setup skills, and the website, follow the
  first run as it now is.
- A typed `port_env` answer such as `PORT, API_PORT` is split into its
  variables, and a name that is not an environment variable is
  refused ([#4]).
- Split `*_HOST`/`*_PORT` pairs in an env example, such as
  `POSTGRES_SERVER` and `POSTGRES_PORT`, are read as a service's
  address, like a URL ([#4]).
- A version file in an app directory (`backend/.nvmrc`) counts:
  `doctor` and `start` check it where that app's processes run, and
  `init` raises the runtime prelude for it rather than leaving it to the
  first `check` ([#5]).
- `doctor` no longer says no version manager is installed while listing
  nvm: a manager that lacks the pinned version is named with its install
  command.
- A program's answer that config would refuse exits 2, as the other
  refusals do.
- The setup screen says "your agent is probably on it" only after a
  settings failure. A machine or base failure is the developer's to
  fix, and the screen says so ([#4]).

### Fixed

- `provision` no longer offers tool caches and artifacts such as
  `.coverage`, `.pytest_cache` or `.venv` ([#4]).

[#4]: https://github.com/mertkaradayi/pando/issues/4
[#5]: https://github.com/mertkaradayi/pando/issues/5
[#7]: https://github.com/mertkaradayi/pando/issues/7
[#9]: https://github.com/mertkaradayi/pando/issues/9

## 0.5.1 — 2026-09-28

### Added

- Binaries for macOS (Apple silicon and Intel) and Linux (x86_64 and
  arm64, static) with every release, so installing pando needs no Rust:
  `brew install mertkaradayi/tap/pando`, or one install script that puts
  it in `~/.local/bin`. Every download has a checksum and a GitHub
  attestation, and every release is installed and run on all four
  before it counts as done.

### Changed

- pando states the oldest Rust it builds with, 1.88, in `Cargo.toml`
  (`rust-version`), so an older toolchain gets a clear message instead
  of a build error, and CI builds with exactly that version.
- The README and the website say how to get Rust (rustup) when there is
  none, and install with `cargo install --locked`, the dependency
  versions CI tested. The website no longer calls what shipped in 0.5.0
  unreleased.

## 0.5.0 — 2026-09-28

### Added

- **The guided first run.** The first `pando` in a project with nothing
  to run opens a setup screen, on a dithered grove of its own, whose
  one-line prompt hands the job to the developer's own coding agent.
  `esc` always skips it, and a project configured before it is never
  sent there.
- `pando init --agent` prints the setup job for the project you are in;
  `pando init --answers - --replace` corrects an answer.
- `pando check` proves the setup in a throwaway detached worktree,
  installs and starts it, asks for its page, and removes it again. In
  namespaced mode it proves the schema step in namespaces of its own.
- The setup screen turns green by itself when a check passes, and the
  first-time tip and a passed check draw the grove on the CLI too.
- The agent offers to remember, in its own memory and never in the
  repository, how to run the project's worktrees with pando, and saves
  it only if the developer says yes.
- The main checkout runs like a worktree, and is listed first.
- The TUI's list shows each worktree's pull request and its state first,
  marks its mode and git state, and lines its columns up.
- The licence (AGPL-3.0-only), contribution guide, code of conduct,
  security policy, and CI on macOS and Linux.

### Fixed

- A failed hook says why, and how to fix it.
- A worktree's namespaced database is made in the main one's shape.
- `start` says a process is ready as soon as it is, and keeps a
  worktree's window; `stop --all` names a running check.

## 0.4.0 — 2026-09-26

### Added

- **Namespaced mode**, experimental: `start --namespaced`, or the TUI's
  mode chooser on enter. A worktree keeps the main checkout's servers
  and gets a MariaDB/MySQL database and a Redis slot of its own in them.
  Every drop and flush goes through one guard, and `rm` drops only what
  pando's records say it made. `doctor` lists databases no record holds.
- A recipe's `[namespace]` table says how an engine makes, finds and
  drops a namespace.
- A worktree's mode is shared, namespaced or isolated, and enter picks it.
- Colour themes as data, a live picker on `T`, and following a terminal
  theme switcher's state file.
- The worktree list is a table with a header row, glyphs for state and
  one colour per meaning.
- Keys that would interrupt a running worktree ask for a second press.
- `new`, `start` and the TUI take pando's first choice instead of asking,
  and print it with the file it went to.
- A workspace whose root dev script starts its own apps runs as that one
  script; a project that gitignores its lockfile gets the plain install.

### Fixed

- About 230 fixes from an audit of the whole project, by module and by
  concern: data safety in isolated and namespaced mode, the lifecycle,
  detection, share, the CLI, the TUI and doctor.
- The test suite is hermetic: it reads no developer's shell profile and
  needs none of their tools.

## 0.3.0 — 2026-09-25

### Added

- `p` in the TUI lists the repository's open pull requests, and enter
  makes a worktree for one. A fork's is fetched from `pull/<n>/head` into
  `pr-<n>/<branch>`. Restarting only the selected process moved to `P`.
- The TUI shows which GitHub account `gh` uses for the project.
- Workspace apps are told each other's port variables, and apps with no
  env file get the root `.env`.
- The project's own schema script is offered as the schema hook.
- `new` names the submodules a new worktree leaves empty.
- `completions` prints a completion script for bash, zsh, fish and more,
  after a UX and correctness pass over the TUI, the CLI and the lifecycle.

### Fixed

- A pull request worktree checks out the pull request, or refuses.
- A readiness timeout names the ports the process opened instead.

## 0.2.0 — 2026-09-23

The first tagged version: everything pando does, built in seven phases
from 2026-09-20, then restructured for maintainability with no change in
behaviour.

### Added

- Worktrees and their lifecycle: `new`, `ls`, `rm`, `path`.
- Detached dev servers with their own ports, logs and readiness:
  `start`, `stop`, `restart`, `status`, `logs`, `open`; several processes
  per worktree.
- The TUI and its log viewer.
- Private per-worktree services from the project's own compose file.
- Public tunnel URLs: `share`, `unshare`.
- `init`, `doctor` and `signals`.
- Native service recipes (Postgres, MariaDB, Redis, MongoDB) for
  machines without Docker.
- The JSON contract an agent reads, the setup brief, and a Claude Code
  plugin and Codex skills over it.
