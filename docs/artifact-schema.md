# Review artifact schema

Current schema version: `7`.

Artifacts are intentionally simple and serializable. JSON is the canonical tool
format; Markdown is rendered for humans. The schema is evolving toward the
durable review-session model described in docs/vision.md: comments, optional action items, and
walkthroughs are local review state that external harnesses may consume or
publish elsewhere.

## Profiles

Artifacts render in one of two profiles:

- `human` (default): the compact shape below, without raw diff content.
- `agent`: adds raw excerpts so tools can reason about the change without
  re-running jj:
  - a top-level `"profile": "agent"` marker
  - `files[].hunks`: the full raw hunks (header, ranges, and per-line
    kind/old_line/new_line/text)
  - `comments[].excerpt`: raw diff lines around each comment anchor
    (3 context lines on each side; file-level comments excerpt the top of
    the first hunk)

Select the profile with `gander export --profile agent`,
`gander tui --artifact-profile agent`, or `[artifact] profile = "agent"` in
config.

## JSON shape

```json
{
  "version": 7,
  "generated_at": "2026-06-30T00:00:00Z",
  "repo": "/path/to/repo",
  "base": "trunk()",
  "revision": "@",
  "profile": "agent",
  "summary": "3 files (1/3 viewed), +10/-2, 1 comments",
  "session": {
    "id": "review-session-id",
    "title": "Review handoff"
  },
  "files": [
    {
      "path": "src/main.rs",
      "old_path": null,
      "status": "mod",
      "viewed": true,
      "generated": false,
      "additions": 5,
      "deletions": 1,
      "fingerprint": "sha256...",
      "hunks": [
        {
          "header": "@@ -39,6 +39,7 @@ fn render() {",
          "old_start": 39,
          "old_len": 6,
          "new_start": 39,
          "new_len": 7,
          "lines": [
            { "kind": "context", "old_line": 39, "new_line": 39, "text": "fn render() {" },
            { "kind": "added", "new_line": 42, "text": "    new_call();" }
          ]
        }
      ]
    }
  ],
  "comments": [
    {
      "id": "stable-ish-id",
      "session_id": "review-session-id",
      "path": "src/main.rs",
      "line": 42,
      "anchor": {
        "type": "line",
        "path": "src/main.rs",
        "old_path": null,
        "side": "new",
        "line": 42,
        "old_line": 41,
        "new_line": 42,
        "hunk_header": "@@ -39,6 +39,7 @@ fn render() {",
        "hunk_old_start": 39,
        "hunk_old_len": 6,
        "hunk_new_start": 39,
        "hunk_new_len": 7,
        "hunk_index": 0,
        "line_index": 4,
        "line_kind": "added",
        "line_text": "    new_call();",
        "line_fingerprint": "sha256...",
        "diff_fingerprint": "sha256..."
      },
      "body": "Comment body",
      "state": "todo",
      "linked_action_item_ids": ["action-item-id"],
      "created_at": "2026-06-30T00:00:00Z",
      "updated_at": "2026-06-30T00:05:00Z",
      "replies": [
        {
          "id": "reply-uuid",
          "body": "Acknowledged; resolving after the fix.",
          "created_at": "2026-06-30T00:05:00Z"
        }
      ],
      "excerpt": [
        { "kind": "context", "old_line": 40, "new_line": 40, "text": "    before();" },
        { "kind": "added", "new_line": 42, "text": "    new_call();" },
        { "kind": "context", "old_line": 42, "new_line": 43, "text": "    after();" }
      ]
    }
  ],
  "action_items": [
    {
      "id": "action-item-id",
      "title": "Fix the unchecked parse path",
      "status": "open",
      "close_disposition": null,
      "action": "fix",
      "linked_comment_ids": ["stable-ish-id"],
      "external_tickets": ["LIN-123"],
      "target": { "file": "src/main.rs", "line": 42, "end_line": null }
    }
  ],
  "walkthroughs": [
    {
      "id": "walkthrough-id",
      "title": "Suggested review order",
      "steps": [
        {
          "id": "step-id",
          "title": "Start with the renderer",
          "why": "This establishes the new data shape.",
          "body": "Confirm the serialized fields before reading callers.",
          "target": { "file": "src/main.rs", "line": 42, "symbol": "render" }
        }
      ]
    }
  ]
}
```

Notes:

- `profile`, `files[].hunks`, and `comments[].excerpt` are omitted entirely
  in the `human` profile.
- `session` is present when the exported change matches an open durable review
  session; it includes the session `id` and optional `title`.
- `comments[].session_id` identifies the durable session that owns a newly
  created comment. Version 5 and older comments omit it; those legacy unscoped
  comments remain visible in matching-session exports for compatibility.
- `action_items` and `walkthroughs` are exported from the active durable session when
  one is present. They are empty arrays otherwise. Action-item targets and walkthrough
  step targets include local file/line/symbol coordinates when recorded.
- Todo comments are the primary implicit action feedback. A todo comment linked
  to an open durable action item is folded into that item as evidence in handoff
  output and is not duplicated as a separate action item. Ordinary comments
  (`draft` notes and resolved history) are not action items.
- Durable action items are optional higher-level coordination records. They can
  link many comments via `linked_comment_ids` and carry opaque
  `external_tickets` strings/URLs. Gander records those references only; it does
  not fetch from or post to forges or ticket systems.
- Export is broader than import: `gander import` currently restores only
  duplicate-safe comments and viewed files whose diff fingerprints still match
  the current target. It does not restore `action_items` or `walkthroughs` from the
  artifact yet.
- Line anchors are 1-indexed diff lines. New-side/post-image anchors are
  preferred; old-side coordinates are used only as a fallback for removed-only
  lines that have no new-side line.
- `comments[].path` is optional as of version 6. It and `line`, `anchor`, and
  `excerpt` are absent/null for general session comments. General comments
  intentionally have no location or excerpt and do not select unrelated hunks
  for handoff reference output. Existing version 5 anchored comments retain
  their string `path` and deserialize unchanged.
- `comments[].state` is one of `draft`, `todo`, `resolved`. `draft` means saved,
  private, and withheld from prompt/delegate handoff; `todo` means ready and
  actionable (every todo asks an agent to address it regardless of kind/action);
  `resolved` is retained history. Missing values deserialize as `draft` for
  artifacts written before version 4 and for persisted state written before
  `CommentState` existed.
- Full exports include all comment states. Prompt handoff and delegation select
  only open durable action items plus unlinked `todo` comments by default; draft
  comments are rejected by explicit delegate selectors unless first readied.
- `comments[].updated_at` and `comments[].replies` are emitted when present.
  Replies are append-only objects with stable UUID `id`, `body`, and
  `created_at`. Import merges same-id comments by timestamp and unions replies,
  so newer reply/state data is not dropped as a duplicate.

## Version history

- `7`: `tasks` is renamed to `action_items`, comment backrefs are
  `linked_action_item_ids`, action items can link many comments and external
  ticket refs, and closed action items record `completed`, `dismissed`, or
  `deferred` disposition. Deferred items require at least one external ticket.
- `6`: comments may carry a durable `session_id`, and `path` is optional so a
  comment can be general to its session. Version 5 anchored comments remain
  readable; missing `session_id` is treated as legacy-visible compatibility
  state.
- `5`: active durable session metadata plus session `tasks` and `walkthroughs`;
  comment `updated_at` and append-only `replies` are backward-compatible fields.
- `4`: comment `state`, optional `profile` marker, agent-profile `hunks` and
  `excerpt` blocks.
- `3`: stable anchors (side, hunk header, line/diff fingerprints).

## Planned schema additions

- explicit artifact `source` block with jj operation/change IDs
- per-file ignored/collapsed metadata
- reviewer identity/profile metadata
- review disposition/intent (`comment`, `approve`, `needs-work`, etc.) as
  local state, not a direct forge-posting integration
