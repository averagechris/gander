# Review artifact schema

Current schema version: `1`.

Artifacts are intentionally simple and serializable. JSON is the canonical tool format; Markdown is rendered for humans.

## JSON shape

```json
{
  "version": 1,
  "generated_at": "2026-06-30T00:00:00Z",
  "repo": "/path/to/repo",
  "revision": "@",
  "summary": "3 files (1/3 viewed), +10/-2, 1 comments",
  "files": [
    {
      "path": "src/main.rs",
      "old_path": null,
      "status": "mod",
      "viewed": true,
      "additions": 5,
      "deletions": 1,
      "fingerprint": "sha256..."
    }
  ],
  "comments": [
    {
      "id": "stable-ish-id",
      "path": "src/main.rs",
      "line": null,
      "body": "Comment body",
      "created_at": "2026-06-30T00:00:00Z"
    }
  ]
}
```

## Planned schema additions

- explicit artifact `source` block with jj operation/change IDs
- line anchors with side (`old`/`new`) and hunk header
- raw excerpt around each comment
- per-file generated/ignored/collapsed metadata
- reviewer identity/profile metadata
- review disposition (`comment`, `approve`, `needs-work`, etc.)
