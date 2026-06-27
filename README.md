# glean

Incremental change tracking for skills. Run a skill cleanup or quality pass only over what changed, the way a linter does.

## Why it exists

A skill can act as a linter for an AI agent — a loop that strips slop or simplifies code every tick. But a linter scopes itself to what changed; a skill run naively has no such memory, so every tick re-processes the whole working-tree diff and redoes files it already cleaned.

`glean` gives a skill that memory. The skill asks "what changed since I last ran?", touches only that, and records what it processed. Each skill keeps its own baseline, so several passes watch the same tree at once without redoing each other's work.

## Why not just git?

`git` tracks changes against commits; `glean` tracks how far each consumer has gotten since it last ran. That per-consumer cursor is all `glean` adds — it leans on git for the diffing and does none of its own. It can't live in commits: a loop ticks many times between them, over uncommitted work that parallel agents are still editing, and each skill needs its own place in that stream. `glean` keeps that cursor per consumer, anchored on file content, inside `.git` and never in history.

## Install

glean is a single Rust binary. Both the `/glean` skill and the MCP server resolve it by name on `PATH`:

```sh
cargo install --path . --root ~/.local   # → ~/.local/bin/glean
```

Install under any prefix on your `PATH`.

## Claude Code

glean ships as a Claude Code plugin — the `/glean` skill and the `glean` MCP server. Install it from this repo:

```
/plugin marketplace add amalucelli/glean
/plugin install glean@glean
```

The plugin's skill and MCP server both call the `glean` binary by name, so install that too (see [Install](#install)).

### `/glean` — a skill on a loop

The `/glean` skill wraps any other skill to run incrementally: it scopes `<skill>` to what changed since it last ran, invokes it, and records the result. Point a loop at it:

```
/loop 2m /glean slop         # slop only new edits, every 2 minutes
/loop 5m /glean simplify     # works with built-in skills too
```

The skill runs in a forked subagent (`context: fork`), so a long loop's main context grows by only a one-line result per tick instead of every file the pass reads and edits; to watch or steer a pass, run the skill directly. The wrapped skill must accept file paths as arguments (most cleanup skills do). (Forking from a plugin needs Claude Code v2.1.101+; older versions run it inline.)

Without the plugin, copy the skill where Claude Code looks for it:

```sh
cp -r skills/glean ~/.claude/skills/
```

## Usage

```
glean list   [--as <consumer>] [-z] [-q] [--json]        files changed since this consumer's last mark
glean mark   [--as <consumer>] [--stdin] [-z] [paths…]   record files as processed (no paths = the whole changed set)
glean status [--as <consumer>] [--json]                  counts; with no --as, summarize every consumer
glean reset  [--as <consumer>] [--all]                   forget a baseline to force a full re-sweep
glean run    [--as <consumer>] [--each] -- <cmd>…        run <cmd> on the changed files, mark on success
glean mcp                                                serve the change-set as MCP tools over stdio
```

A *consumer* is one skill (or tool) with its own baseline. `--as` names it; it defaults to `default`.

`-z` writes NUL-separated paths (safe for paths with spaces or newlines) and `--json` writes a JSON array, in place of the default newline-separated list. `-q` prints nothing and exits 0 if anything changed, 1 if not — a cheap gate for a loop. `mark --stdin` reads paths from stdin (`-z` for NUL-separated input), so a list pipes straight back into a mark:

```sh
$ glean list --as slop        # what has changed since slop last ran?
src/auth.rs
src/db.rs

# ... slop processes those two files ...

$ glean mark --as slop src/auth.rs src/db.rs   # record them as done

$ glean list --as slop         # nothing new since
$

$ glean list -z --as slop | glean mark --stdin -z --as slop   # or round-trip the whole set safely
```

## Run a tool incrementally

`glean run` packages the list → run → mark loop into one command, so any tool that takes file paths runs only over what changed — no shell glue, no agent. The consumer name namespaces the cursor:

```
tool <args> <files>   →   glean run --as tool -- tool <args>
```

It snapshots the changed set, runs the command on it, and marks those files only if the command succeeds — so a failing pass leaves its files for the next tick. `run` exits with the command's status, which lets it drop into CI: cache `.git/glean/` between runs and a step only re-checks files that changed since they last passed.

**Formatters** rewrite in place, so batch (the default) is right — glean records the formatted bytes as the new baseline, and they drop out until edited again:

```sh
glean run --as gofmt -- gofmt -w
```

**Linters** report per file, so use `--each`: glean runs the command once per file and marks only the files that pass. A file with errors keeps coming back until it's fixed while the clean ones settle, instead of one bad file pinning the whole batch:

```sh
glean run --each --as eslint -- eslint
```

Under the hood that's the same wiring as a plain shell loop, which is all you need to run a tool incrementally outside Claude Code:

```sh
while true; do
  glean run --each --as eslint -- eslint   # mark only what passed; failures return next tick
  sleep 300
done
```

A whole-project tool that can't scope to file paths (`cargo clippy`, `tsc`) doesn't fit `run`; gate it on the cheap `-q` check instead, so it only runs when something changed:

```sh
glean list -q --as clippy && cargo clippy && glean mark --as clippy
```

## MCP

`glean mcp` serves the change-set as MCP tools over stdio, for agents that speak MCP instead of shelling out. The plugin registers it automatically (via `.mcp.json`); for another client, run `glean mcp` as the server command. Three tools:

- `glean_list` — paths changed since a consumer last marked them (optional `consumer`, defaults to `default`).
- `glean_mark` — record paths as processed; omit `paths` to mark the whole changed set.
- `glean_status` — tracked and changed counts per consumer (all consumers if none given).

`run` is deliberately not exposed: the MCP surface reads and advances the cursor, it doesn't execute commands.

## Scope vs cursor

"What changed" hides two jobs, and `glean` owns one:

- **Scope** — what's in play for a run: uncommitted edits, or a whole branch against a ref. git answers this directly (`git diff HEAD`, `git diff <branch>`), and a skill that lints branch changes works it out itself.
- **Cursor** — has this consumer already processed these exact bytes? The per-consumer baseline — all `glean` adds.

`glean` is for the uncommitted loop, where the working tree churns between ticks and the cursor skips files already cleaned. Committed work is the opposite: the file set (`git diff <ref>`) is static and swept once before a PR, so the cursor adds nothing. That work is git's to scope, run through the skill's own branch mode (`/slop main`) — `glean` has no base flag, by design.

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
