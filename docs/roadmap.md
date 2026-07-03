# Roadmap

This project should become a fast, local-first review cockpit for jj changes,
and eventually a collaborative one: agents should be able to help conduct the
review, not just watch it (see milestones 7-9 and docs/decisions.md for the
directional decisions behind them).

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

Status: complete.

- [x] review arbitrary revsets (`--base`/`--rev` accept revsets; `R` opens a
  free-form base/tip revset input in the TUI)
- [x] review stack/change sequences (`>`/`<` step through `trunk()..@`
  change-by-change against each change's parent)
- [x] compare current change to prior operation for incremental re-review
  (`I` opens a jj operation picker; unchanged files are marked viewed,
  changed/new files marked unviewed)
- [x] command to mark generated files viewed by policy
- [x] optional split/squash helper affordances that shell out to jj commands
  only after confirmation (`!` popup with a verbatim-command confirm step)

## Milestone 6: performance and resilience

Status: complete.

- [x] streaming diff parser (`DiffSet::parse_reader` builds files
  incrementally from any `BufRead`, fingerprints unchanged)
- [x] lazy file/hunk rendering (diff pane constructs styled lines only for
  the visible viewport window)
- [x] size thresholds with placeholders for huge files
  (`[limits].max-diff-lines`, expand with `L`)
- [x] binary file handling (placeholder rows for `Binary files ...` and
  `GIT binary patch` diffs; files stay markable as viewed)
- [x] robust rename/copy parsing, including quoted paths
- [x] property/fuzz tests for diff parsing (proptest: no-panic, streaming
  equivalence, count/numbering invariants, quoted-path round trips)

## Milestone 7: agent-collaborative review (ACP)

The long-term direction: gander should not just show a diff, it should host a
review that an agent can help conduct.

Status: first full vertical slice complete (see docs/acp.md). Full Agent
Client Protocol schema compliance is future work.

- [x] expose the review session over ACP so agents can read the diff, comments,
  and viewed state (`gander acp`: line-delimited JSON-RPC 2.0 on stdio)
- [x] agent-suggested review ordering: rearrange the review by priority and
  risk instead of file order (`review/set_ordering` + `A` toggle; the TUI
  polls the shared `.gander/agent.json` overlay live)
- [x] agent-flagged critical sections that are surfaced/pinned in the UI
  (`review/flag_section`; red `!` gutter/file pins + `F` flag list popup)
- [x] review chunks: break a change into reviewable units that can span or
  subdivide files, rather than reviewing strictly file-by-file
  (`review/set_chunks` + `S` chunk popup with part-level jumps)
- [x] two-way feedback: agents draft comments/questions into the session; the
  human accepts, edits, or discards them before export
  (`review/draft_comment` + `D` triage popup; dispositions are written back
  to the overlay for agents to observe)
- [x] artifact profile for agent consumption with raw excerpts and stable
  anchors (see milestone 4)
- [x] live ACP endpoint hosted by the TUI on `.gander/acp.sock` (Unix);
  `gander acp` bridges stdio to it when live, so agent-spawned servers see
  the current session instead of a startup snapshot
- [x] summon a configured agent from the TUI (`[agent] command` + `@` key or
  autostart; agent-agnostic shell command, logged to `.gander/agent.log`,
  lifecycle owned by gander)

## Milestone 8: state hygiene — get out of the project directory

Runtime state currently lands in a project-local `.gander/` dir that users
must gitignore in every repo. That is tool droppings, not polish. Decision
record: docs/decisions.md D6. This milestone should land **before**
milestone 9, since the instance registry and MCP routing build on the new
locations.

- [ ] resolve per-workspace state under the XDG state dir
  (`~/.local/state/gander/<workspace-key>/`), keyed by a hash+slug of the
  canonicalized workspace root; respect `XDG_STATE_HOME`
- [ ] ephemeral endpoints (ACP/MCP sockets, instance registry, agent logs)
  under `XDG_RUNTIME_DIR` when set, else the state dir
- [ ] keep committed `gander.toml` and XDG user config; deprecate the
  `.gander/config.toml` layer
- [ ] default artifact output moves off `.gander/review.*` (stdout or
  explicit paths)
- [ ] one-release migration fallback: read legacy `.gander/` state when the
  new location is empty
- [ ] a `gander paths`-style command that prints resolved locations for
  debugging

## Milestone 9: seamless agent workflows (MCP + multi-instance)

The target flow: open gander in a workstream, see the change is large, and
have your already-running harness organize and narrate the review — no
protocol knowledge, no manual orchestration. Decision records:
docs/decisions.md D3, D4, D5.

- [ ] per-instance sockets + instance registry (one gander per workstream;
  workspace root, target, summary, socket, pid, `last_input_at`; cleaned on
  exit; replaces the "second TUI loses the socket" behavior)
- [ ] `gander mcp`: MCP stdio server (prefer the `rmcp` SDK) bridging to the
  live instance by cwd; tools: `review_summary`, `review_files`,
  `file_diff`, `comments`, `set_ordering`, `flag_section`, `set_chunks`,
  `draft_comment`, `current_focus`, `list_reviews`
- [ ] `current_focus` plumbing in the TUI (selected file/line/hunk +
  `last_input_at` heartbeat in the registry)
- [ ] large-change nudge: when a review exceeds a size threshold, hint that
  an agent can organize it (`@` or the harness)
- [ ] tour mode (`T`): step through agent-suggested chunks in order with
  rationale displayed; auto-mark viewed on advance; esc returns to free
  navigation
- [ ] (exploratory, may not ship) ask popup: one-shot "explain this line"
  question routed to the harness, single streamed answer in a popup — only
  if the split-pane + `current_focus` flow leaves a real gap
- [ ] docs: harness setup recipes (opencode/claude MCP registration,
  split-pane workflow, attach-to-running-server summon commands)

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

Feature burn-down (2026-07, second pass): milestones 5, 6, and 7 completed —
revset input, stack stepping, incremental re-review against prior operations,
confirmed split/squash helpers, streaming diff parsing, lazy diff rendering,
size/binary placeholders, diff parser property tests, and the ACP
agent-collaboration slice (session server, ordering, flags, chunks, two-way
drafts). Also fixed along the way: uppercase char keybindings now match
whether or not the terminal reports the SHIFT modifier.

Known debt, in priority order:

- Runtime state pollutes project directories (`.gander/`); superseded by
  milestone 8 (docs/decisions.md D6).
- The ACP surface is a minimal JSON-RPC method set (`gander-acp` v1), not the
  published Agent Client Protocol schema. Direction changed (docs/decisions.md
  D5): the agent-facing surface becomes MCP tools (milestone 9); the JSON-RPC
  socket remains internal plumbing rather than growing toward spec ACP.
- The standalone `gander acp` server (no TUI running) snapshots the
  diff/comments at startup; with a live TUI the socket bridge serves current
  state, so this only affects agents working without a human in the loop.
- Diff-pane scroll offsets count logical rows, not wrapped display lines, so
  scrolling within files containing very long wrapped lines is approximate
  (pre-existing behavior, kept by the lazy renderer).
- jj split/squash helpers only cover non-interactive path-scoped invocations;
  interactive splitting is out of scope for the TUI popup.

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
