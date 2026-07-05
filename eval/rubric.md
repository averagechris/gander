# Dogfood evaluation rubric

Score each dimension from 1 to 5. Use the anchors below as calibration points; interpolate when behavior falls between anchors.

## Discoverability

How quickly an evaluator can find the commands, flags, keys, and workflow without prior project knowledge.

- **1:** Core actions are hidden or surprising; evaluator cannot complete the scenario without source-diving or repeated failed guesses.
- **3:** The workflow is learnable from docs/help, but exact verbs, flags, or key semantics require lookups and retries.
- **5:** The interface guides a first-time evaluator through the intended loop with clear help, names, and next-step cues.

## Output quality for humans

How readable and review-useful CLI/TUI output is for a person inspecting changes.

- **1:** Output is noisy, incomplete, or hard to map back to files and hunks.
- **3:** Output is accurate and usable, but requires extra commands or manual context assembly for routine review.
- **5:** Output is concise, well-formatted, and gives humans the right context, metadata, and navigation affordances by default.

## Output quality for agents

How well machine-readable or agent-profile output supports autonomous coding agents.

- **1:** Agents must re-discover the diff and review state from scratch because exports are missing anchors or actionable metadata.
- **3:** Agents can reconstruct the review with multiple commands and raw hunks, but action state, excerpts, or relationships are fragmented.
- **5:** A single structured artifact gives agents target metadata, comments, tasks, walkthroughs, exact anchors, and compact code excerpts.

## Handoff readiness

How complete the review state is as a one-shot handoff from reviewer to implementer.

- **1:** Handoff is mostly a transcript or loose notes; important intent must be inferred.
- **3:** Handoff contains the key findings, but an implementer still needs to stitch together commands or re-read broad diff context.
- **5:** Handoff is directly actionable: ordered priorities, linked tasks/comments, anchors, excerpts, target range, and expected fixes are bundled.

## TUI review ergonomics

How effective the TUI is for ordinary code review: navigation, diff reading, comments, viewed state, and stack awareness.

- **1:** Basic review actions are unreliable or too cumbersome to complete.
- **3:** Core review works, but common actions are awkward, hidden, or disorienting.
- **5:** Review flow is fast and confident, with clear targets, obvious keys, stable state, and low-friction commenting/viewed tracking.

## Zen uncurated

How useful zen mode is before any agent curation.

- **1:** The tour is misleading, noisy, or worse than normal browsing.
- **3:** The tour is calmer than browsing and helps with progress, but provides limited insight into risk or intent.
- **5:** The fallback tour independently highlights meaningful review questions, important files, relationships, and risks.

## Zen curated

How useful zen mode is after agent-provided change briefs, spotlight/glance chunks, and draft comments.

- **1:** Curation does not materially improve the tour or introduces misleading output.
- **3:** Curation adds useful context, but rough rendering, validation gaps, or pacing limit confidence.
- **5:** Curated zen feels like a high-quality guided briefing with clear narrative, accurate anchors, grouped concepts, and useful glance items.

## Curation protocol ergonomics

How easy and safe it is for an agent or orchestrator to author ACP curation.

- **1:** Protocol usage is fragile enough that correct curation is unlikely without deep debugging.
- **3:** Protocol is documented and capable, but requires careful manual JSON-RPC, exact line spaces, and replace-all updates.
- **5:** Authoring is validated, incremental, and ergonomic, with clear errors/previews and friendly file formats or helpers.

## Watch freshness/follows-@

How accurately the TUI refreshes while the repository changes, especially symbolic revsets such as `@`.

- **1:** The pane often goes stale, loses state, or follows the wrong revision/range.
- **3:** Refresh usually works, but freshness is hard to verify or fails on common jj operations.
- **5:** Refresh is timely, correct, visibly tied to revision identity, and robust across edits, `jj new`, undo, and abandon.

## Watch change awareness

How well an ambient pane explains what changed since the user last looked.

- **1:** Changes are silent or only visible by manually re-reading the whole diff.
- **3:** The pane reports refreshes and aggregate stats, but per-file/per-hunk or stack-change deltas are limited.
- **5:** The pane clearly marks new/changed/reverted files and hunks, summarizes operations, and provides navigation to fresh work.

## Pane-worthiness

Whether gander is valuable as a persistent side pane next to an autonomous coding agent.

- **1:** The pane cannot be trusted to stay accurate or useful without constant manual refresh/restart.
- **3:** Worth keeping open as a live diff/status aid, but not sufficient to know what needs fresh review.
- **5:** A reviewer can rely on it all day for calm, accurate, actionable awareness of agent activity and pending review work.
