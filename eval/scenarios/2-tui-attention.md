# Scenario 2: TUI attention-stream evaluation

Placeholders to substitute before handing to an evaluator:

- `{GANDER_BIN}`: absolute path to the gander release binary.
- `{FIXTURE}`: absolute path to the disposable fixture jj repo.
- `{REPORT}`: absolute path where you must write the final Markdown report.

You are an honest UX evaluator dogfooding gander. Your goal is to surface concrete product friction, not to be polite. Do not modify gander source code. You may modify only gander review state for `{FIXTURE}` and the report at `{REPORT}`.

## tmux rules

Use tmux only through `nix shell nixpkgs#tmux --command tmux ...`. Start a detached session sized about 200x50. Send single keystrokes with `send-keys` without Enter unless the interaction explicitly needs Enter. Sleep about 1 second before each `capture-pane`. Kill the tmux session when done.

Example shape:

```sh
nix shell nixpkgs#tmux --command tmux new-session -d -s gander-tui-eval -x 200 -y 50 -- '{GANDER_BIN} -b main -r @ tui'
nix shell nixpkgs#tmux --command tmux send-keys -t gander-tui-eval '?'
sleep 1
nix shell nixpkgs#tmux --command tmux capture-pane -t gander-tui-eval -p
nix shell nixpkgs#tmux --command tmux kill-session -t gander-tui-eval
```

## Scenario

1. Work from `{FIXTURE}` and launch `{GANDER_BIN} -b main -r @ tui` in tmux.
2. Learn the keys from `?`. Record whether help is task-oriented enough for a first-time reviewer.
3. Review the stack normally:
   - Navigate changed files and diffs.
   - Step change-by-change through the stack.
   - Mark at least one file viewed.
   - Add one line-anchored comment and save it.
4. Exercise the uncurated attention stream:
   - Toggle Focus with `Z`, open Glance with Alt-G, and try Alt-N/Alt-P.
   - Record whether the normal stream keeps comments, search, folds, and
     retargeting available while Focus is active.
   - Note whether generated/routine churn is easy to identify and acknowledge.
5. Curate while the TUI is live:
   - Use `gander walkthrough set` (or MCP `walkthrough_set`) to author chapters
     and 3-4 precise Spotlight steps with why/body narration and one artifact.
   - Use `gander attention set` and `seed-heuristics` to mark routine changes Skim.
   - Draft one durable comment through CLI/MCP/ACP.
   - Record friction around change IDs, fingerprints, stale targets, and JSON.
6. Toggle Focus again, follow Alt-N/Alt-P Spotlight order, inspect Alt-G, and
   compare curated vs uncurated usefulness. Retarget once and confirm Focus and
   normal actions remain safe.
7. Kill the tmux session.

Relevant rubric dimensions to score: TUI review ergonomics, uncurated attention,
curated Focus/Spotlight/Glance, and curation protocol ergonomics.

## Required report sections

Write `{REPORT}` as Markdown with these sections:

1. `# Summary` — concise overall verdict and the most important product gaps.
2. `# Step log` — tmux commands/interactions, captured excerpts, uncurated
   attention log, curation commands used, and curated comparison.
3. `# Findings` — each finding must include severity (`blocker`, `major`, `minor`, or `papercut`) and a concrete repro.
4. `# Scores` — score each relevant rubric dimension from 1 to 5 with justification.
5. `# Top proposals` — prioritized improvements that would make TUI attention
   review and durable curation more useful.
