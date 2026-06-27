---
name: glean
description: Run another skill incrementally — only on the files changed since that skill last ran, tracked by the glean CLI. Use it to loop a cleanup or quality skill without re-processing the whole diff each tick (e.g. /loop 5m /glean humanize). Works with built-in skills too.
argument-hint: "<skill>"
allowed-tools: Bash(glean:*),Bash(git:*),Bash(command:*),Skill
context: fork
---

glean is a per-repo incremental change tracker: it supplies the scope — files changed since this skill last ran — and records what got processed, so each `/loop 5m /glean <skill>` tick touches only new edits instead of the whole diff. Any skill that takes file paths works, including built-ins that can't be made glean-aware themselves.

`context: fork` runs this whole skill in its own subagent, so each loop tick returns only a one-line result to the loop — the wrapped skill's instructions, file reads, and edits stay in the fork and never pile up in the loop's window. To watch or steer a pass instead, run the skill directly (`/slop <files>`).

Requires the `glean` binary on `PATH` (`cargo install --path . --root ~/.local`).

## Arguments

`$ARGUMENTS` = `<skill>` — the skill to run, with or without a leading slash (`humanize`, `/slop`, `simplify`). Strip any leading `/`: the bare name is both the skill to invoke and the glean consumer. Missing `<skill>` → report usage and stop.

## Steps

1. **Preconditions** — need `glean` on `PATH` and a git repo. If `command -v glean` fails or `git rev-parse --show-toplevel` fails, stop and report `glean unavailable — run /<skill> directly`.
2. **Scope** — `$FILES` = `glean list --as <skill>` (changed since this skill last ran; lockfiles and binaries excluded). Empty → report `no changes for <skill>` and stop.
3. **Run** — invoke `/<skill>` through the Skill tool with `$FILES` as positional arguments and nothing else. The target skill scopes itself to those paths and does its own editing and verification.
4. **Mark** — `glean mark --as <skill> <paths>` for the files the skill processed and verified clean; leave anything it couldn't finish unmarked so that file returns next tick. Marking runs in the same fork that ran the skill, so a fork that dies mid-pass records nothing rather than marking work it never did.
5. **Report** — one line: `<skill>: marked N of M files` (note any held back for next tick). Facts only, no preamble.

## Notes

- `context: fork` runs each invocation in its own subagent. From a plugin install it needs Claude Code v2.1.101+ (where plugin skills began honoring it); on older versions the skill runs inline — still correct, just without the context isolation.
- The target skill must accept positional paths (humanize, slop, simplify, …). One that ignores them runs on its own default scope; the run still marks `$FILES`.
- One skill = one consumer = one baseline. `/glean humanize` and `/glean slop` watch the same tree through separate baselines, so each re-checks what the other edited. Run them as separate loops.
- The baseline persists across runs (so continuing a task picks up only new edits) and is content-based, so it stays correct across branches. `glean reset --as <skill>` forces a full re-sweep; `glean reset --all` wipes a repo's baselines. For separate tasks, use git worktrees — each worktree gets its own baseline automatically.
- `glean` tracks uncommitted work only (`git diff HEAD` + untracked); a committed change drops out by design. For committed or whole-branch work, run the skill standalone in its own branch mode (`/slop main`), not through `glean` — scoping by a ref is git's job.
