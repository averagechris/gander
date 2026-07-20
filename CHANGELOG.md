# Changelog

## Unreleased

### Added

- Added the M18 attention Focus preset and glance board. `Z` now applies and
  exactly restores maximum stream/supporting-context folding, file-pane
  override, narration-card state, and viewport context without entering a mode;
  the complete normal review vocabulary remains available. `Alt-G` opens a
  responsive current-attention popup with jump, `Space` peek, selected `a`, and
  current-only bulk `A` acknowledgement, including visible but inert stale
  history. Shared CLI/MCP services expose coverage, skim-fold listing, explicit
  target/stable-id acknowledgement, bulk acknowledgement, and conservative
  whole-file viewed effects. Attention-only TUI mutations participate in
  autosave, Focus refresh preserves both active and underlying viewports,
  spotlight navigation repins exact narration, glance peek is idempotent, and
  CLI/MCP selectors reject unknown, ambiguous, conflicting, or invalid targets.
- Added the continuous M18 salience-driven review stream: one cross-file diff
  projection with stable file/line anchors, contiguous generated-churn skim
  folds (`Space` peek, contextual `a` acknowledgement), fully expanded
  spotlight narration, change chapter headers, walkthrough-ordered
  `Alt-N`/`Alt-P` navigation, `Alt-Up`/`Alt-Down` human overrides, and
  fingerprint-current coverage in the footer. Review-state schema 8 and
  artifact schema 13 retain stale acknowledgement/visit history without
  counting it; partial fold acknowledgement does not mark a file viewed.
  Effective row partitioning lets narrow human overrides punch through broad
  Skim assignments, normal stream rows cannot trigger global mark-all, ranges
  cancel at file boundaries, chapter metadata reloads on retarget/refresh, and
  one signature-keyed projection cache serves rendering and owner lookups.
  Offscreen files use cheap parsed-diff structural rows; syntax/folding rows are
  materialized on demand for the current or visible bounded file window. File
  jumps choose the first selectable member entry, and refresh fallback remains
  anchored to the restored selected file when prior row identity disappears.
- Added the first M18 attention-map package: durable fingerprint-anchored
  spotlight/supporting/skim regions, human > agent > heuristic precedence,
  walkthrough and generated/lockfile/ignore-policy curation, conservative stale
  handling, CLI/MCP automation parity, and private human/agent artifact export.
- Added M18 inline annotation cards as the shared channel-colored presentation
  for durable comments, agent drafts, and spotlight walkthrough narration,
  including identity/lifecycle badges, replies, deterministic diff ownership,
  and ephemeral in-place example/diagram expansion with `E`.
- Added optional durable walkthrough-step authorship. New TUI/CLI steps are
  attributed to the configured human and MCP-authored steps to the configured
  agent; legacy missing authors remain explicitly neutral. Review-state schema
  7 and artifact schema 12 carry the additive field.
- Added durable annotation authors and onboarding/delegation/collaboration/note
  channels to comments, plus reply authors and a one-release migration that
  derives legacy channels and identities and folds pending agent-overlay drafts
  into durable onboarding comments without recreating accepted or discarded
  history.
- Added annotation automation surfaces: layered human/agent identity config,
  comment channel filters for CLI/MCP, durable session disposition, and
  team-profile exports that publish only collaboration todo/resolved threads;
  team JSON is the canonical forge-mappable contract, while Markdown/HTML are
  filtered human summaries.
- Derived TUI theme (`[theme]` config): every chrome color now derives from a
  small light/dark base palette via WCAG-contrast-guarded blending (the
  per-combination contrast contract is documented in docs/theme.md), with
  automatic light/dark detection through a one-shot, DA1-fenced OSC 11
  background query (`mode = "auto"`; explicit `dark`/`light` never query),
  a transparent-background mode that keeps the terminal's own background
  (the default), semantic annotation-channel colors reserved for milestone
  17, and the truecolor-to-xterm-256 downgrade path preserved with
  post-quantization contrast repair. Explicit `[diff.theme]` and
  `[syntax.theme]` values remain literal user styles; unset `[diff.theme]`
  entries now derive from the theme. Known detection edge cases and their
  bounded impact are documented in docs/theme.md.
- Added M19 TUI presentation polish: keybinding presets with gander/hunk
  navigation conventions, explicit next/previous file actions, configurable
  responsive file-pane auto-hide and split sizing with runtime split controls,
  and an optional live-keymap menu bar.
- Added M17 annotation-channel ergonomics: shared conservative channel
  inference using read-only jj author facts and explicit agent/onboarding
  context, optional `[comments].default-channel`, a live Tab-cycled
  channel-colored editor chip/border, and one semantic channel color language
  across gutter marks, inline cards, comment/open-work lists, and agent
  draft rows. CLI comment add/edit now accept `--channel` for scriptable parity.

### Changed

- Removed the former full-screen focused-presentation phases, ephemeral overlay
  chunks/change briefs, their ACP/MCP/CLI compatibility and line-space APIs,
  and the legacy `T` key/config aliases. Durable walkthrough steps and attention
  regions are now the only curation model. `gander tui --tour`, `gander tour
  render`, and `gander present` remain script-compatible by applying Focus and
  driving durable Spotlights in the normal stream, with full review actions and
  retarget/refresh safety. Existing `agent.json` chunk/brief fields are ignored
  rather than migrated and are discarded on the next save; ordering and flags
  survive. Presenter refresh tracks durable `(step_id, part)` identity instead
  of numeric position, and CLI/MCP walkthrough replacement now shares
  deterministic id preservation, target normalization, and exact stack chapter
  validation, with atomic rejection of duplicate explicit or final step ids.

## v0.7.2 - 2026-07-12

### Fixed

- Diff lines now soft-wrap by default through one grapheme-width-aware measured
  layout shared by rendering, visual scrolling, and mouse hit testing. Unified
  and side-by-side views preserve long content, split pairs align to the taller
  wrapped side, and disabling wrap enables fixed-gutter horizontal scrolling;
  config, keybindings, View Options, resize reflow, comments/selections, and
  per-file viewport state all follow the same logical rows (#164).
- Comment editing now uses one grapheme-safe, display-width-aware visual layout
  for wrapping, rendering, cursor placement, vertical navigation, reflow, and
  scrolling, keeping CJK, combining text, and emoji cursors exact in new,
  existing, general, and agent-draft comments (#165).
- TUI bindings now use context-aware validation and effective hints: canonical
  collisions fail only where actions overlap, disjoint modal reuse remains
  valid, unknown fields and invalid key syntax fail clearly, text filters keep
  literal `j`/`k`, and popup, Comment Center, walkthrough, draft, and zen
  controls are configurable and documented with a collision-free Colemak
  Mod-DH override (#168).
- TUI input validation now includes immutable safety fallbacks and layered zen
  precedence, canonicalizes shifted letters consistently, blocks review mouse
  mutations behind every modal, and implements the documented grapheme-safe
  comment-editor controls.
- Bundled review skills now establish the durable session before writing state,
  show targeted hunk inspection, treat todo comments as the primary feedback
  primitive, reserve action items for coordination, and require resolving
  satisfied linked todos separately from closing their parent action item.
## v0.7.1 - 2026-07-10

### Fixed

- Empty `tui --tour` startup now falls back to the normal empty TUI with an
  informational notice, while terminal acquisition and later exits remain
  guarded so raw mode, mouse capture, alternate-screen, and cursor state are
  restored safely.
## v0.7.0 - 2026-07-10

### Added

- Comments now freeze an optional creation observation and replies capture a
  current result snapshot from the already-loaded diff. Evidence includes
  durable session/target identity, exact and portable per-file fingerprints,
  deterministic aggregate identity, rename/not-in-diff transitions, and a
  portable patch changed signal without additional jj queries or mutations.
  Rename lineage follows shared `old_path` across successive full diffs, binary
  rename/copy structure remains distinct from portable comparability, and
  observed general comments cannot acquire provenance from a later location.
- Documented the ready-comment workflow: durable private drafts, actionable
  todos, resolved history, configurable comment initial state, `comments ready`,
  general comments, handoff/delegate selection, TUI keys, MCP parity, and schema
  compatibility expectations.
- `gander tui --tour` starts directly in the zen tour, and `gander tour render`
  renders the slide deck to plain text for automation and review.
- `gander walkthrough set --dry-run` validates a walkthrough replacement and
  prints the would-be result without writing review state.
- Live presentation control: `gander present`, ACP `present/*` methods,
  and matching MCP tools can drive a running TUI's tour/view through the
  per-instance socket, with modal safety guards.
- `gander handoff --mode delegate` emits typed Markdown/JSON delegation packets
  with action-item/comment selectors, recipient, objective, constraints,
  acceptance, and verification text.
- `gander skills list/show/install` exposes bundled CLI-first agent skills
  without requiring repository, config, or jj initialization.
- Comments now carry append-only UUID-addressed replies and `updated_at`
  timestamps. CLI/MCP agents can reply, reply-and-resolve atomically, and carry
  the full thread through artifacts and handoffs.
- Documented the action-item model: todo comments are the primary implicit
  feedback; ordinary comments are not action items; durable action items are
  optional coordination objects with many linked comments and external ticket
  references; linked todo evidence is folded into its parent action item.

### Changed

- Review state schema 3 normalizes legacy serialized `tasks` into
  `action_items` on save. Artifact schema 8 and delegation schema 4 carry the
  action-item shape, while legacy anchored/unscoped comments still deserialize
  unchanged and remain visible for compatibility. Delegation's pre-existing
  top-level diff fingerprint algorithm remains unchanged; comment evidence uses
  its independently versioned review-scope aggregate.
- Public docs now use `action-items` CLI/MCP terminology (`list/show/add/edit`,
  `link-comment`/`unlink-comment`, `add-ticket`/`remove-ticket`,
  `close`/`reopen`/`delete`, repeatable `--comment`, and handoff
  `--action-item`) rather than the retired public tasks vocabulary.
- Zen/tour mode now presents a polished full-screen slide deck with full-bleed
  chapter, spotlight, and at-a-glance slides instead of framed floating cards.
- Removed the deprecated public `gander chunks` and `gander briefs` command
  groups, plus the TUI `S` chunk-list popup and `chunk-list` keybinding. Durable
  walkthroughs, zen/tour mode, drafts, and active ACP/MCP agent-overlay curation
  remain supported.
- Removed the hidden compatibility `gander handoff --only-open` flag; prompt
  handoff continues to include only open action items and ready (`todo`)
  comments by default.
- CLI/help text now describes Gander as local-first durable review state over
  jj-visible work, hides deprecated chunks/briefs from generated help, and
  rejects explicit HTML artifact profiles instead of ignoring them.

### Fixed

- Documentation and examples now use current CLI syntax (`--state-file`,
  per-command `--format`, `reviews`/`walkthrough` commands) and clarify current
  line-anchor, import, MCP/ACP, and concurrent state-merge semantics.
- Comment input supports readline-style line editing shortcuts: Ctrl+U
  deletes to the start of the current line, Ctrl+K deletes to the end,
  Ctrl+W deletes the previous word, Ctrl+A/Ctrl+E move to line
  boundaries, and Alt+B/Alt+F or Ctrl+Left/Ctrl+Right move by word.
- Removed the deprecated chunks/briefs shim conversion path in favor of the
  explicit `walkthrough` commands for durable curation; internal overlay chunks
  and briefs still adapt into walkthrough/zen views.
- Walkthrough authoring validates unknown fields, chapter `change_id`s, and
  target line ranges against the diff line space, and repeated
  `walkthrough set` runs preserve stable step ids.
- Bundled skills no longer contain stale flags or stdin-body examples, and CLI
  parsing/handler validation now enforces comment/task/walkthrough range and
  target invariants consistently.
## v0.6.1 - 2026-07-05

### Fixed

- Activity-popup snapshot tests render timestamps in UTC so the test
  suite passes regardless of the machine or sandbox timezone. This
  failure aborted the v0.6.0 release after its tag was pushed, so
  the v0.6.0 tag carries no binary artifacts — use v0.6.1, which is
  otherwise identical.
## v0.6.0 - 2026-07-05

### Fixed

- Live TUI sessions no longer silently drop curated chunks when the
  repository refreshes: overlay revalidation now runs against real
  per-change diffs on every reload and zen retarget, and chunks that
  genuinely no longer match the diff produce a visible warning
  instead of vanishing.
- External review-state writes (e.g. `gander comments add` while a
  TUI is open) are merged instead of clobbered: the TUI picks them up
  within a poll (`review state updated externally — 1 comment
  added`), merges by id with delete-tombstones before every save,
  and CLI-added comments survive TUI quit.
- The silent default-base trap: read commands (`handoff`, `export`,
  `tasks list`, `walkthrough show/export`, `comments list`) no longer
  create phantom sessions and warn when the target matches no open
  session while one exists for another target
  (`warning: no open review session matches 'trunk()..@'; open
  session "…" targets 'main..@' …`); mutations note when they
  implicitly create a new session alongside an existing one.
- Chunk/brief/draft spec files warn on unknown fields (a typo'd
  `role:` no longer silently degrades to defaults; the warning lists
  the allowed fields).
- `gander summary` aligns its status column (`added` rows no longer
  shift the path column).

### Added

- Zen chapter cards render an honest `partially curated` banner when
  briefs exist without spotlight chunks, instead of claiming the
  tour is uncurated directly above an agent brief.
- Zen stops surface existing review comments anchored inside the
  stop's line range (`comment [todo] a3c7b887: …`).
- Trait impls are labeled precisely in derived symbol facts
  (`impl Default for Priority`, not a duplicate `impl Priority`).
- Handoff JSON action items carry `end_line` for range anchors, and
  export JSON comments carry `linked_task_ids` back-pointers.
- `hunks show --format text` (alias of `diff`), plus compact
  `--format text` echoes on the remaining task/comment mutations
  (`tasks add/complete/reopen/edit/delete`, `comments
  delete/resolve/set-state`).
- The activity feed shows one structured row per event (the
  aggregate summary stays in the footer), annotates change rows with
  the causing jj operation once, truncates embedded 128-char
  operation ids, moves with `j/k`/`n/e`/arrows, keeps the popup open
  on non-file rows, and renders the selected event's full message
  wrapped in a detail area.
- Zen chrome shows the human-facing home target during tours instead
  of the per-change retarget revset; multi-part chunk explanations
  render once (parts 2+ point back to part 1); truncated chapter
  cards say `… d expands`.
- Canned zen review questions appear once per tour instead of
  repeating verbatim on every triggering stop.

### Changed

- The file-badge legend matches actual badge behavior
  (`~ viewed, changed since` vs `± unviewed, changed since`).
- `docs/acp.md` documents `review/draft_comment` line-space
  semantics (new-side, 1-indexed).

### Added

- `--format text` on `files`/`hunks`/`comments`/`tasks`/`reviews
  list`: compact, aligned, one-row-per-item output for humans (JSON
  stays the default). `comments add`/`edit` accept the same flag for
  a 3-line echo instead of the full anchor JSON dump.
- `gander tasks edit` and `gander tasks delete`, with
  ambiguity-guarded id-prefix resolution shared by
  `complete`/`reopen` (unknown and ambiguous prefixes are clean
  errors).
- `hunks list` accepts a positional file (`gander hunks list
  src/queue.rs`), and every subcommand's `--help` separates command
  flags from the global plumbing under a "Target & state (global)"
  heading; the remaining blank help strings are filled.
- Handoff action items are deterministically ordered — action
  priority (fix > test > follow-up), then path, then line —
  identically in markdown and JSON.
- The TUI footer shows a persistent `@`-identity chip
  (`@ <short-id> <description>`, width-aware, refreshed only when
  the repo fingerprint changes).
- The diff pane cycles the selected comment's state
  (draft → todo → resolved) with `s`, with a
  `s state · e edit · x delete` hint while a comment is selected.
- Activity-feed refresh events are labeled with the jj operation
  that caused them (`· op: snapshot working copy`), fetched only
  when a refresh fires; `enter` on a feed event that names a file
  jumps to it.
- `gander acp` announces on stderr whether it bridged to a live TUI
  or is serving a snapshot, and the `initialize` response carries a
  `mode: "live-bridge" | "snapshot"` field.
- Uncurated zen glance rows dedupe across changes with `ch.N`
  attribution, and auto-appended uncovered leftovers on curated
  tours are labeled `· uncovered`.

### Changed

- Comment-backed tasks always carry a string `title` (synthesized
  from the comment body's first line) in `tasks list`, handoff JSON,
  and exports — no more `title: null`.
- The agent-profile markdown export has its own H1
  (`# Review session export (agent profile)`) so it is no longer
  mistakable for the handoff prompt, and the TUI `ctrl-y` yank now
  copies the actual handoff markdown.
- Chunk validation errors echo the valid line ranges for the failing
  path and point at `gander chunks lines`.
- Zen "top changed symbols", per-stop symbol lists, and chapter
  dependency evidence only count symbols overlapping lines the
  change actually added — context-only symbols no longer leak in.
- Zen chapter cards render one `derived facts` section instead of
  repeated `stops:`-prefixed lines; curated cards put the brief
  first and collapse derived facts to a dimmed summary line
  (expandable with `d`).

### Fixed

- A file that counts as viewed (viewed or caught-up) can no longer
  render a bare `±` badge: catch-up and state-restore clear the
  changed-since-look flag, and the renderer falls back to `~`.
- Zen chapter cards size to the terminal (up to ~100 cols instead of
  a fixed 72) and `d` expands a truncated curated brief in place,
  with an explicit `… d expands the brief` affordance when clipped.
- `comments add`/`comments edit` warn on stderr when the target line
  is outside the current diff ("comment stored without an excerpt
  anchor — use 'comments edit' to fix") instead of silently storing
  an anchorless comment.
- Footer refresh notices ellipsize width-aware and append
  `· ctrl-a for detail` when content was dropped; activity-feed lines
  ellipsize instead of hard-clipping.

- Uncurated zen tours the stack change by change: one chapter per
  stack change with the commit message (title and body) as the intent
  card, stops grouped under the change that owns them, and derived
  facts (largest hunk, churn, tests-touched, API evidence) computed
  from each change's own diff — never the session-global diff. Purely
  added public items read `new public API: …` instead of "signature
  changed", and test-subject changes spotlight their test files
  instead of auto-glancing them.
- `gander chunks lines [--change <id>] [--path <p>]` lists the exact
  line ranges chunk validation accepts (computed by the same shared
  helper the validator uses), with per-hunk excerpts for orientation.
- Setting change briefs warns — over ACP, MCP, and `gander briefs
  set` — when a brief's change has no spotlight chunk yet and would
  not render as a curated chapter.
- `gander comments edit <id>` (retarget line/range/path, re-derived
  anchors and excerpts) and `gander comments delete <id>`, so
  anchoring mistakes are no longer permanent.
- `gander handoff --format json` is now a structured action artifact:
  first-class `action_items` (source, kind/action, path, line,
  excerpt, body, state, linked ids), session metadata, walkthrough,
  and reference hunks — not a rendered-content dump. Markdown handoff
  includes task bodies and explicit task↔comment cross-references,
  and is now a distinct, tighter implementation prompt (reference
  hunks trimmed to files with action items or walkthrough stops)
  while `export markdown --profile agent` stays the complete session
  artifact. `--help` examples on both commands explain the split.
- `gander hunks show --format diff` prints a human-readable unified
  diff (JSON stays the default).
- The `I` operation picker joins the standard movement keys
  (`n`/`e`, `j`/`k`, arrows), shows a key-hint line, and renders a
  live preview of what Enter will do (`will mark 3 caught up · 2
  already viewed · 2 need re-review`) before anything is applied.
- Watch notices are revert-aware: description-only updates read
  `change <id> description updated`, and a file whose content returns
  to a previously seen viewed/caught-up version reads `<path> reverted
  to previously seen content` (and drops its `±` badge) instead of
  looking like a forward edit.
- Refresh callouts when reviewed files change: the footer batch notice
  and activity feed flag `was viewed, needs re-review` per file.
- The comment editor shows its anchor in the title
  (`comment · src/config.rs:10`) and `ctrl-s save · esc cancel` hints
  in the popup border and footer.
- `j`/`k` and arrow keys move in every list-like pane; the file tree
  keeps a stable order across badge and viewed-state transitions.
- CLI overlay commands (`chunks`/`briefs`/`drafts`) warn on stderr
  when a live TUI session on the workspace is reviewing a different
  target than the invoked flags.
- Every `reviews`/`comments`/`tasks`/`walkthrough` subcommand and flag
  now has real `--help` documentation, including 1-indexed post-image
  `--line` semantics.
- `gander briefs` (`list`, `set --file <spec|->`, `clear`) and
  `gander drafts` (`list`, `add --file <spec|->`, `remove --id …`)
  command groups extend the chunks spec-file pattern to change briefs
  and draft comments — full CLI parity with ACP
  `review/set_change_briefs` / `review/draft_comment`, validated with
  per-item reasons and writing the same overlay a live TUI watches.
- Zen chapter cards narrate stack dependencies: a derived
  `builds on ch.N` line appears when a chapter touches files or
  symbols an earlier chapter in the stack also touched.
- Zen fallback stop risk facts now name their evidence — the changed
  public signature or the count and location of changed error-handling
  sites — and add a factual, template-based review question per
  trigger ("do callers handle the new signature?").
- The `I` catch-up flow distinguishes caught-up from reviewed:
  never-viewed files unchanged since the chosen operation get a
  quieter `◌` caught-up badge (persisted per content fingerprint)
  instead of a full `✓`; they count as done for progress and
  filtering, promote to viewed on explicit marking, and decay to `~`
  like viewed marks when content changes. The catch-up notice reports
  caught-up, already-viewed, and needs-re-review counts separately.
- Incremental chunk curation: new ACP methods `review/update_chunks`
  (upsert by id, position-preserving) and `review/remove_chunks`
  (strict remove by id), mirrored as MCP tools, alongside the existing
  full-replace `set_chunks`.
- `gander chunks` command group for scriptable curation without
  JSON-RPC: `list`, `set --file <spec|->`, `update --file <spec|->`,
  `remove --id …`, and `clear` all operate on the same overlay file a
  live TUI watches; specs are plain JSON `{ "chunks": [...] }`
  documents with optional ids, validated against the session diff with
  per-part reasons on reject.
- The TUI chunk list popup (`S`) now badges chunk parts that were
  excluded as invalid, with the exclusion reason; overlay re-applies
  after refresh run the same validation as the initial load.
- Multi-part review chunks read as one concept: spotlight stops
  cross-reference their sibling parts, and the zen glance board groups
  a multi-part chunk into a single entry listing its parts.
- Derived zen content now includes factual risk labels ("public API
  change", "touches error handling") on fallback stops and aggregated
  chapter lines.
- Help overlay (`?`) gained a task-oriented "first review loop" section
  and a cluster documenting the in-zen keys.
- Watch legibility: file tree rows badge files changed since you last
  looked (`±`) and viewed-but-changed-since-viewed files (`~`), with
  changed hunk headers marked in the diff pane; `}`/`{` jump to the
  next/previous changed hunk across files.
- Activity feed (`ctrl-a`): a bounded, timestamped feed of refresh
  events — `@ moved to <id>`, `change <id> entered range/updated/left
  range`, per-file update collapsing — with the footer notice
  summarizing each refresh batch instead of a generic message.
- The footer indicates follow mode (`following @`) whenever the review
  target contains a symbolic revset.
- Zen card titles and locations truncate width-aware instead of
  clipping against the card border; long paths keep their tails.
- Zen tours without agent curation now derive useful content from the
  diff itself: chapter cards show files-by-role, churn, tests-touched,
  and top changed symbols; fallback stops anchor on each file's largest
  hunk with touched-symbol summaries; manifests, tests, docs, and
  exports-only files route to the glance board with a rationale label.
- Agent review chunks are validated at ingestion: ACP/MCP `set_chunks`
  rejects parts that reference files outside the anchored change's diff,
  out-of-range lines, or unknown change ids; invalid parts in an overlay
  file written directly to disk are excluded with a TUI notice.
- `gander acp`/`gander mcp` warn on stderr when bridging to a live TUI
  whose review target differs from the requested flags, and
  `review/summary` now reports `active_target` and `live_session`.
- `gander handoff`: one-shot, prompt-style agent handoff of the current
  review — action items (open tasks + unresolved issue/fix comments with
  excerpts) first, walkthrough order, then full hunks as reference.
  Supports `--format markdown|json`, `--only-open`, `--output <path>`,
  and `--copy` (pbcopy/wl-copy/xclip with OSC52 fallback).
- TUI `ctrl-y` (`yank-handoff` keybinding) copies the agent handoff to
  the clipboard without leaving the review.
- Review artifacts (schema v5) now include session metadata, review
  tasks, and walkthrough steps in JSON and Markdown; v4 artifacts still
  import.
- Dogfood evaluation loop: `eval/` harness (fixture generator, scripted
  evaluator scenarios, scoring rubric, archived baseline reports) and a
  living findings/action-item tracker in `docs/dogfood.md`.

### Changed

- The global `--state <STATE-FILE>` flag is now `--state-file` — it
  collided with `comments set-state --state`, making that subcommand
  panic on every invocation.
- `gander handoff --format json` output shape changed (see Added);
  consumers of the old rendered-content envelope must migrate.
- Kind/action enums serialize exactly as the CLI accepts them
  (`follow-up`, not `followup`); legacy `followup` values are still
  accepted on input.

### Fixed

- Zen glance "mark all viewed" now actually persists: marks made
  while zen had retargeted the session to a per-change diff were
  dropped when the tour restored the home target.
- The `I` operation picker fits small panes: preview and key-hint
  lines are always visible (the op list shrinks instead), and long
  op descriptions middle-truncate to exactly one aligned row (no
  more wrapped 128-hex op ids).
- `gander handoff --format json` defaults to the same action-item
  selection as the markdown handoff (open tasks + unresolved
  comments); completed tasks no longer pad `action_items`.
- `tasks add --comment <id>` resolves prefixes to canonical full
  comment ids and rejects unknown or ambiguous references instead of
  storing dangling raw strings.
- "New public API" detection is symbol-scoped: a brand-new `pub fn`
  inside a mixed hunk is labeled new instead of "signature changed"
  with a nonsense review question.
- Stack stepping no longer offers the empty, undescribed working
  copy as a dead final position.
- A never-viewed file that changes and then reverts clears its `±`
  freshness badge (revert detection previously covered only
  viewed/caught-up files), and catching up or viewing a file always
  clears a stale `±`.
- `gander chunks lines` emits honest headers for coalesced adjacent
  hunks instead of only the first hunk's `@@` line.
- `gander comments set-state` no longer panics (clap arg-id
  collision with the global state-file flag).
- Mutating verbs (`tasks complete/reopen`, `comments
  resolve/set-state/edit/delete`, `walkthrough remove-step`) print
  clean `error: unknown task/comment/step …` messages instead of
  color-eyre backtrace walls on bad ids.
- Zen chapters, TUI stack stepping, and ACP `review/stack_changes`
  scope strictly to `base..rev`: the base change no longer appears as
  a chapter or stack position, and step denominators are stable (no
  more `3/4` → `3/5` jumps).
- `gander chunks lines` no longer mixes old-side and new-side
  coordinates (ranges that overlapped and rejected valid-looking
  parts); the listing and the validator share one line-space helper.
- Markdown handoff action items match the JSON artifact: all
  unresolved comments count, regardless of kind — question/note
  comments with reviewer-set actions are no longer demoted below the
  walkthrough while the header claims otherwise.
- Handoff/export artifacts no longer emit silent `id: null` session
  metadata when no durable session exists.
- Reverting a file to previously viewed content clears its `±`
  freshness badge instead of leaving a flagged row that counts as
  viewed.
- The tree-sitter debug line (`tree-sitter: rust root=… errors=true`)
  no longer renders at the top of every diff.
- `G` clamps to the last diff line instead of scrolling into a blank
  pane; footer counts pluralize correctly (`1 comment`).
- Zen glance rows truncate change ids and paths independently
  (no more mangled `[id…path` strings), glance header/footer counts
  agree, chapter cards mark truncated text with an ellipsis, and
  single-file glance rows no longer print their path twice.
- Watch polling no longer pollutes the jj op log: every read-only jj
  invocation passes `--ignore-working-copy`, with exactly one
  deliberate `jj util snapshot` per poll tick so working-copy edits
  are still noticed. (The jj-side footgun that `jj undo` of a snapshot
  op reverts the edits it recorded remains and is documented.)
- Inline comment annotations no longer vanish from the diff pane after
  a watch refresh or retarget: comment anchors are re-derived against
  the freshly loaded diff instead of pointing at stale rows.
- CLI user errors — malformed chunk/brief/draft specs, out-of-diff
  parts, unknown ids, bad revsets — print a clean `error: …` message
  to stderr and exit non-zero instead of a color-eyre report with
  backtrace frames and `Location:` noise.
- **Data loss:** retargeting the TUI to a narrow or empty diff (e.g.
  stack-stepping onto an empty working-copy change) no longer rewrites
  the workspace review state with only what that target can see —
  viewed-state entries and comments outside the current diff now
  survive every save.
- Viewed marks are persisted per content fingerprint
  (`viewed_fingerprints` per path; legacy state files still load), so
  visiting a target where the same path diffs differently no longer
  clobbers the mark, and the viewed-but-changed (`~`) state survives
  refreshes durably.
- Live watch actually refreshes again: the repo-poll fingerprint
  template used `if(self, …)`, which real jj rejects, so the 2s poll
  failed silently on every tick. Persistent poll failures now surface
  a footer error (and a recovery notice) instead of freezing behind
  the `following @` indicator.
- Read-only popups (activity feed, help) no longer pause live refresh;
  popups that do pause show a "refresh paused" hint.
- `}`/`{` changed-hunk navigation cycles within and across files
  instead of sticking on the first changed hunk, and freshness badges
  no longer decay when the file-tree cursor merely passes a file —
  only when its diff is focused or it is marked viewed.
- Activity events name only the files that changed in that refresh,
  with honest churn deltas ("appeared" for new files) instead of
  re-listing every un-reviewed file with whole-file totals; timestamps
  render in local time; help documents `ctrl-a`, `}`/`{`, and the
  freshness badge legend.
- Zen chapter cards no longer repeat session-global stats on every
  chapter: derived facts are scoped to the chapter's own files
  (labelled `stops:`), or omitted when they cannot be honestly
  attributed.
- `t` returns to the target the TUI was launched with (e.g. `-b main`)
  instead of hard-coded `trunk()..@`, so stack stepping always has a
  way home.
- The target chooser ranks exact bookmark matches (then prefix
  matches) above fuzzy matches, snaps the selection cursor to the top
  match as you type, and drops scattered fuzzy matches when a better
  tier exists — typing `main` and pressing Enter selects `main`.
- The prior-operation catch-up flow (`I`) refreshes the diff before
  comparing, and its notice states exactly how many unchanged files
  were marked viewed vs changed files needing re-review.
- CLI commands exit quietly on broken pipes (e.g. `gander hunks list |
  head`) instead of printing an error and backtrace.
- Comments added via `gander comments add` now carry stable line/range
  anchors, so agent-profile exports include the documented
  `comments[].anchor` and `excerpt` context. Anchor derivation is shared
  core logic between the CLI and the TUI.
- File-anchor flags are consistent across command groups: `--path` and
  `--file` are both accepted everywhere a file anchor is taken; `--path`
  is canonical.
## v0.5.0 - 2026-07-04

### Added

- Durable review sessions now carry review tasks, walkthrough steps
  (`title`/`why`/`body` plus file/line/symbol targets), comment kinds
  (`note`/`issue`/`question`/`praise`), and action intents
  (`fix`/`explain`/`test`/`follow-up`) in the persisted review state.
- Session, comment, task, and walkthrough CLI commands: `reviews
  create/list/show`, `comments add/resolve/set-state`, `tasks
  add/complete/reopen/list`, and `walkthrough
  add-step/remove-step/move-step/show/export`, plus machine-readable
  `files list` and `hunks list/show` queries.
- Self-contained static HTML review export via `gander export html`, alongside
  the existing JSON and Markdown artifact formats.
- TUI action tags and kind badges in the comment list (`C`, then `a`/`K`), a
  review-tasks popup (`X`), and maintainer walkthrough authoring (`Y` to mark a
  step, `W` to jump/reorder/delete steps).
- MCP parity tools for durable review state: `reviews_list/show/create`,
  `comment_add/resolve/set_state`, `task_add/complete/reopen/tasks_list`, and
  `walkthrough_add_step/remove_step/move_step/show`, each matching a CLI
  equivalent.
- `nix run .#render-demo` renders docs/demo.gif from docs/demo.tape with the
  flake-built binary, and a new CI manifest (`.builds/demo.yml`) re-renders
  and commits the GIF automatically when the tape or demo fixture change.

## v0.4.1 - 2026-07-04

### Fixed

- Support mouse wheel in zen tour.

## v0.4.0 - 2026-07-04

### Added

- Zen mode (`T`/`Z`): a focused, agent-curated briefing with three
  surfaces (docs/focused-diff-ux.md §6). The **focus card** is a
  full-screen stop per spotlight chunk: only the critical lines,
  extracted and vertically centered, with the agent's multi-sentence
  `explanation` rendered beside them ("why this matters"). `tab` drops
  into the **reading view** — the normal diff with out-of-range rows
  dimmed and the full review vocabulary (comments, flags, context
  expansion, view toggles) available. After the last stop (or via `g`)
  the **glance board** lists every glance chunk *and* every file no
  chunk covers on one skimmable screen with stats and one-liners;
  `enter` jumps into the diff, `a` bulk-marks the lot viewed and
  finishes. Agents label chunks `importance=spotlight` (capped at 3–7 by
  the summon prompt, each requiring an explanation that teaches the
  change) or `importance=glance` for the mechanical rest. Without an
  agent, zen falls back to one stop per file. Retargeting the review
  ends the briefing safely.
- Change-aware walkthroughs for stacked reviews: chunks can carry a
  `change_id` anchoring them to one jj change of the stack, and zen
  retargets the review to that change's own diff (`change-..change`) as
  the tour flows through the stack — stacked-PR review, change by
  change. Ending zen returns to the target it started from. The chunk
  list (`S`) retargets the same way, and chunk/zen locations show the
  anchored change id. Agents get `stack_changes` (the `trunk()..@` stack
  with the reviewed change marked) and `change_diff` (one change against
  its parent) over both MCP and ACP (`review/stack_changes`,
  `review/change_diff`), and the summon prompt and MCP instructions
  teach the stacked-PR workflow.
- Zen chapters: the walkthrough is organized change by change instead of
  dropping the reviewer onto bare change ids. Every run of stops
  anchored to the same jj change opens with a full-screen *chapter card*
  showing the change's description, bookmarks, and live diff stats plus
  the agent's high-level *change brief* — a few sentences on what the
  change accomplishes, why it exists, and how it builds on the previous
  changes (`review/set_change_briefs` over ACP, `set_change_briefs` over
  MCP; briefs live in the shared overlay like every other suggestion).
  Every walkthrough starts with an opening chapter for its target, the
  progress strip groups stop dots by chapter (`▎` bars), and chapter
  cards mark nothing viewed. Cards show the change's full multiline
  description (headline bold, body beneath); `d` collapses the body to
  the headline. Stack queries (`stack_changes`, the target picker's
  backing data) now carry full descriptions — agents get the change's
  own words, one-line surfaces show the first line.
- Zen artifacts: agents can attach exhibits to spotlight chunks and
  change briefs (`artifacts: [{title, kind: example|output|diagram|note,
  body}]`) — a usage example of the changed API, output captured by
  exercising the code, an ASCII diagram of the new flow. Cards with
  exhibits show an `e` hint; `e` opens a modal scrollable viewer over
  the focus card (`j`/`k` scroll, `h`/`l` cycle between exhibits,
  `esc` closes). The zen footer also advertises `e` with the exhibit
  count whenever the current stop or chapter carries artifacts. The
  summon prompt and MCP instructions teach agents to
  show, not just tell.
- Live review refresh: the TUI polls jj on idle (throttled, ~2s) and,
  when the reviewed range changes — new changes landing, rewrites,
  working-copy edits — reloads the diff in place with a footer notice.
  View state (pane visibility, filters, folds, selection, viewports) is
  preserved, viewed marks and comments carry over by fingerprint, agent
  suggestions are reapplied, and an active zen walkthrough rebuilds its
  stops instead of going stale.

### Changed

- The `tour` keybinding is renamed `zen` (old configs with
  `[keybindings] tour` keep working); the default binding gains `Z`
  alongside `T`.

## v0.3.0 - 2026-07-03

### Added

- Diff visual cues that make changes obvious at a glance
  (docs/focused-diff-ux.md): word-level change highlights within modified
  line pairs (unicode word diff, similarity-thresholded so rewrites don't
  over-highlight) and subtle added/removed line background tints, both on
  by default, plus an opt-in colored gutter change bar. Configured under
  the new `[diff]` / `[diff.theme]` sections; defaults are
  GitHub-dark-inspired truecolor tints that quantize to the nearest
  indexed color on terminals without truecolor support.
- View options popup (`V`): session-only runtime toggles for the visual
  cues, the file pane, and the side-by-side view. Every toggle also has a
  bindable action under `[keybindings]`.
- Collapsible file pane (`w`): hide the file tree to give the diff the
  full terminal width. Hiding moves focus to the diff, focusing the files
  pane re-shows it (never traps), and the diff pane title carries the
  selected file path and viewed mark while the tree is hidden.
- Side-by-side diff view (`|`, or `[diff] view = "side-by-side"`):
  removed/context cells on the left, added/context on the right, with
  side-specific line numbers and word-level emphasis aligned across the
  divider. Implemented as a render-time projection over the unified rows,
  so the cursor, comments, anchors, flags, and range selection behave
  identically in both layouts. Falls back to unified on terminals
  narrower than 100 columns.
- Per-gap hunk context expansion (`+` expand by `[diff] context-step`
  (default 10), `=` expand fully, `-` re-collapse): pull in file lines
  beyond what the jj diff emitted, above, between, and below hunks.
  Content is fetched lazily via `jj file show` with real line numbers on
  the expanded rows; adjacent hunks render contiguously when a gap
  closes. Expanded context rows are not commentable in this release.
- Style specs (diff cue theme and syntax themes) now support `on <color>`
  backgrounds, indexed colors (`22`), and hex (`#rrggbb`).

### Documentation

- docs/focused-diff-ux.md: design for the focused diff UX work above and
  the direction for a future agent-guided "zen mode" walkthrough
  (roadmap milestone 10).

## v0.2.1 - 2026-07-03

### Changed

- Collapse assert shortened by the opencode example fix.

### Documentation

- Drop nonexistent --quiet flag from opencode run examples.

## v0.2.0 - 2026-07-02

### Added

- Tour mode (`T`): step through agent-suggested review chunks in order
  with the agent's rationale shown in a bottom panel; advancing marks the
  current stop's file viewed, Esc returns to free navigation. Movement and
  scroll keys keep working within a stop.
- Large-change nudge: when a review exceeds the new `[limits]`
  `nudge-diff-lines` (default 1000) or `nudge-files` (default 25)
  thresholds and no agent has organized it yet, the footer suggests
  summoning an agent (`@`) or asking the harness; re-raised when loading a
  new target. Set a threshold to 0 to disable that criterion.
- `docs/harness-setup.md`: harness setup recipes — MCP registration for
  opencode/Claude Code/Codex, the split-pane review workflow, and
  attach-to-running-server `[agent] command` examples.
- `gander mcp`: an MCP stdio server (official `rmcp` SDK) exposing the
  review session as typed tools — `review_summary`, `review_files`,
  `file_diff`, `comments`, `current_focus`, `set_ordering`,
  `flag_section`, `set_chunks`, `draft_comment`, `list_reviews`. Tool
  calls route to the workspace's live TUI instance by cwd, with a
  snapshot fallback when no TUI is running.
- Per-instance ACP sockets (`acp-<pid>.sock`) plus a shared instance
  registry (workspace root, target, summary, socket, pid,
  `last_input_at`; heartbeats on input, cleaned up on exit). A second TUI
  on the same workspace now gets its own endpoint, `gander acp` routes to
  the most recently touched live instance for the workspace, and the new
  `review/current_focus` method reports what the human is looking at.
- `gander paths`: prints every resolved state/runtime/config location for
  the current workspace.

### Changed

- **Runtime state moved out of project directories.** Review state, the
  agent overlay, the live ACP socket, and agent logs now live in
  per-workspace directories under the XDG state dir
  (`~/.local/state/gander/<workspace-key>/`; `XDG_STATE_HOME` respected),
  with sockets/logs preferring `XDG_RUNTIME_DIR` when set. Legacy
  `.gander/` state is migrated automatically (one release of read
  fallback), and the new `gander paths` command prints every resolved
  location. The `.gander/config.toml` layer is deprecated (still loads,
  with a warning) — use a committed `gander.toml` or the XDG user config.
- Artifacts default to stdout: `gander export` and
  `--artifact-on-quit write` only write files for explicit output paths or
  a configured `[artifact] output-dir` (previously `.gander/review.*`).
- CI (flake check, fmt, clippy, tests) now runs on builds.sr.ht for every
  push via `.builds/ci.yml`; the Linux release manifest moved to
  `builds/release-linux-x86_64.yml` so artifacts and the downloads page are
  only built and published for explicit releases.

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
