# Changelog

## Unreleased


## v0.1.0 - 2026-07-01

### Added

- First vertical slice of the jj review TUI: parses `jj show --git` output
  into structured files and hunks with a navigable file tree and diff pane.
- Durable per-file viewed state keyed by a content fingerprint, restored only
  when the file's diff is unchanged.
- File-level and line-level review comments recorded from the TUI.
- Review artifact export as JSON or Markdown.
- Repeated `--ignore <glob>` filters with generated/noisy file labeling.
- Syntax highlighting via a built-in tree-sitter language registry.
- Nix flake packaging, dev shell, and `jj lint` local verification suite.
- Release tooling: `prepare-release`, `release-tag`, `release-artifact`,
  `build-pages`, `publish-pages`, and a `release` orchestrator, plus a
  SourceHut Pages downloads site.
