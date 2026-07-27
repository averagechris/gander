# Derived theme and terminal background detection

Home of the M19 presentation-polish items "derived theme system" and "auto
light/dark via OSC 11" (docs/roadmap.md, milestone 19). This page records
both the **original requirements** and the **deltas we consciously accepted**
after review, so nobody has to rediscover them adversarially. Configuration
lives in the `[theme]` section (see the README config example); the
implementation is `src/theme.rs` (shared palette→slot derivation),
`src/tui/theme.rs` (terminal adapter and detection), and `src/tui/osc_guard.rs`
(late-reply containment).

## What the theme guarantees

- Every TUI chrome color derives from a small light/dark base palette
  (background, foreground, accent, add/remove hues, info) through
  WCAG-2.x-contrast-guarded blending. Semantic slots are resolved *before*
  cells are written; no color value ever doubles as semantic metadata.
- `[theme] name` selects a built-in light/dark palette pair for the shared core:
  `gander`, `catppuccin`, `gruvbox`, `solarized`, `nord`, `tokyo-night`, or
  `dracula`. Names are normalized to lowercase with spaces/underscores treated
  as hyphens; aliases include `default`, `gander-default`, `catppuccin-mocha`,
  `catppuccin-latte`, `gruvbox-dark`, `gruvbox-light`, `solarized-dark`,
  `solarized-light`, `nordic`, `tokyonight`, `tokyo`, `tokyo-night-storm`, and
  `dracula-pro`. Unknown names fail config loading with the accepted canonical
  names. `[theme.palette.dark]` and `[theme.palette.light]` may override any base
  entry (`background`, `foreground`, `accent`, `positive`, `negative`, `info`)
  with `#rrggbb` values.
- User-provided `[diff.theme]` and `[syntax.theme]` specs are literal styles
  and are never reinterpreted. Unset `[diff.theme]` entries derive from the
  palette. Syntax-token styles remain literal generally; changed-word
  emphasis is the intentional exception, overriding the foreground with the
  theme foreground so its tint satisfies the contrast contract.
- On terminals without truecolor (`COLORTERM`), every derived slot quantizes
  to xterm-256 and the contrast contract below is re-established in indexed
  space. Explicit user hex specs quantize to the nearest indexed color, as
  before.
- Transparent mode (default) suppresses only painting the base background;
  contrast is then guaranteed against the *detected* terminal background
  when available, the palette background otherwise.

### Contrast contract

The original requirement was a blanket 4.5:1 (WCAG AA) target. A blanket
target is not satisfiable for every combination: a colored slot sitting at
its own 4.5:1 floor against the base background has zero headroom left for
any brighter surface, so demanding AA for every slot on every surface would
force all surfaces down to the plain background and erase the highlights.
The **actual, tested contract** (asserted in the final output color space,
after quantization, in `src/theme.rs` / `src/tui/theme.rs` tests and the rendered-buffer test
`rendered_diff_output_meets_the_documented_contrast_contract` in
`src/tui/render.rs`) is:

| combination | minimum |
| --- | --- | --- |
| primary foreground vs. effective background | 7.0:1 (AAA), capped at the best physically achievable ratio for extreme detected backgrounds |
| standard chrome text slots vs. effective background | 4.5:1 (AA) |
| muted text and gutter bars vs. effective background | 3.0:1 |
| primary foreground on every derived surface | 4.5:1 |
| positive/negative text on their own line backgrounds | 4.5:1 |
| changed-word emphasis (renders in the primary foreground) on its tint | 4.5:1 |
| foreground/subtle text on cursor-row and range surfaces | 4.5:1 |
| colored semantic text and muted text on cursor-row and range surfaces | 3.0:1 (WCAG 1.4.11 non-text level; these are transient highlights) |

Syntax-token colors are user-owned literals and are exempt: we render them
  untouched and cannot guarantee their contrast.

## Named theme syntax defaults

`[theme] name` may select one of Gander's built-in named palettes. Each named
theme exposes an optional Gander-owned syntax default through `gander themes
list` (text or JSON):

| palette theme | dark/auto syntax default | light syntax default |
| --- | --- |
| `gander` | `gander-dark` | `gander-light` |
| `catppuccin` | `gander-dark` | `gander-dark` |
| `gruvbox` | `gander-dark` | `gander-light` |
| `solarized` | `gander-dark` | `gander-light` |
| `nord` | `gander-dark` | `gander-dark` |
| `tokyo-night` | `gander-dark` | `gander-dark` |
| `dracula` | `gander-dark` | `gander-dark` |

These names describe Gander's built-in mappings only; they are not claims of
exact equivalence to third-party editor themes. Gander currently has only the
`gander-dark`, `gander-light`, and `monochrome` syntax palettes available, so
third-party-inspired palette themes intentionally map to the closest available
Gander syntax palette. `mode = "auto"` applies the dark/default syntax choice at
config load because runtime terminal background detection does not rewrite config
after startup. Explicit `[syntax.theme]` values keep higher precedence and
override the named-theme syntax default for both TUI and HTML/web highlighting.

## What background detection guarantees

`mode = "auto"` queries the terminal once (OSC 11, DA1-fenced, ≤1 s budget)
via `terminal-colorsaurus`, before Gander enables crossterm application raw
mode and before the crossterm event reader starts. `terminal-colorsaurus`
temporarily uses and restores its own guarded raw mode for the query. Explicit
`dark`/`light` modes and every noninteractive command never query and never
open the terminal.

- No crashes, no busy loops: waits are deadline-bounded; zero-byte reads
  (EOF/HUP) terminate immediately; termios is restored on success, error,
  and unwind (verified by PTY tests in `src/tui/pty_tests.rs`, including a
  hangup-mid-query scenario).
- No hand-rolled input parsing: the crossterm event pipeline is untouched.
  Function keys, modified keys, Alt chords, split UTF-8, mouse, focus,
  paste, and resize all flow through crossterm exactly as without the
  feature (PTY-tested end to end).
- Only a *parsed reply* disarms containment. Any other outcome — timeout,
  I/O error, malformed reply, and even "unsupported", which the query
  library can report without a fully validated DA1 fence — arms
  [`OscTailGuard`] for 8 seconds. The guard watches decoded events for the
  exact shape of a stray OSC 11 reply (or its headless tail), drops matches,
  releases low-confidence holds in order on the next 150 ms idle tick, and
  never dispatches a confident payload (`Alt+']' 11;` matched) under any
  interruption. Resize events pass through matches untouched. A full leak,
  were one ever to occur, is limited to the payload alphabet
  (hex/`rgb:/;#]` characters); those characters can trigger configured
  commands and mutate durable local review state (for example mark viewed,
  accept a draft, or delete a walkthrough). Leaked payload characters can
  **never mutate the code workspace, under any keybinding configuration**:
  gander no longer launches agent processes (docs/decisions.md D9), and the
  only shell-out left — the jj helper popup — requires a literal Enter press
  on its final verbatim-command confirmation regardless of how
  `popup-select` is bound, and an OSC payload can never contain Enter.
- Fallback is always the historical dark look.

## Documented deltas from the original safety requirements

The original requirement set ("preserve unrelated input in order",
"OSC payload must never dispatch", "no input loss") cannot all be met
simultaneously by any existing maintained query mechanism: the query and
the event loop are two readers of one file descriptor, consumed bytes
cannot be re-injected, and owning byte-level input framing ourselves is the
previously rejected approach (a partial reimplementation of the terminal
input grammar). Rather than pursue technical purity, we accept these
**bounded, non-crashing, non-destructive** residuals:

1. **Startup typeahead loss (auto mode only).** Keys typed during the
   sub-second query window are consumed by the query's reader and
   discarded — never misdispatched. This matches the behavior of other
   tools that query terminal colors (e.g. delta). Not present with
   `mode = "dark"`/`"light"`.
2. **Drip-fed headless tail.** A late reply whose head was consumed by the
   timed-out query *and* whose remaining bytes arrive fragmented with
   >150 ms gaps can release a few payload characters as key events. This
   requires a timeout plus partial head consumption plus pathological
   byte-level fragmentation; contiguous tails (the realistic case) are
   fully contained and PTY-tested.
3. **Armed-window input quirks (≤8 s after a failed query only).**
   Payload-alphabet keystrokes are normally delayed by one 150 ms poll
   tick; sustained payload-alphabet key repeat that never goes idle can
   hold up to the 64-event cap (a couple of seconds at typical repeat
   rates) before the overflow flush. Input that byte-for-byte forms an OSC
   payload terminated by BEL/`Alt+\` is dropped; a user literally typing
   `Alt+']' 1 1 ;` loses those keystrokes.
4. **Replies later than the 8 s window dispatch ungated.** Once the guard
   window expires it disarms; a pathologically late reply tail then reaches
   the key handler unfiltered. Exposure is still confined to the payload
   alphabet plus `Alt+']'` (unbound by default) and `Ctrl+G`
   (cancel-range-comment), and requires a terminal that answers an OSC
   query more than eight seconds late.
5. **Split-after-ESC misclassification (upstream).** A reply fragmented
   immediately after its leading ESC makes the query library report
   "unsupported" (safe dark fallback). The guard stays armed for this
   outcome, so the late tail is contained (PTY-tested), but auto-detection
   yields dark instead of the true background color.
6. **Query-window memory is time-bounded, not byte-capped.** An adversarial
   terminal can make the query buffer input for up to its 1 s budget. The
   post-timeout guard side is capped at 64 held events.

Revisiting these requires a query/input layer that validates the DA1 fence,
returns unrelated bytes, and exposes partial parser state — realistically an
upstream contribution (crossterm learning OSC responses, or a colorsaurus
API extension), tracked as future work under milestone 19.

## Cross-references

- Requirements: docs/roadmap.md, milestone 19 (first two items).
- Config reference: README `[theme]` and `[diff.theme]` sections.
- Diff-cue derivation details: docs/focused-diff-ux.md §"Config".
- Implementation: `src/theme.rs`, `src/tui/theme.rs`, `src/tui/osc_guard.rs`,
  `src/tui/pty_tests.rs` (production-path PTY scenarios).
- Web reuse (M16): the palette→slot derivation lives in a shared core with
  built-in named palettes and config overrides; `gander web` and static HTML
  exports serve the slots as CSS custom-property tokens with a system/light/dark
  toggle. See docs/web.md §Theming.
