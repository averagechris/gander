# Roadmap

This project should become a fast, local-first review cockpit for jj changes.

## Product principles

- **Speed first.** Startup should feel instant on normal changes. Generated or huge files must be easy to hide or collapse.
- **Review state is durable but conservative.** A file is considered viewed only if the current diff fingerprint matches the saved fingerprint.
- **Local artifacts are first-class.** JSON is for agents/tools; Markdown is for humans in Slack/email/docs.
- **Syntax awareness should help navigation.** Tree-sitter should power highlighting, symbol context, and changed-symbol outlines instead of being cosmetic only.
- **The TUI should not trap the user.** Common operations need obvious keys, non-interactive equivalents, and plain files on disk.

## Milestone 1: usable local vertical slice

Status: started.

- [x] jj-backed repo scaffold
- [x] Rust binary scaffold
- [x] Nix flake and dev shell
- [x] parse `jj show --git`
- [x] file list and diff panes
- [x] viewed-state persistence by diff fingerprint
- [x] JSON/Markdown artifact exports
- [x] basic tree-sitter Rust parse hook
- [x] unit tests for diff parsing, markdown export, and syntax hook
- [ ] render diff snapshots in tests
- [x] add config file loading

## Milestone 2: ergonomic review flow

- [x] hierarchical file tree grouped by directory
- [ ] collapse directories/files
- [ ] ignored/collapsed generated-file section
- [ ] default ignore presets for common generated files:
  - lockfiles
  - generated OpenAPI/GraphQL clients
  - vendored/minified assets
- [ ] fuzzy file search
- [ ] viewed/unviewed filters
- [x] jump to next unviewed file
- [ ] preserve cursor and scroll position per file

## Milestone 3: syntax-aware reviewing

- [ ] integrate `tree-sitter-highlight` for themed diff lines
- [ ] add language registry for common file types
- [ ] changed-symbol outline per file
- [ ] jump between changed functions/classes/modules
- [ ] parse-aware folding for unchanged context
- [ ] detect generated files by syntax/path heuristics

## Milestone 4: comments and artifacts

- [x] line-level comments
- [ ] multiline editor widget
- [ ] comment list pane
- [ ] comment states: draft/resolved/todo
- [ ] artifact schema versioning
- [ ] artifact includes stable anchors:
  - file path
  - side/new-or-old line
  - hunk header
  - diff fingerprint
- [ ] import prior artifacts
- [ ] agent-oriented artifact profile with raw excerpts

## Milestone 5: jj-native workflows

- [ ] review arbitrary revsets
- [ ] review stack/change sequences
- [ ] compare current change to prior operation for incremental re-review
- [ ] command to mark generated files viewed by policy
- [ ] optional split/squash helper affordances that shell out to jj commands only after confirmation

## Milestone 6: performance and resilience

- [ ] streaming diff parser
- [ ] lazy file/hunk rendering
- [ ] size thresholds with placeholders for huge files
- [ ] binary file handling
- [ ] robust rename/copy parsing, including quoted paths
- [ ] property/fuzz tests for diff parsing

## Delegation notes for future agents

Good isolated tasks:

- Parser agent: improve git diff parsing and add fixture tests.
- TUI agent: implement file tree/folding without touching jj or artifact modules.
- Syntax agent: build language registry and highlighting abstraction.
- Artifact agent: stabilize schema and add import/export round trips.
- Nix agent: improve package metadata and add `nix flake check` apps/checks.

Before making large changes, run:

```sh
jj status --no-pager --color=never
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
