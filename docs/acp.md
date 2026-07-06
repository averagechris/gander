# Agent-collaborative review (ACP)

> **Direction note.** This custom JSON-RPC surface is transitional internal
> plumbing. The product direction is a shared local review core with complete
> CLI automation and an optional MCP adapter over the same code (see
> docs/decisions.md D7 and docs/vision.md). The protocol below keeps working
> in the meantime for the TUI bridge and compatibility.

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
  `gander acp` as a subprocess get the live session for free. Before serving,
  `gander acp` writes a one-line stderr notice saying either
  `gander acp: bridged to live TUI session (target <revset>)` or
  `gander acp: serving snapshot (no live TUI for this workspace)`. Without a
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
| `initialize` | – | protocol name, version, `mode` (`"live-bridge"` or `"snapshot"`), capability list |
| `review/summary` | – | repo, base, revision, summary line |
| `review/files` | – | array of `{path, old_path, status, additions, deletions, viewed, generated, fingerprint}` |
| `review/file_diff` | `{path}` | `{path, fingerprint, raw}` (raw git-style diff) |
| `review/comments` | – | array of `{id, path, line, end_line, body, state}` |
| `review/current_focus` | – | what the human is looking at: `{repo, base, revision, pane, path, line?}` where `line` is `{side, old_line, new_line, hunk_header}` when the diff cursor sits on an anchorable row (live through the TUI socket; a snapshot server reports its initial selection) |
| `review/stack_changes` | – | the jj stack (`trunk()..@`, oldest first): `{base, revision, changes: [{change_id, bookmarks, description, current}]}` — `description` is the full multiline message; the human often reviews these like stacked PRs, so prefer organizing chunks change-by-change when several exist |
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
| `review/update_chunks` | `{chunks: [{id?, title, importance?, change_id?, rationale?, explanation?, artifacts?, parts: [{path, start_line?, end_line?}]}]}` | upsert reviewable units. Chunks whose `id` matches an existing overlay chunk replace it in place; chunks with new/generated ids append. Response: `{chunks, updated, added}` |
| `review/remove_chunks` | `{ids: ["..."]}` | strictly remove chunks by id. If any id is unknown the request is rejected and nothing is removed. Response: `{chunks, removed}` |
| `review/set_change_briefs` | `{briefs: [{change_id, summary, artifacts?}]}` | replace the per-change briefings: a few sentences of high-level narrative per change (what it accomplishes, why it exists, how it builds on the previous changes). Zen renders each brief on the chapter intro card shown before that change's spotlight stops. Response: `{briefs, warnings}`; warnings are advisory |
| `review/draft_comment` | `{path, line?, body}` | add a draft comment for human triage; returns `{id}` |

`artifacts` (on chunks and briefs) is `[{title, kind?, body}]` with `kind`
one of `example` (default), `output`, `diagram`, or `note`: exhibits that
*show* the change — a usage example of the changed API, output captured by
running the code, a small ASCII diagram. The human opens them from the zen
focus or chapter card with `e` (scrollable, `h`/`l` cycles).

### Chunk validation

`review/set_chunks` and `review/update_chunks` validate every incoming chunk
part before mutating the overlay. A single invalid part rejects the whole
request with all per-part reasons and leaves the previous chunk list intact.
`review/remove_chunks` is similarly all-or-nothing for unknown ids. The CLI
equivalent is `gander chunks`: `list`, `set --file spec.json`,
`update --file spec.json`, `remove --id <id>...`, `lines [--change <id>] [--path <p>]`, and `clear`. The spec file is
JSON in the same chunk shape: `{ "chunks": [ { "id": "optional", "title":
"...", "importance": "spotlight|glance", "change_id": "optional",
"rationale": "...", "explanation": "...", "artifacts": [{"title":"...",
"kind":"example|output|diagram|note", "body":"..."}], "parts":
[{"path":"...", "start_line": 1, "end_line": 10}] } ] }`; `--file -` or an
omitted `--file` reads stdin.
Parts must reference a file in the anchored change diff (or the current session
diff when `change_id` is omitted), and any supplied line range must intersect
that file's diff line space. Unknown or unresolvable `change_id`s are invalid.
If any part is invalid the entire request is rejected with a JSON-RPC error
listing the invalid parts; no valid subset is applied.

Use `gander chunks lines` while authoring specs to list the exact line space
accepted by `chunks set`/`update`. Without `--change` it lists the current
session diff; with `--change <id>` it lists the same change-scoped diff used by
chunks whose `change_id` is that id. `--path <p>` narrows the JSON output to one
file. Each file contains hunks with `start_line`, `end_line`, and first/last
content excerpts for orientation:

```json
[
  {
    "path": "src/lib.rs",
    "hunks": [
      {
        "header": "@@ -10,2 +10,3 @@",
        "start_line": 10,
        "end_line": 12,
        "first_line": " unchanged context",
        "last_line": "+new line"
      }
    ]
  }
]
```

The same spec-file authoring path is available for briefs and drafts. Use
`gander briefs list`, `set --file spec.json`, or `clear` with the ACP
`review/set_change_briefs` shape: `{ "briefs": [{ "change_id": "...",
"summary": "...", "artifacts": [{"title":"...", "kind":"note",
"body":"..."}] }] }`; `set` validates `change_id` values against the jj
stack before replacing the overlay. If a brief's change has no spotlight chunk
yet in the current overlay (for example, there are no chunks yet or only
`glance` chunks for that change), ACP returns and `gander briefs set` prints an
advisory warning: `brief for change <id> has no spotlight chunk yet and will not
render on a curated zen chapter right now`. This does not reject the write;
briefs are often authored before chunks. Use `gander drafts list`,
`add --file spec.json`, or `remove --id <id>...` with the ACP
`review/draft_comment` shape (`{ "path": "...", "line": 12, "body": "..." }`)
or `{ "drafts": [ ... ] }` for bulk adds. Draft adds append pending drafts and
print the generated ids; remove is strict and rejects unknown ids without
mutating the overlay. For all three groups, `--file -` or an omitted `--file`
reads stdin and writes the same overlay file that the TUI watches.

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
