# Agent-collaborative review (ACP)

`gander acp` hosts the review session for agents: a line-delimited JSON-RPC
2.0 server on stdio (the transport style used by the Agent Client Protocol).
Agents read the diff, comments, and viewed state, and write review
suggestions into the shared **agent overlay** (`.gander/agent.json`), which
the running TUI polls and surfaces live.

```sh
gander --base 'trunk()' --rev '@' acp
```

Each request/response is one JSON object per line. Requests without an `id`
are treated as notifications and get no response.

## Live session over the TUI socket

While the TUI runs it also hosts the same protocol on a Unix socket,
`.gander/acp.sock` (Unix platforms only). Requests answered there hit the
**live** session: current viewed state, comments, and the active review
target, and agent writes surface in the UI within one event-loop tick —
no file polling latency.

Two ways to reach it:

- connect to `.gander/acp.sock` directly and speak line-delimited JSON-RPC;
- run `gander acp` as usual: when the socket is live it transparently
  bridges stdio to the TUI, so agent clients that spawn `gander acp` as a
  subprocess get the live session for free. Without a running TUI it falls
  back to serving a snapshot loaded at startup.

If the socket cannot be bound (say, a second gander TUI on the same repo)
the TUI shows a notice and collaboration degrades gracefully to the overlay
file.

## Read methods

| Method | Params | Result |
| --- | --- | --- |
| `initialize` | – | protocol name, version, capability list |
| `review/summary` | – | repo, base, revision, summary line |
| `review/files` | – | array of `{path, old_path, status, additions, deletions, viewed, generated, fingerprint}` |
| `review/file_diff` | `{path}` | `{path, fingerprint, raw}` (raw git-style diff) |
| `review/comments` | – | array of `{id, path, line, end_line, body, state}` |
| `review/overlay` | – | the full agent overlay (ordering, flags, chunks, drafts with dispositions) |

## Write methods

All write methods persist the overlay atomically; the TUI picks changes up
within one poll tick.

| Method | Params | Effect |
| --- | --- | --- |
| `review/set_ordering` | `{paths: [string]}` | suggested review order, highest priority first; unknown paths are rejected |
| `review/flag_section` | `{path, line?, reason, priority?}` | flag a critical section (`priority`: `critical`/`high`/`medium`/`low`, default `high`) |
| `review/set_chunks` | `{chunks: [{id?, title, rationale?, parts: [{path, start_line?, end_line?}]}]}` | replace the reviewable units |
| `review/draft_comment` | `{path, line?, body}` | add a draft comment for human triage; returns `{id}` |

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
