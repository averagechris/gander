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
workspace, post selected comments to a forge, or edit files in response to todo
comments/action items. Gander itself should not fetch PRs, mutate code, or post remote reviews;
it persists local review sessions, comments, optional action items, walkthroughs, and exports.

## CLI-first automation

Prefer the CLI when you want explicit, low-context interactions or when MCP
tool definitions would pollute an agent prompt. Commands should expose the same
capabilities as MCP tools over the same core business logic.

Current shape (see [docs/cli.md](cli.md) for the complete reference):

```sh
gander reviews create --title "Agent pass"
gander reviews list
gander reviews show <id>
gander comments add --path src/lib.rs --line 42 --kind issue --action fix --state todo --body "Check this invariant."
gander comments add --general --state draft --body "Private reviewer note; not ready for handoff."
gander comments ready --all-drafts
gander action-items add --title "Tighten invariant" --action fix --comment <comment-id> --path src/lib.rs --line 42
gander walkthrough add-step --title "Start at the invariant" --path src/lib.rs --line 42 --why "This controls the rest of the change."
gander walkthrough show
gander handoff --copy
gander hunks show <hunk-id>
```

Review state schema 8 records durable fingerprint-anchored attention regions and
append-only, fingerprint-current skim/spotlight progress in
addition to optional comment `session_id`, `path`, immutable
observations, reply results, annotation authors/channels, and session
disposition, plus normalized `action_items` when legacy `tasks` state is read.
Artifact schema 13 and delegation schema 5 expose action-item shape, private
human/agent attention assignments, team-profile
collaboration exports, plus portable comment-observation and reply-result evidence. A snapshot label or an
unchanged portable patch is context, not proof that the requested outcome was
implemented or tested; report actual verification separately.
State/artifact readers still accept older anchored comments unchanged. A legacy
comment with no `session_id` remains visible in the active session, while a
pathless comment is explicitly general to its owning session.

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
  useful for headless review passes;
- durable MCP comments/replies obtain provenance and anchors from that same
  selected live-or-snapshot session, including its active target; they do not
  fall back to stale startup fingerprints when a live TUI is selected.

Current tools exposed: `review_summary`, `review_files`, `file_diff`, `comments`,
`current_focus` (file/line/hunk the human is looking at right now),
`stack_changes` (the `trunk()..@` stack, oldest first — treat it like
stacked PRs), `change_diff` (one change against its parent),
`set_ordering`, `flag_section`, `set_chunks` (deprecated compatibility input;
prefer `walkthrough_*` for new curation, with optional stack `change_id`),
`draft_comment`, `review_disposition`, `review_disposition_set`,
`list_reviews`, plus CLI-parity state-file tools for `reviews_*`,
`comment_*`, `action_item_*`, and `walkthrough_*`. Suggestions written through the mutating tools surface
live in the reviewer's terminal (ordering via `A`, flags via `F`, walkthrough/zen
mode via `T`/`Z`, comments via `C`, and open work via `X`). MCP must preserve CLI
semantics: new comments honor the configured initial state unless an explicit
state is supplied; draft comments are durable/private/withheld, delegation todos
are agent-directed work, collaboration todos are open team feedback, resolved comments are history,
and general comments have no file location or excerpt. Todo comments are the
primary implicit feedback; ordinary comments are not action items. Durable action
items are optional higher-level coordination objects and linked todo evidence is
folded into its parent item.

The CLI-parity state-file tools load and save the persisted review state
directly, matching the corresponding `gander reviews`, `comments`, `action-items`,
and `walkthrough` commands. They are safe to use beside a live TUI: the TUI
watches the state file and merges external additions/updates before saving, so
CLI-added comments, action items, and walkthrough steps survive TUI save/quit. Same-ID
comment updates use the newer `updated_at` value for body/state metadata and
union append-only replies by reply id; sessions, action items, walkthroughs, and
walkthrough steps likewise prefer the newer timestamp.

### MCP ⇄ CLI parity table

Each parity tool documents its CLI equivalent and is backed by the same core
review service. The MCP adapter is optional; the CLI remains the canonical
scriptable surface.

| MCP tool | CLI equivalent |
| --- | --- |
| `reviews_list` | `gander reviews list` |
| `reviews_show` | `gander reviews show <id>` |
| `reviews_create` | `gander reviews create [--title <title>]` |
| `review_disposition` | `gander reviews disposition show` |
| `review_disposition_set` | `gander reviews disposition set <state>` / `gander reviews disposition clear` |
| `comments` with `channel` | `gander comments list --channel onboarding|delegation|collaboration|note` |
| `comment_add` | `gander comments add (--path <path> [--line <n>] [--end-line <n>] \| --general) --body <text> [--kind ...] [--action ...] [--state ...]` |
| `comment_reply` | `gander comments reply <id> --body <text> [--resolve]` |
| `comment_resolve` | `gander comments resolve <id> [--reply <text>]` |
| `comment_set_state` | `gander comments set-state <id> --state draft|todo|resolved` |
| `comment_ready` | `gander comments ready (<id>... \| --all-drafts)` |
| `action_item_list` | `gander action-items list` |
| `action_item_show` | `gander action-items show <id>` |
| `action_item_add` | `gander action-items add --title <title> [--body <text>] [--action ...] [--comment <id>]... [--ticket <ref>]... [--path <path>] [--line <n>]` |
| `action_item_edit` | `gander action-items edit <id> ...` |
| `action_item_link_comment` | `gander action-items link-comment <id> --comment <comment-id>` |
| `action_item_unlink_comment` | `gander action-items unlink-comment <id> --comment <comment-id>` |
| `action_item_add_ticket` | `gander action-items add-ticket <id> --ticket <ref>` |
| `action_item_remove_ticket` | `gander action-items remove-ticket <id> --ticket <ref>` |
| `action_item_close` | `gander action-items close <id> --disposition completed|dismissed|deferred [--outcome <text>]` (use `add-ticket` before a deferred close) |
| `action_item_reopen` | `gander action-items reopen <id>` |
| `action_item_delete` | `gander action-items delete <id>` |
| `walkthrough_add_step` | `gander walkthrough add-step --title <title> [--file <path>] [--line <n>] [--end-line <n>] [--symbol <name>] [--why <text>] [--body <text>]` |
| `walkthrough_remove_step` | `gander walkthrough remove-step <id>` |
| `walkthrough_move_step` | `gander walkthrough move-step <id> --to <zero-based-index>` |
| `walkthrough_show` | `gander walkthrough show` |
| `attention_list` | `gander attention list [--mode effective|assigned]` |
| `attention_set` | `gander attention set --path <path> ... --salience <value>` |
| `attention_clear` | `gander attention clear --path <path> ...` |
| `attention_promote` / `attention_demote` | `gander attention promote|demote --path <path> ...` |
| `attention_seed_heuristics` | `gander attention seed-heuristics` |
| `attention_recompute_heuristics` | `gander attention recompute-heuristics` |

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
   - *"Use gander's review_summary and walkthrough tools to break this change
     into reviewable steps and chapters, ordered by risk."* — then press `T` in
     gander for a zen walkthrough;
   - *"Flag anything security-sensitive with flag_section."* — flags show
     as red `!` pins, `F` lists them;
   - *"What am I looking at?"* / *"Explain this function."* — the harness
     calls `current_focus` and answers about the exact file/line under
     your cursor;
   - *"Draft review comments for the problems you see."* — triage them in
      gander with `C`/`D`: keep private drafts, ready todos when they should be
      addressed, or discard/resolve them. Dispositions are written back for the
      agent to observe.

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
- `gander acp`: lower-level line-delimited JSON-RPC bridge for compatibility
  and live-session plumbing. Prefer CLI or MCP for normal agent harnesses.
- `gander export --profile agent`: a static JSON artifact with raw
  excerpts and stable anchors when you don't need a live session. It is an
  archive/reference artifact; import currently restores comments and matching
  viewed state, not action items or walkthroughs.
