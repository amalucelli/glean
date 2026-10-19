<img width="170" alt="glean" src="https://raw.githubusercontent.com/amalucelli/glean/main/assets/logo.png" />

# glean

Claude Code can run a skill on a timer with `/loop`. Say you have a skill, `/slop`, that cleans up AI-generated code in your uncommitted changes, and you loop it every 2 minutes while you work. Every tick, it re-reads the whole diff, even if you only touched one file since the last run.

`glean` tracks which files each skill has already processed. Wrap the skill with `/glean` and each tick sees only what changed since the last one:

```
/loop 2m /glean slop
```

- **Per-skill baselines.** Each skill tracks its own position, so `/glean slop` and `/glean simplify` (a built-in skill) can loop over the same tree without stepping on each other.
- **Content-hashed.** A file counts as changed when its bytes differ from the baseline, so rebases, branch switches, and mtime-only rewrites don't trigger a re-run.
- **Flat context.** Each pass runs in a forked subagent, so the loop's context grows by one line per tick, not by every file the skill reads.

## Install

Install the binary, then the plugin. The plugin provides the `/glean` skill and the `glean` MCP server; both call the binary on `PATH`.

```sh
brew install --cask amalucelli/tap/glean
```

```
/plugin marketplace add amalucelli/glean
/plugin install glean@glean
```

From source: `cargo install --path . --root ~/.local`. Without the plugin: `cp -r skills/glean ~/.claude/skills/`.

## Usage

`/glean <skill>` takes the name of any skill: one you wrote, one from a plugin, or a Claude Code built-in. It lists the files changed since that skill last ran, runs the skill on them, and marks the ones it finished. Files it couldn't finish come back next tick.

```
/loop 2m /glean slop         # your own /slop skill, every 2 minutes
/loop 5m /glean simplify     # Claude Code's built-in /simplify, every 5 minutes
```

- The wrapped skill must accept file paths as arguments.
- Forking needs Claude Code v2.1.101+; older versions run the skill inline.
- Only uncommitted work is tracked. To cover a whole branch, run the skill directly instead of through `/glean`.

Check each skill's progress from the shell:

```sh
$ glean status
simplify: 106 tracked, 0 changed, last mark 3h ago
slop: 106 tracked, 1 changed, last mark 12m ago
```

`glean reset --as slop` forgets a baseline and forces a full re-sweep.

## Without Claude Code

The same baselines work for any tool. `glean run` runs a command on the changed files and marks them only if it succeeds:

```sh
glean run --as gofmt -- gofmt -w              # formatters: one batch
glean run --each --as eslint -- eslint        # linters: per file, only passing files are marked
glean list -q --as clippy && cargo clippy && glean mark --as clippy   # whole-project tools
```

```
glean <command> [--as <consumer>] [options]

  list   [-z] [-q] [--json]        files changed since this consumer's last mark
  mark   [--stdin] [-z] [paths…]   record files as processed; with no paths, the whole changed set
  status [--json]                  counts and last mark time; with no --as, every consumer
  reset  [--all]                   forget a baseline to force a full re-sweep
  run    [--each] -- <cmd>…        run <cmd> on the changed files, marking them when it succeeds
  mcp                              serve the change-set as MCP tools over stdio
```

`glean <command> --help` covers each flag.

## How it works

- The candidate set is `git diff HEAD` plus untracked files. Lockfiles and binary files are excluded.
- Baselines live in `.git/glean/<consumer>.json`: never committed, separate per worktree.

## License

MIT. See [LICENSE](LICENSE).
