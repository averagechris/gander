# Scenario 3: ambient watch-pane evaluation

Placeholders to substitute before handing to an evaluator:

- `{GANDER_BIN}`: absolute path to the gander release binary.
- `{FIXTURE}`: absolute path to the disposable fixture jj repo.
- `{REPORT}`: absolute path where you must write the final Markdown report.

You are an honest UX evaluator dogfooding gander. Your goal is to surface concrete product friction, not to be polite. Do not modify gander source code. You may modify only the disposable fixture repo, gander review state for `{FIXTURE}`, and the report at `{REPORT}`.

## tmux rules

Use tmux only through `nix shell nixpkgs#tmux --command tmux ...`. Start a detached session sized about 200x50. Send single keystrokes with `send-keys` without Enter unless the interaction explicitly needs Enter. Sleep about 1 second before each `capture-pane`; after repo mutations, wait about 5 seconds before capture because the TUI polls every ~2 seconds. Kill the tmux session when done.

Use jj commands with explicit evaluation identity and no signing, for example:

```sh
jj --config 'user.name="Eval"' --config 'user.email="eval@example.com"' --config signing.behavior=drop status
```

## Scenario

1. Work from `{FIXTURE}` and launch `{GANDER_BIN} -b main -r @ tui` in tmux as an ambient side pane.
2. Seed some review state:
   - Use `?` if needed.
   - Mark one file viewed.
   - Add one line comment and save it.
3. Mutate the fixture while the TUI stays open, capturing the pane about 5 seconds after each mutation:
   - Make a working-copy edit to an already changed file.
   - Make rapid successive edits to the same file.
   - Run `jj describe`, then `jj new`, then edit a different file so symbolic `@` moves.
   - Use `jj undo` and/or abandon the newest change to remove work.
4. For each mutation, judge:
   - Did the pane refresh accurately?
   - Did it preserve selection, viewed state, and comments?
   - Did it explain what changed since the last look?
   - Did it visibly follow `@` and stack movement?
   - Would this be trustworthy as an all-day pane beside an autonomous coding agent?
5. Kill the tmux session.

Relevant rubric dimensions to score: watch freshness/follows-@, watch change awareness, pane-worthiness.

## Required report sections

Write `{REPORT}` as Markdown with these sections:

1. `# Summary` — concise overall verdict and the most important product gaps.
2. `# Step log` — mutations, jj commands, capture excerpts, and refresh observations.
3. `# Findings` — each finding must include severity (`blocker`, `major`, `minor`, or `papercut`) and a concrete repro.
4. `# Scores` — score each relevant rubric dimension from 1 to 5 with justification.
5. `# Top proposals` — prioritized improvements for a first-class watch/follow mode.
