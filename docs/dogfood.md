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

| Dimension                     | 2026-07-05 baseline | 2026-07-05 post-W1 | 2026-07-05 post-W2/W3 | 2026-07-05 round 2 | Target |
| ----------------------------- | ------------------- | ------------------ | --------------------- | ------------------ | ------ |
| CLI discoverability           | 3                   | 4                  | —                     | —                  | 4      |
| TUI review ergonomics         | 4                   | —                  | 4                     | **2**¹             | 4.5    |
| CLI output quality for agents | 3                   | 5                  | —                     | —                  | 4      |
| Handoff readiness             | **2**               | **4**              | —                     | —                  | 4      |
| Zen usefulness, uncurated     | **2**               | —                  | 3                     | 3                  | 3.5    |
| Zen usefulness, curated       | 4                   | —                  | 4                     | 3.5¹               | 4.5    |
| Curation protocol ergonomics  | **2**               | —                  | 3                     | 3.5                | 4      |
| Watch: freshness / follows @  | 4                   | —                  | —                     | 4²                 | 4.5    |
| Watch: change awareness       | **2**               | —                  | —                     | 3²                 | 4      |
| Pane-worthiness overall       | 3                   | —                  | —                     | 3.5²               | 4.5    |

¹ Round-2 scenario 2 hit the state-erasure blocker and the
chapter-stats bug; both (plus the launch-target and chooser majors)
were fixed and re-verified the same day — the scores predate the fixes.
² Scenario 3 re-run after the fingerprint-template fix; the residual
majors it found (stuck `}` navigation, badge decay, popup-paused
polling, noisy per-file events) were fixed and re-verified the same
day.

Baseline reports: `eval/reports/2026-07-05-baseline/`. Post-W1 recheck of
scenario 1: `eval/reports/2026-07-05-w1-recheck/`. Post-W2/W3 recheck of
scenario 2: `eval/reports/2026-07-05-w2w3-recheck/`. Round-2 reports
(scenario 2 full run + fix verification, scenario 3 broken-build run +
re-run): `eval/reports/2026-07-05-w2w3b-w4-recheck/`.

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

- [ ] `handoff --format json` should be a structured action artifact
      (action items as first-class objects), not a rendered-content dump.
- [ ] Markdown handoff: include task bodies and render task↔comment
      relationships explicitly.
- [ ] Human-readable diff inspection in the CLI (e.g. `hunks show
      --format diff`) so human reviewers aren't stuck reading JSON.
- [ ] `--help` examples clarifying `handoff` vs `export --profile agent`.

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
- [ ] Chapter-level dependency narration between stack changes (which
      change builds on which).
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
- [ ] Round-2 recheck gap: change briefs and comment drafts still
      require raw JSON-RPC — extend the `gander chunks` spec-file
      pattern to briefs (and consider drafts) for full CLI parity.

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
- [ ] Round-2 gap: `I` catch-up marks never-viewed files as viewed when
      unchanged since the op — the notice copy is now honest, but
      consider distinguishing "unchanged since op" from "reviewed".
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
