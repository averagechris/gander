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
- [x] render diff snapshots in tests (insta TUI buffer snapshots)
- [x] add config file loading

## Milestone 2: ergonomic review flow

- [x] hierarchical file tree grouped by directory
- [x] collapse directories/files
- [x] ignored/collapsed generated-file section (`generated/noisy` group + hide toggle)
- [x] default ignore presets for common generated files:
  - lockfiles
  - generated OpenAPI/GraphQL clients
  - vendored/minified assets
- [ ] fuzzy file search
- [ ] viewed/unviewed filters
- [x] jump to next unviewed file
- [x] preserve cursor and scroll position per file
- [x] configurable keybindings

## Milestone 3: syntax-aware reviewing

- [ ] integrate `tree-sitter-highlight` for themed diff lines
- [ ] add language registry for common file types
- [ ] changed-symbol outline per file
- [ ] jump between changed functions/classes/modules
- [ ] parse-aware folding for unchanged context
- [ ] detect generated files by syntax/path heuristics

## Milestone 4: comments and artifacts

- [x] line-level comments
- [x] multiline editor widget
- [x] comment navigation
- [ ] comment list pane
- [ ] comment states: draft/resolved/todo
- [x] artifact schema versioning (current version documented in docs/artifact-schema.md)
- [x] artifact includes stable anchors:
  - file path
  - side/new-or-old line
  - hunk header
  - diff fingerprint
- [x] import prior artifacts (`import` subcommand, fingerprint-guarded)
- [ ] agent-oriented artifact profile with raw excerpts

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
- [ ] robust rename/copy parsing, including quoted paths
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
- [ ] artifact profile for agent consumption with raw excerpts and stable
  anchors (see milestone 4)

## Known debt (from the 2026-07 pre-MVP code review)

Fixed during the review: base picker filter dropped `g`/`G`/shifted chars,
diff-bottom scrolled past content, `tui.rs` monolith split into submodules,
duplicate tree builders, no-op `apply_viewed_state` hook, non-atomic state
saves.

Still open, roughly priority ordered:

1. **`ReviewSession` is the next split candidate** (`src/app.rs`, ~1200
   non-test lines). It mixes navigation, comments, syntax caching, and diff-row
   building. Extract diff-row construction and the syntax cache into
   submodules the way `tui/` was split.
2. **`diff_rows_for_selected_file()` allocates fresh rows on every call** and
   is called several times per event (draw, cursor movement, anchor lookup).
   First thing that will feel slow on large files. A per-file memoized row
   cache keyed by diff fingerprint would fix most of it; pairs with the
   milestone 6 lazy-rendering work.
3. **Comment IDs can collide.** IDs are `{timestamp_millis}-{count}`, so
   delete-then-add within the same millisecond can reuse an ID, and artifact
   import dedupes by ID. Use a UUID or a persisted counter before artifacts
   are shared between people/agents.
4. **Diff parser path handling is fragile for exotic paths.** Quoted paths and
   paths with spaces in `diff --git` lines, and rename/copy similarity lines,
   are not parsed robustly. Paths key state, comments, and matchers, so a
   malformed path degrades several features at once (tracked in milestone 6).
5. **State is only saved on clean quit.** Atomic writes prevent corruption,
   but a panic mid-session loses viewed marks and comments. Consider
   save-on-mark or a panic-hook save.
6. **Footer hints are two giant `format!` calls** in `src/tui/render.rs`. A
   small `Vec<(key, label)>` builder would make adding actions less
   error-prone.
7. **`can_run_jj_with_timeout` polls with 10ms sleeps** (`src/jj.rs`). Works,
   but proper `wait_timeout` semantics would be cleaner.
8. **`Esc` doubles as quit in normal mode.** Once more modes/popups exist,
   users will expect Esc to only dismiss the current layer. Revisit the
   default `quit` binding.

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
