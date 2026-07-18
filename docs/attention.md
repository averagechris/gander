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

## The review stream

One diff view. Salience changes rendering, never data:

- **Skim** regions collapse to a one-line fold —
  `⌄ 4 files · generated churn · +812 −340` — expandable in place, and
  acknowledgeable: one key marks every contained file viewed. This is
  "don't even scan it", with the peek always one keypress away.
- **Supporting** regions render as a normal diff.
- **Spotlight** regions render fully expanded with the agent's narration
  (an onboarding annotation card: title, why, rationale) inline beside the
  code. Step artifacts (examples, diagrams) render inside the card,
  expandable.

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
  folds, acknowledgment state, and bulk acknowledge.

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

- **Viewed state**: acknowledging a fold marks contained files viewed via the
  existing fingerprints; a stale fold (fingerprint drift) reverts to
  unacknowledged, preserving the conservative-viewed guarantee.
- **Heuristics**: generated/lockfile detection seeds `Skim` automatically;
  `[ignore]`/`[generated]` config feeds the same map.
- **Human override**: promote/demote the region under the cursor with one
  key; human assignments outrank agent curation and persist in the session.
