<img width="170" alt="glean" src="https://raw.githubusercontent.com/amalucelli/glean/main/assets/logo.png" />

# glean

`glean` records which files a tool has already processed, so the next run only sees what changed since. A skill on a loop scopes itself to new edits instead of re-sweeping the whole diff every tick.

- **Per-consumer baselines.** Each skill or tool tracks its own position, so parallel passes share a tree without redoing each other's work.
- **Content-hashed.** A file is changed when its bytes differ from the baseline — surviving rebases, branch switches, and rewrites that only bump mtimes.
- **Scales with the diff.** The candidate set is the uncommitted working set, so a monorepo with three edits costs three reads.
- **Out of history.** Baselines live in `.git/glean/`, never committed, separate per worktree.

## Install

A single Rust binary. The `/glean` skill and the MCP server both resolve it by name on `PATH`:

```sh
brew install --cask amalucelli/tap/glean
```

Or from source:

```sh
cargo install --path . --root ~/.local   # → ~/.local/bin/glean
```

## Usage

```
glean <command> [--as <consumer>] [options]

  list   [-z] [-q] [--json]        files changed since this consumer's last mark
  mark   [--stdin] [-z] [paths…]   record files as processed; with no paths, the whole changed set
  status [--json]                  tracked and changed counts; with no --as, every consumer
  reset  [--all]                   forget a baseline to force a full re-sweep
  run    [--each] -- <cmd>…        run <cmd> on the changed files, marking them when it succeeds
  mcp                              serve the change-set as MCP tools over stdio

  -v, --version                    print the version
  -h, --help                       print help; per command with glean <command> --help
```

A *consumer* is one skill or tool with its own baseline. `--as` names it; it defaults to `default`, and it reads the same before or after the subcommand.

```sh
$ glean list --as slop        # what has changed since slop last ran?
src/auth.rs
src/db.rs

# ... slop processes those two files ...

$ glean mark --as slop src/auth.rs src/db.rs   # record them as done

$ glean list --as slop         # nothing new since
$
```

`-z` writes NUL-separated paths, safe for paths with spaces or newlines; `--json` writes a JSON array. `-q` prints nothing and exits 0 if anything changed, 1 if not. `mark --stdin` reads paths from stdin:

```sh
glean list -z --as slop | glean mark --stdin -z --as slop
```

The notices (`marked 6 files`, `removed …`, errors) go to stderr, so they stay out of a pipe carrying the change set. On a terminal they and `status`'s counts are coloured; `list` never is, since its output is data. Colour is dropped when the stream is not a terminal, or when `NO_COLOR` is set.

Consumers are independent. When a file changes, every consumer that has not seen those exact bytes picks it up, including files the other passes edited:

```sh
glean list --as slop        # code smells
glean list --as simplify    # structure
glean list --as check       # type checking
```

## Claude Code

glean ships as a Claude Code plugin — the `/glean` skill and the `glean` MCP server:

```
/plugin marketplace add amalucelli/glean
/plugin install glean@glean
```

Both call the `glean` binary by name, so install that too. Without the plugin, copy the skill where Claude Code looks for it:

```sh
cp -r skills/glean ~/.claude/skills/
```

`/glean <skill>` scopes `<skill>` to what changed since it last ran, invokes it, and marks the result. Point a loop at it:

```
/loop 2m /glean slop         # slop only new edits, every 2 minutes
/loop 5m /glean simplify     # works with built-in skills too
```

It runs in a forked subagent, so the loop's main context grows by one line per tick rather than by every file the pass reads. Run the skill directly to watch or steer it. The wrapped skill must accept file paths as arguments. Forking from a plugin needs Claude Code v2.1.101+; older versions run it inline.

## Run a tool incrementally

`glean run` packages list → run → mark into one command:

```
tool <args> <files>   →   glean run --as tool -- tool <args>
```

It snapshots the changed set, runs the command on it, and marks those files only if the command succeeds — a failing pass leaves its files for the next tick. `run` exits with the command's status. In CI, cache `.git/glean/` between runs and a step only re-checks files that changed since they last passed.

**Formatters** rewrite in place, so batch (the default) fits: glean records the formatted bytes as the new baseline.

```sh
glean run --as gofmt -- gofmt -w
```

**Linters** report per file, so use `--each`: the command runs once per file and only passing files are marked. A file with errors returns each tick until it is fixed, instead of pinning the whole batch.

```sh
glean run --each --as eslint -- eslint
```

Outside Claude Code that is a plain shell loop:

```sh
while true; do
  glean run --each --as eslint -- eslint
  sleep 300
done
```

Whole-project tools that cannot take file paths (`cargo clippy`, `tsc`) do not fit `run`. Gate them on `-q` instead:

```sh
glean list -q --as clippy && cargo clippy && glean mark --as clippy
```

## MCP

`glean mcp` serves the change-set over stdio. The plugin registers it automatically; for another client, run `glean mcp` as the server command.

- `glean_list` — paths changed since a consumer last marked them.
- `glean_mark` — record paths as processed; omit `paths` to mark the whole changed set.
- `glean_status` — tracked and changed counts per consumer.

`run` is not exposed: the MCP surface reads and advances the cursor, it does not execute commands.

## How it works

- The candidate set is `git diff HEAD` plus `git ls-files --others` — staged, unstaged, and untracked.
- Baselines are stored at `.git/glean/<consumer>.json`. Git does not track its own directory, so the state stays out of history and out of glean's view of the tree.
- Each consumer is the sole writer of its own file, so consumers need no locking.
- Lockfiles and binary files are excluded from `list`.
- `core.fsmonitor` and `core.untrackedCache` speed up large trees.

There is no base flag. For committed work the file set (`git diff <ref>`) is static and swept once, so a cursor adds nothing — scope that with git, or with the skill's own branch mode.

## License

MIT. See [LICENSE](LICENSE).
