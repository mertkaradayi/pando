# The shapes a program reads

pando publishes what it knows as JSON so that a program — a coding agent, a
Makefile, a CI job — can read it instead of parsing English. This file is
the contract. Everything in it is covered by the compatibility statement
below; anything pando prints that is *not* in here is a human-readable
convenience and may change without notice.

Six shapes are published:

| Command | Shape | Read it for |
|---|---|---|
| `pando signals` | one object | what the repository says about how to run itself, and every question pando would ask |
| `pando doctor --json` | one object | what this *machine* answers, and everything that is wrong |
| `pando status --json` | one object | what is running, on which ports, with which URL |
| `pando ls --json` | one object | the worktrees and their git state |
| `pando logs <name> --json` | one object **per line** | a log, with a level and a timestamp per line |
| `pando check --json` | one object | whether the setup works: a test run in a throwaway worktree, and why it failed |

There is one write path, and it is not JSON output: `pando init --answers`.
It is documented in [The answers file](#the-answers-file) below.

One more file is published rather than printed:
`~/.pando/projects/<id>/decisions.jsonl`, one object per line, which
records every question a program answered. See
[The decisions log](#the-decisions-log).

## Compatibility

Every shape carries `version`, an integer, currently **2**. It is bumped
independently of pando's own version, so a program pins what it parses
rather than which release it runs against.

Within a version:

- **Keys are added, never removed and never renamed.** A new key may appear
  in any release. Parse leniently: read the keys you need, ignore the rest,
  and never assert on the *set* of keys.
- **A key's type never changes**, and neither does the meaning of its value.
- **`null` means "pando has nothing to say here."** It never means `false`,
  never means zero, and never means an empty list. `"dirty": null` is "git
  could not be asked", which is a different fact from `"dirty": false`.
- **A key may be absent where it would be `null` or empty.** Treat absent,
  `null` and empty as the same thing — `signals` omits
  `slots[].proposal.candidates[].service.recipe` for a compose service,
  and the decisions log omits an empty list rather than printing `[]`.
- Object key **order is not part of the contract**. Array order is: every
  list pando prints is deterministic, and two runs of a read-only command
  print the same bytes.

A change that breaks any of the above bumps `version`. Pin it, check it,
and refuse a shape you were not written for rather than guessing.

**Changed in 2.** `signals.targets` went from a name-to-string map, where the
string was the first line of the target's recipe, to a name-to-object map
carrying `tool`, `prereqs` and `recipe`. The old shape was not merely
narrower, it was wrong: one line of a recipe is not the command a target
runs, and pando proposed such a line as a dev command on a real project.

One coarseness worth knowing: `signals`, `doctor`, `check`, `status` and
`logs` share a single version, so a break in one bumps all five. Nothing about `doctor`,
`status` or `logs` changed in 2. Splitting them so a program can pin what it
actually parses is recorded as a follow-up, not done here.

## stdout, stderr, and exit codes

**stdout is the answer; stderr is the narration.** Everything pando says
about what it is doing — every guess it made visible, every warning, every
question — goes to stderr. stdout carries only what the command was asked
for. `pando signals > signals.json` is a file and nothing else.

Exit codes are the first thing to read, and they are the same everywhere:

| Code | Means | What a program does |
|---|---|---|
| `0` | it worked | carry on |
| `1` | it failed | read stderr; the sentence says what and usually what to do. Do not retry |
| `2` | you asked wrongly | a bad flag, or an answers file naming a question pando does not ask. Fix the request, then retry |
| `3` | **pando has a question** | the question, its options, and the ways out are on stderr. Answer it; do not retry unchanged |

Exit 3 is not a failure. It is the whole reason the answers file exists: a
program that cannot answer a question gets the question rather than a
guess, a hang, or a wrong config.

An `init` that stops on a question has still kept the answers it wrote
before it: stdout says `wrote <file>` for each file it changed, as a run
that finishes does. They were not refused; answer the question.

One exit 3 has no options: `pando init` on a project that would run
nothing — no process configured, and no proposal at `processes` or
`dev_cmd`. The question is `dev_cmd`, `--yes` has nothing to take for it,
and `init --agent` lists it open. Answer it with a command of your own, or
`processes` with an object of process tables (see the answers file below).
`new` and `start` never ask it.

One exit 3 has options and nothing preferred: `processes` below a root
with no manifest, when an app directory outside `packages/` has nothing in
the per-app option to start it. The option's `why` names it — `backend has
uv.lock but no dev script: nothing here starts it` — and its
`needs_a_human` is true. Answer `processes` with an object of process
tables covering every process. `new` and `start` take the option.

Two traps worth knowing:

- `pando doctor` exits **1** when it found something that will break a
  command. The report is on stdout and is complete — there is no extra
  sentence on stderr. `doctor --json` exits the same way and puts the same
  verdict in `ok`, so a program never has to count severities.
- `pando start` and `pando restart` wait for readiness only when stderr
  is a terminal. From a script or an agent they return as soon as
  everything is spawned, so exit 0 means "spawned", not "ready". Pass
  `--wait` to block until every process is ready; a process that fails
  while it waits exits **1**, with its reason and the closing lines of its
  log on stderr.

## `pando signals`

Read-only, identical on two runs, and it spawns nothing: no probe of this
machine's runtime, no `docker compose config`. It is a statement about the
repository, which is what makes it safe to read before deciding anything.

```jsonc
{
  "version": 2,
  "project": { "id": "...", "root": "/abs/path", "name": "..." },
  "signals": {
    "scripts": {},              // package.json scripts, and their equivalents
    "targets": {},              // Makefile / justfile targets: name to
                                //   { tool, prereqs, recipe } — see below
    "lockfiles": [],            // every lockfile at the root
    "workspace_markers": [],    // pnpm-workspace.yaml, turbo.json, …
    "version_files": [],        // .nvmrc, .python-version, mise.toml, …
    "runtime_requirements": [   // what those files and `engines` ask for
      { "language": "node", "spec": "22", "source": ".nvmrc", "pinned": true },
      { "language": "node", "spec": "20", "source": "backend/.nvmrc", "pinned": true,
        "dir": "backend" }      // an app directory's: only there is `dir` set
    ],
    "env_example": [],          // [key, value] pairs from the env example
    "markers": [],              // framework and toolchain markers
    "compose_files": [],        // the root's; with none there, those one directory below
    "ignored_present": [],      // gitignored files that exist in the checkout
    "provision_seeds": [],      // examples a worktree file could be copied from
    "workspace_env_links": [],  // [app/.env, .env]: the root .env for apps with none
    "dependency_dirs": [],      // gitignored node_modules in the checkout, the root's
                                //   and up to two directories down: what `clone` offers
    "app_dirs": [               // only when the root has no manifest, lockfile or marker:
      {                         //   the apps below it, directly or in apps/* and packages/*
        "dir": "backend",
        "lockfiles": ["uv.lock"],
        "scripts": {},
        "markers": ["pyproject.toml"]
      }
    ]
  },
  "compose": [                  // one entry per compose file, as pando's own reader sees it
    {
      "file": "docker-compose.yml",
      "services": ["postgres", "redis"],
      "extends": [],            // services whose real definition is elsewhere
      "include": false,         // a top-level `include:` this reader does not follow
      "error": null
    }
  ],
  "slots": [ /* one per question — see below */ ]
}
```

A target is published whole, because one line of a recipe is not the
command a target runs:

```jsonc
"targets": {
  "dev": {
    "tool": "make",           // or "just"
    "prereqs": ["build"],     // what runs first
    "recipe": [               // every command line, continuations joined,
      "@./build.sh",          //   `@`/`-`/`+` prefixes kept, comments dropped
      "@./serve"
    ]
  }
}
```

pando proposes `make dev` for that target and only proposes a recipe line
itself when the target *is* that line: no prerequisites, one command, no `$`
in a makefile or `{{` in a justfile, no `-` or `+` prefix, and nothing at the
top of the file that every recipe runs with — make's `export` or `include`,
a justfile's `set` or `export`.

A repository whose root is not an app — `backend/` beside `frontend/`, and
nothing at the root that says how to build either — is read one level
down. `app_dirs` lists what was found there, and the proposals come from
it: the install is each app's own, run in its directory, and the
`processes` answer has one process per app with a dev script, each with
its `cwd`, even when only one app has one. An app with no dev script gets
no process and is in no option, and `processes` stays open for it: answer
it with an object of process tables that covers every app (see "The
answers file"). An app's version files join `version_files` under its
path, `backend/.nvmrc`, and once that answer is written a start checks the
runtime there, in the directory its processes run in. Each app's env
example joins `env_example`, and its local env files (`backend/.env`,
never a cache or a coverage file) join `ignored_present` and
`provision_seeds` under its path.

A compose file one directory down, `docker/compose.yml`, is listed in
`compose_files` and read under `compose` when the root has none of its
own, but the services question is only proposed from one at the root: a
file kept beside a deployment is as often the production stack as the
development one.

`extends`, `include` and `error` are why a services proposal can be missing
or under-ticked: they are the parts of a compose file this build did not
follow, published rather than papered over.

`namespaced` says what a namespaced start would do with each service,
read from config, the recipes and the main checkout's env files, with
nothing asked of a server — the input to the `namespaced` answer:

```jsonc
"namespaced": [
  { "service": "db", "how": "database", "recipe": "postgres",
    "keys": ["DATABASE_URL"] },          // the keys pando points at the worktree's own
  { "service": "cache", "how": "slot", "recipe": "redis", "keys": ["REDIS_URL"] },
  { "service": "search", "how": "prefix", "keys": ["SEARCH_INDEX_PREFIX"] },
  { "service": "queue", "how": "shared",
    "why": "pando has no recipe that knows its engine — [namespaced.queue] recipe names one" },
  { "service": "worker", "how": "undeclared",
    "why": "docker-compose.yml runs it, and nothing says how a worktree gets data of its own in it, …" }
]
```

`how` is `database` or `slot`, which the server makes and `rm` drops;
`prefix`, a name prefix the app is told in `keys` and nothing is made
for; `shared`, on the main checkout's data, with `why`; or `undeclared`,
a compose service neither `services` nor `namespaced` names, which every
worktree reaches as the main checkout's. A `namespaced` answer may name
it as it is: no private copy is needed for data of its own. A mail
catcher, and a service the compose file builds, are never listed.

### The twelve questions

`slots` has one entry per question pando can ask, in the order it asks
them. The names are frozen — they are the same strings `--answers` takes:

```
install  version_files  prelude  processes  dev_cmd  port_env  services  schema_hook  provision  clone  base  namespaced
```

```jsonc
{
  "slot": "dev_cmd",
  "prompt": "Which command starts the local development server?",
  "answered": false,      // config already says — from any layer
  "proposal": {
    "decided": false,     // the rules are sure enough to take this without asking
    "preferred": 0,       // the option a question preselects, and the one --yes takes
    "checked": [],        // for the set question: the options that start ticked
    "multi": false,       // the answer is a set of the options, not one of them
    "allow_custom": true, // a command of your own is an answer here
    "allow_none": false,  // null is an answer here
    "file": null,         // the compose file a services proposal is about
    "none_because": null, // why a rule settled the slot with the empty answer
    "candidates": [
      {
        "value": "pnpm dev",               // what an answers file names to pick this
        "why": "package.json scripts.dev", // the evidence, written into the config
        "preselected": false,
        "needs_a_human": false,            // --yes may not take this; a file naming it may
        "ports": null,
        "processes": null,                 // the whole [processes] table a workspace answer is,
                                           // or the env and ready a dev_cmd option brings to [dev]
        "service": null,                   // { "file": "...", "recipe": "...", "env_key": "..." }
        "hook": null,                      // the whole [[hooks]] entry this option is
        "provision_from": {}               // which paths would be copied from an example
      }
    ]
  }
}
```

**`proposal` has three states, and they mean three different things.**
Getting this wrong is the most common way a program wastes a question or
loops:

| State | Means | What a program does |
|---|---|---|
| `"proposal": null` | no rule had anything to say about this slot at all | **Nothing to choose, and still answerable.** There are no options and nobody is asked, so an `--answers` value is taken as a command of your own, or at `processes` as process tables — validated and written like any other. `services` and `prelude` are the two exceptions and report it as unused. Do not put the slot to a human: pando is not asking |
| `"decided": true` | a rule settled it; no question will be asked | Leave it alone unless it is wrong. An `--answers` value for it wins over the rule's choice, and is checked and written as an answer to the question would be |
| `"decided": false` | pando will ask | Answer by value |

A proposal with `"decided": true`, no candidates and a `none_because` is a
rule deciding the answer is *none of them* — which is a real answer, and
the opposite of `"proposal": null`.

`answered` is the key that says a `null` proposal is closed. A slot a
program filled still has no proposal — nothing about the rules changed —
so a reader that watches `proposal` alone will answer it again on every
run. Watch `answered`.

Four more facts about `slots` that are not visible in the shape:

- **`prelude` is never proposed here.** It is the one question about this
  laptop rather than this repository, it costs a `bash -lc` probe, and
  `signals` spawns nothing. It is asked only when this machine does not
  resolve what the project pins; `doctor`'s `runtime` section is where that
  evidence lives.
- **Two slots take no answer when nothing was proposed.** The set question,
  `services`, is answered by naming options, and there are none. `prelude`
  is verified against this machine before it is written, and the check only
  exists behind the proposal that raises the question. A value for either
  is reported as unused.
- **`base` is proposed only when origin/HEAD is far behind** the branch
  the main checkout is on: a hundred commits and thirty days. Its options
  are that branch, then origin/HEAD's; `"proposal": null` means origin/HEAD
  is where `new` forks from and `check` tests, as it is for most
  repositories. Either way it takes a branch name of your own, written as
  `[project] base`, and one this repository has no branch for, here or on
  origin, is refused with exit 2.
- **Answering one slot can settle another.** Taking the per-app form at
  `processes`, or answering it with an object of process tables, fills
  the dev command and its ports for every process, so
  `dev_cmd` and `port_env` are never asked and an answer sent for them is
  reported as unused. That is correct, not an error.

## `pando doctor --json`

The whole report, plus the exit code as a value. It probes this machine —
`bash -lc`, the tools on `PATH`, the engines a recipe needs — so it is the
half `signals` deliberately leaves out. It writes nothing anywhere.

```jsonc
{
  "version": 2,
  "ok": true,               // the exit code as a value: false means exit 1
  "project":  { "id": "...", "root": "...", "home": "...", "worktrees_dir": "...",
                "home_mode": "700", "port_min": 17000, "port_max": 32767,
                "base_step": 8, "bases_in_range": 1971, "windows_held": 0 },
  "config":   { "layers": [ { "layer": "committed|user|project", "path": "...",
                              "present": true, "error": null,
                              "keys": [ { "key": "dev.cmd", "value": "\"pnpm dev\"",
                                          "note": "# detected: package.json scripts.dev",
                                          "ignored": false } ] } ],
                "error": null },
  "runtime":  { "prelude": "...", "prelude_from": "...",
                "languages": [...], "requirements": [...] },
  "tools":    [ { "name": "git", "path": "/usr/bin/git", "version": "...",
                  "detail": null, "needed_for": "...",
                  "install": null,  // for a tool not found: the line that gets it
                  "found": true } ],
  "worktrees":[ { "name": "...", "path": "...", "phase": "...", "created_by_pando": true,
                  "main": false,  // the main checkout's record, once pando has run it
                  "mode": "shared|namespaced|isolated", "isolated": false,
                  "locked": false, "prunable": false,
                  "prunable_reason": null, "known_to_git": true,
                  "processes": [...], "services": [...] } ],
  "services": { "compose": [...], "native": [...], "isolation": { ... } },
  "hooks":    [ { "name": "migrate", "after": "services", "cmd": "...",
                  "fingerprint": ["prisma/migrations/**"],
                  "matches": 3,     // null when the hook is keyed on nothing
                  "runs": [ { "worktree": "feat+one", "ran_at": "...",
                              "will_run_again": false } ] } ],
  "adoption": [],           // project folders that look like this repo from before it moved
  "findings": [ { "section": "project|config|runtime|tools|worktrees|services|hooks|adoption",
                  "severity": "problem|note", "message": "...", "fix": "..." } ]
}
```

`tools[].install` is set only for a tool the shell was asked about and
did not have, when pando knows how it is got: `"brew install cloudflared
  (or Cloudflare's cloudflared package)"`. It is advice for the developer,
not a command to run for them; pando never installs anything.

`findings` is the part to read first. `severity` is `problem` — something
here will break a command, and `ok` is `false` — or `note`, which is
something to know that breaks nothing. Every finding carries its own `fix`.

`config.layers[].keys[].note` is the provenance of every key pando wrote,
verbatim from the file and so with the comment's own `#` on the front:
`# detected: <evidence>`, `# answered: <date>`, `# answered: a program,
<date>`, `# answered: --yes took the first of N options` (`# answered: --yes took the only option` when there was one), or, for the
services question, whose answer is a set, `# answered: --yes took the
<taken> of <offered> the rules resolved`. A `[[services]]` entry's note
is on the entry's own key, its place in the list: `services[0]`. A key
with a comment a developer wrote themselves carries that instead, and one
with no comment carries `null`. It is how a developer, or a program, tells
what decided each line.

A `# detected:` value that pando's rules **would not write now** is a
finding in the `config` section, at `note`. pando asks each question once
and never asks again, so a value an older rule wrote survives every
improvement to that rule — and so does a value that was right until the
repository changed under it. The finding names the key, the file, what it
holds, and what the rules offer for that key instead. The fix is the
`pando init --answers - --replace` command that writes the first of them,
where that question's answer is what the key holds — for a key under a
process table, the command replaces every process table in pando's own
file, and the fix says so; for a service or a hook, whose answer is one
entry of a set, there is none — and deleting the line where that makes
the next command that needs it ask again — not always: a `[dev]` whose
command is still there has answered its ports.
pando never rewrites the value itself. A value marked `# answered:` is
never reported: that is a decision, by a person or by a program, and pando
does not second-guess decisions — so a deliberate answer closes this for
good.

A process with no `ports` key at all whose command runs a framework's
server — named in the command, or in the `package.json` script it runs in
its directory — is a `config` finding at `problem`: it listens on the
framework's own port in every worktree, so two running at once clash.
`ports = []` is never reported; it says the process has none. The fix is
the `init --answers - --replace` command for a lone `[dev]`, or the
`ports` line to add to a named process table.

`services.isolation` carries the native-versus-container decision and the
evidence behind it. It is the report a program should quote to a human
rather than re-deriving.

A worktree pando did not create that lacks a file `provision` names is a
`worktrees` finding, at `note`: one per path, naming every worktree that
lacks it. pando never writes into such a worktree, so the fix is the
command for the developer to run — a `cp`, or an `ln -s` where
`provision_mode` links, one loop over them all when there are several.
Where that worktree's `.gitignore` does not ignore the path, the finding
says so and gives no command; a worktree without the directory the path
goes in, a branch from before that app, is not named. A worktree pando created gets a file it
lacks at its next `start`, and is not reported.

`hooks[].matches` is how many files the hook's `fingerprint` globs match
in the main checkout — at `0` it runs on every start, and a `hooks`
finding says so — and `null` for a hook keyed on nothing, which runs on
every start by design. `hooks[].runs` has one entry per worktree the
hook has run in: `ran_at`, and `will_run_again`, whether that worktree's
next start runs it again. A worktree it has never run in has no entry.

## `pando status --json`

```jsonc
{
  "version": 2,
  "project": { "id": "...", "root": "...", "name": "..." },
  "worktrees": [
    {
      "name": "feat+one",
      "main": false,                   // true for the main checkout, listed first
      "branch": "feat/one",
      "path": "/abs/path",
      "ports": { "web": 17008 },       // the roles this worktree holds
      "observed_ports": [17008],       // what is actually listening
      "url": "http://localhost:17008", // the page a browser opens, or null
      "mode": "shared|namespaced|isolated", // which services it talks to
      "isolated": false,               // true exactly when mode is "isolated"
      "share": null,                   // or { url, local_port, proxy_port, since }
      "processes": {
        "dev": { "pid": 1234, "phase": "starting|running|failed",
                 "since": "2026-09-21T23:22:05Z", "reason": null,
                 "log": "/abs/path/dev.log",
                 "app": null },           // or, for Expo's Metro, the object below
        "mobile": { "...": "…",
                    "app": { "client": "Expo Go|its development build",
                             "url": "exp://127.0.0.1:17012",
                             "simulator": "xcrun simctl openurl booted 'exp://127.0.0.1:17012'",
                             "android": "adb reverse tcp:17012 tcp:17012 && adb shell am start -a android.intent.action.VIEW -d 'exp://127.0.0.1:17012'",
                             "development_build": "exp+shop://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A17012",
                             "native": { "base": "main",   // or null: no native change
                                         "changed": ["apps/mobile/ios/Podfile"],
                                         "build": "npx expo run:ios --port 17012",
                                         "builds": { "ios": "npx expo run:ios --port 17012",
                                                     "android": "npx expo run:android --port 17012" } },
                             "installed": { "device": "iPhone 17 Pro",   // or null
                                            "sdk": 55, "expected_sdk": 57,
                                            "build": "npx expo run:ios --port 17012" } } }
      },
      "services": {
        "postgres": { "kind": "compose|native", "port": 17010, "up": true,
                      "logging": true, "project": "pando-…" }
      },
      "namespaces": [                  // its own, in the main checkout's servers
        { "service": "mariadb", "database": "shop__feat_one",
          "host": "localhost", "port": 3306, "in_use": true },
        { "service": "redis", "slot": 3,
          "host": "127.0.0.1", "port": 6379, "in_use": true }
      ],
      "hooks": { "install": { "fingerprint": "md5:…", "ran_at": "…" } }
    }
  ]
}
```

`main` is true for the main checkout, which pando runs too. It is the
first entry once pando has anything recorded for it — or when it is the
one named — and absent before, so a project whose main checkout was never
started lists its worktrees alone. Its `mode` is always `shared`.

`mode` says which services a worktree's processes talk to, or last
talked to once it is stopped: `shared` is the main checkout's servers and
its data, `namespaced` the main checkout's servers with a database, a
slot or a name prefix of the worktree's own in them, and `isolated`
servers of its own. A
worktree never started reads `shared`. `isolated` came first and stays,
true exactly when `mode` is `isolated`, so a program written against it
keeps working.

`namespaces` lists what a worktree holds in the main checkout's own
servers: a `database` of its own, or a `slot` of its own, each on the
`host` and `port` it was made on. `in_use` is true while the worktree runs
namespaced, and false for one kept through a switch to another mode —
kept until `rm`, which drops it. The list is empty for a worktree that
never started namespaced. A prefix is never in it: nothing is made for
one, so nothing is recorded or dropped. No login is ever in this shape.

`phase` is per process; a worktree is only as up as its worst one.
`reason` is non-null exactly when `phase` is `failed`, and it is the
sentence to show a human. `log` is an absolute path — read it with
`pando logs`, not by opening the file, so truncation and partial lines are
handled for you.

`url` is the worktree's page: the `web` role's, else the first role of
the first process by name, among the processes that serve a page. A
process serves none when its table says `page = false`, or, saying
nothing, when it is a bundler whose app runs on a device (Expo's Metro,
below). A worktree that runs nothing else has `url: null`, and `pando
open` opens its app instead of launching a browser, as `pando open
--app` does beside a page: on the booted iOS simulator, else on a
connected Android device or emulator, else, on a Mac with Xcode, on a
simulator it starts and waits for, running the `simulator` or `android`
command below. With nowhere to open it, it prints why and both commands.
`pando share` still publishes such a worktree's port.

`app` is non-null for a process whose app runs on a phone, a tablet or a
simulator rather than in a browser: Expo's Metro, known from the settings
by its port variable, `RCT_METRO_PORT`, or by `expo start` in its command.
`client` is what opens `url`: `Expo Go`, or `its development build` for
an app whose `package.json` depends on `expo-dev-client`, which Expo Go
cannot run. `simulator` is the command that opens `url` on the booted
iOS simulator, `android` the one that opens it on a connected Android
device or emulator (`adb reverse` first, so the device's `127.0.0.1`
port reaches this machine's Metro), and `development_build` is the link
a development build opens, whichever `client` is. Its scheme is the one `expo-dev-client`
registers: `exp+` and the app's `expo.slug`, lowercased with anything but
letters, digits, `+`, `-` and `.` dropped — never `expo.scheme`. pando
reads the slug from the worktree's `app.json`, else from `app.config.*`
without running it: a `slug: "…"` string literal, taken only when every
place those files set `slug` gives the same one. An app whose config
computes its slug gets, while it runs, the slug of its development build
on a booted simulator (`app.installed`, below), else `exp+<slug>`: fill
it in. `pando open` runs no command that holds `<slug>`, since no build
registers that scheme, and prints the commands to fill in instead. Every
address is `127.0.0.1`, which the simulator shares and a physical device
cannot reach; for a device, use the machine's LAN address the developer
gives.
It is there whatever the process's `phase`, and opens something only while
it is `running`.

`app.native` is non-null when the worktree's branch changes the app's
native code against `base`, the base the branch forks from: a file under
an `ios/` or `android/` directory anywhere in the app (a local module's
`modules/<name>/ios/` included), or its `app.json` or `app.config.*`,
committed since the branch left `base` or not. A build of another branch
lacks what those add, so the bundle this worktree's Metro serves can
crash into a missing module. `changed` lists the files, relative to the
worktree, and `builds`, by platform (`ios`, `android`), run in the app's
directory, builds and installs this worktree's own development build on
the iOS simulator or on an Android emulator or device, pointed at its
running Metro, which it reuses rather than starting a second one; a
simulator or a device keeps one build per bundle id, so it replaces the
one there. `build` is `builds.ios`, kept for readers written before
`builds`. A new native dependency in `package.json` alone
is not seen. It is always `null` for the main checkout.

`app.installed` is the app's development build on a booted iOS
simulator, while the process is `running`; `null` when none is found,
and always where there is no `xcrun`. pando reads it from the
simulator's disk (the config Expo embeds in the build) and never boots,
launches or installs anything. A build is the app's when its slug or
its `ios.bundleIdentifier` is the app's own, from `app.json` or a literal
in `app.config.*`, or, where the config computes both, when the config
quotes the build's slug or bundle id. `device` is the simulator's name,
`sdk` the major Expo SDK the build was made with, and `expected_sdk`
the one the worktree needs, from its installed `expo` package, else
its `package.json`; either is `null` when unknown. When the two differ,
the build loads this worktree's JavaScript and fails on native code it
lacks (`Property 'MessageQueue' doesn't exist`, say): `build`, run in the
app's directory, replaces it, and `status` says so under the process;
`pando open` and the TUI's `o` do not open the app in it, and give that
command instead (`open` exits 1). When the app's config names no slug, the build's slug
fills `development_build`, unless the build was made for another SDK.

`share.url` is the public URL. **No cookie is ever in this shape**, even
when the share is behind an auth command.

## `pando ls --json`

```jsonc
{
  "version": 2,
  "project": { "id": "...", "root": "...", "name": "...", "repos": ["backend", "frontend"] },
  "worktrees": [
    { "name": "feat+one", "main": false, "path": "...", "branch": "feat/one", "head": "abc1234",
      "detached": false, "dirty": false, "ahead": 0, "behind": 0,
      "created_by_pando": true, "mode": "shared|namespaced|isolated",
      "prunable": false, "locked": null,
      "pr": { "number": 12, "state": "open|merged|closed", "url": "..." },
      "repos": [
        { "name": "backend", "path": ".../feat+one/backend", "branch": "feat/one",
          "head": "abc1234", "detached": false, "dirty": false, "ahead": 0, "behind": 0,
          "prunable": false }
      ] }
  ]
}
```

The first entry is always the main checkout, with `main` true and
`created_by_pando` false: pando runs it, and never removes it. Every other
entry is a worktree, `main` false.

`project.repos` and each entry's `repos` are there only for a multi-repo
project: a plain folder whose immediate subdirectories are git
repositories, which pando runs as one project. Its main checkout is the
folder, and each worktree is a *set*, a directory holding one checkout of
every repository under the repository's name. `repos` lists those
checkouts in `project.repos`' order, with what git says of each; the
entry's own `branch` is the one they are all on (`null` when they differ),
`dirty` is true when any is, and `ahead` and `behind` are summed over them,
each repository measured against its own base.

`dirty`, `ahead` and `behind` are `null` when git could not be asked.
`locked` is `null` when the worktree is not locked, and the lock reason —
possibly an empty string — when it is. `pr` comes from a cache: `ls --json`
never spawns `gh`, so it works offline and may be stale or `null`.

## `pando logs <name> --json`

One JSON object per line, on stdout, forever if `-f` was passed:

```jsonc
{ "version": 2, "ts": "2026-09-20T10:00:00+00:00", "level": "debug|info|warn|error", "line": "ready in 412ms" }
```

`ts` is the line's *own* timestamp when pando could read one, normalised to
UTC, and `null` when the line has none — it is not the time pando read it.
`line` is the text with ANSI colour removed. `level` is pando's reading of
the line, not the writer's: a line containing no level word is `info`.

`--source` picks the log: a process name, a service name, or a hook name.
Without it, `dev` — or, for a worktree with no `dev` log and exactly one
process, that process. A worktree with no `dev` log and several processes
gets all of their logs merged into one stream, and then every line
carries one more key, `source`, naming the process it came from:

```jsonc
{ "version": 2, "ts": null, "level": "info", "line": "listening on 17008", "source": "api" }
```

A read of one log — any `--source` — has no `source` key. `status --json`
names every log a worktree has; pass `--source` rather than relying on
the default.

Every command that takes a worktree name also takes its branch: `feat/one`
and `feat+one` name the same worktree. The JSON always carries the
directory form, `feat+one`, in `name`.

## `pando check --json`

The result of one `pando check`: it makes a worktree of the commit a new
worktree would fork from, with no branch, installs it and starts every
process, waits until each one is ready, asks the process that owns the
worktree's URL for `/`, then stops and removes all of it. It writes under
pando's home and, for the throwaway worktree it removes again, inside
`.git`; never into the repository. Printed whatever the result — a pass,
a failure, a question still open, an interruption — as the one object on
stdout.

`pando check --base <branch>` tests the commit that branch is at, for that
run only, looked up as `new --base` looks one up; `base_ref` says which ref
it read. A base the repository does not have exits 1 with nothing tested.
`new` still forks from the project's own base, or origin/HEAD, so a run at
another one is a probe: its result is printed in full and never saved, and
the last check at the project's own base still says where the setup
stands. `notes` says so. A pass at another base is the setup's once
`base` is answered with it and `pando check` passes again. While the
`base` question is open, a plain `pando check` tests nothing and exits 3
with it (`not_set_up`, `slot: "base"`); with `--base` the run is a probe
of the base it names.

`mode` says where it ran the project's data. `namespaced` when the
project has hooks after `services` or after `dev` to prove — a schema
step — and a `start --namespaced` of the check's worktree could run
without a question: a database, and a Redis slot, of the check's own are
made in the main checkout's servers, those hooks run there, and all of it
is dropped with the worktree, never the main checkout's own. `shared`
otherwise, as a plain start runs: those hooks are not run, since there
they would run against the developer's own data, and `notes` says which,
and — when there were some to prove — that the schema step was not
tested, and why.

```jsonc
{
  "version": 2,
  "project": { "id": "...", "root": "...", "name": "..." },
  "result": "passed|failed|not_set_up|interrupted",
  "mode": "shared|namespaced",     // where its data ran: see below
  "kind": "settings|machine|base", // whose the failure is; null unless "failed"
  "reason": "web exited with status 1 — ...", // null when "passed"
  "slot": "dev_cmd",               // the open question; null unless "not_set_up"
  "commit": "a1b2c3d4…",           // the full sha tested, or null; one per repository, space-separated, for a multi-repo project
  "base_ref": "origin/main",       // the ref it was read from; null for HEAD
  "processes": [
    { "name": "web", "ready": true, // it came up: its port answered, or,
                                     // with no port, it stayed up
      "port": 17008,
      "http_status": 200,          // the page's status; null for every other process
      "secs": 4.2 }                // how long it took to be ready, or to fail
  ],
  "failed_process": "web",         // or "install"; null when nothing failed
  "failed_tail": ["..."],          // its last lines, with secrets hidden
  "notes": ["skipped the hooks that run after services (migrate): ..."],
  "ran_by": "tui|terminal|program",
  "pando_version": "0.5.0",
  "started_at": "2026-09-27T10:00:00Z",
  "finished_at": "2026-09-27T10:01:12Z",
  "settings_changed": false        // the settings changed while it ran
}
```

`kind` says who fixes a failure. `settings` is pando's settings for the
project — a dev command that exits, an install that fails, a schema step
that fails in a namespaced check, a page that answers `5xx` — and is
fixed through `pando init --answers -`, then checked again; a failed hook
is `failed_process`, by its name. `machine` is this machine: a shared
service nothing answers on, found before anything was made, or in a
namespaced check a server that refuses the login or a login with no grant
to make a database and no administrator pando can find to give it one. `reason` then carries the command that fixes it, and
no setting changes it. `base` is the commit tested: a step failed on a
file — one its failure names, or a lockfile of a manager the install
runs — that the main checkout's branch has and that commit does not.
`reason` names both refs and `pando check --base <branch>`; no setting
fixes it, and loosening the install until it passes would pass on the
wrong commit. Which branch work starts from is the developer's to say,
and the `base` answer records it.

`ready` is about the process alone, judged as `start --wait` judges it.
The page is judged apart: a process can be `ready` with a page that
failed, and then `http_status` and `reason` say how. A check passes only
when every process is ready and the page answers.

A page answers when its status is below 500; a server that answers in
something other than plain HTTP — TLS, say — passes with a note, since
it answers. No answer within 90 seconds fails: a dev server compiling
its first page gets that long.

Exit codes: `0` passed, `1` failed or interrupted, `3` a question is still
open. On exit 3 the object has `"result": "not_set_up"`, `slot` naming the
question, `reason` and `commit` null and `processes` empty, and the
question itself is on stderr, as every exit 3 puts it; nothing was made.
When another check of the same project is running, it exits 1 with
nothing on stdout: that check's result is the one to read.

`failed_tail` has at most ten lines. Passwords in URLs, the values of keys
named like a secret, and bearer tokens read `(hidden)`: this list is
meant to be pasted into a conversation. `settings_changed` true means the
project's run settings were edited while the check ran, so the result
speaks for neither the old ones nor the new.

## The decisions log

`~/.pando/projects/<id>/decisions.jsonl`. Appended to, never rewritten;
one JSON object per line, oldest first.

Every answer a **program** supplies to a question the rules could not
decide is written here with the evidence it was decided from, and a later
line records it if a person changes it afterwards. Nothing else goes in:
not a rule's own answer, not a person's, and not `--yes` taking what the
rules already preferred.

It exists because a skill that answers pando's questions is a crutch
unless somebody reads what it answered. This is the labelled corpus that
improves the rules — and the rules are what every developer gets,
including the ones with no agent.

```jsonc
// a program answered a question
{ "version": 1, "at": "2026-09-21T23:22:05Z", "slot": "dev_cmd", "kind": "answer",
  "answer": "pnpm dev:web",   // exactly the shape an answers file sends
  "shape": "choice|custom|set|none",
  "wrote": "pnpm dev:web",    // what config says about the slot afterwards
  "evidence": {
    "prompt": "Which command starts the local development server?",
    // the three below are omitted when they are empty, as everywhere else
    "details": [],            // what the question printed above its options
    "mechanism": null,        // "compose" or "native", at the services question
    "weighed": [],            // and the facts that chose that mechanism
    "preferred": 0,           // the option the rules would have taken
    "options": [ { "value": "pnpm dev", "why": "package.json scripts.dev",
                   "preselected": true, "needs_a_human": false } ]
  } }

// …and a person later changed it
{ "version": 1, "at": "2026-09-22T09:04:11Z", "slot": "dev_cmd", "kind": "override",
  "was": "pnpm dev:web", "now": "pnpm dev:all" }
```

Two things follow from `answer` being the shape an answers file sends:
the log **replays** — every `answer` line for a project is an answers
file, with `jq` and nothing else — and it is directly comparable with
what pando would propose today, which is what makes it a corpus rather
than an audit trail.

An override is noticed by comparing what config says about the slot
against what the log last recorded, on the next command that resolves
anything. The comparison is on the slot's *answer*: a comment or a
reordering is not an override, and neither is editing a command inside
the shape that was chosen at `processes` — switching between a process
per app and the root script is. pando would rather miss an override than
invent one. `version` here is the line's own, bumped independently of the
`version` on the printed shapes.

## The answers file

`pando init --answers <path>` — or `-` for stdin — is the one way a program
writes config. It is not a TOML editor: every value goes through the same
question, the same validation and the same file a person's answer does, and
lands with `# answered: a program, <date>` beside it.

One JSON object. Keys are the question names above. Values:

| JSON | Means |
|---|---|
| `"some text"` | the option whose `value` is exactly that text, **or**, if nothing matches — or there were no options at all — a command of your own (where `allow_custom` is true) |
| `["a", "b"]` | the set answer, at the one question where `multi` is true; every element must name an option |
| `["a", "b"]` | the whole list, at `version_files`, `provision` and `clone`, whose single answer is a list of paths |
| `null` | "none of them", where `allow_none` is true. At `schema_hook` it is "no": the step is written with `on = "never"` |
| `[]` | "none of them" at the set question. A usage error anywhere else — `null` is how you say none |
| `{"api": {...}, "web": {...}}` | at `processes` only: process tables of your own, one per key, each exactly what `[processes.<name>]` takes — `cmd`, and optionally `cwd`, `ports`, `env`, `ready` and `page` |
| `{"db": {...}, "search": {...}}` | at `namespaced`, and only an object there: each service's settings, `recipe`, `db_env` and `prefix_env`, written to `[namespaced.<service>]` beside any login. A `user` or `password` in it is refused: a login is never an answer |

The object is how a project of several processes is answered when no
option fits it. A string of your own at `processes` is **one** command,
written as `[dev]`; the `name: cmd in dir; …` text pando shows its own
per-app option in names that option and is never a way to describe one.

```jsonc
{ "processes": {
    "api":    { "cmd": "uv run uvicorn app.main:app --port {port}", "cwd": "backend",
                "ports": ["api"] },
    "worker": { "cmd": "uv run python -m app.worker", "cwd": "backend", "ports": [] },
    "web":    { "cmd": "npm run dev", "cwd": "frontend", "ports": { "PORT": "web" },
                "env": { "API_URL": "http://127.0.0.1:{port:api}" },
                "ready": { "timeout_s": 90 } } } }
```

`ports` is a list of the roles the process owns, whose port reaches it
as `{port}` or `{port:<role>}`, or a map of environment variable to role;
`[]` is a process with no port, a worker. `{port:<role>}` in `cmd` or
`env` may name any process's role or a service's name. `ready` takes
`role` and `timeout_s`. `page = false` marks a process whose port no
browser opens, which the worktree's URL then skips. Each table is written with
`# answered: a program` on its header.

**Answers are by value, never by index.** An index breaks the day a rule
finds one more candidate; the text does not, and picking an option by its
text is what brings everything else the option carries — the ports a
command owns, the whole process table a workspace answer is, a service's
env key, the hook entry.

`namespaced` names, for each service the project declares or its compose
files run, the recipe
its engine is when its image does not say (`"recipe": "elasticsearch"`),
the env keys its app names its database or slot by when pando did not
find them (`"db_env": ["REDIS_DB"]`), and the keys it reads a prefix
from (`"prefix_env": ["SEARCH_INDEX_PREFIX"]`), each set to the main
checkout's value with the worktree's slug after it:

```jsonc
{ "namespaced": {
    "search": { "recipe": "elasticsearch", "prefix_env": ["SEARCH_INDEX_PREFIX"] },
    "cache":  { "db_env": ["REDIS_DB"] } } }
```

Each key is optional, and one is enough: `prefix_env` alone gives a
service a prefix whatever its engine; `recipe` is for an image that does
not say what it is, so its recipe's own keys and commands apply.

Refusals, all exit 2 and all naming the key: a name that is not a question;
a shape the question cannot take; a value that is not one of the options at
a question that has them; an empty string; a `prelude` that fails its own
probe on this machine, which is never written down; a `port_env` of your
own that is not environment variable names; a string at `processes` or
`dev_cmd` in the `name: cmd in dir; …` form that is none of the options;
process tables with a key a table does not take, no `cmd`, a variable
that is not a name, a `cwd` that is not a directory of the repository or
leaves it, a role two processes claim, or a `{…}` nothing will fill; at
`namespaced`, a service neither the project's config nor its compose files have, a recipe there is
not or one that knows no namespace or prefix, a key that is not an env
key, a service with nothing said, or a login. Each refusal says what the
question does take.

A `port_env` of your own may name several variables, as one string
separated by commas, `"PORT, API_PORT"` — the way the option naming
several is written — and each owns the role its name says: `PORT` owns
`web`, `API_PORT` owns `api`. One variable owns `web` whatever it is
called.

An answer for a slot that is already answered is **refused with exit 2,
and nothing is written**, `--dry-run` included: the refusal names the slot
and says how to change it, `--replace` for most. An answer for a slot
nothing asked about in this run — `dev_cmd` once the same file's
`processes` has answered it — is reported on stderr and not applied, and
the run still exits 0. A slot with no proposal at all is not "nothing asked about":
it takes a custom answer, except at `services` and `prelude`. See the
three-state table above.

`--dry-run` runs the same pass against copies of the files it would write
and prints them on stdout, config first. Use it to show a diff before
writing. A question nothing answers is not a failure there: the slot is
listed as `(unanswered)` and the dry run still exits 0.

### Correcting an answer: `--replace`

`pando init --answers - --replace` applies the file to questions that are
already answered too. Preview it first with
`pando init --answers - --dry-run --replace`.
It is how a setup found wrong is corrected without editing `pando.toml`.
Every slot the file names takes the file's answer, in place of what config
says and of what a rule would decide, through the same checks a first
answer gets. It is written with the same note, `# answered: a program`
and the date, and an `answer` line in the decisions log, so the change is
never read as a person's override. Only pando's own config for the project
is written; a slot the file does not name is left as it is. `--replace`
needs `--answers`; without it, clap refuses the command with exit 2.

Refused under `--replace`, exit 2, before anything is written:

- a `prelude` that is already answered. The prelude is about the machine,
  and only a person changes it. If it has no answer yet, the file's answer
  is a first answer, as without the flag.
- `processes`, `services` or `schema_hook` when the committed `pando.toml`
  or the machine-wide config declares them. pando never writes those files,
  and its own config written over them would hide their tables or merge
  with them rather than replace them.
