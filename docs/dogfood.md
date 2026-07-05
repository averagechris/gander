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

| Dimension                     | 2026-07-05 baseline | 2026-07-05 post-W1 | Target |
| ----------------------------- | ------------------- | ------------------ | ------ |
| CLI discoverability           | 3                   | 4                  | 4      |
| TUI review ergonomics         | 4                   | —                  | 4.5    |
| CLI output quality for agents | 3                   | 5                  | 4      |
| Handoff readiness             | **2**               | **4**              | 4      |
| Zen usefulness, uncurated     | **2**               | —                  | 3.5    |
| Zen usefulness, curated       | 4                   | —                  | 4.5    |
| Curation protocol ergonomics  | **2**               | —                  | 4      |
| Watch: freshness / follows @  | 4                   | —                  | 4.5    |
| Watch: change awareness       | **2**               | —                  | 4      |
| Pane-worthiness overall       | 3                   | —                  | 4.5    |

Baseline reports: `eval/reports/2026-07-05-baseline/`. Post-W1 recheck of
scenario 1: `eval/reports/2026-07-05-w1-recheck/`.

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

- [ ] Every fallback stop answers "why this matters": derived signals
      (new/changed public API, symbols touched, error/retry/unsafe-pattern
      heuristics, test coverage relation, file role) instead of a bare file
      card.
- [ ] Chapter cards narrate the stack: per-change description, dependency
      between changes, tests added/missing, open review tasks.
- [ ] Glance board groups by concept with expandable locations; uncovered
      files carry a rationale so bulk mark-viewed is a confident act.
- [ ] Multi-part spotlight chunks render as "part 1/2 / part 2/2" with
      cross-references, not disconnected stops.
- [ ] Empty/uncurated tours say so explicitly and point at the summon flow.

### W3 — Curation protocol ergonomics

- [ ] Validate agent chunks: reject or visibly flag parts not present in
      the anchored change's diff, out-of-range lines, unknown files.
- [ ] Incremental chunk updates (add/update/remove) alongside full replace.
- [ ] Friendlier authoring path than hand-written JSON-RPC (e.g. spec file
      / stdin document the CLI translates).
- [ ] Live-bridge transparency: when `gander acp`/`mcp` bridges to a live
      TUI whose target differs from the invoked flags, say so in the
      response and on stderr.
- [ ] MCP `comment_add` still stores `anchor: None` (it never loads full
      diff context), so MCP-created comments lack excerpts in agent
      exports. Derive anchors once MCP loads diff context (M14 parity;
      `TODO(M14)` marker in `src/mcp.rs`).

### W4 — Watch / ambient pane legibility

- [ ] Per-file/hunk "changed since last look" badges, plus a distinct
      "viewed but changed since viewed" state.
- [ ] Activity feed of refresh events (`queue.rs updated 3×, +9`, `@ moved
      …`, `change abandoned …`), collapsed and calm.
- [ ] Explicit follow-mode indication for symbolic revsets (`following @`)
      and durable notices when `@` moves or a change enters/leaves the
      reviewed range.
- [ ] Navigation keys for next/previous changed-since-last-look hunk.

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
