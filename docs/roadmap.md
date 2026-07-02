# Roadmap

This project should become a fast, local-first review cockpit for jj changes,
and eventually a collaborative one: agents connected over ACP should be able to
help conduct the review, not just watch it (see milestone 7).

## Product principles

- **Speed first.** Startup should feel instant on normal changes. Generated or huge files must be easy to hide or collapse.
- **Review state is durable but conservative.** A file is considered viewed only if the current diff fingerprint matches the saved fingerprint.
- **Local artifacts are first-class.** JSON is for agents/tools; Markdown is for humans in Slack/email/docs.
- **Syntax awareness should help navigation.** Tree-sitter should power highlighting, symbol context, and changed-symbol outlines instead of being cosmetic only.
- **The TUI should not trap the user.** Common operations need obvious keys, non-interactive equivalents, and plain files on disk.

## Milestone 1: usable local vertical slice

Status: complete.

- [x] jj-backed repo scaffold
- [x] Rust binary scaffold
- [x] Nix flake and dev shell
- [x] parse `jj show --git`
- [x] file list and diff panes
- [x] viewed-state persistence by diff fingerprint
- [x] JSON/Markdown artifact exports
- [x] basic tree-sitter Rust parse hook
- [x] unit tests for diff parsing, markdown export, and syntax hook
- [x] render diff snapshots in tests (insta TUI buffer snapshots)
- [x] add config file loading

## Milestone 2: ergonomic review flow

Status: complete.

- [x] hierarchical file tree grouped by directory
- [x] collapse directories/files
- [x] ignored/collapsed generated-file section (`generated/noisy` group + hide toggle)
- [x] default ignore presets for common generated files:
  - lockfiles
  - generated OpenAPI/GraphQL clients
  - vendored/minified assets
- [x] fuzzy file search (`/` popup)
- [x] viewed/unviewed filters (`f` cycles all/unviewed/viewed)
- [x] jump to next unviewed file
- [x] preserve cursor and scroll position per file
- [x] configurable keybindings

## Milestone 3: syntax-aware reviewing

Status: complete for built-in grammars. External grammar/query loading is
future work.

- [x] integrate `tree-sitter-highlight` for themed diff lines
- [x] add language registry for common file types
- [x] changed-symbol outline per file (`o` popup)
- [x] jump between changed functions/classes/modules (`]`/`[`)
- [x] parse-aware folding for unchanged context (`z`, symbol-labelled folds)
- [x] detect generated files by syntax/path heuristics (header markers like
  `@generated`/`DO NOT EDIT` plus glob presets)

## Milestone 4: comments and artifacts

Status: complete.

- [x] line-level comments
- [x] multiline editor widget
- [x] comment navigation
- [x] comment list pane (`C` popup: jump, cycle state, delete)
- [x] comment states: draft/resolved/todo
- [x] artifact schema versioning (current version documented in docs/artifact-schema.md)
- [x] artifact includes stable anchors:
  - file path
  - side/new-or-old line
  - hunk header
  - diff fingerprint
- [x] import prior artifacts (`import` subcommand, fingerprint-guarded)
- [x] agent-oriented artifact profile with raw excerpts (`--profile agent`,
  schema v4)

## Milestone 5: jj-native workflows

- [ ] review arbitrary revsets
- [ ] review stack/change sequences
- [ ] compare current change to prior operation for incremental re-review
- [x] command to mark generated files viewed by policy
- [ ] optional split/squash helper affordances that shell out to jj commands only after confirmation

## Milestone 6: performance and resilience

- [ ] streaming diff parser
- [ ] lazy file/hunk rendering
- [ ] size thresholds with placeholders for huge files
- [ ] binary file handling
- [x] robust rename/copy parsing, including quoted paths
- [ ] property/fuzz tests for diff parsing

## Milestone 7: agent-collaborative review (ACP)

The long-term direction: gander should not just show a diff, it should host a
review that an agent can help conduct.

- [ ] expose the review session over ACP so agents can read the diff, comments,
  and viewed state
- [ ] agent-suggested review ordering: rearrange the review by priority and
  risk instead of file order
- [ ] agent-flagged critical sections that are surfaced/pinned in the UI
- [ ] review chunks: break a change into reviewable units that can span or
  subdivide files, rather than reviewing strictly file-by-file
- [ ] two-way feedback: agents draft comments/questions into the session; the
  human accepts, edits, or discards them before export
- [x] artifact profile for agent consumption with raw excerpts and stable
  anchors (see milestone 4)

## Known debt (from the 2026-07 pre-MVP code review)

Fixed during the review: base picker filter dropped `g`/`G`/shifted chars,
diff-bottom scrolled past content, `tui.rs` monolith split into submodules,
duplicate tree builders, no-op `apply_viewed_state` hook, non-atomic state
saves.

Fixed post-MVP (2026-07): `app.rs` split into session/diff-row/syntax-cache
submodules; diff rows memoized per file fingerprint; comment ids switched to
UUIDs; diff parser handles quoted/spaced paths, rename/copy lines, and
`/dev/null` markers with header-only marker parsing; review state autosaves
after each TUI event instead of only on clean quit; footer hints built from a
declarative segment list; jj probe uses proper `wait_timeout` semantics; `Esc`
now dismisses the current layer (range selection, notices) instead of
quitting.

Feature burn-down (2026-07): milestones 2, 3, and 4 completed — fuzzy file
search, viewed filters, changed-symbol outline/jumps, symbol-aware context
folding, generated-content detection, comment states + list pane, and the
agent artifact profile (schema v4).

No known debt is currently tracked. New findings should be added here in
priority order.

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
