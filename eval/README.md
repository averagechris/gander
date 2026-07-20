# Dogfood evaluation harness

This directory contains repeatable dogfood scenarios for evaluating gander against a disposable jj fixture repo. The harness is docs/scripts only; do not point evaluators at a real working repo.

Archived reports preserve the vocabulary and commands of the removed
full-screen walkthrough and overlay-curation experiments. They are historical
evidence, not current instructions; scenario 2 now evaluates durable
walkthrough/attention curation with Focus, Spotlight navigation, and Glance in
the normal stream.

## Loop

1. Build the binary under test:

   ```sh
   nix develop --command cargo build --release
   ```

2. Create one fresh fixture per evaluator/scenario. Never share fixture paths between evaluators:

   ```sh
   eval/make-fixture.sh /tmp/gander-eval/fixture-1
   eval/make-fixture.sh /tmp/gander-eval/fixture-2
   eval/make-fixture.sh /tmp/gander-eval/fixture-3
   ```

3. Hand one scenario prompt from `eval/scenarios/` to a fresh evaluator agent with placeholders substituted:

   - `{GANDER_BIN}`: absolute path to the release binary, usually `target/release/gander`
   - `{FIXTURE}`: absolute path to that evaluator's fixture repo
   - `{REPORT}`: absolute path where the evaluator should write its report

4. Archive completed reports under `eval/reports/<date>-<label>/`.

5. Update the scorecard for that run and fold action items into `docs/dogfood.md`.

Evaluator review state is stored in the XDG state directory keyed by fixture path, so a fresh fixture path gives fresh gander review state. If a scenario needs a clean slate, generate a new fixture path rather than reusing or resetting an old one.

The fixture script creates a small `taskq` Rust stack with intentional review bugs. Do not fix those bugs in the fixture; they are calibration targets for the evaluators.

Baseline reports from the first run live in `eval/reports/2026-07-05-baseline/`.
