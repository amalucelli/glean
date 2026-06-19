# glean

Incremental change tracking for skills. Run a skill cleanup or quality pass only over what changed, the way a linter does.

## Why it exists

A skill can act as a linter for an AI agent — a loop that strips slop or simplifies code every tick. But a linter scopes itself to what changed; a skill run naively has no such memory, so every tick re-processes the whole working-tree diff and redoes files it already cleaned.

`glean` gives a skill that memory. The skill asks "what changed since I last ran?", touches only that, and records what it processed. Each skill keeps its own baseline, so several passes watch the same tree at once without redoing each other's work.

## Why not just git?

`git` tracks changes against commits; `glean` tracks how far each consumer has gotten since it last ran. That per-consumer cursor is all `glean` adds — it leans on git for the diffing and does none of its own. It can't live in commits: a loop ticks many times between them, over uncommitted work that parallel agents are still editing, and each skill needs its own place in that stream. `glean` keeps that cursor per consumer, anchored on file content, inside `.git` and never in history.

## Claude Code

`glean` ships a companion skill, `/glean`, that wraps any other skill to run incrementally. It scopes `<skill>` to what changed since it last ran, invokes it, and records the result. Point a loop at it:

```
/loop 2m /glean slop         # slop only new edits, every 5 minutes
/loop 5m /glean simplify     # works with built-in skills too
```

The wrapped skill must accept file paths as arguments (most cleanup skills do). Install the companion skill where Claude Code looks for it:

```sh
mkdir -p ~/.claude/skills
cp -r .claude/skills/glean ~/.claude/skills/
```

The skill is at [`.claude/skills/glean/SKILL.md`](.claude/skills/glean/SKILL.md).

## Install

```sh
cargo install --path . --root ~/.local   # → ~/.local/bin/glean
```

Install under any prefix on your `PATH`. The skills that call `glean` need to resolve it by name.

## Usage

```
glean list   [--as <consumer>]            files changed since this consumer's last mark
glean mark   [--as <consumer>] [paths…]   record files as processed (no paths = the whole changed set)
glean status [--as <consumer>]            counts; with no --as, summarize every consumer
glean reset  [--as <consumer>] [--all]    forget a baseline to force a full re-sweep
```

A *consumer* is one skill (or tool) with its own baseline. `--as` names it; it defaults to `default`.

```sh
$ glean list --as slop        # what has changed since slop last ran?
src/auth.rs
src/db.rs

# ... slop processes those two files ...

$ glean mark --as slop src/auth.rs src/db.rs   # record them as done

$ glean list --as slop         # nothing new since
$
```

## Scope vs cursor

"What changed" hides two jobs, and `glean` owns one:

- **Scope** — what's in play for a run: uncommitted edits, or a whole branch against a ref. git answers this directly (`git diff HEAD`, `git diff <branch>`), and a skill that lints branch changes works it out itself.
- **Cursor** — has this consumer already processed these exact bytes? The per-consumer baseline — all `glean` adds.

`glean` is for the uncommitted loop, where the working tree churns between ticks and the cursor skips files already cleaned. Committed work is the opposite: the file set (`git diff <ref>`) is static and swept once before a PR, so the cursor adds nothing. That work is git's to scope, run through the skill's own branch mode (`/slop main`) — `glean` has no base flag, by design.

## A skill on a loop

This is the case it's built for: point a recurring loop at a skill and it only ever touches new edits. In Claude Code the skill calls `glean` itself, so you run `/loop 5m /glean /slop`. It's the same wiring as a plain shell loop around any tool:

```sh
while true; do
  files=$(glean list --as slop)
  if [ -n "$files" ]; then
    run-the-pass $files          # a skill, a linter, a formatter — anything that takes paths
    glean mark --as slop $files
  fi
  sleep 300
done
```

Mark only what succeeded. If a pass fails on a file, leave it out of `mark` and it comes back next tick.

## Multiple skills, one tree

Each consumer has its own baseline, so independent skills (or parallel agent loops) watch the same repo at once:

```sh
glean list --as slop        # code smells — same tree, independent baseline
glean list --as simplify    # structure
glean list --as check       # type checking
```

When a file changes, every skill that hasn't seen those exact bytes picks it up, so each pass also re-checks files the others edited, through its own lens. A pass that finds nothing to do is a cheap no-op.

## How it works

- **Content-based, not mtime.** A file counts as changed when its bytes hash differently from the recorded baseline, so it survives rebases, branch switches, and formatters that bump mtimes without touching content.
- **Scales with the diff, not the repo.** The candidate set is the whole uncommitted working set — staged, unstaged, and untracked (`git diff HEAD` + `git ls-files --others`) — so a giant monorepo with a handful of edits costs a handful of reads. Nothing waits on a commit, which is what lets a loop keep working while parallel agents edit the same tree. On huge trees, enable git's `core.fsmonitor` and `core.untrackedCache` and `glean` inherits the speedup.
- **State in `.git`.** Baselines live in `.git/glean/<consumer>.json`: never committed, scoped to the repo, separate per worktree. Git can't track its own dir, so the state can't leak, and because `glean` reads the tree through `git diff` and `git ls-files` (which both ignore `.git/`), it never sees its own state. The state persists across runs, which is what makes it incremental, and dies with the repo. `glean reset --all` wipes a repo's baselines; different worktrees get their own automatically.
- **No coordination.** Each consumer is the sole writer of its own file, so independent consumers run against the same tree without locks.
- **Skips noise.** Lockfiles (`Cargo.lock`, `package-lock.json`, `go.sum`, …) and binary files never appear in `list`.
