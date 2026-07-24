# Architecture map

Gander reads jj-visible code and writes review state. The CLI, TUI, MCP server,
and exporters are adapters around the same durable model and review services;
none of them fetches from or posts to a forge.

## Review stream

- `src/app/` loads a target and combines parsed diffs with durable
  `ReviewState`/`ReviewSession` data.
- `src/app/stream.rs` projects that combined model into the cross-file stream:
  chapter headers, file and diff rows, folds, and annotation-card owners.
- `src/app/reading.rs` is a toolkit-independent adapter over that canonical
  stream. It groups rows into stable regions for web-sized rendering and
  carries effective salience and shared card ownership forward without
  resolving attention, folds, chapters, or annotation placement again.
- `src/tui/viewport.rs` owns stable row selection, scrolling, transitions, and
  restoration across reprojections. `src/tui/mod.rs` coordinates input,
  refresh, autosave, and projection invalidation.

## Attention and generation caches

- `src/attention.rs` resolves effective salience and acknowledgement from
  durable regions, generated-file heuristics, and fingerprints. Human
  overrides outrank agent curation, which outranks heuristics; stale evidence
  remains visible but does not count toward current coverage.
- `src/app/` owns monotonic durable and projection generations. Mutation seams
  bump the relevant generation; stream and owner-lookup caches validate those
  counters rather than hashing the session each frame.
- `src/review.rs` owns review-state file transaction seams. Short-lived
  CLI/MCP/ACP writers lock across reload/mutate/atomic-save; live instances
  retain a last-persisted baseline and merge only their changed fields over the
  latest locked snapshot. Stable-id children merge independently, append-only
  fingerprinted attention history unions, explicit deletions remain deleted,
  and unchanged stale state is never replayed.
- `src/app/stream.rs` materializes expensive syntax/folding rows only for the
  bounded visible window. Callers must mutate through the established service
  or app seams so cache invalidation remains correct.

## Channels and identity

- `src/state.rs` defines `Identity`, annotation `Channel`, comments, replies,
  anchors, and serde-compatible durable state.
- `src/review.rs` is the shared mutation and selector service used by CLI and
  MCP adapters; `src/config.rs` supplies configured human/agent identities and
  channel defaults.
- Channel inference and TUI composition live in `src/tui/mod.rs`. Publication
  boundaries are enforced by artifact profiles, not merely by presentation.
  See [annotations.md](annotations.md) for the lifecycle and privacy contract.

## Annotation cards and output

- `src/tui/annotation_card.rs` projects comments, drafts, and walkthrough
  narration into one card view model; `src/tui/render.rs` renders that model
  with the shared channel color language.
- `src/theme.rs` derives UI-toolkit-agnostic RGB theme slots; `src/tui/theme.rs`
  adapts them to ratatui styles, terminal background detection, and xterm-256.
- `src/artifact.rs` builds JSON/Markdown review artifacts from durable state.
  `src/web_render.rs` is the pure HTML layer: it accepts the toolkit-independent
  `ReadingProjection` plus file metadata and explicit transport capabilities,
  and owns guide DOM, escaping, theme tokens, and component assets.
  `src/web_export.rs` profile-filters first, then embeds that renderer eagerly
  with local-only handwritten navigation; `src/delegation.rs` builds the
  narrower external-harness work packet.
- `src/main.rs` and `src/mcp.rs` should remain thin adapters over the same core
  operations. A new MCP or TUI capability requires a scriptable CLI equivalent.

## Live instance adapters

- `src/registry.rs` and `src/acp.rs::socket` are shared lifecycle plumbing for
  both `gander tui` and `gander web`; each live process advertises the same
  workspace/target/summary/socket/pid/heartbeat record and drains the same
  typed ACP request loop.
- `src/web.rs` is the M16 loopback HTTP adapter. Its centralized middleware
  validates the per-process capability token, exact bound Host, and same-origin
  Origin before every route (including fragments, SSE, and assets). Phase 2
  server-renders the attention overview and near-viewport reading regions from
  `src/app/reading.rs`, leaves offscreen structural skeletons, and lazily serves
  generation-checked stable-region fragments. The traditional mode expands the
  same shared folds through explicit full-mode fragments rather than resolving
  salience independently; guided responses do not embed hidden skim lines.
  Phase 3a's coalescing watcher applies the same merge-aware durable-state and
  overlay reload semantics as the TUI, and gives every jj poll exactly one
  deliberate snapshot followed only by `--ignore-working-copy` reads. It
  projects and renders outside the short shared-projection lock, diffs stable
  overview/coverage/footer/stream regions, and publishes bounded SSE updates;
  lagged or stale clients recover with a full region set. Later
  Phase 4 actions enter the live loop through a bounded command channel, check
  the projection generation, call `src/review.rs`/shared attention services,
  complete a baseline-aware locked atomic save, and only then return the new
  generation. It supplies live action/lazy-fragment capabilities to
  `src/web_render.rs`; tokens, guarded URLs, SSE, and presenter chrome stay in
  this adapter and never enter static HTML. The web adapter contains no
  independent review-domain policy.

## Change guide

Persisted fields start in `src/state.rs` with serde defaults, then flow through
`src/review.rs`, artifacts, CLI/MCP adapters, and tests. Stream-visible changes
also need an explicit generation bump and projection/card coverage. Keep review
state writes separate from code-workspace mutation, as required by
[vision.md](vision.md).
