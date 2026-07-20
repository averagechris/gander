# Roadmap

This project should become a fast, local-first review workspace for jj-visible
changes: humans and agents use durable review sessions to understand, annotate,
walk through, and act on code changes. See [docs/vision.md](vision.md) for the
north star, non-goals, and post-MVP milestone plan.

## Product principles

- **Speed first.** Startup should feel instant on normal changes. Generated or huge files must be easy to hide or collapse.
- **Review state is durable but conservative.** A file is considered viewed only if the current diff fingerprint matches the saved fingerprint.
- **Local artifacts are first-class.** JSON is for agents/tools; Markdown is for humans in Slack/email/docs.
- **Gander reads code state and writes review state.** Users or external
  harnesses prepare/fetch work; Gander inspects jj state and persists review
  sessions, comments, optional action items, and walkthroughs without mutating the code
  workspace or posting to forges.
- **CLI parity is required.** Anything available through MCP, the TUI, or a
  future web UI must have an equivalent scriptable CLI path over the same core
  services.
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
  polls the shared agent overlay live)
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
- [x] live ACP endpoint hosted by the TUI on a Unix socket (see `gander paths`);
  `gander acp` bridges stdio to it when live, so agent-spawned servers see
  the current session instead of a startup snapshot
- [x] summon a configured agent from the TUI (`[agent] command` + `@` key or
  autostart; agent-agnostic shell command, logged to the workspace agent log,
  lifecycle owned by gander)

## Milestone 8: state hygiene — get out of the project directory

Runtime state used to land in a project-local `.gander/` dir that users had
to gitignore in every repo. Decision record: docs/decisions.md D6.

Status: complete. Legacy `.gander/` state is still read (copied forward) for
one release; the `.gander/config.toml` layer still loads with a deprecation
warning.

- [x] resolve per-workspace state under the XDG state dir
  (`~/.local/state/gander/<workspace-key>/`), keyed by a hash+slug of the
  canonicalized workspace root; respect `XDG_STATE_HOME`
- [x] ephemeral endpoints (ACP/MCP sockets, instance registry, agent logs)
  under `XDG_RUNTIME_DIR` when set, else the state dir
- [x] keep committed `gander.toml` and XDG user config; deprecate the
  `.gander/config.toml` layer (still loads, with a startup warning)
- [x] default artifact output moves off `.gander/review.*` (stdout unless
  an explicit path or `[artifact] output-dir` is given)
- [x] one-release migration fallback: read legacy `.gander/` state when the
  new location is empty (copied forward on startup)
- [x] a `gander paths` command that prints resolved locations for
  debugging

## Milestone 9: seamless agent workflows (MCP + multi-instance)

The target flow: open gander in a workstream, see the change is large, and
have your already-running harness organize and narrate the review — no
protocol knowledge, no manual orchestration. Decision records:
docs/decisions.md D3, D4, D5.

Status: complete (the exploratory ask popup was deliberately not built; see
below).

- [x] per-instance sockets + instance registry (one gander per workstream;
  workspace root, target, summary, socket, pid, `last_input_at`; cleaned on
  exit; replaces the "second TUI loses the socket" behavior; `gander acp`
  routes to the live instance by cwd via the registry)
- [x] `gander mcp`: MCP stdio server (rmcp SDK) bridging to the live
  instance by cwd through the instance registry, with a snapshot fallback;
  tools: `review_summary`, `review_files`, `file_diff`, `comments`,
  `set_ordering`, `flag_section`, `set_chunks`, `draft_comment`,
  `current_focus`, `list_reviews`
- [x] `current_focus` plumbing in the TUI (selected file/line/hunk via the
  `review/current_focus` method + `last_input_at` heartbeat in the
  registry)
- [x] large-change nudge: when a review exceeds the `[limits]`
  nudge thresholds (`nudge-diff-lines`/`nudge-files`, 0 disables) and no
  agent has organized it yet, the footer hints that an agent can (`@` or
  the harness); re-raised on retarget
- [x] tour mode (`T`): step through agent-suggested review chunks in order with
  rationale displayed in a bottom panel; auto-mark viewed on advance; esc
  returns to free navigation (evolved into walkthrough-backed zen mode in
  milestone 10)
- [x] docs: harness setup recipes (docs/harness-setup.md — opencode/claude/
  codex MCP registration, split-pane workflow, attach-to-running-server
  summon commands)
- ~~ask popup~~ deliberately deferred (2026-07): one-shot "explain this
  line" routed to the harness would require gander to become a
  harness-API/spec-ACP client (docs/decisions.md D4). The split-pane +
  `current_focus` flow covers the need — the human asks "what am I looking
  at?" in the harness chat. Revisit only if that leaves a real gap.

## Milestone 10: focused diff UX

A diff pane that makes changes obvious at a glance and lets the reviewer
strip away everything else. Design: docs/focused-diff-ux.md.

Status: designed, in progress.

- [x] word-level change highlights within modified line pairs (`similar`
  crate, similarity-thresholded), on by default
- [x] added/removed line background tints, on by default
- [x] optional colored gutter change bar
- [x] `[diff]` config section + view-options popup (`V`) with runtime,
  session-only toggles; every toggle bindable via `[keybindings]`
- [x] collapsible file pane (`w`) with never-trap re-show and file path in
  the diff pane title
- [x] side-by-side view (`|`) as a render-time projection over the unified
  rows (cursor/comments/anchors unchanged), unified fallback on narrow
  terminals
- [x] per-gap hunk context expansion (`+`/`=`/`-`) backed by lazy
  `jj file show` content; expanded rows not commentable in v1
- [x] zen mode (tour mode + the above + agent-curated walkthroughs): tour
  evolved into a non-modal walkthrough layer (`T`/`Z`) — file pane
  hidden, out-of-stop rows dimmed, progress panel, file-order fallback when no
  walkthrough exists, full review vocabulary available mid-walkthrough
  (docs/focused-diff-ux.md §6)

## Milestone 11: first-class review sessions

Design: [docs/vision.md](vision.md).

Status: in progress. First persistence slice landed: serializable session,
task, walkthrough, comment kind, and action-intent fields are now part of
`ReviewState` with backward-compatible deserialization.

- [x] promote the durable session to Gander's core product object, above raw
  diff/artifact exports
- [x] model comments, optional durable action items, walkthrough steps, stable
  targets, and migration-friendly defaults in the state layer
- [ ] add reviewer/author metadata and richer session lifecycle commands
- [x] keep existing viewed-state/comment/artifact behavior working through the
  new session model
- [x] maintain the product boundary: write Gander review state, not code state
  or remote-provider state

## Milestone 12: complete CLI automation surface

Status: mostly complete. Mutating session/comment/task/walkthrough commands,
file and hunk queries, JSON/Markdown/HTML exports, and the CLI/MCP parity table
have landed. Remaining work is a stable JSON contract audit.

- [x] add scriptable commands for sessions, files, hunks, comments, action items,
  walkthroughs, and exports
- [x] provide initial stable JSON output suitable for harnesses and agents
- [x] ensure every MCP capability has a documented CLI equivalent backed by the
  same core service
- [x] make CLI automation usable without MCP, for users who avoid MCP context
  pollution
- [ ] audit and freeze stable JSON output contracts across commands

## Milestone 13: TUI over the shared session core

Status: in progress. Action tags, task popup, and walkthrough authoring have
landed; zen/focused modes still need to become views over the durable
walkthrough/session state.

- [ ] make TUI state mutations call the same services as the CLI/MCP adapters
- [x] add first-class key hunk and walkthrough editing affordances
- [x] support action-tagged comments/action items (`fix`, `explain`, `test`,
  `follow-up`) for agent handoff
- [ ] keep zen/focused review modes as views over walkthrough/session state —
  direction refined into the attention map (milestone 18,
  docs/attention.md)

## Milestone 14: MCP parity adapter

Status: complete for the parity adapter. Caveat: state-file tools load/save the
persisted state directly, so avoid concurrent use with an active TUI that may
later autosave an older in-memory snapshot.

- [x] make `gander mcp` a thin adapter over the same core API used by the CLI
- [x] retain live-instance routing and `current_focus` where useful
- [x] document the CLI equivalent for each tool
- [x] avoid making MCP the only or most capable automation path
- [ ] route parity tools through the live TUI session (or add a state-reload
  handshake) so state-file writes cannot race an active TUI autosave; until
  then the documented guidance is to use them when no TUI is open

## Milestone 15: static web walkthrough export

Status: partially complete. `gander export html` now writes a self-contained
static review page; a pages-style hosted tour remains separate future work.

- [x] export a self-contained local HTML artifact with walkthrough navigation,
  key hunks, comments, and task state
- [ ] keep export local/static first, with no hosted sync or direct forge
  integration
- [x] use the same session data as JSON/Markdown exports

## Milestone 16: optional local web UI

Status: future.

- [ ] add an interactive local browser UI only after the session core is stable
- [ ] expose the same capabilities as the TUI/CLI where appropriate
- [ ] preserve the no-code-mutation and forge-agnostic boundaries

## Milestone 17: annotation channels

Every annotation knows who wrote it and who it is for. Design:
[docs/annotations.md](annotations.md). Subsumes backlog item 2
(reviewer/author metadata).

Status: complete.

- [x] add `author` (Identity: human/agent + name) and `channel`
  (onboarding/delegation/collaboration/note) to comments, plus reply authors, with
  serde-defaulted migration (todo → delegation, else note)
- [x] infer channel from context (thread > onboarding card > agent attached >
  foreign change author > note); `[comments] default-channel` pins it
- [x] channel indicator in the comment editor: channel-colored border + one
  compact chip; one key cycles channel live while composing
- [x] one channel color language across editor, cards, gutter, and comment
  list (onboarding=accent, delegation=warning, collaboration=info, note=muted)
- [x] fold agent drafts into the comment model (author=agent, state=draft);
  retire the overlay draft bucket; channel resolved at accept time
- [x] `[identity]` config for the human; agent identities from agent config
- [x] `--channel` filters on comment CLI/MCP surfaces
- [x] `--profile team` export: collaboration threads only, JSON forge-mappable
  anchors + fingerprints, optional session disposition
  (comment/approve/request-changes)
- [x] import preserves foreign authorship so two humans can review over an
  artifact file today

Deferred follow-up (not part of M17 completion):

- [ ] add exact durable source-comment linkage for delegation requests created
  from onboarding annotations; M17 only preserves the source annotation and
  co-locates the new delegation comment on the same anchor

## Milestone 18: attention map and the review stream

Spend attention where the mental-model delta is; dismiss the rest with
confidence — in one diff view, not a separate mode. Design:
[docs/attention.md](attention.md). Supersedes backlog item 5 and the
remaining M13 zen item.

Status: domain and automation foundation in progress.

- [x] durable per-region salience (spotlight/supporting/skim) on the session;
  sources: human override > agent curation > generated/lockfile heuristics
- [x] skim regions render as one-line folds, expandable in place; one key
  acknowledges a fold and marks only wholly covered files viewed
- [x] spotlight regions render with inline narration cards (onboarding
  annotations: title/why/rationale, artifacts expandable)
- [x] inline annotation cards as the single render primitive for comments,
  drafts, and walkthrough steps, channel-colored (see milestone 17)
- [x] chapters become change-scoped stream headers (description, bookmarks,
  stats)
- [x] walkthrough = ordering over spotlight regions; next/prev drive the
  normal view; coverage (spotlights visited + skims acknowledged) replaces
  files-viewed as footer progress
- [ ] focus is a one-key view preset (max fold, file pane hidden, cards
  pinned) — no modal phases, full review vocabulary throughout
- [ ] glance board becomes a summary popup over the attention map
- [ ] delete `ZenPhase` machinery and the overlay-chunk model + `set_chunks`
  compatibility path; regions re-anchor/stale via fingerprints instead of
  tearing down on retarget

## Milestone 19: presentation polish

Status: complete — derived theme/OSC detection, keybinding presets,
responsive layout, and live-keymap menu chrome have landed. Independent of
milestones 17–18.

- [x] derived theme system: an `AppTheme` with all chrome slots computed from
  a small base palette (bg/fg/accent/diff hues) via contrast-guarded
  blending; route all hardcoded render colors through it
- [x] auto light/dark via OSC 11 background query; transparent-background
  mode; keep the xterm-256 downgrade path
- [x] keybinding presets (`preset = "gander" | "hunk"`) layered under the
  existing per-key overrides; adopt non-conflicting conventions as defaults
  (`[`/`]` hunks, `,`/`.` files)
- [x] responsive layout: breakpoint-driven file-pane auto-hide and
  percentage/adjustable split
- [x] optional menu bar rendered from the live keymap for discoverability

## Follow-up backlog (next session pick-up)

Open items from the 2026-07 review-sessions push, consolidated so a future
session can start here without re-deriving them:

1. **Stable `--json` contract audit** (M12). The v1 JSON shapes shipped by
   `reviews`/`comments`/`action-items`/`walkthrough`/`files`/`hunks` are captured in
   docs/cli.md; audit them for consistency (naming, envelope objects,
   id-prefix semantics), fix inconsistencies once, then declare the contract
   stable and note versioning rules in docs/cli.md.
2. **Reviewer/author metadata** (M11). Comments/sessions/action items have no author
   field yet. Subsumed by milestone 17 (annotation channels,
   docs/annotations.md): author identity + channel land together.
3. **MCP/CLI mutations vs live TUI autosave** (M14 unchecked box). Parity
   tools and mutation CLI commands write the state file directly; an open TUI
   holds state in memory and can autosave over those writes. Options: route
   mutations through the live instance socket (registry already exists), or
   make the TUI reload/merge state on external change (mtime watch). Caveat
    is documented in docs/harness-setup.md until fixed.
4. **Watch-mode jj snapshot footgun.** Live TUI refresh now runs read-only jj
   commands with `--ignore-working-copy` and has one explicit `jj util
   snapshot` point per poll, so gander does not fill the op log with
   incidental `log`/`diff`/`op log` reads. Residual jj behavior remains: if
   that deliberate snapshot records dirty working-copy edits, `jj undo` of the
   snapshot operation can revert those edits on disk. A future non-mutating
   working-copy fingerprint (or filesystem watcher that only snapshots after a
   visible prompt) would be needed to remove the footgun entirely.
5. **Zen/focused modes over durable walkthroughs** (M13). Superseded by
   milestone 18 (attention map, docs/attention.md): zen collapses into
   salience-driven rendering of the one diff view instead of touring either
   chunk source.
6. **TUI comment creation through the service layer** (M13). TUI comment adds
   still go through `app::ReviewSession::add_comment`; unify with
   `review::add_comment` so kind/action can be set at creation time in the
   TUI.
6. ~~**Re-render docs/demo.gif**~~ Done: `nix run .#render-demo` renders the
   tape with the flake-built binary, and CI (`.builds/demo.yml`) re-renders
   and commits the GIF whenever the tape or fixture change (guarded by
   docs/demo.gif.inputs-sha256 to avoid render loops). The flake now uses a
   single `nixpkgs-unstable` pin with a working ttyd/vhs on darwin.
7. **Example review size** (M15 polish). docs/pages/example.html is
    ~1.6 MB because it embeds the full diff; consider regenerating from a
    smaller change or trimming hunks for the example link.

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

- The ACP surface is a minimal JSON-RPC method set (`gander-acp` v1), not the
  published Agent Client Protocol schema. Direction changed twice: D5 moved the
  agent-facing surface toward MCP, and D7 clarifies that MCP is an optional
  adapter with mandatory CLI parity. The JSON-RPC socket remains internal
  plumbing rather than growing toward spec ACP.
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
