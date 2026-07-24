# Local web UI (`gander web`)

Design for milestone 16. Status: Phase 5 implemented (secure standalone peer,
shared-projection reader, lazy regions, durable SSE liveness, agent-guided
browser presentation, full review-state mutation parity, and static-export
template convergence).

Gander exists to spend reviewer attention where the mental-model delta is. In
an age of abundant generated code, most of a change is boilerplate and glue;
the review's job is to keep the reviewer's mental model current and give them
the footing to critique intent. The web UI is the surface where that curation
breathes: guided onboarding paths authored by agent harnesses, live
agent-driven presentation, and an attention-first reading experience — while a
full, traditional every-line review remains equally first-class.

In one sentence:

> `gander web` is a live, local, browser-rendered peer of the TUI that agents
> can guide in real time through the same instance surface they already use.

## Model

`gander web` is a **standalone peer instance**, not a TUI feature and not a
new kind of agent endpoint:

- It is its own process serving HTTP on `127.0.0.1` (random free port by
  default, `--port` to pin; prints the URL and optionally opens the browser).
- It registers in the existing instance registry (workspace root, target,
  summary, socket path, pid, `last_input_at`) exactly like a TUI instance
  (docs/decisions.md D3), and hosts the same per-instance ACP Unix socket —
  including `present/*` (D8).
- `gander acp`, `gander mcp`, and `gander present` therefore route to a web
  instance by cwd with zero new agent-facing protocol. Agents drive the web
  view with the same commands that drive the TUI.
- Browser interaction feeds `last_input_at` and `review/current_focus`, so a
  harness can answer "what am I looking at?" for the web view just as it does
  for the TUI.

Command contract:

```sh
gander web [--port <port>] [--no-open]
```

The process prints the complete capability-bearing URL on stdout. Browser
auto-open is deliberately unavailable for now because Gander has no existing
cross-platform, dependency-free opener convention; the default is to leave
opening to the user, and `--no-open` suppresses the explanatory stderr note for
scripts. The token exists only in process memory and the printed URL. `gander
paths` reports the bind convention and existing socket/registry locations,
never the token.

The browser is a **renderer, not a second implementation**. The server
projects the same review stream the TUI renders (`src/app/stream.rs`: chapter
headers, file rows, skim folds, spotlight narration cards, annotation cards)
into server-rendered HTML view models, and pushes updates over SSE. Business
logic lives only in the core services (vision design rule 3); web mutations
call the same `src/review.rs` seams as the CLI/MCP. CLI parity (design rule 4)
holds by construction because the web surface is a strict subset of existing
core capabilities.

No JS toolchain: assets are embedded in the binary, templates rendered in
Rust, with small hand-written scripts. `src/web_render.rs` owns a pure guide
view model over `ReadingProjection`, semantic HTML rendering, theme tokens,
and component CSS. Both `src/web.rs` and `src/web_export.rs` supply explicit
transport capabilities to it; neither calls through the other's HTTP/export
internals. A guide authored once therefore has the same overview, chapters,
guided/full stream, folds, narration, artifacts, comments, channels, and
salience semantics live or hosted as an exported file.

## Liveness

The server observes the same inputs as TUI watch mode and pushes changes to
connected browsers in roughly real time (one poll/event tick):

- durable review state (`state.json`) and the agent overlay (`agent.json`)
  via the state watcher;
- the jj target via the existing read-only `--ignore-working-copy` refresh
  discipline, with the same single deliberate snapshot point per poll
  (roadmap backlog item 4 applies unchanged).

Change delivery is generation-based: the server re-projects affected view
models, bumps the projection generation, and emits an SSE `state` event
carrying patch fragments keyed by stable region ids (file sections, cards,
folds, footer/coverage). The client swaps those regions in place; a dropped
SSE connection reconnects and requests a full re-render. Fingerprint-guarded
durability semantics (viewed state, acknowledgements, stale regions) are
identical to the TUI because they are computed by the same core.

Phase 3a polls files every 250 ms and jj every two seconds. Bursts coalesce at
the poll boundary. Each effective projection change increments a server-owned
monotonic generation and emits one `state` event (`id` is that generation):

```json
{"generation":43,"full":false,"order":["chapter-…","file-…"],"patches":[{"id":"coverage","guided":"<div …>","full":"<div …>","remove":false},{"id":"file-old","guided":null,"full":null,"remove":true}]}
```

Stable patch ids include `overview`, `coverage`, `file-tree`, `footer`, and the
shared reading-region ids. `order` is the canonical stream order so additions
and moves do not require a page render. The initial EventSource query supplies
`generation`; on automatic reconnect the browser's `Last-Event-ID` is
authoritative because EventSource retains its original query string. An absent,
stale, ahead, or broadcast-lagged cursor receives a `full: true` projection
patch set (still region swaps, not a whole-page response). The stream sends a
15-second comment keepalive, uses bounded per-client and broadcast queues, and
drops its forwarding task as soon as the client disconnects.

Concurrent instances (a TUI and a web server on one workspace) mirror each
other through durable-state watching, the same way a TUI mirrors external CLI
writes today. M16 Phase 0a completed the prerequisite M14 autosave-race fix:
short-lived CLI/MCP/ACP mutations are locked transactions, while each live
instance saves only changes since its last persisted baseline over the latest
locked state. A stale instance therefore cannot replay unchanged comments,
sessions, viewed marks, or fingerprinted attention progress over another
writer.

## Reading experience: attention-first, full review always

The landing view is the **attention map, not the file tree**: session
summary, chapters, spotlight count, skim-fold totals, coverage progress, and a
"start guided tour" affordance when a walkthrough exists. One click drops into
the stream.

The stream is the same one-diff-view contract as the TUI (docs/attention.md):

- **Skim** regions render as one-line folds — expandable in place,
  acknowledgeable with one key/click; whole-file folds mark viewed through the
  existing fingerprint-guarded service.
- **Supporting** regions render as a normal diff.
- **Spotlight** regions render expanded with inline narration cards
  (onboarding annotations: title, why, rationale, expandable artifacts),
  where the web's typography and space can outclass the terminal.

A **traditional full review is a peer mode, not a fallback**: every file,
every line, file tree, search, viewed checkboxes, comment threads. Salience
still tints the margins but nothing is folded unless asked. Switching between
guided and full is one control and loses no state.

## Full review parity (v1 mutations)

Everything the TUI can write, the web can write, through the same services:

- mark files viewed / unviewed;
- acknowledge skim folds (single and bulk), with identical fold identity,
  progress fingerprints, and stale-exclusion rules;
- add, edit, reply to, and change the state of comments, with channel
  inference and the channel color language (docs/annotations.md);
- accept/discard agent draft comments (triage);
- promote/demote salience on the region under the cursor (human overrides
  outrank agent curation, as everywhere);
- follow walkthrough ordering (next/prev/goto over spotlights);
- expand/collapse context and folds.

Mutations POST to small action endpoints that map one-to-one onto review
service calls and return the new generation; the SSE stream then patches every
connected client, including a concurrently open TUI (via durable-state
watching).

## Agent-guided presentation over the web

This is the "ask my agent about the review and watch it guide me" flow. It
extends D8 unchanged in spirit: presentation is ephemeral, socket-transported
UI control.

- `present/*` requests arriving on the web instance's ACP socket (from
  `gander present`, MCP tools, or raw JSON-RPC) are validated against the
  current diff and broadcast to connected browser tabs as SSE `present`
  events: scroll to a target, flash/pin a highlight range, show an ephemeral
  note callout, start/next/prev/goto/end over durable spotlights.
- Presenter events carry the normal status schema plus a stable reading-region
  and row target. A latest-value channel coalesces bursts before each tab's
  bounded SSE forwarder, so navigation storms cannot build a scroll queue.
- **Follow mode.** When a presentation starts, tabs follow the presenter by
  default. Manual scrolling breaks follow ("following paused — rejoin"), with
  the presenter's position kept visible as an edge indicator; one key/click
  snaps back. The human always wins.
- **Busy gating.** If the human is mid-edit in a browser editor/modal,
  presentation
  commands do not yank the view: the server answers the agent with the
  existing `user is busy: <mode>` error and the client shows a pending
  presenter indicator instead. Phase 3b reports the existing search editor;
  Phase 4 comment forms plug into the same renderer-state field.
- **Browser interaction.** Each tab reports its stable visible/selected stream
  row (file, old/new line, hunk, pane) and current editing/modal state through a
  guarded renderer endpoint. The most recently server-observed connected tab
  controls `review/current_focus` and busy gating; tab id is the deterministic
  tie-breaker. Reports heartbeat the existing instance registry.
- **Ephemeral vs durable.** `present/focus` notes are transient callouts.
  Anything meant to persist — narration, walkthrough steps, attention
  regions, comments — flows through the durable CLI/MCP surfaces exactly as
  today, and appears in the web view through normal state liveness.

The intended composition is the split-screen harness workflow from
docs/harness-setup.md with the browser in place of (or beside) the TUI: the
human asks a question in the harness chat; the agent reads
`review/current_focus`, answers in chat, and runs `gander present focus ...`
to walk the human's web view through the relevant regions while it talks.

## Catered guide and onboarding experiences

Harness-built onboarding experiences are **data, not plugins** (D9 stands:
gander never runs agent code and never spawns agents). A catered experience is
a curated durable session:

- a walkthrough (ordering over spotlights, narration, artifacts, chapters);
- an attention map (spotlight/supporting/skim with rationales);
- onboarding-channel annotations and draft comments;
- optionally a live presenter driving `present/*` while the reader follows.

The web UI's job is to render that curation as a coherent guided read:
overview page, chapter navigation, coverage progress, and prev/next flow. An
agent harness "builds an onboarding experience" purely by writing review state
through the CLI/MCP — the same artifact renders in the TUI, the live web UI,
and the static HTML export.

The static artifact uses the exact shared guide regions eagerly, then adds only
publication-safe export provenance/action-item summaries. Its handwritten
inline script provides theme/mode switching, file and chapter anchors, fold and
context disclosure, and previous/next Spotlight navigation without a server.

## Look and feel

The web UI should look like a considered modern tool, not a rendered
terminal. Design language, pinned so implementation doesn't drift:

- **Typography first.** A system UI stack for chrome and a good monospace
  stack for code (no bundled webfonts; the binary stays lean). Generous line
  height and measure in narration cards — prose deserves prose typography.
- **Color is semantics, nothing else.** Chrome is quiet and neutral; color
  appears only where it means something: diff add/remove, channel identity
  (onboarding/delegation/collaboration/note), salience, presenter highlights.
  This is the same discipline as the TUI's channel color language and it is
  what makes the attention map legible.
- **Cards and folds carry the hierarchy.** Narration/annotation cards get
  subtle elevation and rounded corners; skim folds read as compact, calm
  one-liners; spotlight regions get the space. No gradients-for-decoration,
  no ornamental noise.
- **Motion is meaning.** Smooth scroll for presenter navigation and a brief
  highlight pulse on `present/focus` targets — and nothing else moves.
  `prefers-reduced-motion` swaps smooth scroll for instant jumps and pulses
  for static outlines.

## Theming

One derived theme core, two renderers. The M19 palette→slot derivation
(docs/theme.md, `src/tui/theme.rs`) moves to a shared core module: a small
base palette (background, foreground, accent, positive, negative, info) is
expanded through the same contrast-guarded blending into semantic slots. The
web server renders those slots as **CSS custom properties** — one token block
per scheme (`[data-theme="dark"]`, `[data-theme="light"]`) — and all
component CSS references tokens only. No literal colors in component styles,
ever: that rule is what keeps the stylesheet themable and maintainable
instead of spaghetti, and it means every theme inherits the WCAG contrast
contract for free.

Static HTML embeds the same generated token blocks and component stylesheet.
The export command applies the configured theme while retaining both schemes;
there is no export-only palette or component style fork.

- **Built-in themes.** A theme is just a named light/dark palette pair plus
  an optional syntax theme, so shipping many is cheap. Gander ships its own
  default pair plus common community palettes (e.g. Catppuccin, Gruvbox,
  Solarized, Nord, Tokyo Night, Dracula), all derived through the same
  contrast guards. `gander themes list` enumerates them (CLI parity).
- **Configuration.** `[theme] name = "gruvbox"` selects a built-in for both
  TUI and web; `[theme.palette.dark]`/`[theme.palette.light]` override
  individual base-palette entries; `[syntax.theme]` keeps working unchanged.
  Configure once, both renderers match.
- **Light/dark/system toggle.** A header control cycles system → light →
  dark. "System" follows `prefers-color-scheme` live (the web analog of the
  TUI's OSC 11 auto-detection). The choice persists per browser in
  localStorage; a tiny inline script applies it before first paint so there
  is no flash of the wrong scheme. Both schemes' token blocks are always
  served; switching is one attribute flip.
- **Escape hatch.** An optional user stylesheet (`[web] extra-css = "path"`)
  is loaded last for power users. The custom-property tokens are the stable
  theming contract; DOM structure and class names are not. Gander still embeds
  its own assets; this configured stylesheet is the only intentional local file
  read by the web server, and failures include the path and config key.

## Performance budgets

Snappiness is an attention feature: every stall between intent and response
burns the reviewer's focus, and gander's whole point is spending that focus
on the change. Budgets, asserted where practical:

- **First meaningful paint is server-rendered HTML** — the overview and the
  visible stream window arrive in the initial response; no framework boot,
  no client-side data fetch before content. Target: interactive on a normal
  change in well under a second on localhost.
- **Long diffs are windowed, like the TUI.** The initial document carries
  full rendering for the near-viewport window plus a lightweight structural
  skeleton for offscreen files (mirroring the stream's cheap structural
  rows); scrolling fetches rendered fragments on demand. `content-visibility:
  auto` keeps offscreen sections out of layout. A huge generated file must
  never make the page heavy — it is a one-line fold with lazy expansion.
- **Mutations feel instant.** Viewed marks, fold acknowledgements, and
  comment saves apply optimistically in the client and reconcile against the
  generation returned by the action endpoint; a conflict falls back to the
  server's patch. Perceived interaction feedback within one frame.
- **SSE patches are surgical.** Region-keyed swaps, no full-page re-renders,
  no layout thrash outside the patched region. Presenter events coalesce so
  a fast-driving agent cannot queue up a scroll storm.
- **Measured, not vibes.** The demo-sized fixture gets a perf smoke test
  (document size, time-to-render, patch-apply cost) so regressions show up
  in CI rather than in reviewers' attention.

Static export intentionally embeds every full-review row because it must work
offline with no fragment endpoint. It still uses the shared guided folds and
`content-visibility` component styles; artifact size therefore tracks the diff
payload, unlike the live first-paint window.

## Security and boundaries

- Loopback only. The server binds `127.0.0.1` and refuses non-loopback bind
  addresses in v1; sharing is what artifact export is for.
- A per-session capability token is generated at startup, embedded in the
  printed URL, and required on every request including the SSE stream.
  `Origin`/`Host` are validated to block DNS-rebinding and cross-origin
  browser probes. Tokens are never written to durable state.
- All product boundaries hold: gander reads code state and writes review
  state. No forge fetching or posting, no code-workspace mutation, no agent
  spawning, no chat UI. The web server adds no capability that lacks a CLI
  equivalent.
- Static export is a separate capability profile: no capability token, guarded
  URL, external asset, fetch, SSE, presenter, or mutation endpoint is emitted.
  Artifact-profile filtering runs before the shared projection, so team HTML
  cannot recover private walkthrough, attention, task, viewed, or note data
  from hidden DOM or script state.
- The confirmed jj helper popup does not cross to the web in v1; mutating jj
  helpers stay in the TUI where the literal-Enter confirmation model is
  established.

## Protocol sketch

| Endpoint | Purpose |
| --- | --- |
| `GET /` | server-rendered app shell (overview + stream) |
| `GET /events` | SSE: `state` (generation + region patches), `present` (presenter events), `notice` |
| `POST /interaction` | Ephemeral per-tab visible focus and busy report; updates live routing heartbeat |
| `POST /actions/<verb>` | one-to-one review-service actions (viewed, acknowledge, comment add/edit/reply/state, salience set/clear/promote/demote, triage, walkthrough nav); token-gated; returns the new generation |
| `GET /fragment/<region>` | re-fetch a single rendered region (reconnect/patch fallback) |

Phase 4 exposes `GET /`, embedded CSS/handwritten JS assets, token-gated `GET
/fragment/<region>` lazy rendering, and the long-lived generation protocol on
`GET /events`.
Every route passes through one centralized guard enforcing the exact bound
`Host`, absent-or-exact-same `Origin`, and capability token; unknown paths are
guarded too. Fragment ids are stable projection-region ids and requests carry
the projection generation, so unknown ids return 404 and stale generations
return 409 rather than silently substituting content. Phase 4 action endpoints
use the same guarded, generation-checked path. Guided skim regions never serialize their hidden
rows into the initial page (including search metadata); switching to full mode
requests those rows explicitly, preserving both the all-lines contract and the
compact first paint for huge generated changes.

Action bodies require the expected projection generation plus stable
fold/comment ids or explicit current file/range targets. Unknown JSON fields
and malformed bodies return 400, unknown selectors return 404, and generation
mismatches return 409. Success returns
`{ "generation": N, "result": ... }` only after the merge-aware locked atomic
save. Browser context/fold/card expansion remains ephemeral and never enters
review state.

Phase 4 action verbs and payload fields (all also include
`expected_generation`) are:

- `file-viewed` / `file-unviewed`: `path`;
- `skim-acknowledge`: exactly one of `fold_id` or `path` plus optional
  `line`/`end_line`; `skim-acknowledge-all` has no selector;
- `comment-add`: optional `path`/`line`/`end_line`, `body`, optional
  `kind`/`action`/`state`/`channel`/`source_comment_id`; `comment-edit`:
  `id` plus editable fields; `comment-reply`: `id`, `body`, optional `resolve`;
  `comment-state`: `id`, `state`;
- `draft-accept`: `id`, optional edited `body`/`channel`; `draft-discard`:
  `id`;
- `salience-set|clear|promote|demote`: `target` (`path`, optional
  `line`/`end_line`), plus `salience` for set and optional `rationale` where
  meaningful;
- `walkthrough-next|prev`: no selector; `walkthrough-goto`: `step_id` and
  optional `part`.

## Runtime dependency decision

The live server uses Axum 0.8 with default features disabled and only
`tokio`, `http1`, `json`, and `query`. This is the sole direct runtime
dependency added for M16 Phase 1; assets and templates remain embedded Rust
strings, and the existing Tokio, serde_json, and UUID facilities provide the
runtime, protocol values, and ephemeral capability token.
Phase 3a names the already-transitive `futures-core` package directly only for
the standard `Stream` trait required by Axum's SSE body; it adds no package or
runtime implementation.
Cargo-deny grants BSD-3-Clause only to Axum's exact `matchit 0.8.4` transitive
dependency; upgrades must revisit that crate-scoped exception.

The ACP socket surface is unchanged; `present/*` and `review/current_focus`
gain a web-backed implementation. Anything new that proves useful must land in
the CLI first or simultaneously (design rule 4).

## Non-goals (v1)

- Hosting for anyone but the local user (no TLS, no auth beyond the token, no
  non-loopback binding).
- A JS build toolchain or SPA framework.
- Web-initiated jj mutations, forge integration, or agent invocation.
- Editing walkthrough/attention curation from the browser beyond
  promote/demote — authoring stays CLI/MCP/TUI in v1.
- Mobile-first layout (should degrade acceptably, not be designed for).

# Local web UI performance budgets

The M16 smoke coverage is browser-independent. It measures deterministic
proxies that CI can run without adding a browser, package, feature, or toolchain:
server-side document generation time, HTML and patch byte counts, initial
meaningful-content placement, bounded initial stream materialization, structural
skeleton counts, guarded fragment lookup, generation-guarded action
reconciliation, surgical SSE patch scope, and presenter coalescing. These
numbers do **not** claim browser layout, paint, network, or JavaScript execution
timing.

Demo-sized CI budgets in `src/web.rs`:

- initial server-rendered document: at most 64 KiB;
- initial meaningful content marker: within the first 12 KiB;
- server render proxy elapsed time after warmup: at most 250 ms;
- initial full regions match the live window plus chapter rules, with remaining
  regions represented as structural skeletons;
- guarded fragment lookup returns full shared-renderer HTML for the current
  generation and rejects stale generation requests;
- surgical patch scope stays ordered and below 16 KiB for the fixture update;
- generation-guarded actions reject stale optimistic writes;
- presenter coalescing keeps only the latest move in the watch channel.

Elapsed ceilings are intentionally generous and backed by structural byte/count
assertions so the gate is deterministic on SourceHut and local Nix runners.
