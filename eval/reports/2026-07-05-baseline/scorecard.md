# Baseline dogfood scorecard — 2026-07-05

| Dimension | Score | Justification |
| --- | ---: | --- |
| TUI review ergonomics | 4 | Normal file navigation, diff focus, comments, viewed state, and stack stepping were present and mostly fast; stack target mental model and dense help were the main deductions. |
| CLI output quality for agents | 3 | JSON agent export included full hunks, but actionable state was fragmented across export, tasks, comments, and walkthrough commands, and comments lacked documented excerpts/anchors. |
| Handoff readiness | 2 | No one-shot command exported comments, tasks, walkthrough order, code excerpts, exact anchors, and target metadata together for a coding agent. |
| Zen uncurated | 2 | The fallback tour was calm and could mark files viewed, but mostly replayed file browsing without intent, risk, review questions, or useful spotlighting. |
| Zen curated | 4 | Agent-provided briefs and chunks turned zen into a real guided briefing, but invalid chunk handling, truncation, and glance-board rough edges remained. |
| Curation protocol ergonomics | 2 | ACP was capable and documented, but manual line-delimited JSON-RPC, change-specific line spaces, and replace-all chunk updates were brittle. |
| Watch freshness/follows-@ | 4 | Polling refreshed stats/diffs and `main..@` followed a moving `@`, including undo/abandon recovery, though revision identity was not visible enough. |
| Watch change awareness | 2 | The UI reported a generic repository refresh and aggregate stats, but not which files, hunks, or stack changes changed since last look. |
| Pane-worthiness | 3 | Useful as a live diff/status pane for an attentive user, but not yet trustworthy as an all-day ambient review pane beside an autonomous agent. |
