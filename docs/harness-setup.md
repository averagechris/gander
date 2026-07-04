# Harness setup recipes

How to wire gander into an agent harness (opencode, Claude Code, Codex,
Zed, shell scripts, ...) so the target flow works with zero forge-specific
behavior in gander: prepare work in a jj workspace, open gander there, and let
your harness organize, narrate, act on, or publish review state externally.

The baseline automation surface is the CLI, with MCP as an optional adapter
(`gander mcp`, docs/decisions.md D7). Everything below assumes `gander` is on
`PATH`.

## Boundary: harness prepares and acts; gander reviews

Gander expects the change to already be visible to jj in the current
workspace. A user or harness can fetch a teammate branch, prepare an agent
workspace, post selected comments to a forge, or edit files in response to
tasks. Gander itself should not fetch PRs, mutate code, or post remote reviews;
it persists local review sessions, comments, tasks, walkthroughs, and exports.

## CLI-first automation

Prefer the CLI when you want explicit, low-context interactions or when MCP
tool definitions would pollute an agent prompt. Commands should expose the same
capabilities as MCP tools over the same core business logic.

Current shape (see [docs/cli.md](cli.md) for the complete reference):

```sh
gander reviews list
gander reviews show <id>
gander hunks show <hunk-id>
gander comments list
gander tasks list
gander tasks complete <task-id> --summary "Handled by agent workspace changes"
gander walkthrough export
```

Use MCP when your harness benefits from typed tools and live `current_focus`.

## How routing works (why cwd matters)

`gander mcp` is a stdio MCP server. On each tool call it looks up the
instance registry for a **live gander TUI reviewing the current working
directory** (one gander per workstream, docs/decisions.md D3) and bridges
to its per-instance socket. That means:

- register the MCP server with the *workspace* as its working directory —
  a harness session started in `~/src/foo` talks to the gander reviewing
  `~/src/foo`;
- several instances in the same workspace resolve to the most recently
  touched one (`last_input_at`);
- without a running TUI, tools serve a snapshot loaded at startup — still
  useful for headless review passes.

Current tools exposed: `review_summary`, `review_files`, `file_diff`, `comments`,
`current_focus` (file/line/hunk the human is looking at right now),
`stack_changes` (the `trunk()..@` stack, oldest first — treat it like
stacked PRs), `change_diff` (one change against its parent),
`set_ordering`, `flag_section`, `set_chunks` (anchor chunks to a stack
change with `change_id`), `draft_comment`,
`list_reviews`, plus CLI-parity state-file tools for `reviews_*`,
`comment_*`, `task_*`/`tasks_list`, and `walkthrough_*`. Suggestions written through the mutating tools surface
live in the reviewer's terminal (ordering via `A`, flags via `F`, chunks
via `S` and zen mode `T`/`Z`, drafts via `D`).

The CLI-parity state-file tools load and save the persisted review state
directly, matching the corresponding `gander reviews`, `comments`, `tasks`,
and `walkthrough` commands. They are best used when no TUI is actively
autosaving: the TUI holds review state in memory and writes it back on save or
quit, so concurrent state-file edits can be overwritten by an older in-memory
snapshot.

### MCP ⇄ CLI parity table

Each parity tool documents its CLI equivalent and is backed by the same core
review service. The MCP adapter is optional; the CLI remains the canonical
scriptable surface.

| MCP tool | CLI equivalent |
| --- | --- |
| `reviews_list` | `gander reviews list` |
| `reviews_show` | `gander reviews show <id>` |
| `reviews_create` | `gander reviews create [--title <title>]` |
| `comment_add` | `gander comments add --path <path> [--line <n>] [--end-line <n>] --body <text> [--kind ...] [--action ...]` |
| `comment_resolve` | `gander comments resolve <id>` |
| `comment_set_state` | `gander comments set-state <id> --state draft|todo|resolved` |
| `task_add` | `gander tasks add --title <title> [--body <text>] [--action ...] [--comment <id>] [--path <path>] [--line <n>]` |
| `task_complete` | `gander tasks complete <id> [--summary <text>]` |
| `task_reopen` | `gander tasks reopen <id>` |
| `tasks_list` | `gander tasks list` |
| `walkthrough_add_step` | `gander walkthrough add-step --title <title> [--file <path>] [--line <n>] [--end-line <n>] [--symbol <name>] [--why <text>] [--body <text>]` |
| `walkthrough_remove_step` | `gander walkthrough remove-step <id>` |
| `walkthrough_move_step` | `gander walkthrough move-step <id> --to <zero-based-index>` |
| `walkthrough_show` | `gander walkthrough show` |

`gander paths` prints every resolved location (state dir, overlay, socket
pattern, registry) when you need to debug a connection.

Future tools should document their CLI equivalents and remain no more capable
than the CLI surface.

## Register the optional MCP server

### opencode

Project-level `opencode.json` (or global `~/.config/opencode/opencode.json`):

```json
{
  "mcp": {
    "gander": {
      "type": "local",
      "command": ["gander", "mcp"]
    }
  }
}
```

opencode spawns the server in the project directory, so cwd routing works
out of the box.

### Claude Code

```sh
# from the workspace root; --scope project writes .mcp.json for the repo
claude mcp add gander --scope project -- gander mcp
```

Or hand-write `.mcp.json`:

```json
{
  "mcpServers": {
    "gander": {
      "command": "gander",
      "args": ["mcp"]
    }
  }
}
```

### Codex CLI

`~/.codex/config.toml`:

```toml
[mcp_servers.gander]
command = "gander"
args = ["mcp"]
```

### Anything else

Any MCP client that can spawn a local stdio server works: command
`gander`, args `["mcp"]`, working directory = the workspace under review.

## The split-pane workflow

The conversation lives in the harness, not in gander (docs/decisions.md
D4). The recommended setup is two panes in the same directory:

```
┌─────────────────────────┬──────────────────────────┐
│ gander                  │ opencode / claude / ...  │
│ (review TUI)            │ (chat, gander MCP tools) │
└─────────────────────────┴──────────────────────────┘
```

1. start `gander` in one pane (tmux/zellij/wezterm split, terminal tabs —
   anything);
2. start your harness in the other pane, same cwd, with the gander MCP
   server registered;
3. talk to the harness about the review. Useful prompts:
   - *"Use gander's review_summary and set_chunks to break this change
     into reviewable units, ordered by risk."* — then press `T` in gander
     for a zen walkthrough of the chunks;
   - *"Flag anything security-sensitive with flag_section."* — flags show
     as red `!` pins, `F` lists them;
   - *"What am I looking at?"* / *"Explain this function."* — the harness
     calls `current_focus` and answers about the exact file/line under
     your cursor;
   - *"Draft review comments for the problems you see."* — triage them in
     gander with `D` (accept/edit/discard; dispositions are written back
     for the agent to observe).

When a review is large, gander nudges you in the footer
(`[limits] nudge-diff-lines` / `nudge-files` in `gander.toml`, 0 to
disable); that's the cue for step 3.

## Summoning from inside gander (`@`)

If you'd rather not switch panes, configure `[agent] command` in
`gander.toml` and press `@`. The command is agent-agnostic: any CLI that
accepts a prompt, spawned with the workspace as cwd, output logged to the
workspace agent log (`gander paths`).

```toml
[agent]
command = "opencode run"   # or: claude -p
autostart = false                  # true: summon on TUI startup
```

### Attach to an already-running harness server

Reusing a warm server is the harness CLI's job, not gander's — put the
attach flag in the command:

```toml
[agent]
# opencode: reuse a running `opencode serve` (default port 4096)
command = "opencode run --attach http://localhost:4096"
```

```toml
[agent]
# claude: continue the most recent conversation in this project
command = "claude -p --continue"
```

The prompt gander hands the command is customizable (`[agent] prompt`,
with `{repo}`/`{base}`/`{rev}` placeholders); the default one tells the
agent how to reach the review session and what to do with it.

## Headless / scripting

- `gander mcp` with no TUI: snapshot-backed tools for one-shot agent
  passes (e.g. CI review bots).
- `gander acp`: the raw line-delimited JSON-RPC socket bridge, kept for
  scripting (docs/acp.md).
- `gander export --profile agent`: a static JSON artifact with raw
  excerpts and stable anchors when you don't need a live session.
