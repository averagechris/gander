# Attention map

A review's job is to update the reviewer's mental model so they can course
correct. The attention map is how gander spends the reviewer's attention where
the mental-model delta is — and lets them dismiss everything else with
confidence, without a separate presentation mode. Status: design accepted;
implementation tracked in roadmap milestone 18. Supersedes the zen phase
machinery and the ephemeral overlay-chunk model.

## Model

Salience is a durable property of the review session, assigned per region
(file or hunk range):

```rust
pub enum Salience { Spotlight, Supporting, Skim }

pub struct AttentionRegion {
    pub target: ReviewTarget,          // existing anchor/fingerprint machinery
    pub salience: Salience,
    pub rationale: Option<String>,     // why this matters / why it's skippable
    pub source: SalienceSource,        // Human | Agent | Heuristic
}
```

Sources, by precedence: human override > agent curation (today's
`StepImportance::{Spotlight, Glance}`, generalized) > heuristics (generated
and lockfile detection, ignore presets). Unassigned regions are `Supporting`.

The effective resolver applies source precedence before specificity, so a human
file override outranks an agent hunk. Within one source, hunk/range beats file,
overlapping ranges prefer the narrowest range, and malformed duplicate ties use
a stable higher-salience/canonical-target ordering. Durable identity is
`(source, file, normalized inclusive line range)`; setting the same identity is
an upsert. Regions reuse the existing `CommentAnchor` evidence through
`ReviewTarget.anchor`. A changed/missing file fingerprint leaves the assignment
durable but stale and excludes it from effective attention, conservatively
falling back to the next current assignment or implicit `Supporting`.
Effective list output partitions overlapping ranges into deterministic disjoint
spans at every assignment boundary, so a narrow assignment is never reported as
covering an entire broader range. Invalid legacy/merged regions are likewise
retained but treated as stale. Clearing a human override uses normalized
file/range identity and therefore works after a file disappears or its range
drifts outside the current diff; set/promote/demote still require a current
fingerprint anchor.

Walkthrough synchronization retains an existing Agent assignment unchanged and
stale while its source step target still exists but cannot currently re-anchor;
only removing that source target removes the assignment. Agent rationales use
the first trimmed non-empty `why`, `body`, or `title`.

Effective spans are formed only from actual anchorable diff rows. Sparse hunk
gaps are never synthesized into a region, singleton/final endpoints are
inclusive without `end + 1`, and adjacent assignment boundaries remain
explicit. TUI on-quit exports reload the final target's unfiltered diff through
jj's read-only `--ignore-working-copy` path, so a retarget or refresh cannot use
startup fingerprints for attention staleness.

Current automation is available through
`gander attention list|set|clear|promote|demote|seed-heuristics|recompute-heuristics`
and matching MCP tools. TUI folds, narration cards, chapters, walkthrough
navigation, coverage, the ephemeral Focus preset, and the attention glance
board are implemented. Legacy zen removal remains a later package.

## The review stream

One diff view. Salience changes rendering, never data:

- **Skim** regions collapse to a one-line fold —
  `⌄ 4 files · generated churn · +812 −340` — expandable in place, and
  acknowledgeable: one key records fingerprint-guarded region progress. A file
  is marked viewed only when the fold covers that file's entire current diff;
  partial fold acknowledgement never marks the whole file viewed. This is
  "don't even scan it", with the peek always one keypress away.
- **Supporting** regions render as a normal diff.
- **Spotlight** regions render fully expanded with the agent's narration
  (an onboarding annotation card: title, why, rationale) inline beside the
  code. Step artifacts (examples, diagrams) render inside the card,
  expandable.

Folding is formed from effective disjoint row spans, not raw file assignments:
a narrower higher-precedence Supporting or Spotlight override always punches
through a broad file-level Skim. A fold is whole-file only when it covers every
current anchorable changed row after that partition.

The cross-file index stays cheap: offscreen files contribute structural rows
directly from the parsed diff, while full syntax, word-diff, context-fold, and
gap rows are cached only when a file becomes current or enters a bounded stream
window. The stream projection and file/local owner indexes share one signature
cache that includes syntax configuration.

Contiguous spotlight regions sharing a `change_id` open with a chapter
header row (jj description, bookmarks, diff stats) — replacing the zen
chapter card.

## Navigation and progress

- **A walkthrough is an ordering over spotlight regions**, not a mode.
  Next/previous jump the normal view through the curated sequence.
- **Coverage** replaces files-viewed as the progress signal:
  spotlights visited + skims acknowledged, shown in the footer.
- **Focus is a view preset, not a room**: one key applies maximum folding,
  hides the file pane, and pins narration cards; the same key restores. The
  full review vocabulary (comments, folds, search, retargeting) works
  throughout — there is no modal phase to exit and no vocabulary loss.
- The glance board survives as a summary popup over the attention map: skim
  folds, acknowledgment state, and bulk acknowledge. `Alt-G` opens it from
  normal review; `Space` peeks the selected current fold in the stream, `a`
  acknowledges one, and `A` acknowledges every current unacknowledged fold.
  Stale durable skim history is visible but cannot be acknowledged or count.

`Z` toggles Focus. Focus captures and restores the exact explicit pane override,
skim peeks, supporting-context expansions/folds, inline-card expansion and
selection, and viewport context. While active it hides the pane, applies maximum
folding, and pins narration at the current spotlight. It is not a `Mode` or
phase: comments, ranges, search, retargeting, file navigation, and salience
actions keep their normal dispatch.

## What this deletes

- `ZenPhase` (Focus / Reading / Glance / Artifact) and its full-screen
  takeover surfaces.
- The ephemeral overlay-chunk model and its `set_chunks` compatibility path;
  durable walkthrough steps + attention regions are the only curation
  surface for agents (CLI, MCP, ACP alike).
- The "walkthrough dies on retarget" cliff: attention regions re-anchor with
  the same fingerprint machinery as comments and mark themselves stale
  instead of tearing down the presentation.

## Interactions

- **Viewed state**: acknowledging a whole-file fold marks that file viewed via
  the existing fingerprint state. Partial folds record region progress only. A
  stale fold or spotlight visit (fingerprint drift) reverts to unacknowledged /
  unvisited without deleting history, preserving the conservative guarantee.
- **Heuristics**: generated/lockfile detection seeds `Skim` automatically;
  `[ignore]`/`[generated]` config feeds the same map.
- **Human override**: promote/demote the region under the cursor with one
  key (`Alt-Up` / `Alt-Down`); human assignments outrank agent curation and
  persist in the session. `Alt-N` / `Alt-P` follow walkthrough-ordered
  spotlights in the normal stream. `Space` peeks a selected skim fold and `a`
  acknowledges it contextually. On an ordinary stream row, `a` is an explicit
  no-op and never falls through to global mark-all. Stream range selections are
  single-file and cancel when keyboard or mouse movement crosses a file
  boundary.

The shared automation surface is `gander attention coverage show`, `gander
attention skim-fold list`, and `gander attention acknowledge (--path ... |
--fold-id ... | --all)`, with matching MCP tools. All adapters use the same
stable fold identity, progress fingerprint, stale exclusion, and whole-file
viewed-effect service. Explicit unknown/ambiguous selectors fail rather than
reporting a successful zero-match operation; only bulk acknowledgement of an
empty current set is a documented no-op.
