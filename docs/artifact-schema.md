# Review artifact schema

Current schema version: `3`.

Artifacts are intentionally simple and serializable. JSON is the canonical tool format; Markdown is rendered for humans.

## JSON shape

```json
{
  "version": 3,
  "generated_at": "2026-06-30T00:00:00Z",
  "repo": "/path/to/repo",
  "base": "trunk()",
  "revision": "@",
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
      "fingerprint": "sha256..."
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
      "created_at": "2026-06-30T00:00:00Z"
    }
  ]
}
```

## Planned schema additions

- explicit artifact `source` block with jj operation/change IDs
- raw excerpt around each comment
- per-file ignored/collapsed metadata
- reviewer identity/profile metadata
- review disposition (`comment`, `approve`, `needs-work`, etc.)
