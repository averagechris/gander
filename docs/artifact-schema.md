# Review artifact schema

Current schema version: `5`.

Artifacts are intentionally simple and serializable. JSON is the canonical tool
format; Markdown is rendered for humans. The schema is evolving toward the
durable review-session model described in docs/vision.md: comments, tasks, and
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
  "version": 5,
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
      "state": "draft",
      "linked_task_ids": ["task-id"],
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
  "tasks": [
    {
      "id": "task-id",
      "title": "Fix the unchecked parse path",
      "status": "open",
      "action": "fix",
      "linked_comment_ids": ["stable-ish-id"],
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
- `tasks` and `walkthroughs` are exported from the active durable session when
  one is present. They are empty arrays otherwise. Task targets and walkthrough
  step targets include local file/line/symbol coordinates when recorded.
- Export is broader than import: `gander import` currently restores only
  duplicate-safe comments and viewed files whose diff fingerprints still match
  the current target. It does not restore `tasks` or `walkthroughs` from the
  artifact yet.
- Line anchors are 1-indexed diff lines. New-side/post-image anchors are
  preferred; old-side coordinates are used only as a fallback for removed-only
  lines that have no new-side line.
- `comments[].state` is one of `draft`, `todo`, `resolved`; missing values
  deserialize as `draft` for artifacts written before version 4.
- `comments[].updated_at` and `comments[].replies` are emitted when present.
  Replies are append-only objects with stable UUID `id`, `body`, and
  `created_at`. Import merges same-id comments by timestamp and unions replies,
  so newer reply/state data is not dropped as a duplicate.

## Version history

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
