# Agent-collaborative review (ACP)

> **Direction note.** This custom JSON-RPC surface is transitional internal
> plumbing. The product direction is a shared local review core with complete
> CLI automation and an optional MCP adapter over the same code (see
> docs/decisions.md D7 and docs/vision.md). The protocol below keeps working
> in the meantime for the TUI bridge and compatibility.

`gander acp` hosts a lower-level line-delimited JSON-RPC 2.0 server on stdio
(the transport style used by the Agent Client Protocol). Normal automation
should prefer the CLI or optional MCP adapter; ACP remains available for the
TUI live bridge and compatibility. Agents can read the diff, comments, and
viewed state. Ordering and flag suggestions go
to the shared **agent overlay** (`agent.json` in the per-workspace state
directory; run `gander paths` to see where), while agent draft comments are
durable review-state comments. Walkthrough and attention curation is durable;
ACP-driven agents should invoke the scriptable `gander walkthrough ...` and
`gander attention ...` commands (or use the equivalent MCP tools). The running
TUI surfaces all of these live.

```sh
gander --base 'trunk()' --rev '@' acp
```

Each request/response is one JSON object per line. Requests without an `id`
are treated as notifications and get no response.

## Live session over an instance socket

While the TUI or `gander web` runs it also hosts the same protocol on a per-instance Unix
socket (`acp-<pid>.sock`) in the workspace's runtime directory
(`gander paths` prints the pattern; Unix platforms only). Each instance
also registers itself (workspace root, target, summary, socket, pid,
`last_input_at`) in a shared instance registry, cleaned up on exit, so
several reviews can run at once — one gander per workstream
(docs/decisions.md D3). Requests answered there hit the
**live** session: current viewed state, comments, and the active review
target, and agent writes surface in the instance within one event-loop tick —
no file polling latency.

Two ways to reach it:

- connect to an instance socket directly and speak line-delimited
  JSON-RPC;
- run `gander acp` as usual: it looks up the registry for a live instance
  reviewing the current workspace (most recently touched first) and
  transparently bridges stdio to it, so agent clients that spawn
  `gander acp` as a subprocess get the live session for free. Before serving,
  `gander acp` writes a one-line stderr notice saying whether it bridged to a
  live session or is serving a snapshot. Without a running TUI or web peer it
  falls back to serving a snapshot loaded at startup.

If a TUI socket cannot be bound, the TUI shows a notice and collaboration
degrades gracefully to the overlay file. Web startup fails rather than serving
HTTP without its required peer socket.

## Bringing an agent to the session

Gander never spawns agents (docs/decisions.md D9): the harness owns the
agent lifecycle and connects from the outside. Any process started in the
workspace can run `gander acp`, which bridges to the live socket, and its
suggestions stream into the UI. Prefer the CLI or `gander mcp` for normal
harnesses (docs/harness-setup.md); ACP remains the lower-level plumbing.
Gander itself stays agent-agnostic — anything that can run `gander acp`
(or speak MCP/CLI) works.

## Read methods

| Method | Params | Result |
| --- | --- | --- |
| `initialize` | – | protocol name, version, `mode` (`"live-bridge"` or `"snapshot"`), capability list |
| `review/summary` | – | repo, base, revision, summary line |
| `review/files` | – | array of `{path, old_path, status, additions, deletions, viewed, generated, fingerprint}` |
| `review/file_diff` | `{path}` | `{path, fingerprint, raw}` (raw git-style diff) |
| `review/comments` | – | array of comments with `id`, optional `session_id`, optional target fields/anchor, optional immutable `observation`, `body`, `kind`, `action`, `state`, `author`, `channel`, replies (each with an author and optional result snapshot), `created_at`, and `updated_at`; legacy comments may omit snapshots, and general comments have no path/line/excerpt |
| `review/provenance_context` | – | internal MCP bridge context containing the selected live/snapshot session's repo, base, revision, and already-loaded parsed file diffs; used for comment capture without another jj query |
| `review/current_focus` | – | what the human is looking at: `{repo, base, revision, pane, path, line?}` where `line` is `{side, old_line, new_line, hunk_header}` when the diff cursor sits on an anchorable row (live through the TUI socket; a snapshot server reports its initial selection). Scriptable equivalent: `gander current-focus [--format json|text]`, which deliberately requires a live instance. |
| `review/stack_changes` | – | the jj stack (`trunk()..@`, oldest first): `{base, revision, changes: [{change_id, bookmarks, description, current}]}` — `description` is the full multiline message; use change ids for durable walkthrough chapters |
| `review/change_diff` | `{change_id}` | one change against its parent (`change_id-..change_id`): `{change_id, base, revision, files: [{path, status, additions, deletions}], raw}`; line numbers can anchor durable walkthrough and attention targets |
| `review/overlay` | – | the current overlay object (`version`, `ordering`, `flags`). Durable comments are returned only by `review/comments`; the one-release legacy `drafts` input is consumed during startup and is never returned |

## Write methods

Overlay suggestion methods persist the overlay atomically. Draft comments write
durable review state. Live and standalone ACP acknowledge a draft only after its
merge-aware atomic state save succeeds; standalone writes participate in the
same locked read/mutate/save transaction as CLI/MCP, and live writes merge only
their baseline-relative delta. Read/error requests never rewrite review state.
The TUI picks external durable changes up within one poll tick.

| Method | Params | Effect |
| --- | --- | --- |
| `review/set_ordering` | `{paths: [string]}` | suggested review order, highest priority first; unknown paths are rejected |
| `review/flag_section` | `{path, line?, reason, priority?}` | flag a critical section (`priority`: `critical`/`high`/`medium`/`low`, default `high`) |
| `review/draft_comment` | `{path, line?, body}` | add a draft comment for human triage; `line` is a 1-indexed diff line for the current session diff, preferring new-side/post-image coordinates with old-side fallback for removed-only lines; returns `{id}` |

Durable comment state semantics match the CLI/MCP review-state tools: `draft`
is saved/private/withheld, `todo` is ready/actionable and asks an agent to
address it regardless of kind/action, and `resolved` is retained history. Prompt
handoff selects open durable action items plus unlinked todo comments; linked
todo evidence is folded into its parent action item, and full export includes all
states. `review/draft_comment` creates a durable agent-authored onboarding
comment in draft state for triage; use CLI/MCP `comment_add` with an explicit
state for ready comments or general comments.

### Durable curation

Use `gander walkthrough set|add-step|show` and
`gander attention set|seed-heuristics|list` from ACP harnesses. MCP exposes the
same operations as typed tools, including `walkthrough_set`, `attention_set`,
`attention_list`, and `attention_seed_heuristics`. Walkthrough artifacts use
`{title, kind, body}` with `kind` `example`, `output`, `diagram`, or `note` and
render inside normal-stream narration cards. `walkthrough_set` returns
`{walkthrough, warnings}`; omitted step ids deterministically preserve matching
existing identities one-to-one after explicit ids reserve their prior slots.
Duplicate explicit or final ids reject atomically with the same error as CLI;
supplied/current targets are normalized by the shared service, and chapter
change ids must exactly match the current jj stack.

Legacy overlay curation is an explicit breaking deletion: existing `agent.json`
`chunks` and `briefs` fields are ignored rather than migrated and disappear on
the next overlay save. Ordering, flags, and pending legacy draft ingestion are
unaffected.

The public spec-file authoring path is available for drafts. Use `gander drafts list`,
`add --file spec.json`, or `remove --id <id>...` with the ACP
`review/draft_comment` shape (`{ "path": "...", "line": 12, "body": "..." }`;
`line` is a 1-indexed diff line in the current session diff, new side preferred
with old-side fallback for removed-only lines)
or `{ "drafts": [ ... ] }` for bulk adds. Draft adds append durable drafts and
print the generated ids; remove is strict and rejects unknown ids without
mutating review state. For all three groups, `--file -` or an omitted `--file`
reads stdin and writes the same review-state file that the TUI watches.

## Durable draft flow

Agent draft comments start as `state=draft`, `author.kind=agent`, and
`channel=onboarding`. The human accepts (optionally editing) or discards them in
the TUI. Acceptance preserves the comment id and promotes it to an actionable
delegation todo; discard deletes the durable draft. Agents observe current
outcomes through `review/comments`. For one release, pending legacy overlay
drafts fold into this model on startup; accepted and discarded overlay history
is consumed without recreating comments.

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
## Live presentation control (`present/*`)

`present/*` methods are only available through a live instance's per-instance
ACP Unix socket. Snapshot ACP servers return a clear live-instance error. The
owning UI loop applies these as typed commands. Both the TUI and web peer
validate against the current shared stream. The web peer broadcasts ephemeral,
coalesced `present` SSE events to every connected tab and uses the most recently
active connected tab for busy/current-focus arbitration. Both peers reject
commands while the human is in a modal/editor (`user is busy: <mode>`).

- `present/status` → `{ "active": false }` or `{ "active": true,
  "slide_index": 0, "slide_count": 5, "view": "focus", "current":
  { "step_id": "...", "part": 0, "path": "src/lib.rs", "line": 42,
  "end_line": 60, "stale": false } }`.
- `present/start` applies Focus and lands on the first current durable Spotlight
  in the normal stream.
- `present/end` ends presentation and restores the prior view if presentation
  applied Focus.
- `present/next`, `present/prev` move between durable Spotlights and return status.
- `present/goto` accepts `{ "index": 3 }` or `{ "step_id": "..." }` and
  returns status. Indexes are zero-based.
- `present/focus` accepts `{ "path": "src/foo.rs", "line": 42,
  "end_line": 60, "note": "look here" }`, validates that `path` is in the
  current diff, jumps the review view there, and surfaces `note` as a TUI
  notice.
- `present/reload` re-reads local review/agent state and re-anchors the active
  Spotlight, then returns status. Retarget/refresh preserve Focus and presenter
  state. Presenter identity is `(step_id, part)`, so insertions/reordering update
  `slide_index` without changing the current target. Removed or fingerprint-stale
  identities return `current.stale=true` with that identity and no path rather
  than silently selecting the same numeric slot.

### Presentation architecture seam

`src/presentation.rs` is the toolkit-independent semantic boundary for live
presentation. In particular, both TUI and web adapters resolve
`present/focus` through the same current-diff path/range validator and consume
the same typed target, status payload, and error taxonomy/messages. Adapters
retain only renderer concerns: terminal viewport/notice movement for the TUI,
and tab broadcast/DOM targeting for the web peer. The live socket and
`gander present` remain the canonical transport and CLI surface; this seam does
not add a protocol.
