# Review artifact schema

Current schema version: `4`.

Artifacts are intentionally simple and serializable. JSON is the canonical tool format; Markdown is rendered for humans.

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
  "version": 4,
  "generated_at": "2026-06-30T00:00:00Z",
  "repo": "/path/to/repo",
  "base": "trunk()",
  "revision": "@",
  "profile": "agent",
  "summary": "3 files (1/3 viewed), +10/-2, 1 comments",
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
      "created_at": "2026-06-30T00:00:00Z",
      "excerpt": [
        { "kind": "context", "old_line": 40, "new_line": 40, "text": "    before();" },
        { "kind": "added", "new_line": 42, "text": "    new_call();" },
        { "kind": "context", "old_line": 42, "new_line": 43, "text": "    after();" }
      ]
    }
  ]
}
```

Notes:

- `profile`, `files[].hunks`, and `comments[].excerpt` are omitted entirely
  in the `human` profile.
- `comments[].state` is one of `draft`, `todo`, `resolved`; missing values
  deserialize as `draft` for artifacts written before version 4.

## Version history

- `4`: comment `state`, optional `profile` marker, agent-profile `hunks` and
  `excerpt` blocks.
- `3`: stable anchors (side, hunk header, line/diff fingerprints).

## Planned schema additions

- explicit artifact `source` block with jj operation/change IDs
- per-file ignored/collapsed metadata
- reviewer identity/profile metadata
- review disposition (`comment`, `approve`, `needs-work`, etc.)
