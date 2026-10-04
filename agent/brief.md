# Setting pando up on a project, and running it

This is the procedure. It is written once and both host packagings — the
Claude Code plugin in `skills/`, the Codex skills in `codex/` — point at
this file rather than repeating any of it. If you are reading one of those
wrappers and it seems to contain reasoning, the wrapper is wrong.

You will need the contract for every shape named here, which is
versioned: `pando init --agent --reference json` prints it (or
[`json.md`](./json.md) beside this file).

Your job in one sentence: **turn the evidence pando already publishes into
answers, ask the human only the things that are genuinely theirs, and write
nothing except through `pando init --answers`.**

## First run: set pando up, then prove it with `pando check`

The developer pasted one line: run `pando init --agent` and follow what it
says. You are done when `pando check` passes and you have offered to remember
how to run the project. Change no file in the developer's repository, and
none of theirs outside it without a yes: every setting goes through
`pando init --answers -`, and the worktree `pando check` makes is pando's
own, inside `.git`, removed when it is done.

1. **Ask the developer nothing that pando or the project's docs answer.**
   Taking pando's first choices gets them to ready soonest, and they
   change any of it later. The one question a first run always asks
   comes at the end, in step 7.
2. **Save pando's choices in one step: `pando init --yes`.** It takes
   pando's first choice for every open question and writes it under
   `~/.pando`, never into the repository. pando runs every app it found a
   command for. The check uses the developer's own services, as the main
   checkout does, so how private ones would run does not matter to it.
3. **A question with no option is yours to answer.** When
   `pando init --yes` leaves one open (exit 3, the question on stderr),
   answer it from `pando signals` and `pando doctor --json`. Do not
   re-derive what they report. Answer through stdin, never an answers
   file, which in the project would be a file in the developer's
   repository:

   ```bash
   pando init --answers - --dry-run    # one JSON object on stdin; writes nothing
   pando init --answers -              # the same object, written
   pando init --yes                    # then pando's choice for the rest
   ```

   When `signals` lists `app_dirs` that no process covers, such as an app
   with no dev script, pando leaves `processes` open. Answer it with an
   object of process tables, one per process the project runs, from its
   README or docs: `{"processes": {"api": {"cmd": "…", "cwd": "backend",
   "ports": {"API_PORT": "api"}}, "worker": {…, "ports": []}}}`. A
   process gets its own port through the variable it reads, mapped in
   `ports`, and another's address in `env` as `{port:<role>}`. Give the
   role a person opens in a browser the name `web`: the URL and the
   page the check asks for are its, else the first role by name. Ask the
   developer only what the docs do not say.
4. **Run `pando check`, with a timeout of at least 10 minutes.** It makes a
   throwaway worktree of the commit a new branch would fork from, runs the
   install, starts every process, checks the one the browser opens really
   answers, and removes it all. The prompt the developer pasted is their
   consent to this test. Its notes say whether the schema step was tested:
   only namespaced mode with a login runs it. All it runs sees
   `PANDO_CHECK=1`, so a process can skip, say, opening a simulator.
5. **When the check fails, fix, then rerun; never rerun unchanged.**
   - A settings failure is yours: correct the answer with
     `pando init --answers - --replace`, then run the check again.
   - A machine failure (`kind: "machine"`: a server not running, Docker
     stopped, a runtime missing) is the developer's: tell them the
     command pando printed, and change no setting for it.
   - A base failure (`kind: "base"`): the tested commit lacks a file,
     often a lockfile, that the main checkout's branch has. Change no
     setting to get past it, and never drop a frozen install's flag.
     Which branch work starts from is the developer's: tell them both
     refs from `reason`, answer `base` with the one they name, and check
     again. `pando check --base <branch>` tests one without saving it as
     the base, and without replacing the last result.
   - Exit 3 is not a failure: a question is still open, and the check
     started nothing. Answer it, then run the check again.
   - Stop after three changed attempts, and tell the developer what is
     wrong in pando's own words.
6. **Remember how to run it, only if the developer says yes.** The block
   at the end of the job says how the project runs with pando;
   `pando init --agent --reference memory` prints it alone. It belongs in
   your own persistent memory — Claude Code: `~/.claude/CLAUDE.md`;
   Codex: `~/.codex/AGENTS.md`; any other agent: its own. That file is
   the developer's, read in every session of every project, so write
   nothing to it without their yes. On a yes, save the block there,
   replacing an earlier pando block for the same project root (its
   heading names the root). On a no, or no answer, write nothing, and
   give them the block to keep. Never a `CLAUDE.md`, `AGENTS.md` or any
   other file inside the repository. If a sandbox will not let you write
   the file, give them the block and its name.
7. **Tell the developer you're done, in two lines, then ask step 6's
   question:**

   > pando is set up and tested for <project>.
   > You're ready: run `pando`.
   >
   > Want me to remember how to run it with pando? I'd add a short block
   > to `~/.claude/CLAUDE.md`, which I read in every session.

   Name your own memory file in the question. Before it, add one line
   for each of these that applies: the schema step is untested until the
   first `pando start --namespaced` asks for its login; the job's line
   about a phone or tablet; the line `pando doctor` notes about a queue
   worker on a shared Redis.

The rest of this brief is for a failure that needs it; a first run that
passes never does.

---

## 0. Read before you ask

Two commands, in this order, before you form any opinion:

```bash
pando signals            # what the repository says about how to run itself
pando doctor --json      # what this machine answers, and what is wrong
```

`signals` is read-only, spawns nothing, and is identical on two runs.
`doctor` is the half that probes the machine: the login shell, the tools on
`PATH`, the engines a private service would need.

**Do not re-derive any of this.** No `cat package.json`, no `ls`, no
`docker compose config`, no `node --version`. Everything those would tell
you is already in the two objects above, and for everything the rules
cover, an absence there is itself the answer — a rule looked and found
nothing, and your job is to notice that, not to go looking with different
eyes.

There is one exception, and §2 is where it is spelled out: a slot with
**no proposal at all** is not a rule saying "no". It is the rules having
nothing to offer, and it is the one place where something the project
says about itself — a line in its README, a `Makefile` target the
developer points you at — may become the answer. Even there, read for
*that* question only, and never to second-guess a proposal `signals`
already made.

Read the repository's own files, otherwise, only when you are about to
ask a human something and need one more sentence to make the question
intelligible. Never as a substitute for `signals`.

## 1. The question budget

> On a first run (the section above): **zero** questions the rules or the
> project's docs answer.
> On a project the rules fully understand: **zero** questions.
> On a project they half understand: **one**.

Three things, and only three, are genuinely a human's:

1. **Which mechanism runs private services on this laptop** — containers or
   engines installed on the machine. It is a preference about their
   computer, not a fact about their repository. See §5.
2. **Which apps of a monorepo they want running.** It is a preference about
   their work this week.
3. **Which branch work starts from**, when origin/HEAD is far behind the
   branch the main checkout is on. It is how their team works, which the
   repository does not say: see the `base` row in §3, and step 5 of the
   first run for the check that fails on it.

Everything else — the install command, what pins the runtime, the dev
command, how the port reaches it, the schema step, which local files a
worktree needs — is *evidence*, and evidence is the rules' job. If you
find yourself wanting to ask about one of those, you have either not read
`signals` properly or you have found a genuine gap in the rules. In the
second case the right move is to answer it from the evidence, let pando
record it (see §7), and say so in your summary — that record is what makes
the rule better for everyone who has no agent.

If you must ask, ask **once**, with the options pando published and the
`why` beside each. Never ask a question whose answer is in `signals`.

## 2. Reading `signals`: three states, three behaviours

`slots` has ten entries, one per question pando can ask, in the order it
asks them. Each has a `proposal`, and its state decides what you do:

| State | Means | You |
|---|---|---|
| `"proposal": null` | no rule had anything to say | **nothing to choose from — and the one place your own knowledge is the only thing there is.** No options, no preselection, and nobody is asked. A value you send here is taken as a command of your own, validated and written like any other. Send one only if you know it |
| `"decided": true` | a rule settled it | **do nothing.** A value you send is reported as unused |
| `"decided": false` | pando will ask | **this is the one you answer from the options** |

Also check `"answered": true` — config already says, from some layer, and
the slot is closed.

`null` is not "this slot is closed". It is "the rules found nothing", and a
project with no lockfile is the plain case: `"install": {"proposal": null}`,
because pando will not propose an install that can rewrite a lockfile and
there is no frozen one to propose. (One exception is pando's, not yours:
when the project *gitignores* its lockfile, pando proposes the plain
install itself — `npm install` — because the file it writes is one git
ignores, and says so in the `why`.) Whether anything should go there is then
a question about the project that only its developer — or a program reading
their README — can answer. `{"install": "make deps"}` is a legitimate
answer. `{"install": "npm install"}` is not, and no absence of a rule makes
it one: the guardrail is yours as much as pando's.

A `decided: true` proposal with no candidates and a `none_because` is a
different thing again — a rule deciding the answer is *none of them*, which
is a real answer and the opposite of `null`.

Four more facts that are not visible in the shape:

- **`prelude` is never proposed by `signals`** — it reads as
  `"proposal": null` on every project, answered or not. It is the one
  question about the laptop rather than the repository and it costs a
  shell probe, so it is raised at the moment an answer is needed, which
  means `init` can exit 3 on a slot `signals` showed you nothing for.
  `doctor`'s `runtime` section is the evidence: what the project pins,
  what `bash -lc` resolves here, and the exact line that would reconcile
  them, as the finding's `fix`. That line is the developer's to approve —
  it runs in front of every command pando spawns for them. Once they have,
  `{"prelude": "<the line>"}` is how it goes in, and `{"prelude": null}`
  is "this machine needs nothing". Never install a runtime to make the
  question go away.
- **Answering `processes` with the per-app form, or with an object of
  process tables, settles `dev_cmd` and `port_env` too** — every process
  gets its command and its port. Answers you
  sent for those two are then reported as unused, which is correct and not
  an error.
- **A process with no `ports` at all has answered the port question too,
  and may still need one.** One whose command runs a framework's server
  listens on that framework's own port in every worktree, and the second
  worktree started finds it taken. The job's `port_env` line then says
  "not given one" and gives the fix, which `pando doctor` gives as a
  problem as well: the answers command for `[dev]`, or the `ports` line
  for a named process. `ports = []` is a process that has none, and is
  left alone.
- **Two slots take no answer when nothing was proposed.** `services` is a
  set of the options, and with no options there is nothing to name — a
  service pando did not find is not a service it can run. `prelude` is
  checked against this machine before it is written, and that check only
  exists behind the proposal that raises the question; a line nothing
  verified, written machine-wide by a program, runs in front of every
  command pando spawns. Both report your value as unused instead.
- **A `needs_a_human: true` candidate is not one `--yes` may take**, but an
  answers file naming its exact text *is* an explicit answer and is
  accepted. Seeding a worktree's `.env` from a committed example is the one
  that behaves this way: it creates a file out of contents pando did not
  write, so nobody's flag gets to decide it. If you name it, you are
  answering for the developer — be sure they want it.

## 3. The ten questions, and who answers each

| Question | Who | Notes |
|---|---|---|
| `install` | rules, then you | a frozen install; the plain one where the project gitignores its lockfile; or silence — and silence is a slot you may fill from what the project's own docs say. Never a non-frozen one of your own |
| `version_files` | rules | which file pins the runtime |
| `prelude` | machine → human | only when the machine does not resolve the pin. `doctor` gives the exact line; the human decides whether to run it |
| `processes` | **human** | one process, or one per app: of a workspace, or of the app directories below a root with no manifest. When no option runs every app — one has no dev script — answer with an object of process tables (§4, §9) |
| `dev_cmd` | rules | ask only when several scripts are plausible dev servers. When nothing would run at all, `init` asks it with no options and `--yes` exits 3: answer with the project's own command, or `processes` with an object |
| `port_env` | rules | which variables carry the ports. The env example beats a framework convention |
| `services` | rules + **human** | which services get a private copy — see §5 for the mechanism |
| `schema_hook` | rules + **human** | the command that brings a fresh database to the schema. Always a question — it touches data — and only an isolated or namespaced start asks it. The hook runs on those starts only unless its entry says `on = "always"`; `null` answers "no" and writes it with `on = "never"` |
| `provision` | rules, mostly | which local files a worktree needs. Seeding from an example needs a human |
| `base` | rules, then **human** | the branch `new` forks from and `check` tests. Asked only when origin/HEAD is far behind the main checkout's branch; otherwise origin/HEAD, and nothing to answer. A check that fails with `kind: "base"` is the case for it: see step 5 |

## 4. Writing: `pando init --answers`, and nothing else

```bash
pando init --answers - --dry-run   # look first: the answers on stdin
pando init --answers -             # then write
```

The answers are one JSON object on stdin — never a file in the
developer's repository — keys are the slot names above, and:

- a **string** is the option whose `value` is exactly that text — **by
  value, never by index.** An index breaks the day a rule finds one more
  candidate, and picking by text is what brings everything else the option
  carries with it: the ports a command owns, the whole process table a
  workspace answer is, a service's env key, the hook entry.
- a **list of strings** is the set answer at the one question where
  `multi` is true, and the whole list at `version_files` and `provision`.
- **`null` is the only spelling of "none"**, and only where `allow_none`
  is true. An empty string is a usage error. `[]` means "none of them" at
  the set question and is a usage error anywhere else.
- a string that matches no option is a command of your own, wherever
  `allow_custom` is true. At `processes` it is **one** process.
- an **object** at `processes` is several processes of your own, one
  `[processes.<name>]` table per key: `{"api": {"cmd": "…", "cwd":
  "backend", "ports": ["api"]}, "web": {"cmd": "npm run dev", "cwd":
  "frontend", "ports": {"PORT": "web"}, "env": {"API_URL":
  "http://127.0.0.1:{port:api}"}}}`. It is how a project of several apps is
  answered when no option fits it — never one command that backgrounds
  them all, which loses each one's log, readiness and port. The
  `name: cmd in dir; …` text pando shows its own per-app option in names
  that option and is refused as a description of yours. `pando init
  --agent --reference json` has every key a table takes.
- a `port_env` of your own may name several variables, comma-separated:
  `"PORT, API_PORT"`, each owning the role its name says.
- **a slot with no proposal at all takes that same custom answer.** There
  are no options for it to match, so whatever you send is a command of your
  own: validated, refused if it would make the config unloadable, and
  written with the same `# answered: a program` beside it. `services` and
  `prelude` are the two exceptions, for the reasons in §2.

**Never edit `pando.toml`.** Not with `sed`, not with an editor, not "just
this once". Every answer that goes through `init --answers` is validated,
is refused if it would make the config unloadable, and lands with
`# answered: a program, <date>` beside it, so the developer can see at a
glance which lines a machine chose. A key you wrote by hand has none of
that and is indistinguishable from one they wrote themselves.

There is **one thing you cannot write**, and it matters:

```toml
# ~/.pando/config.toml
[isolation]
prefer = "native"     # or "compose"
```

That key decides whether a private service runs in a container or from an
engine installed on the machine, and it is **machine-wide**. Not
per-project, not per-worktree: one line, and it governs **every repository
this developer opens with pando**, including the ones you have never seen
and the ones they have not written yet. That is why it is not one of the
ten questions and why `--answers` has no key for it — an answer inferred
from the evidence in front of you would quietly settle a question about
projects that evidence says nothing about.

So when it comes up, you do exactly two things: quote pando's own
evidence line, and name the file and the key. pando prints the line
itself, and it is the better one because it says what it is deciding
*between*:

```
nobody has said which to prefer, so the project's own compose file wins —
set `[isolation] prefer = "native"` in ~/.pando/config.toml to run the
recipes instead
```

Then stop. Do not write the file, do not append to it, do not offer to.
See §5 for when this arises at all — which is less often than it sounds.

## 5. Private services: container, native, or neither

This is the one place where reading carefully beats reasoning quickly.

pando decides in this order, and publishes every step of it in
`doctor --json` under `services.isolation`:

1. **What the project declares.** A compose file with a service the
   *application depends on*; an address in the env example that names an
   engine pando has a recipe for.
2. **What this machine can run.** Whether docker answers, and whether each
   recipe's binaries exist in the shell pando actually spawns.
3. **The preference** — and only to break a tie.

### A compose file is not automatically a container option

**A compose file whose only service is built from the repository is not a
container option at all — it packages the application.** `build:` pointing
inside the project means that service *is* the app: running a private copy
of it per worktree would run a second copy of the thing pando is already
starting. pando filters those out, and if nothing is left it records the
negative so the question never comes back:

```jsonc
"services": { "proposal": { "decided": true, "candidates": [],
  "none_because": "docker-compose.yml declares only app, built from this repository" } }
```

Seeing that, you do nothing. There is no question, there is no mechanism to
choose, and asking the human "containers or native?" here would be asking
them to choose between two things that do not exist.

### A project can need services and describe none

The opposite shape: nothing in the repository says how to run anything, but
the env example addresses a database and a cache.

```jsonc
"env_example": [["DATABASE_URL", "postgres://user:pass@localhost:5432/appdb"],
                ["CACHE_URL", "redis://localhost:6379"]]
```

That is a declaration too — the application plainly needs a Postgres and a
Redis. With no compose file there is no container option, so pando proposes
its own recipes, and the engine comes from the **URL scheme or the default
port**, never from the key's name. `DATABASE_URL` says nothing about which
database; `postgres://` says everything.

Whether that proposal is `decided` depends on the machine: decided when the
engines are installed, a question when one is not — and the missing engine
is still offered, because the project plainly wants it. If it is a question
and you know the human wants those services, answer it by naming them.
**Never install an engine.** Not with brew, not with apt, not with a
container. Report what is missing and let the human decide.

### When both are real

Both a compose file with real dependencies *and* engines the machine has:
that is the one genuine tie, and pando breaks it towards the project's own
compose file, saying so:

```
nobody has said which to prefer, so the project's own compose file wins —
set `[isolation] prefer = "native"` in ~/.pando/config.toml to run the
recipes instead
```

Quote that line to the developer, once, and stop. You cannot write it,
`--answers` has no key for it, and it is not a fact about this repository
at all: `[isolation] prefer` is machine-wide, so the answer you would be
inferring from *this* project's evidence would govern every other project
on their machine too. That asymmetry is the whole reason the key is theirs
and not yours. Say which project raised it, and let them decide for all of
them.

If they have no docker and the engines are there, pando has already chosen
the recipes on the evidence and there is nothing to ask at all.

## 6. Verify, don't claim

A setup ends with proof, not with a summary.

1. `pando init --answers - --dry-run` — stdout is the file as it would
   be, with the provenance comments. Show it to the developer.
2. `pando init --answers -` — the real write.
3. `pando doctor` — and **report what it said**, including the notes.
   Exit 0 means nothing found will break a command; exit 1 means something
   will, and every problem is printed with its own fix.

If doctor exits 1, read the finding before touching anything. Some are not
yours to fix:

- **a runtime the machine does not resolve.** The fix is a `prelude` line,
  and `doctor` prints the exact one. It is about their laptop: offer it,
  do not run an installer.
- **a non-frozen install in a config somebody wrote by hand.** pando never
  *proposes* one — but it does not overrule one either. What
  `project.install` says is what `pando new` runs, because a developer who
  wrote that line has answered the question and a tool that silently
  refuses their answer is worse than one that runs it and says so. So
  doctor calls it a **problem** rather than a note, and prints the frozen
  form as its fix; and if the command really does rewrite a lockfile, the
  `new` that ran it says *that* out loud too, naming the worktree the file
  changed in. Show them the finding, let them change it, and do not "fix"
  it by loosening anything. Never write one yourself — see §8.

Do not claim a project starts unless you started it: `pando check` starts
it, and so does `pando start`. If you ran neither, say so.

### Prove it by running, when the developer agrees

On a first run, `pando check` is this proof and the pasted prompt is the
agreement: see the first-run section. What follows is for proving more
than a check does, such as an isolated start.

Rules read files; only a start meets the project. A config every slot of
which is `decided` can still describe an environment that does not run —
an app that reads its port from a variable nobody set, a gateway pointed
at a sibling's default port, a database with no schema. The only way to
find those is to start it, so ask once whether you may, and then:

```bash
pando new pando-setup-check              # a scratch worktree, on its own branch
pando start pando-setup-check --isolated --wait
pando status pando-setup-check --json    # every process, its phase, its ports
pando stop pando-setup-check
```

Read what came back, not what you expected:

- **every process `running`, `observed_ports` equal to `ports`** — it
  works. Say so, with the URL `status` gave.
- **a `reason` that says `listening on … instead`** — the process runs,
  but the port pando assigned never reached it. `observed_ports` shows the
  port it chose. Which variable it reads is in the project's own code or
  its env example; if that variable is one pando did not set, that is the
  finding.
- **`command not found`** — dependencies are not installed in the
  worktree. With no install configured, that is the no-lockfile case in §2:
  report it, and never loosen the install to make it pass.
- **`connection refused`, or an app log about a missing database or
  table** — a service is not running, or the schema step is missing.
  That is the `services` and `schema_hook` questions, not a retry.

A fix that is one of the ten answers goes through `init --answers`, a
variable one process needs included (§9), and you start the scratch
worktree again. A fix that is not — an app whose own config pins a port —
is the developer's: name the process, what it did, and the line that
would fix it, and write nothing. Stop the scratch worktree when you are
done and tell the developer its name; removing it is theirs, as removing
anything is.

## 7. What pando records about you

Every answer you supply to a question the rules could not decide is
appended to `~/.pando/projects/<id>/decisions.jsonl` with the evidence you
had, and a later line records it if the developer changes it. You do not
write that file; pando does.

This is deliberate and it is in your interest to make it accurate. It is
the corpus that turns a question the rules could not answer into a rule —
which is what every developer without an agent gets. So: answer from the
published evidence, or refuse. **An answer you guessed pollutes a corpus
somebody will train a rule on.** A question asked is cheap; a wrong config
written confidently is not.

There is exactly one thing the log cannot hold, and you are the only one
who can put it on the record: **`[isolation] prefer`.** It is a question
the rules could not settle — pando says so in as many words, in the line
§4 quotes — and it is one no program may write, so it never becomes an
`answer` line and never becomes an `override` line either. It is a gap in
the corpus, and a gap nobody names is a gap nobody fixes.

So when a project reaches that tie, **say so in your report**: that pando
found both mechanisms, that it kept the project's compose file because
nobody had said otherwise, and that the developer is the only one who can
change it. One sentence, in the words pando used. That sentence is what
the decisions log would have held.

## 8. Guardrails

Absolute. None of these has an exception worth taking.

- **Never write into the developer's repository.** Not a config file, not a
  cache, not a marker, not a `.env`, not a `CLAUDE.md` or `AGENTS.md`. pando's own promise is "not a byte",
  and a developer will not distinguish your plugin from the tool. If a
  project cannot run without an untracked file, say which file and why, and
  let them create it in their own project.
- **Never write or run a non-frozen install.** `npm ci`, not
  `npm install`. `pnpm install --frozen-lockfile`, not `pnpm install`.
  This one is a rule about *you*, not a promise about pando: pando will
  honour a line a developer wrote there, and it should — but a line you
  wrote has no developer behind it. No lockfile means no *frozen* install
  exists, so pando proposes none — unless the lockfile is gitignored, where
  pando proposes the plain install itself and you take its proposal; if the project installs by a step of its
  own that you have actually read, naming that step is an answer. Guessing
  one, or reaching for the non-frozen form because the slot looked empty,
  is not.
- **Never run a mutating pando command against a repository the developer
  did not point you at.** `new`, `start`, `stop`, `restart`, `rm`,
  `share`, `unshare`, `init`, `check` are mutating. `ls`, `status`,
  `path`, `logs`, `doctor`, `signals` and `init --agent` are not; `open`
  changes nothing but launches a browser, which is the developer's to ask
  for. Check the working directory is the repository they asked about.
- **Never install a toolchain or a database engine.** Not node, not a
  version manager, not Postgres, not docker. Report what is missing, with
  what pando said about it.
- **Never edit `pando.toml`, and never edit a framework config file.** If
  an app hardcodes its port, that is a one-line change in *their* project
  and their decision to make.
- **Never take a `needs_a_human` option without a human.** If nobody is
  there to ask, exit and say which question is open.
- **Never start a worktree namespaced unless the developer asked for it.**
  It writes into their own database server. It is experimental, it needs
  a grant only they can give, and `rm` of a namespaced worktree drops its
  database.

## 9. The process table: what a `processes` object writes

A process with an `env` of its own, a readiness wait, a port another
process has to know: each is a key of a process table, and an object at
`processes` writes the tables whole through `pando init --answers -`,
with `--replace` when the slot is already answered. Each key of the
object is a process, and each value takes the keys below: `cmd`, `cwd`,
`ports`, `env` and `ready`. The first table below, as an answer, is
`{"processes": {"api": {"cmd": "…", "cwd": "backend", "ports": ["api"]},
…}}`, and every process the project runs is one key of it. `pando
check` proves them.

The one case you write nothing is a committed `pando.toml`, or the
machine-wide config, that declares processes: pando never writes those
files, so `--replace` refuses there. Give the developer the exact lines
to change in the file `pando doctor` names instead. The shape, as TOML:

```toml
[processes.api]                       # one table per process; [dev] is one called dev
cmd = "uv run uvicorn app.main:app --port {port:api}"
cwd = "backend"                       # relative to the worktree; the root when left out
ports = ["api"]                       # the roles it owns: {port:api} is the api role's port

[processes.web]
cmd = "npm run dev"
cwd = "frontend"
ports = { PORT = "web" }              # a map puts the role's port in that variable
env = { VITE_API_URL = "http://127.0.0.1:{port:api}" }  # another process's port, by role
ready = { timeout_s = 90 }            # how long its first start may take

[processes.worker]
cmd = "uv run python -m app.worker"
cwd = "backend"
ports = []                            # no port: ready once it stays up

[processes.mobile]
cmd = "npx expo start"
cwd = "apps/mobile"
ports = { RCT_METRO_PORT = "metro" }  # Expo reads this, never PORT
env = { EXPO_PUBLIC_API_URL = "http://127.0.0.1:{port:api}" }
page = false                          # no browser opens it; Metro's is the default
```

- **`{port:<role>}`** is the port pando gave that role in this worktree,
  in `cmd` and in `env` alike, whichever process owns the role. It is
  how a frontend finds its own worktree's backend: `VITE_API_URL`,
  `NEXT_PUBLIC_API_URL`, `EXPO_PUBLIC_API_BASE_URL`, whatever the app
  reads. A variable a bundler inlines, such as `EXPO_PUBLIC_*` or
  `VITE_*`, has to reach it this way, as the process's environment.
- **The URL** `status` and `open` give, and the page `check` asks for,
  is the `web` role's, else the first role of the first process by
  name, among the processes that serve a page. The other roles need no
  page: a check asks none of them for one, and `http_status: null`
  there is right. A process whose port no browser opens says `page =
  false`, and Expo's Metro is one without saying it; a role nobody opens
  in a browser — an API, a mobile bundler — still wants its own name
  rather than `web`. A worktree with no page has no URL: `open` opens
  its app instead, and `share` still publishes its port.
- **A phone cannot reach `127.0.0.1`.** Every URL pando gives, and every
  `{port:<role>}` address written as `127.0.0.1`, is this machine's. An
  app on a physical device needs the machine's LAN address instead: in
  its backend URL, and, for Expo, in `REACT_NATIVE_PACKAGER_HOSTNAME` on
  the bundler's process. Which address that is belongs to the
  developer's network, so tell them the variable and where it goes;
  never guess the address.
- **An Expo app is opened by a link, not a page.** pando runs Metro
  with no terminal, so its "press i" is gone. The iOS simulator shares
  `127.0.0.1`: `pando open <name>` opens the app of a worktree that
  serves no page (`--app` beside a page) on the booted simulator, a
  connected Android device or emulator, or a simulator it starts.
  `pando status <name>` prints, under a running Metro, the commands it
  runs, and `--json` carries them as the process's `app`.
  That is Expo Go's `exp://127.0.0.1:<port>`, or, for an app that
  depends on `expo-dev-client`, its development build's
  `exp+<slug>://expo-development-client/?url=http%3A%2F%2F127.0.0.1%3A<port>`,
  `<slug>` being `expo.slug` lowercased (never `expo.scheme`), filled in
  from `app.json`, or from a `slug: "…"` literal in `app.config.*`,
  which pando reads and never runs, or from its build on a booted
  simulator. Only an app whose config computes its slug, with no build
  installed, keeps `<slug>` for you to fill; `pando open` then runs
  nothing and prints the commands. On a device, the same links take the
  LAN address above. A development build made for another Expo SDK than
  the worktree's crashes on this worktree's JavaScript (`Property
  'MessageQueue' doesn't exist`, say): `status` reads the build off the
  booted simulator and says so, with the command that replaces it
  (`app.installed` in `--json`), and `pando open` gives that command
  rather than opening the app in it. A branch that changes the app's native
  code (`ios/`, `android/`, a local module's, or `app.json`/`app.config.*`)
  needs a build of its own: `status` says so under the process, with the
  `npx expo run:ios --port <port>` and `npx expo run:android --port
  <port>` that make one on Metro's port, reusing the Metro pando runs.

---

# Worked examples

Real output, from real runs. Your project will differ; the shape of the
reasoning will not.

## A. A single-app repository the rules fully understand

`signals` (abridged) — every slot either decided or silent:

```
install     decided=True   ['npm ci']            why: package-lock.json
dev_cmd     decided=True   ['npm run dev']       why: package.json scripts.dev
port_env    decided=True   ['PORT']              why: the Node convention
provision   decided=True   ['.env']              why: gitignored and present in the main checkout
services    proposal null
schema_hook proposal null
```

**Questions to ask: none.** There is no answers file to write at all.

```bash
pando init          # no --yes needed: nothing is undecided
pando doctor
```

`init` prints one line per slot saying what it took and why. If you pass
`--yes` here you have added nothing and told the config a flag decided
something the rules did. Do not.

## B. A workspace monorepo with several apps

Several apps under `apps/`, workspaces declared in `package.json`, and —
in this shape — no lockfile at all, and no lockfile name in `.gitignore`:

```
install     proposal null                        ← no lockfile, so no frozen install exists
processes   decided=False
              1) 'api: npm run dev in apps/api; web: npm run dev in apps/web'
                    why: a dev script in each of 2 workspace apps
              2) 'npm run dev'
                    why: package.json scripts.dev
port_env    decided=False  ['WEB_PORT, API_PORT', 'WEB_PORT', 'API_PORT']
```

`processes` is the human question — which apps they want running — and it
is the *only* one, because taking the per-app form settles the dev command
and the ports for every app with it.

Ask once, with both options and their `why`. Then:

```json
{ "processes": "api: npm run dev in apps/api; web: npm run dev in apps/web" }
```

Note what you did **not** do. You did not invent an `install`: the slot has
no proposal because there is no lockfile to freeze against, and `npm
install` is the one command the guardrail forbids. The slot is answerable —
if the repository's own README said it is installed with `make deps`, then
`{"install": "make deps"}` answers a question pando had nothing to offer
for, and the decisions log records that the rules offered nothing. A guess
in that slot is worse than silence. And you did not answer `port_env`: the
`processes` answer settles it, and pando reports your value as unused if
you send it, which is noise in front of the thing that matters.

## C. A compose file that only packages the app

```jsonc
"compose": [ { "file": "docker-compose.yml", "services": ["app"], "extends": [], "include": false } ],
"services": { "proposal": { "decided": true, "candidates": [],
  "none_because": "docker-compose.yml declares only app, built from this repository" } }
```

There is a compose file, and there is still **no container question**. The
one service is built from the repository: it *is* the application. pando
records the negative — `include = []` — so the question never comes back on
an isolated start.

**Questions to ask: none.** Not about services, not about mechanisms. If
you ask the developer "should I run your services in containers?" here, you
have asked them about something that does not exist, and you have spent the
entire question budget doing it.

Afterwards, `doctor` says so in its own words, which is what you report:

```
services
  isolation     nothing here to run a private copy of
                `[[services]]` names the compose file docker-compose.yml, for none of its services
  compose       docker-compose.yml
```

## D. Services with no manifest — where the machine decides

```
services  decided=<depends on this machine>  mechanism=native
    * postgres | why: .env.example DATABASE_URL=postgres://…; postgres is on this machine
    * redis    | why: .env.example CACHE_URL=redis://localhost:6379
```

No compose file, so there is no container option and the preference never
comes into it. The app's own addresses name the engines. On a machine that
has them, this is `decided` and you do nothing; on one that does not, it is
a question whose options are still those two engines, and the `why` says
what is missing.

If you answer it — `{"services": ["postgres", "redis"]}` — say plainly in
your report that the engines are not installed and that `start --isolated`
will fail until they are. Then stop. Installing Postgres is not your
decision to make on somebody's laptop.

---

# Operating, day to day

Setup happens once. This is the part you do every day, and it is a much
smaller contract.

**Never parse human-readable output.** Every command below has a `--json`
form or an exit code that answers the question. The text is for people and
it changes.

```bash
pando status --json              # what is running, on which ports, with which URL
pando status <name> --json       # one worktree
pando ls --json                  # the worktrees and their git state
pando logs <name> --json         # a log, one JSON object per line
pando logs <name> --source <s> --json
pando new <branch>               # create a worktree
pando start <name>               # start it; returns once spawned
pando start <name> --wait        # …and block until it is ready
pando start <name> --isolated    # …with private copies of its services
pando start <name> --namespaced  # …experimental: its own database and slot in the main checkout's servers
pando stop <name>
pando share <name>               # publish it at a public URL
pando unshare <name>
```

`<name>` is the worktree's branch (`feat/one`) or its directory
(`feat+one`); both name the same worktree, and the JSON always says
`feat+one`. A name pando does not know is exit 1, with the likely names
on stderr. A string that is one worktree's directory and another's branch
is exit 2: name it the other way. Inside a worktree most verbs take no
name, but always pass one. Without it, `stop` stops only the worktree the
shell is in, or **every** worktree when the shell is in none of them.

The main checkout runs too: `pando start <its branch>` or its directory's
name, and it is the first entry in `ls --json` with `"main": true`. pando
runs only its processes there — no install, no hooks, shared services
only — so it must already be set up; `--isolated`, `--namespaced` and
`rm` refuse it.

`logs` without `--source` reads `dev`. A worktree with no `dev` log and
several processes gets them all merged, and each `--json` line then carries
a `source` key. Pass `--source` when you know which log you want.

### The exit-code discipline

| Code | You |
|---|---|
| `0` | carry on |
| `1` | read stderr. It is one sentence. **Do not retry** — a failure that repeats is a failure that repeats |
| `1` from `pando check` | the setup is wrong, or the machine is, or the commit it tested: fix it before trying again. The first-run section says which is yours |
| `2` | you asked wrongly: a bad flag, or an answers file naming a question pando does not ask. Fix the request |
| `3` | **a question is unanswered, and it is on stderr** with its options. Answer it through `init --answers`, or put it to the human. Never retry unchanged, and never add `--yes` to make it go away |

Two questions come only from a namespaced start, and `init --answers`
cannot answer either:

- **`login`** — which login may create and drop the worktree's own
  databases, when the main checkout's env files carry none. It is a
  password: never guess one, never put one in an answers file or a
  command line. Tell the human, who writes `[namespaced.<service>]` with
  `user` and `password` in the file stderr names, or runs the start on a
  terminal and types it.
- **`free_slot`** — which stopped worktree gives up its Redis slot when
  every slot is held. Its answer empties that slot. Never answer it:
  report the list to the human, who can answer it on a terminal or `rm`
  a worktree they no longer need.

A namespaced start that says a service stays shared because "the app
reads no slot setting" or "nothing … names its database", when the app
does read one under a name pando did not guess (one `REDIS_DB` that
several Redis roles share), is fixed in config, not code:
`[namespaced.<service>] db_env = ["REDIS_DB"]` in `pando.toml`. It is no
secret, so the committed file may carry it.

A namespaced start that stops with a `GRANT …` or `ALTER ROLE …`
statement on stderr means the app's login may not make the worktree's
database. Report the statement; running it is the human's, as an
administrator of their own server. A start that says `psql`, `mariadb` or
`redis-cli` is not on PATH names what to install: report that too, since
a server in Docker leaves the host with no client.

`--yes` is not a way past exit 3. It takes the rules' own preferred option,
which is a decision you are making on the developer's behalf with no
evidence you did not already have. Use it only when the developer asked for
it.

A `prelude` you supplied that fails its probe on this machine is exit
**2**, like any other bad value in the answers file, and nothing is
written. stderr says which probe failed. Fix the value, not the machine.

`start` and `restart` return as soon as everything is spawned when their
stderr is not a terminal, which is how you run them. Exit 0 then means
"spawned", not "ready". Add `--wait` when the next thing you do needs the
server up: it blocks until every process is ready, and a failure exits 1
with the process, its reason and the last lines of its log on stderr.

### Reading a failure

```bash
pando status <name> --json
```

- a process with `"phase": "failed"` carries `"reason"` — the sentence to
  show the human — and `"log"`, the file to read with
  `pando logs <name> --source <process>`.
- `"up": false` on a service means nothing is answering on its port.
- `"logging": false` with `"up": true` means the log pump died: the log tab
  has stopped filling. `pando start` or `pando restart` puts it back.
- `"observed_ports"` is what is really listening, against `"ports"`, which
  is what pando assigned.

### What not to do while operating

- Do not `start` a worktree the developer did not name.
- Do not `rm` anything. Removing a worktree is theirs.
- Do not `share` without being asked: it publishes their machine on the
  public internet.
- Do not restart something to "see if it works" while they are using it.
- Do not read log files from disk. `pando logs` handles truncation,
  partial lines and levels; opening the file gets you none of that.
