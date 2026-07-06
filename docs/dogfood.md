# Dogfood loop

Gander is graded by agents actually using it. Evaluator agents drive the real
binary (CLI directly; TUI via tmux `send-keys`/`capture-pane`) against fixture
jj repos, follow scripted scenarios, and score the experience on a fixed
rubric. Findings become action items here; after a workstream lands, the same
scenario is re-run and the score trend tells us whether we actually improved.

The harness lives in [`eval/`](../eval/README.md): fixture generator,
scenario scripts, rubric, and archived reports.

The loop:

1. Run the relevant scenario(s) from `eval/scenarios/` with a fresh fixture.
2. Archive the report under `eval/reports/<date>-<label>/`.
3. Update the scorecard and action items below.
4. Pick the next workstream; repeat.

## Scorecard

Scores are 1–5 per rubric dimension (see `eval/rubric.md`).

| Dimension                     | 2026-07-05 baseline | 2026-07-05 post-W1 | 2026-07-05 post-W2/W3 | 2026-07-05 round 2 | 2026-07-05 round 3 | 2026-07-05 round 4 | 2026-07-05 round 5 | 2026-07-05 round 6 | Target |
| ----------------------------- | ------------------- | ------------------ | --------------------- | ------------------ | ------------------ | ------------------ | ------------------ | ------------------ | ------ |
| CLI discoverability           | 3                   | 4                  | —                     | —                  | —                  | 3⁴                 | **4** ✓            | **4** ✓⁶           | 4      |
| TUI review ergonomics         | 4                   | —                  | 4                     | **2**¹             | 3.5³               | 4⁴                 | 4⁵                 | 4⁶                 | 4.5    |
| CLI output quality for humans | —                   | —                  | —                     | —                  | —                  | —                  | 3⁵                 | 3.5⁶               | 4      |
| CLI output quality for agents | 3                   | 5                  | —                     | —                  | —                  | 4⁴                 | **4** ✓            | **4** ✓⁶           | 4      |
| Handoff readiness             | **2**               | **4**              | —                     | —                  | —                  | 3⁴                 | **4** ✓            | **4** ✓⁶           | 4      |
| Zen usefulness, uncurated     | **2**               | —                  | 3                     | 3                  | 3³                 | **2**⁴             | 3⁵                 | 3⁶                 | 3.5    |
| Zen usefulness, curated       | 4                   | —                  | 4                     | 3.5¹               | 4³                 | 4⁴                 | 3.5⁵               | 3.5⁶               | 4.5    |
| Curation protocol ergonomics  | **2**               | —                  | 3                     | 3.5                | 3.5³               | 3.5⁴               | **4** ✓            | **4** ✓⁶           | 4      |
| Watch: freshness / follows @  | 4                   | —                  | —                     | 4²                 | 4³                 | **5** ✓            | **5** ✓            | **5** ✓⁶           | 4.5    |
| Watch: change awareness       | **2**               | —                  | —                     | 3²                 | 4³                 | **4** ✓            | **4** ✓            | **4.5** ✓⁶         | 4      |
| Pane-worthiness overall       | 3                   | —                  | —                     | 3.5²               | 3³                 | 4⁴                 | 4⁵                 | **4.5** ✓⁶         | 4.5    |

¹ Round-2 scenario 2 hit the state-erasure blocker and the
chapter-stats bug; both (plus the launch-target and chooser majors)
were fixed and re-verified the same day — the scores predate the fixes.
² Scenario 3 re-run after the fingerprint-template fix; the residual
majors it found (stuck `}` navigation, badge decay, popup-paused
polling, noisy per-file events) were fixed and re-verified the same
day.
³ Round-3 re-runs graded the round-3 build (reports in
`eval/reports/2026-07-05-round3-recheck/`). Change awareness hit its
4 target; TUI 2→3.5 confirms the erasure fix. The three majors that
drove the round-3 scores (op-log pollution/undo trap, vanishing
comment annotations, CLI backtrace noise) were fixed the same day —
the scores predate those fixes (W7 below).
⁴ Round-4 re-runs (all three scenarios, reports in
`eval/reports/2026-07-05-round4-recheck/`) graded the build with W7
open items landed. Watch hit 5/4/4 — freshness and awareness targets
met, op-log clean, comments surviving refreshes confirmed. Zen
uncurated dropped 3→2: per-change chapters landed but derived facts
came from the wrong diff (worse than saying nothing). CLI
discoverability 4→3 on empty help descriptions; handoff 3 on
markdown/JSON disagreement and a `set-state` panic blocker. The
blocker and all seven round-4 majors were fixed the same day (W8
below) — the scores predate those fixes.
⁵ Round-5 re-runs (all three scenarios, reports in
`eval/reports/2026-07-05-round5-recheck/`) graded the post-W8 build.
Six of ten dimensions now at/above target. All round-4 fixes
verified working (set-state, clean errors, round-trips, base-scoped
stack, change-scoped facts, `chunks lines` agreeing with the
validator, op-log clean, comment anchoring hints). Zen curated dipped
4→3.5 on brief truncation; the six round-5 majors were fixed the
same day (W9 below) — the scores predate those fixes.
⁶ Round-6 re-runs (all three scenarios, reports in
`eval/reports/2026-07-05-round6-recheck/`) graded the round-6 build
(W8/W9 opens landed). Eight of eleven dimensions at/above target;
watch swept its board (5 / 4.5 / 4.5 — pane-worthiness target met,
op attribution, catch-up preview, badge consistency, zero idle
snapshots all confirmed) and human output moved 3→3.5 on the new
`--format text` surface. Every round-6-wave fix was independently
verified (footer identity chip, comment state cycling "worked first
try", glance dedup, brief-first cards, deterministic ordering,
"exemplary" chunk validation errors, live-bridge notice). New majors
found: the silent default-base session trap and comments/tasks
scoping asymmetry (scenario 1), and live sessions silently dropping
curated chunks on working-copy snapshot reload plus contradictory
partial-curation rendering (scenario 2) — the chunk-drop bug pinned
zen curated at 3.5. Tracked as W10 below.

Baseline reports: `eval/reports/2026-07-05-baseline/`. Post-W1 recheck of
scenario 1: `eval/reports/2026-07-05-w1-recheck/`. Post-W2/W3 recheck of
scenario 2: `eval/reports/2026-07-05-w2w3-recheck/`. Round-2 reports
(scenario 2 full run + fix verification, scenario 3 broken-build run +
re-run): `eval/reports/2026-07-05-w2w3b-w4-recheck/`. Round-3 re-runs of
scenarios 2 and 3: `eval/reports/2026-07-05-round3-recheck/`. Round-4
re-runs of all three scenarios:
`eval/reports/2026-07-05-round4-recheck/`. Round-5 re-runs of all
three scenarios: `eval/reports/2026-07-05-round5-recheck/`. Round-6
re-runs of all three scenarios:
`eval/reports/2026-07-05-round6-recheck/`.

## Baseline findings (2026-07-05)

What the three evaluations established:

- **The core review flow holds up** (4/5): navigation, comments, viewed
  state, stack stepping, and revset targeting all worked for a first-time
  agent user learning from the `?` overlay.
- **Handoff is the weakest link (2/5).** `export --profile agent` omits
  tasks and walkthroughs entirely; the Markdown profile drops kind/action
  tags; comment excerpts/anchors documented in the schema were missing from
  the JSON output; there is no clipboard integration and no one-shot
  "give the agent everything actionable" command. `gander … | head` printed a
  color-eyre backtrace on EPIPE.
- **Zen's concept is validated; its fallback is empty.** Uncurated zen is "a
  pleasant full-screen file slideshow, not a review briefing" — its
  `src/retry.rs` stop displayed the buggy file without pointing at the bug.
  The *curated* tour scored 4/5 and landed the reviewer directly on the
  planted bug with a why-this-matters card. The gap is (a) no derived
  content without an agent, (b) curation via hand-written JSON-RPC is
  brittle (per-change line spaces, full-replace semantics, zero
  validation), (c) invalid chunks render misleadingly instead of erroring.
- **Watch mechanics are solid; legibility is missing.** The TUI followed a
  symbolic `@` through `jj describe`/`new`/`undo`/abandon, preserved
  comments and viewed state throughout, and stayed calm under rapid edits —
  but every event collapses into `repository changed — refreshed main..@`
  with no indication of what changed, where, or that `@` moved.

## Action items

Kept in sync as work lands. Workstream order: W0 → W1 → W2 → W3 → W4 (W5
parked).

### W0 — Papercuts

- [x] Handle EPIPE/broken-pipe on stdout as a normal quiet exit (no
      backtrace) for all CLI output paths. *(2026-07-05)*
- [x] Fix agent-profile JSON export to include documented comment
      `excerpt`/anchor context. Root cause: CLI-added comments were stored
      with `anchor: None`; anchor derivation now lives in core
      (`anchor::comment_anchor_for_file_lines`) and is shared with the TUI
      row anchors. *(2026-07-05)*
- [x] Unify file-anchor flags across command groups: every file-anchor
      flag accepts both `--path` and `--file`; `--path` is canonical.
      *(2026-07-05)*

### W1 — Seamless agent handoff

- [x] Make the agent artifact complete: session metadata, comments with
      kind/action/state + excerpts + anchors, tasks (with linked comment
      ids), walkthrough steps, base/rev/repo, raw hunks as trailing
      reference. Schema v5; v4 imports still accepted. *(2026-07-05)*
- [x] Agent-profile Markdown reads like a prompt: preamble, action items
      first (file:line, kind/action, excerpt), reference material after.
      *(2026-07-05)*
- [x] `gander handoff` command: `--format markdown|json`, `--only-open`,
      `--output <path>`, `--copy` (pbcopy/wl-copy/xclip, OSC52 to
      /dev/tty fallback). *(2026-07-05)*
- [x] TUI: `ctrl-y` yanks the agent handoff to the clipboard without
      quitting (`yank-handoff` keybinding, footer notice with action-item
      count). *(2026-07-05)*

Follow-ups from the post-W1 recheck (scored 4/5; polish, not blockers):

- [x] `handoff --format json` should be a structured action artifact
      (action items as first-class objects), not a rendered-content
      dump. Landed; round 4 graded it "close to the rubric's 5".
      *(2026-07-05)*
- [x] Markdown handoff: include task bodies and render task↔comment
      relationships explicitly. *(2026-07-05)*
- [x] Human-readable diff inspection in the CLI: `hunks show
      --format diff` (JSON stays the default). *(2026-07-05)*
- [x] `--help` examples clarifying `handoff` vs `export --profile
      agent`; the two markdown outputs are now genuinely distinct
      (W8). *(2026-07-05)*

### W2 — Zen presents useful information without an agent

- [x] Every fallback stop answers "why this matters" with derived facts:
      file role, largest-hunk anchoring (no more top-of-file cards),
      churn, and tree-sitter symbols touched. *(2026-07-05)*
- [x] Chapter cards narrate the stack with derived lines: files by role,
      churn, tests-touched, top changed symbols; uncurated tours say so
      and point at the summon flow. *(2026-07-05)*
- [x] Glance board entries carry a rationale (role / "exports only") so
      bulk mark-viewed is a confident act; manifests/tests/docs route to
      glance instead of full-screen stops. *(2026-07-05)*
- [x] Multi-part spotlight chunks render as "part 1/2 / part 2/2" with
      cross-references, not disconnected stops; glance board groups
      multi-part conceptual chunks instead of duplicating bullets.
      *(2026-07-05)*
- [x] Chapter-level dependency narration between stack changes (which
      change builds on which): derived `builds on ch.N` lines from
      file/symbol overlap with earlier chapters. *(2026-07-05)*
- [x] Recheck gap: fallback rationale is still mostly metadata —
      factual derived risk labels landed ("public API change",
      "touches error handling") on fallback stops and chapter lines.
      *(2026-07-05)*
- [x] Recheck gap: zen card headers wrap poorly for long titles/paths.
      Width-aware truncation (tail for titles, middle for paths) on
      stop/chapter cards, reading panel, and backdrop header.
      *(2026-07-05)*
- [x] Recheck gap (TUI-wide): help overlay needs a task-oriented "first
      review loop" section. Landed, plus an in-zen keys cluster.
      *(2026-07-05)*

### W3 — Curation protocol ergonomics

- [x] Validate agent chunks at ingestion: ACP/MCP `set_chunks` rejects
      requests with parts outside the anchored change's diff,
      out-of-range lines, or unknown change ids (full reject, per-part
      reasons); overlay files loaded from disk get invalid parts excluded
      with a TUI footer notice. *(2026-07-05)*
- [x] Incremental chunk updates (add/update/remove) alongside full
      replace: ACP `review/update_chunks` (upsert by id) and
      `review/remove_chunks` (strict), mirrored as MCP tools.
      *(2026-07-05)*
- [x] Friendlier authoring path than hand-written JSON-RPC: `gander
      chunks list/set/update/remove/clear` over plain JSON spec files
      (or stdin), validated against the session diff with per-part
      reasons, writing the same overlay a live TUI watches.
      *(2026-07-05)*
- [x] Live-bridge transparency: `gander acp`/`mcp` warn on stderr when
      bridging to a live TUI whose target differs from the invoked flags;
      `review/summary` gains additive `active_target`/`live_session`
      fields. *(2026-07-05)*
- [x] Badge invalid chunks in the TUI chunk list popup (`S`) — excluded
      parts stay listed with an `[invalid]` badge and reason; overlay
      re-applies after refresh now validate like the initial load.
      *(2026-07-05)*
- [ ] MCP `comment_add` still stores `anchor: None` (it never loads full
      diff context), so MCP-created comments lack excerpts in agent
      exports. Derive anchors once MCP loads diff context (M14 parity;
      `TODO(M14)` marker in `src/mcp.rs`).
- [x] Round-2 recheck gap: change briefs and comment drafts still
      require raw JSON-RPC — landed `gander briefs list/set/clear` and
      `gander drafts list/add/remove` over the same validated spec-file
      pattern as `gander chunks`. *(2026-07-05)*

### W4 — Watch / ambient pane legibility

- [x] Per-file/hunk "changed since last look" badges (`±` files,
      marked hunk headers), plus a distinct "viewed but changed since
      viewed" state (`~`) instead of silently unmarking viewed.
      *(2026-07-05)*
- [x] Activity feed of refresh events (`ctrl-a` popup; `@ moved …`,
      `change … entered range/updated/left range`, collapsed repeats),
      with the footer notice summarizing each refresh batch.
      *(2026-07-05)*
- [x] Explicit follow-mode indication for symbolic revsets
      (`following @` in the footer). *(2026-07-05)*
- [x] Navigation keys for next/previous changed-since-last-look hunk
      (`}`/`{`, cross-file). *(2026-07-05)*
- [x] Round-2 blocker: the fingerprint poll template used `if(self, …)`,
      which real jj rejects — live refresh never fired (mock-backed
      tests could not catch it). Fixed with `current_working_copy`;
      persistent poll failures now surface a footer error and a
      recovery notice instead of freezing silently. *(2026-07-05)*
- [x] Round-2 gap: popups block polling by design, so leaving the
      activity feed (`ctrl-a`) open froze the very updates it shows.
      Read-only popups (activity, help) now refresh live; editing
      popups show a "refresh paused" hint. *(2026-07-05)*
- [x] Round-2 gap: `I` catch-up marks never-viewed files as viewed when
      unchanged since the op — now a distinct caught-up state (`◌`
      badge, fingerprint-persisted, promoted to viewed on explicit
      mark, decays like viewed); notice reports caught-up vs
      already-viewed vs needs-re-review counts. *(2026-07-05)*
- [x] Round-3 fix: `}`/`{` changed-hunk navigation got stuck on the
      first hunk; now cycles within and across files and wraps.
      *(2026-07-05)*
- [x] Round-3 fix: freshness badges decayed on mere file-tree
      selection; "looked" now means focusing the file's diff or marking
      it viewed. *(2026-07-05)*
- [x] Round-3 fix: activity events re-listed every un-reviewed file
      with whole-file churn on each refresh; events now name only the
      files that changed in that refresh, with honest churn deltas and
      local timestamps; help documents `ctrl-a`, `}`/`{`, and the badge
      legend. *(2026-07-05)*

### W6 — Round-2 trust fixes (from the second eval round)

- [x] **Data-loss blocker:** retargeting to a narrow/empty diff (stack
      stepping onto an empty change) rewrote state.json with only what
      that target could see — wiping all viewed-state and comments.
      Saves now preserve entries outside the current diff. *(2026-07-05)*
- [x] Zen chapter cards repeated session-global stats on every chapter,
      contradicting per-change summaries; derived facts are now scoped
      to the chapter's own files or omitted. *(2026-07-05)*
- [x] `t` returns to the launch target (honoring `-b`/`-r`) instead of
      hard-coded `trunk()..@`; chooser ranks exact bookmark matches
      first; `I` op-compare refreshes the diff before comparing and
      reports accurate counts. *(2026-07-05)*
- [x] Round-3 (fix-verification residuals): viewed marks are now
      persisted per content fingerprint (`viewed_fingerprints` set per
      path, legacy state honored), so visiting a target where the same
      path diffs differently no longer clobbers the mark, and the `~`
      viewed-stale state survives refreshes durably. *(2026-07-05)*
- [x] Round-3: chooser selection cursor snaps to the top-ranked match
      when the filter changes (Enter now targets what `›` points at),
      and scattered fuzzy matches are dropped when an exact/prefix
      match exists. Zen derived chapter lines carry a `stops:` scope
      label so the two churn figures on a card read as intended.
      *(2026-07-05)*

### W7 — Round-3 findings (from the third eval round)

Majors, fixed same day:

- [x] CLI user errors (invalid chunk parts, malformed specs, unknown
      ids) rendered as color-eyre reports with backtrace frames and
      `Location:` noise; now plain `error: …` on stderr, exit 1, with
      backtraces reserved for genuinely unexpected errors.
      *(2026-07-05)*
- [x] Watch polling wrote `snapshot working copy` ops into the user's
      jj op log on every dirty poll and made `jj undo` a trap
      (evaluator's undo reverted their own disk edit). Read-only jj
      invocations now pass `--ignore-working-copy`; freshness comes
      from one deliberate `jj util snapshot` per poll tick.
      *(2026-07-05)*
- [x] Inline comment annotations vanished from the diff pane after any
      watch refresh (comment persisted; rendering anchor pointed at
      stale diff rows). Anchors are re-derived on every diff reload.
      *(2026-07-05)*

Open, landed in the round-4 build (graded by the round-4 re-runs):

- [x] Uncurated zen ignores the jj stack — per-change chapters with
      commit-message intent landed and were verified by the round-4
      evaluator; the derived-facts-from-wrong-diff residual it found
      is fixed under W8. *(2026-07-05)*
- [x] Uncurated stop "why this matters" contains no why — intent
      sections now derive from roles/symbols/API evidence and the
      mechanics line moved to a footer; W8 fixed the remaining
      wrong-diff sourcing. *(2026-07-05)*
- [x] Briefs for changes with only glance chunks are silently dropped
      — authoring-time warnings over ACP/MCP/CLI ("no spotlight chunk
      yet"), verified firing 3× in round 4. *(2026-07-05)*
- [x] Chunk line-space authoring is manual — `gander chunks lines`
      landed; its coordinate-mixing bug (round-4 finding) is fixed
      under W8 with a helper shared with the validator.
      *(2026-07-05)*
- [x] Comment editor is anchorless / save undiscoverable — anchored
      title, `ctrl-s save · esc cancel` hints, editing footer;
      verified in round 4. *(2026-07-05)*
- [x] Viewed→changed transitions are quiet — "was viewed, needs
      re-review" in footer notice and activity feed; verified in
      round 4. *(2026-07-05)*
- [x] File tree re-sorts on state transitions — stable path ordering;
      verified in round 4 ("rows never re-sorted"). *(2026-07-05)*
- [x] Papercut batch: tree-sitter debug line removed (root cause:
      fragments parsed standalone, noted for later), `j`/`k`/arrows
      everywhere, `G` clamps, base-commit stepping excluded (fully
      fixed under W8), `1 comment` pluralization, glance truncation /
      count mismatch / card clipping / duplicate paths. *(2026-07-05)*

Still open:

- [ ] jj `undo`-of-snapshot residual footgun (upstream jj behavior):
      documented in code and docs; revisit if jj grows a non-mutating
      working-copy fingerprint.

### W8 — Round-4 findings (from the fourth eval round)

Blocker and majors, fixed same day:

- [x] **Blocker:** `gander comments set-state` panicked on every
      invocation (clap arg-id collision with the global `--state`
      state-file flag, which is now `--state-file`). Regression net:
      `Command::debug_assert` test + parse tests for every mutating
      subcommand. *(2026-07-05)*
- [x] Mutation-path user errors (unknown task/comment/step ids)
      dumped color-eyre backtrace walls; now clean one-line errors.
      *(2026-07-05)*
- [x] Markdown handoff contradicted the JSON artifact (question/note
      comments demoted below the walkthrough; header counted them
      anyway); selection now shared with JSON. Handoff markdown is
      now a distinct implementation prompt (trimmed reference hunks)
      vs the complete export artifact, and the help says so.
      *(2026-07-05)*
- [x] `followup`/`follow-up` round-trip trap: enums serialize as the
      CLI accepts (`follow-up`), legacy accepted on input.
      *(2026-07-05)*
- [x] Anchoring mistakes were irreversible: `comments edit` (anchor
      re-derivation) and `comments delete` added. *(2026-07-05)*
- [x] Zen/stack/ACP counted the session base as a reviewable change
      and stepping denominators jumped (`3/4`→`3/5`): stack list now
      strictly `base..rev` from the launch target, consistent across
      TUI stepping, zen chapters, and ACP `stack_changes`.
      *(2026-07-05)*
- [x] Uncurated zen derived facts came from the session-global diff
      while excerpts were change-scoped (churn/API claims about code
      introduced chapters later; "no tests touched" on a change
      adding tests): facts now derive from each change's own cached
      diff, omitted when unavailable; purely-added symbols read "new
      public API", and test-subject changes spotlight their tests.
      *(2026-07-05)*
- [x] `gander chunks lines` reported ranges the validator rejected
      (old/new side mixing): listing and validator now share one
      line-space helper; round-trip property test. *(2026-07-05)*
- [x] The `I` op picker ignored standard movement keys, had no
      hints, and mass-applied against the newest op with no preview:
      standard keys, hint line, and a live `will mark N caught up …`
      preview landed. *(2026-07-05)*

Minors fixed same day: chunks/briefs/drafts warn when a live TUI
session targets a different range; describe-only updates say
"description updated"; reverts to previously seen content are
announced and clear the `±` badge; empty help descriptions filled
across reviews/comments/tasks/walkthrough with documented `--line`
semantics; `session: null` no longer emitted silently.

Open:

- [x] Handoff action items carry no priority/ordering signal:
      deterministic ordering landed (action priority fix > test >
      follow-up, then path, then line), identical in markdown and
      JSON via one shared sort, documented in `handoff --help`.
      *(2026-07-05, round-6 build)*
- [x] Human-authored comments render with a `[draft]` label in the
      TUI with no visible way to finalize: the diff pane now cycles
      the selected comment's state (draft → todo → resolved) with
      `s`, shows a `s state · e edit · x delete` hint when a comment
      is selected, and the binding is in the help screen.
      *(2026-07-05, round-6 build)*
- [x] Curated zen: repetition between chapter cards and stops —
      curated cards now render the brief first and collapse derived
      facts to a dimmed summary line (expandable with `d`).
      *(2026-07-05, round-6 build)*
- [x] Activity feed: jj operation names label refresh events
      (`· op: <description>`), fetched only when a refresh actually
      fires — no idle-poll jj calls. *(2026-07-05, round-6 build)*
- [x] `chunks set` validation errors now echo the valid ranges for
      the failing path and point at `gander chunks lines`.
      *(2026-07-05, round-6 build)*
- [ ] Key overloading noted by evaluators (same key different
      meanings across panes); audit once bindings settle.
- [ ] tree-sitter parses diff fragments standalone so `errors=true`
      is common; revisit if syntax quality complaints surface.

### W9 — Round-5 findings (from the fifth eval round)

Majors, fixed same day:

- [x] Zen glance "mark all viewed" didn't stick: marks were applied
      to the per-change retargeted session and dropped when the tour
      restored the home target; marking now re-applies against the
      restored session through the standard viewed path.
      *(2026-07-05)*
- [x] Curated brief text silently truncated on a fixed 72-col
      chapter card: cards now size to the terminal (≤ ~100 cols) and
      `d` expands the brief, with an explicit affordance when
      clipped. *(2026-07-05)*
- [x] The op-picker preview and hint lines were invisible at 50-row
      pane height (the geometry watch mode targets): fixed-line
      accounting corrected, ops render as exactly one aligned row,
      list shrinks before hints clip; render tests at 50 and 30
      rows. *(2026-07-05)*
- [x] Default markdown handoff ≠ default JSON handoff (JSON included
      done tasks): JSON now defaults to the markdown selection —
      open tasks + unresolved comments. *(2026-07-05)*
- [x] Task→comment links stored unvalidated raw strings (dangling
      ids, prefixes): links resolve to canonical full ids; unknown
      and ambiguous references are clean errors (CLI + MCP).
      *(2026-07-05)*
- [x] Comments with an out-of-diff `--line` persisted silently
      without an anchor: `comments add`/`edit` now warn on stderr
      and point at `comments edit`. *(2026-07-05)*

Minors/papercuts fixed same day: symbol-scoped "new public API"
labels (new fns in mixed hunks no longer read "signature changed"),
empty undescribed `@` removed from stack positions, stale `±` clears
when a never-viewed file reverts (and on catch-up/view), footer and
activity-feed lines ellipsize with a `ctrl-a for detail` cue,
`chunks lines` coalesced-hunk headers are honest.

Open:

- [x] Human-readable output for list commands: `--format text` landed
      on `files`/`hunks`/`comments`/`tasks`/`reviews list` (compact
      aligned rows; JSON stays the default for agents), and
      `comments add`/`edit` grew a 3-line text echo instead of the
      ~70-line anchor dump. *(2026-07-05, round-6 build)*
- [x] `hunks list` accepts a positional file, `--file` has real help
      text, and every subcommand's `--help` now separates domain
      flags from the global plumbing under a
      "Target & state (global)" heading; remaining blank help
      strings filled (`hunks show <ID>`, `export`/`handoff`
      `--output`/`--format`). *(2026-07-05, round-6 build)*
- [x] Comment-backed tasks emit `title: null`: titles are now
      synthesized from the comment body's first line (ellipsized,
      with a `comment <id>` fallback) — `title` is always a string
      in `tasks list`, handoff JSON, and exports.
      *(2026-07-05, round-6 build)*
- [x] Persistent current-`@` identity chip in the TUI footer
      (`@ <short-id> <description first line>`, width-aware,
      refreshed only when the fingerprint changes).
      *(2026-07-05, round-6 build)*

Also landed in the round-6 build (scenario-1/2/3 leftovers): the
agent-profile export H1 is now distinct
(`# Review session export (agent profile)`) and the TUI `ctrl-y`
yank genuinely copies the handoff prompt (it copied the agent
export before); `tasks edit`/`tasks delete` with ambiguity-guarded
prefix resolution (complete/reopen too); zen "top changed symbols"
and dependency evidence only count symbols overlapping actually
added lines (no more context-line noise); uncurated glance rows
dedupe across changes with `ch.N` attribution and auto-appended
uncovered leftovers are labeled `· uncovered`; the repeated
`stops:` prefixes became one `derived facts` section; the
viewed-tally/badge invariant holds (a counted-viewed file can
never render bare `±`); the activity feed jumps to the named file
on enter; `gander acp` announces `live-bridge` vs `snapshot` on
stderr and in the `initialize` response.

### W10 — Round-6 findings (from the sixth eval round)

Majors, fixed same day (round-7 build):

- [x] Silent default-base session trap: read commands (`handoff`,
      `export`, `tasks list`, `walkthrough show/export`,
      `comments list`) no longer create phantom sessions and warn on
      stderr when the target matches no open session while one
      exists for another target; mutations note when they implicitly
      create a new session alongside an existing one. *(2026-07-05)*
- [x] Comments and tasks disagree on scoping: the shared mismatch
      warning now fires on `comments list` too, making the
      asymmetry visible instead of a trap (deep rescoping of
      comments deliberately deferred — comments stay
      workspace-scoped by design). *(2026-07-05)*
- [x] Live sessions silently drop curated chunks on snapshot reload:
      root cause was `reapply_agent_overlay` revalidating against an
      empty change-diff context after every refresh/zen retarget —
      all five call sites now validate against real per-change
      diffs, and genuinely-invalid chunks warn
      (`N curated chunk part(s) no longer match the diff …`)
      instead of vanishing. Regression tests cover survive +
      warn-on-invalid. *(2026-07-05)*
- [x] Contradictory partial-curation rendering: chapter cards show
      an honest `partially curated — agent briefs below; stops are
      derived from the diff (no spotlight chunks yet)` banner when
      briefs exist without chunks. *(2026-07-05)*

Blocker found during wave-7 smoke (orchestrator, not evaluators),
fixed same day:

- [x] Lost-update data loss: a comment added via CLI while a TUI
      held the session was clobbered on the TUI's next save
      (including quit). The TUI now watches the state-file mtime,
      merges external writes by id (with delete-tombstones and
      `updated_at` conflict resolution) on poll and before every
      save, and announces `review state updated externally — 1
      comment added`. CLI-added comments appear in the live TUI
      within a poll and survive quit. *(2026-07-05)*

Minors, fixed same day (round-7 build):

- [x] `hunks show --format text` (alias of `diff`); text echoes on
      `tasks add/complete/reopen/edit/delete` and
      `comments delete/resolve/set-state`. *(2026-07-05)*
- [x] Handoff JSON action items carry `end_line`; export JSON
      comments carry `linked_task_ids`. *(2026-07-05)*
- [x] Trait impls labeled `impl Default for Priority` (duplicate
      `impl Priority` gone). *(2026-07-05)*
- [x] Canned review questions appear once per tour. *(2026-07-05)*
- [x] Comments render on zen stops covering their lines
      (`comment [todo] a3c7b887: …`); a stop-card height under-count
      that clipped appended lines was found and fixed during
      integration. *(2026-07-05)*
- [x] Activity feed: aggregate row is footer-only (no triplication),
      op attribution once per batch, 128-char op ids truncated,
      `j/k`/`n/e`/arrows all move, Enter on non-file rows keeps the
      popup open with a hint, selected event's full message wraps in
      a detail area. *(2026-07-05)*
- [x] Badge legend matches actual behavior (`~ viewed, changed
      since` vs `± unviewed, changed since`). *(2026-07-05)*
- [x] Spec files warn on unknown fields (found via a `role:` →
      `importance` typo during smoke). *(2026-07-05)*

Papercuts fixed same day: `… d expands` labels on truncated chapter
cards; multi-part explanations render once (parts 2+ point back); zen
chrome shows the home target during tours; `draft_comment` line space
documented in docs/acp.md; `summary` status-column alignment (the
same width-ignoring Display bug as the round-6 `files list` fix).

Open:

- [ ] Dependency claims are file-overlap only: chapter cards miss
      call-graph dependencies (worker → `enqueue_with_priority`) and
      test chapters that share no files get no builds-on line.
- [ ] No goto-line in the diff pane for comment targeting.
- [ ] Deep comment scoping (per-session comments) deferred pending a
      deliberate data-model decision.
- [ ] Handoff prompt noise: linked task+comment pairs render as two
      near-identical action items (merge the pair), and
      `praise`-kind comments count as action items in an
      implementation prompt.
- [ ] Markdown excerpt readability: old-side `-` lines interleave
      between new-side line numbers in action-item excerpts —
      technically correct per side, reads out-of-order; group
      removed lines before added lines.
- [ ] Export JSON optional-field convention is inconsistent:
      `end_line` omitted-when-unset on comments vs explicit `null`
      elsewhere; schema consumers must handle both.
- [ ] Walkthrough verb convention: `add-step`/`remove-step`/
      `move-step` vs `add`/`edit`/`delete` everywhere else; no
      `walkthrough edit-step`; walkthrough mutations have no
      `--format text` echo.
- [ ] `tasks reopen` on an already-open task exits 0 and bumps
      `updated_at` with no "already open" notice.
- [ ] Ambient identity weight: revision identity lives only in the
      small footer line; evaluators want it in the diff/files pane
      titles (with a brief highlight when `@` moves) for
      across-the-room glances.
- [ ] Hunk-level freshness: after catch-up or a viewed-file change,
      mark which hunks are new since last look so re-reviewing a
      large file doesn't mean rereading all of it (`}`/`{` navigate
      changed hunks, but nothing marks them).
- [ ] Per-op digest rows in the activity feed (one expandable row
      per jj op with per-file children) so catching up after long
      agent runs scales with ops, not rows.

### W5 — Web (parked)

Static export stays as-is until the session/tour model stabilizes; a future
export should inherit W2's tour content. Revisit after W4.

## History

- **2026-07-05** — Baseline established (reports in
  `eval/reports/2026-07-05-baseline/`). Fixture stack contained one planted
  bug and one accidental bug; both evaluators found both.
- **2026-07-05** — W0 landed: quiet EPIPE exits, CLI comment anchors +
  agent-export excerpts, `--path`/`--file` flag parity. Harness
  institutionalized under `eval/`.
- **2026-07-05** — W1 landed: artifact schema v5 (tasks, walkthroughs,
  session meta), prompt-style agent Markdown, `gander handoff`
  (`--only-open`/`--output`/`--copy`), TUI `ctrl-y` yank.
- **2026-07-05** — Scenario 1 re-run post-W1: handoff readiness 2→4
  (target met), agent output quality 3→5, discoverability 3→4, no
  regressions. Remaining polish captured as W1 follow-ups.
- **2026-07-05** — W2 fallback zen content + W3 chunk validation and
  live-bridge transparency landed.
- **2026-07-05** — Scenario 2 re-run post-W2/W3: zen uncurated 2→3
  (target 3.5), curation ergonomics 2→3 (target 4), curated 4, TUI 4.
  Remaining friction is authoring ergonomics (spec-file/incremental
  updates) and glance grouping — both still open above.
- **2026-07-05** — W3b curation authoring (incremental
  update/remove_chunks over ACP/MCP, `gander chunks` spec-file CLI,
  invalid-chunk badges in `S`), W2 polish (multi-part grouping +
  sibling cross-references, factual risk labels, width-aware card
  truncation, "first review loop" help section), and W4 watch
  legibility (freshness badges, viewed-stale state, activity feed with
  `@ moved`/entered/left events, follow-mode indicator, `}`/`{`
  changed-hunk navigation) landed.
- **2026-07-05** — Round-2 re-runs (reports in
  `eval/reports/2026-07-05-w2w3b-w4-recheck/`) surfaced two blockers the
  unit suite could not see: state erasure on narrow retargets (data
  loss) and a jj-rejected fingerprint template that silently disabled
  live refresh entirely (scenario 3 scored 1/1/1.5 against that build).
  Scenario 2 scored TUI 2 (erasure-driven), zen uncurated 3, curated
  3.5, curation ergonomics 3.5 — evaluator called the chunks CLI +
  incremental updates "genuinely good". All four blockers/majors fixed
  same day (W6 above).
- **2026-07-05** — Scenario 3 re-run against the fixed build: watch
  freshness 1→4, change awareness 1→3, pane-worthiness 1.5→3.5. A
  targeted scenario-2 fix-verification confirmed the erasure fix,
  chapter scoping, and launch-target return, and surfaced residuals
  (viewed marks clobbered across targets, chooser cursor stale index,
  stuck `}` navigation, badge decay, popup-frozen polling, noisy
  per-file events) — all fixed and manually re-verified the same day
  (round-3 items above). Next re-run should grade the round-3 build.
- **2026-07-05** — Round-3 work landed in one stack: `gander briefs`/
  `gander drafts` CLI parity (last raw-JSON-RPC holdout), zen chapter
  dependency narration + evidence-backed stop risk lines with review
  questions, and the distinct caught-up state for `I`.
- **2026-07-05** — Round-3 re-runs of scenarios 2 and 3 against the
  round-3 build (reports in `eval/reports/2026-07-05-round3-recheck/`):
  TUI 2→3.5, zen curated 3.5→4, curation ergonomics 3.5 (evaluator
  found a real third bug via the curated tour), watch awareness 3→4
  (target met), freshness 4, pane-worthiness 3.5→3 — dragged down by
  two new majors: vanishing comment annotations after refresh and
  op-log pollution making `jj undo` a trap. Both plus the CLI
  backtrace major were fixed the same day (W7); the remaining opens
  are tracked in W7.
- **2026-07-05** — Round-4 build landed the W7 opens plus the W1
  follow-ups in one stack: per-change uncurated zen chapters with
  honest intent sections, `gander chunks lines`, brief-drop warnings,
  anchored comment editor, viewed-changed callouts, stable file-tree
  order, the TUI papercut batch, structured handoff JSON, markdown
  task bodies/links, and `hunks show --format diff`.
- **2026-07-05** — Round-4 re-runs of ALL THREE scenarios (reports in
  `eval/reports/2026-07-05-round4-recheck/`): watch pane vindicated —
  freshness 5, awareness 4, pane-worthiness 4, op-log confirmed
  clean, comments survive refreshes, no re-sorting. TUI 3.5→4;
  curated zen 4 with the CLI curation path "materially easier" and
  verified end-to-end. But: a CLI blocker (`comments set-state`
  panic), zen uncurated 3→2 (facts derived from the wrong diff),
  handoff markdown/JSON disagreement, and `chunks lines`
  contradicting its own validator. The blocker plus all seven majors
  were fixed the same day (W8); remaining minors tracked open under
  W8. Next re-run grades the post-W8 build — scenarios 1 and 2 are
  the ones with headroom (discoverability help gaps now filled,
  set-state fixed, zen facts change-scoped).
- **2026-07-05** — Round-5 re-runs of all three scenarios against the
  post-W8 build (reports in
  `eval/reports/2026-07-05-round5-recheck/`): six of ten dimensions
  at/above target — discoverability 3→4 ✓, agent output 4 ✓, handoff
  3→4 ✓, curation ergonomics 3.5→4 ✓, watch 5/4 held ✓✓. Every W8
  fix verified working by fresh evaluators. New majors: zen glance
  marks not persisting, curated brief truncation (curated 4→3.5),
  op-picker hints invisible at 50 rows, handoff JSON/markdown default
  mismatch, dangling task→comment links, silent anchorless comments —
  all six fixed the same day (W9), plus the round's minors.
  Remaining below target: TUI 4/4.5, zen uncurated 3/3.5, curated
  3.5/4.5, pane-worthiness 4/4.5 — zen curated polish and human
  output formats are the highest-leverage opens.
- **2026-07-05** — Round-6 build landed the W8/W9 opens in one
  three-track stack (CLI / zen / TUI chrome): human-readable
  `--format text` across the list commands with compact mutation
  echoes, `tasks edit`/`delete` with ambiguity-guarded id
  resolution, synthesized titles for comment-backed tasks,
  help-heading separation of global flags plus the last blank help
  strings and a positional file for `hunks list`, deterministic
  handoff action-item ordering shared by markdown and JSON, a
  distinct agent-export H1 (and `ctrl-y` now yanks the actual
  handoff), chunk errors that echo valid ranges, ACP
  bridge/snapshot mode signaling, added-line-scoped zen symbols,
  deduped/attributed glance rows with labeled uncovered leftovers,
  brief-first curated chapter cards with collapsed derived facts, a
  persistent `@`-identity footer chip, the viewed/`±` badge
  invariant, inline comment-state cycling, op-named activity-feed
  events, and feed→file jump navigation. 517 tests green. Next
  re-run grades this build — headroom targets are human output
  (3→4), zen uncurated (3→3.5), curated (3.5→4.5), TUI (4→4.5), and
  pane-worthiness (4→4.5).
- **2026-07-05** — Round-6 re-runs of all three scenarios against the
  round-6 build (reports in
  `eval/reports/2026-07-05-round6-recheck/`): eight of eleven
  dimensions at/above target. Watch swept — freshness 5, awareness
  4→4.5, pane-worthiness 4→4.5 (target met): the evaluator confirmed
  op attribution, the identity chip, catch-up preview consistency,
  revert detection, and a clean op log, calling watch "close to
  first-class". Human output 3→3.5 on `--format text`; every
  round-6 fix verified by fresh evaluators (comment state cycling,
  glance dedup, brief-first cards, deterministic handoff ordering,
  "exemplary" chunk errors, live-bridge notice). Four new majors
  (W10): the silent default-base session trap plus comments/tasks
  scoping asymmetry, and live sessions dropping curated chunks on
  snapshot reload plus contradictory partial-curation rendering —
  the latter pinned zen curated at 3.5 despite the rendering
  upgrades landing as designed. Remaining below target: TUI 4/4.5,
  zen uncurated 3/3.5, curated 3.5/4.5, humans 3.5/4.
- **2026-07-05** — Round-7 build landed all four W10 majors and the
  round's minors in a four-track stack: session-mismatch warnings
  with non-creating read lookups (the default-base trap), overlay
  revalidation against real per-change diffs on every refresh/zen
  retarget (the chunk-drop trust-breaker), the honest
  partially-curated banner, comments rendered on zen stops (plus a
  stop-card height under-count found during integration), trait-impl
  labels, per-tour question dedup, activity-feed digest rows with
  humanized op ids and a working detail area, reconciled badge
  legend, `end_line`/`linked_task_ids` artifact parity, the
  remaining text echoes, and spec-file unknown-field warnings.
  Orchestrator smoke also caught a blocker the evaluators missed:
  CLI comments added while a TUI held the session were clobbered on
  the TUI's next save — fixed with mtime-watched state merging
  (tombstones + `updated_at` resolution), giving live CLI→TUI
  comment pickup as a bonus. 534 tests green. Next re-run grades
  this build — headroom: humans 3.5→4, zen uncurated 3→3.5, curated
  3.5→4.5, TUI 4→4.5.
