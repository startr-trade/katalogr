# Using katalogr with Claude Code

katalogr works with Claude Code through the command line, a skill and a few hooks. There is no MCP
server on this path: Claude Code runs `katalogr` commands directly, so no tool schema is loaded into
its context. Hosts that cannot run commands use the [MCP server](mcp.md) instead.

None of this is required. Every query works without an agent.

## What each part does

| Part | What it gives the agent |
|---|---|
| The skill | Query craft, playbooks and the output contract: which command answers which question |
| The `CLAUDE.md` block | A short list of the commands to try before reading whole files |
| `session-start` | A compact map of the project at the start of a session |
| `hook pre-read` | Refuses a whole-file read of a large, fresh, indexed file and names the commands to use |
| `hook post-search` | After a search, adds index facts about the names that were searched for |
| `hook post-edit` | Queues and reindexes a file as soon as it is edited |

## Set up

Run these inside the repository (the project must already exist; see
[Getting started](getting-started.md)).

```text
$ katalogr skill install
installed …/.claude/skills/katalogr/SKILL.md
$ katalogr hooks install
…/solo/.claude/settings.json: PreToolUse katalogr hook pre-read: added
…/solo/.claude/settings.json: PostToolUse katalogr hook post-search: added
…/solo/.claude/settings.json: PostToolUse katalogr hook post-edit: added
…/solo/.claude/settings.json: SessionStart katalogr session-start: added
post-commit: installed
post-checkout: installed
post-merge: installed
$ katalogr claude sync
created …/solo/CLAUDE.md
```

### The skill

`katalogr skill install` writes `SKILL.md` and reports `installed`, `updated` or `unchanged`. Run it
again after you upgrade katalogr to refresh the text. `--scope user` (the default) installs under
`~/.claude`; `--scope repo` installs under the repository's own `.claude`.

### The hooks

`katalogr hooks install` merges four entries into the repository's `.claude/settings.json` and adds
the three git hooks that queue changes after a commit, a checkout and a merge. It is safe to run
again. It changes only katalogr's own entries and lines.

- `--global` writes the Claude Code entries to your user settings instead of the repository's.
- `katalogr hooks status` shows what is installed where. `installed (points at the current binary)`
  means the entry runs the `katalogr` you are using now.
- `katalogr hooks uninstall` removes only katalogr's entries and git hook lines (also takes
  `--global`).

```text
$ katalogr hooks status
project PreToolUse katalogr hook pre-read: installed (points at the current binary)
project PostToolUse katalogr hook post-search: installed (points at the current binary)
project PostToolUse katalogr hook post-edit: installed (points at the current binary)
project SessionStart katalogr session-start: installed (points at the current binary)
global PreToolUse katalogr hook pre-read: missing
…
git post-commit: installed
git post-checkout: installed
git post-merge: installed
```

The settings entries it writes are these (shown trimmed):

```json
"PreToolUse":   [{ "matcher": "Read",
                   "hooks": [{ "type": "command", "command": "katalogr hook pre-read" }] }],
"PostToolUse":  [{ "matcher": "Grep|Glob",
                   "hooks": [{ "type": "command", "command": "katalogr hook post-search" }] },
                 { "matcher": "Edit|Write|MultiEdit|NotebookEdit",
                   "hooks": [{ "type": "command", "command": "katalogr hook post-edit" }] }],
"SessionStart": [{ "hooks": [{ "type": "command", "command": "katalogr session-start" }] }]
```

The `katalogr hook …` subcommands read the hook's JSON on standard input. You do not run them by
hand.

### The `CLAUDE.md` block

`katalogr claude sync` writes a short block between `<!-- katalogr:begin -->` and
`<!-- katalogr:end -->` markers in the repository's `CLAUDE.md`, creating the file when it is
missing. Text outside the markers is left alone. Run it again to refresh the block (it prints
`unchanged` when nothing differs). `katalogr claude sync --show` prints the block without writing.

## What the hooks do, and how they fail

**Fail open.** A hook that cannot do its job lets Claude Code carry on: it exits 0 and prints
nothing. A missing project, a missing index, an unreadable payload or a timeout never blocks the
agent. Each hook has a time limit (800 ms by default, `hooks.timeout_ms`). Decisions are logged to
`hooks.log` under the project's `.cache` folder.

```text
$ echo 'garbage' | katalogr hook post-edit ; echo "exit $?"
exit 0
```

**`pre-read` is the one strict hook.** It denies a read of a whole file only when the file is
large (32768 bytes by default, `hooks.read_block_bytes`), indexed and fresh. The reply tells the
agent what to do instead:

```text
katalogr: src/big.rs is 42001 bytes and indexed (fresh). Read the symbols you need instead of the
whole file. Or read with offset/limit to fetch a line range.
```

A read with a line range is allowed.

**`post-edit`** only acts on files inside the current checkout. It queues the file and processes it
within the time limit; if processing does not finish, the change stays queued and the next read
completes it.

**`session-start`** prints the project map, ranked by importance, cut to a token budget
(`claude.session_budget`, 1500 by default), and a `freshness:` line. It always exits 0. While a
first index is queued it says so instead of printing a map.

```text
$ katalogr session-start
katalogr: project ledger (repos: ledger); index watermark 6, 0 pending. Top symbols by PageRank:
freshness: context current; index at ledger 1514e9ac; 0 pending.
fn  ledger:src/rules.rs#fn:limit  src/rules.rs:8
fn  ledger:src/totals.rs#fn:add  src/totals.rs:1
…
Ask the graph before reading files: use the katalogr skill (find, search, explain, show, refs, why).
```

The `hooks.*` and `claude.*` names are project settings, kept under `settings:` in the project's
`config.yaml`.

## Worktrees

Claude Code often works in linked git worktrees. katalogr finds the marker through the main
checkout and reads and writes that worktree's own context, so an agent in a worktree sees its own
edits, not the main checkout's. The git hooks run with the worktree as the current directory and
queue into its context. See [Change and history](../features/change-and-history.md).

## Remove

`katalogr hooks uninstall` removes the hooks. Delete the skill folder to remove the skill, and the
marked block from `CLAUDE.md` to remove the block.

---

What changed in each version is in the [release notes](../releases/index.md).
