# Scenario 1: CLI handoff evaluation

Placeholders to substitute before handing to an evaluator:

- `{GANDER_BIN}`: absolute path to the gander release binary.
- `{FIXTURE}`: absolute path to the disposable fixture jj repo.
- `{REPORT}`: absolute path where you must write the final Markdown report.

You are an honest UX evaluator dogfooding gander. Your goal is to surface concrete product friction, not to be polite. Do not modify gander source code. You may modify only gander review state for `{FIXTURE}` and the report at `{REPORT}`.

## Scenario

1. Work from `{FIXTURE}`. Use `{GANDER_BIN}` directly; do not rely on shell aliases.
2. Create a review session for `main..@` with a descriptive title.
3. CLI-only human review:
   - List changed files.
   - List hunks, including trying common CLI composition such as piping through `head`; record any friction or noisy behavior.
   - Show several hunks across the stack, including queue, retry, worker, tests/config if present.
   - Add at least two line-anchored comments with meaningful `kind` and `action` values.
   - Add at least one task, preferably linked to a comment and anchored to a file/line.
   - Add two walkthrough steps that would help a later reader understand the review order.
4. Role-switch to a coding agent consuming the review:
   - Try `export markdown --profile agent`.
   - Try `export json --profile agent`.
   - Run `action-items list`, `comments list`, and `walkthrough export`.
   - Judge whether the available outputs are complete enough for an ideal one-shot handoff without re-reading the whole diff.
5. Compare what you could do with what you expected from a polished agent handoff command.

Relevant rubric dimensions to score: discoverability, output quality for humans, output quality for agents, handoff readiness.

## Required report sections

Write `{REPORT}` as Markdown with these sections:

1. `# Summary` — concise overall verdict and the most important product gaps.
2. `# Step log` — commands run and notable output/friction. Include exact repro commands for surprising behavior.
3. `# Findings` — each finding must include severity (`blocker`, `major`, `minor`, or `papercut`) and a concrete repro.
4. `# Scores` — score each relevant rubric dimension from 1 to 5 with justification.
5. `# Top proposals` — prioritized improvements that would most improve CLI review and agent handoff.
