# Agent-collaborative review (ACP)

> **Direction note.** This custom JSON-RPC surface is transitional: the
> agent-facing tool surface is moving to MCP (`gander mcp`, roadmap
> milestone 9), which harnesses discover natively. See docs/decisions.md
> D5. The protocol below keeps working in the meantime and remains the
> internal TUI-bridge plumbing.

`gander acp` hosts the review session for agents: a line-delimited JSON-RPC
2.0 server on stdio (the transport style used by the Agent Client Protocol).
Agents read the diff, comments, and viewed state, and write review
suggestions into the shared **agent overlay** (`agent.json` in the
per-workspace state directory; run `gander paths` to see where), which the
running TUI polls and surfaces live.

```sh
gander --base 'trunk()' --rev '@' acp
```

Each request/response is one JSON object per line. Requests without an `id`
are treated as notifications and get no response.

## Live session over the TUI socket

While the TUI runs it also hosts the same protocol on a per-instance Unix
socket (`acp-<pid>.sock`) in the workspace's runtime directory
(`gander paths` prints the pattern; Unix platforms only). Each instance
also registers itself (workspace root, target, summary, socket, pid,
`last_input_at`) in a shared instance registry, cleaned up on exit, so
several reviews can run at once — one gander per workstream
(docs/decisions.md D3). Requests answered there hit the
**live** session: current viewed state, comments, and the active review
target, and agent writes surface in the UI within one event-loop tick —
no file polling latency.

Two ways to reach it:

- connect to an instance socket directly and speak line-delimited
  JSON-RPC;
- run `gander acp` as usual: it looks up the registry for a live instance
  reviewing the current workspace (most recently touched first) and
  transparently bridges stdio to it, so agent clients that spawn
  `gander acp` as a subprocess get the live session for free. Without a
  running TUI it falls back to serving a snapshot loaded at startup.

If the socket cannot be bound (say, a second gander TUI on the same
workspace)
the TUI shows a notice and collaboration degrades gracefully to the overlay
file.

## Summoning an agent from the TUI

Instead of orchestrating the agent yourself, configure one and let gander
launch it:

```toml
[agent]
# Any CLI that accepts a prompt. gander appends its review prompt as the
# final shell-quoted argument, or substitutes a {prompt} placeholder.
command = "opencode run"
# command = "claude -p"
# command = "opencode run --attach http://localhost:4096"  # reuse a running server
autostart = false   # true: summon on TUI startup
# prompt = "custom template; {repo}, {base}, {rev} are substituted"
```

Press `@` in the TUI (or use `autostart`) and gander spawns the command in
the repo root with a prompt explaining the ACP workflow. The agent runs
`gander acp`, which bridges to the live socket, and its suggestions stream
into the UI. Output is logged to the workspace's agent log (see
`gander paths`); the footer announces
summon, completion, or failure, and the process is killed if you quit
mid-review. gander itself stays agent-agnostic — anything that can take a
prompt and run a subprocess works.

## Read methods

| Method | Params | Result |
| --- | --- | --- |
| `initialize` | – | protocol name, version, capability list |
| `review/summary` | – | repo, base, revision, summary line |
| `review/files` | – | array of `{path, old_path, status, additions, deletions, viewed, generated, fingerprint}` |
| `review/file_diff` | `{path}` | `{path, fingerprint, raw}` (raw git-style diff) |
| `review/comments` | – | array of `{id, path, line, end_line, body, state}` |
| `review/current_focus` | – | what the human is looking at: `{repo, base, revision, pane, path, line?}` where `line` is `{side, old_line, new_line, hunk_header}` when the diff cursor sits on an anchorable row (live through the TUI socket; a snapshot server reports its initial selection) |
| `review/stack_changes` | – | the jj stack (`trunk()..@`, oldest first): `{base, revision, changes: [{change_id, bookmarks, description, current}]}` — the human often reviews these like stacked PRs, so prefer organizing chunks change-by-change when several exist |
| `review/change_diff` | `{change_id}` | one change against its parent (`change_id-..change_id`): `{change_id, base, revision, files: [{path, status, additions, deletions}], raw}`; line numbers here are what chunk parts anchored to this change must reference |
| `review/overlay` | – | the full agent overlay (ordering, flags, chunks, change briefs, drafts with dispositions) |

## Write methods

All write methods persist the overlay atomically; the TUI picks changes up
within one poll tick.

| Method | Params | Effect |
| --- | --- | --- |
| `review/set_ordering` | `{paths: [string]}` | suggested review order, highest priority first; unknown paths are rejected |
| `review/flag_section` | `{path, line?, reason, priority?}` | flag a critical section (`priority`: `critical`/`high`/`medium`/`low`, default `high`) |
| `review/set_chunks` | `{chunks: [{id?, title, importance?, change_id?, rationale?, explanation?, artifacts?, parts: [{path, start_line?, end_line?}]}]}` | replace the reviewable units. `importance` is `spotlight` (zen walkthrough stop; give it a teaching `explanation`) or `glance`. `change_id` anchors the chunk to one jj change of the stack: the walkthrough retargets to that change's diff for the stop, and part line numbers must come from `review/change_diff` for that change. `artifacts` attaches exhibits (see below) |
| `review/set_change_briefs` | `{briefs: [{change_id, summary, artifacts?}]}` | replace the per-change briefings: a few sentences of high-level narrative per change (what it accomplishes, why it exists, how it builds on the previous changes). Zen renders each brief on the chapter intro card shown before that change's stops |
| `review/draft_comment` | `{path, line?, body}` | add a draft comment for human triage; returns `{id}` |

`artifacts` (on chunks and briefs) is `[{title, kind?, body}]` with `kind`
one of `example` (default), `output`, `diagram`, or `note`: exhibits that
*show* the change — a usage example of the changed API, output captured by
running the code, a small ASCII diagram. The human opens them from the zen
focus or chapter card with `e` (scrollable, `h`/`l` cycles).

## Two-way draft flow

Draft comments start `pending`. The human accepts, edits, or discards them in
the TUI; gander writes the disposition (`accepted`/`discarded`, plus
`accepted_comment_id`) back into the overlay. Agents observe outcomes via
`review/overlay`. Server writes merge with on-disk dispositions rather than
clobbering them.

## Example session

```
→ {"jsonrpc":"2.0","id":1,"method":"initialize"}
← {"jsonrpc":"2.0","id":1,"result":{"protocol":"gander-acp","version":1,"capabilities":[...]}}
→ {"jsonrpc":"2.0","id":2,"method":"review/files"}
← {"jsonrpc":"2.0","id":2,"result":[{"path":"src/app.rs","status":"mod",...}]}
→ {"jsonrpc":"2.0","id":3,"method":"review/draft_comment","params":{"path":"src/app.rs","line":42,"body":"handle the None case"}}
← {"jsonrpc":"2.0","id":3,"result":{"id":"8f14..."}}
```

## Compatibility notes

This is a minimal ACP-style surface, not (yet) a full implementation of the
published Agent Client Protocol schema. The method set is versioned via
`initialize.version` and will grow toward spec compliance.
