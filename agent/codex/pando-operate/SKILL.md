---
name: pando-operate
description: Run a project's worktrees day to day with pando — start, stop, check status, read logs, share a URL, diagnose a failure. Use when someone asks what is running, to start or stop a branch's dev server, to see a worktree's logs, or to publish one at a public URL.
---

# Operate pando

**The procedure is `brief.md`, beside this file, section "Operating, day
to day".** Read it. This file is glue and contains none of the reasoning;
`json.md`, also beside this file, is the contract for the shapes. (In a
pando checkout they are `agent/brief.md` and `agent/json.md`.) Or run
`pando init --agent --reference brief`, which prints the brief.

## The two rules that matter most

**Never parse human-readable output.** `status --json`, `ls --json`,
`logs --json`. The text forms are for people and they change.

**Read the exit code first**: `0` carry on · `1` read stderr and do not
retry · `2` you asked wrongly, fix the request · `3` **a question is
unanswered and it is on stderr**. Answer a `3` with
`pando init --answers -`, the answers on stdin and never in a file in the
repository, or hand the question to the developer.
Never retry unchanged, and never reach for `--yes` to make a `3` go away:
that is you deciding on their behalf with no evidence you did not already
have.

## Working set

```bash
pando status --json          # what is running, on which ports, with which URL
pando ls --json              # the worktrees and their git state
pando ls --all --json        # every project on this machine, from any directory
pando logs <name> --json     # one JSON object per line
pando new <branch>
pando start <name> --wait    # blocks until ready; without it, exit 0 is only "spawned"
pando stop <name>            # always name it: bare `stop` outside a worktree stops all
pando share <name>           # publishes on the public internet — only when asked
pando unshare <name>
```

`<name>` is the branch (`feat/one`) or the directory (`feat+one`).
A pando block in your memory (`~/.codex/AGENTS.md`) is this, for one project;
`pando init --agent --reference memory` prints it again.

## Diagnosing

`pando status <name> --json`: a process with `"phase": "failed"` carries
the `"reason"` to show and the log to read; `"up": false` on a service
means nothing answers on its port. The brief's "Reading a failure" has the
rest, and its "What not to do while operating" is a list to take
literally.
