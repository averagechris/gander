# Scenario 2: TUI and zen evaluation

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
4. Run zen without any curation:
   - Enter zen and step stop by stop.
   - Keep a stop-by-stop log: did each stop add information beyond normal browsing?
   - Note whether zen explained intent, risk, dependencies, and review questions.
5. Curate while the TUI is live, evaluating both authoring paths:
   - Set change briefs for the meaningful fixture changes via `gander acp` JSON-RPC per `docs/acp.md`.
   - Set 3-4 chunks, mixing spotlight and glance where appropriate. Author at least one full set via the `gander chunks` CLI (`gander chunks --help`; spec file or stdin) and at least one incremental edit (`chunks update` / `chunks remove` or ACP `review/update_chunks`).
   - Draft one comment through ACP.
   - Record friction around change IDs, line spaces, validation, and authoring JSON — and whether the CLI path is discoverable and materially easier than raw JSON-RPC.
6. Run zen again and compare curated vs uncurated usefulness.
7. Kill the tmux session.

Relevant rubric dimensions to score: TUI review ergonomics, zen uncurated, zen curated, curation protocol ergonomics.

## Required report sections

Write `{REPORT}` as Markdown with these sections:

1. `# Summary` — concise overall verdict and the most important product gaps.
2. `# Step log` — tmux commands/interactions, captured excerpts, uncurated stop log, ACP commands conceptually used, and curated comparison.
3. `# Findings` — each finding must include severity (`blocker`, `major`, `minor`, or `papercut`) and a concrete repro.
4. `# Scores` — score each relevant rubric dimension from 1 to 5 with justification.
5. `# Top proposals` — prioritized improvements that would make TUI review and zen more useful.
