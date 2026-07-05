# Summary

Re-evaluation verdict: gander is now substantially closer to a polished CLI-to-agent handoff tool. I could complete the whole review loop from the CLI, including session creation for `main..@`, changed-file and hunk inspection, line-anchored comments with `kind`/`action`, anchored tasks, walkthrough steps, and both Markdown/JSON agent handoffs.

The major improvement is the new `handoff` command. Its Markdown is prompt-shaped, action-first, includes repository/target/session metadata, counts, open tasks, unresolved comments, code excerpts, walkthrough order, and full hunks. That directly addresses the previous 2/5 handoff-readiness gap. The remaining gaps are mostly polish and schema completeness: `handoff --format json` is still mostly a rendered Markdown string plus metadata rather than a deeply structured action graph; task bodies are not shown in the Markdown action list; task/comment linking is one-way and easy to miss unless the reviewer manually passes `--comment`; and `hunks list | head` still prints partial JSON by design, which is composable but not useful without a line-oriented/summary mode.

No regressions versus the baseline were obvious. New friction: the top-level help still advertises old/normal `export` first enough that a user can miss `handoff`, and some subcommand help summaries are blank or terse (`comments add`, `tasks add`, `walkthrough add-step`).

# Step log

All commands were run from:

```sh
/var/folders/vc/ngphl55953z3y1wcb363syjc0000gn/T/opencode/gander-eval/fixture-w1-recheck
```

Using the release binary directly:

```sh
/Users/chris/projects/gander/target/release/gander
```

I first explored help, including looking for any new handoff-oriented command:

```sh
/Users/chris/projects/gander/target/release/gander --help
/Users/chris/projects/gander/target/release/gander reviews --help
/Users/chris/projects/gander/target/release/gander files --help
/Users/chris/projects/gander/target/release/gander hunks --help
/Users/chris/projects/gander/target/release/gander comments --help
/Users/chris/projects/gander/target/release/gander tasks --help
/Users/chris/projects/gander/target/release/gander walkthrough --help
/Users/chris/projects/gander/target/release/gander export --help
/Users/chris/projects/gander/target/release/gander handoff --help
```

Notable help output:

- Top-level help now includes `handoff                Print or copy a prompt-style handoff for a coding agent`.
- `export --profile agent` says: `Artifact profile; agent adds raw hunks and comment excerpts`.
- `handoff` supports `--format json|markdown`, `--only-open`, `--output`, and `--copy`.
- I initially tried `gander session --help` out of habit from the scenario wording and got `error: unrecognized subcommand 'session'`; the actual command group is `reviews`.

Created the review session for `main..@`:

```sh
/Users/chris/projects/gander/target/release/gander reviews create -b main -r @ --title "Recheck CLI handoff review"
```

This returned a session with id `46904a28-326a-41ac-a9c0-641b2dc4f686`, title `Recheck CLI handoff review`, and target metadata showing `revset: main..@`, `base: main`, `revision: @`, and the fixture repo path.

Listed changed files:

```sh
/Users/chris/projects/gander/target/release/gander files list -b main -r @
```

This produced compact JSON for 7 files: `src/config.rs`, `src/lib.rs`, `src/priority.rs`, `src/queue.rs`, `src/retry.rs`, `src/worker.rs`, and `tests/basic.rs`, with additions/deletions, hunk counts, fingerprints, generated flags, and viewed state.

Listed hunks and tried Unix composition:

```sh
/Users/chris/projects/gander/target/release/gander hunks list -b main -r @
/Users/chris/projects/gander/target/release/gander hunks list -b main -r @ | head
```

The full list is useful JSON with stable ids like `src/queue.rs:1` and `src/worker.rs:0`. Piping through `head` no longer emitted the baseline broken-pipe backtrace in this run. It simply printed the first lines of a JSON object, which is acceptable Unix behavior but not semantically useful because the output is truncated invalid JSON. A line-oriented `--summary` or JSONL mode would make `head` genuinely useful.

Showed several hunks across the stack:

```sh
/Users/chris/projects/gander/target/release/gander hunks show 'src/queue.rs:1' -b main -r @
/Users/chris/projects/gander/target/release/gander hunks show 'src/retry.rs:0' -b main -r @
/Users/chris/projects/gander/target/release/gander hunks show 'src/worker.rs:0' -b main -r @
/Users/chris/projects/gander/target/release/gander hunks show 'tests/basic.rs:1' -b main -r @
/Users/chris/projects/gander/target/release/gander hunks show 'src/config.rs:0' -b main -r @
```

These hunk outputs were verbose but precise. They include `kind`, old/new line numbers, and text for each line, which is strong for tooling and good enough for human CLI review if the user is comfortable reading JSON.

Added two line-anchored comments with meaningful `kind` and `action` values:

```sh
/Users/chris/projects/gander/target/release/gander comments add -b main -r @ --path src/retry.rs --line 20 --kind issue --action fix --body "Off-by-one retry gate: attempts is incremented before this check, so attempts <= max_retries permits one extra retry. Use < max_retries (or redefine attempts semantics) and align tests."

/Users/chris/projects/gander/target/release/gander comments add -b main -r @ --path src/worker.rs --line 34 --kind issue --action fix --body "Re-enqueueing a failed job creates a fresh Job id, but attempts are keyed by job.id. A permanently failing payload can retry forever because each dequeue sees a new id with no attempt history."
```

Both returned rich anchors, including hunk header, hunk index, line kind, line text, line fingerprint, and diff fingerprint.

Added anchored tasks, including one linked to a comment:

```sh
/Users/chris/projects/gander/target/release/gander tasks add -b main -r @ --title "Fix retry accounting and add regression coverage" --body "Preserve retry identity across re-enqueue or key attempts by a stable original job id; fix RetryPolicy boundary and add tests for max_retries=1 and permanently failing empty payload." --action fix --path src/worker.rs --line 34

/Users/chris/projects/gander/target/release/gander tasks add -b main -r @ --title "Correct RetryPolicy off-by-one" --body "Change should_retry boundary and update assertions around max_retries semantics." --action fix --comment 90ff32d1-fd8b-45b5-9fd5-55647e9ea019 --path src/retry.rs --line 20
```

The linked task returned `source_comment_id: 90ff32d1-fd8b-45b5-9fd5-55647e9ea019`.

Added two walkthrough steps:

```sh
/Users/chris/projects/gander/target/release/gander walkthrough add-step -b main -r @ --title "Start with queue priority semantics" --path src/queue.rs --line 30 --end-line 48 --why "Priority ordering is the core behavior change and affects worker retry re-enqueue behavior." --body "Verify enqueue/dequeue preserve priority and FIFO within a priority band before reviewing consumers."

/Users/chris/projects/gander/target/release/gander walkthrough add-step -b main -r @ --title "Then inspect retry/worker interaction" --path src/worker.rs --line 22 --end-line 36 --why "The correctness risk is at the boundary between RetryPolicy and Worker attempt tracking." --body "Follow a failed job through attempts, should_retry, and enqueue_with_priority to ensure retry limits terminate."
```

Role-switched to a coding agent consuming the review and ran the required commands:

```sh
/Users/chris/projects/gander/target/release/gander export markdown --profile agent -b main -r @
/Users/chris/projects/gander/target/release/gander export json --profile agent -b main -r @
/Users/chris/projects/gander/target/release/gander tasks list -b main -r @
/Users/chris/projects/gander/target/release/gander comments list -b main -r @
/Users/chris/projects/gander/target/release/gander walkthrough export -b main -r @
```

Important observations:

- `export markdown --profile agent` now includes action items, comment excerpts, walkthrough steps, other comments, and full hunks. This is much more agent-ready than the baseline.
- `export json --profile agent` now includes session metadata, files/hunks, comments with anchors and excerpts, tasks, and walkthroughs. This fixes the baseline fragmentation for JSON export.
- `tasks list`, `comments list`, and `walkthrough export` remain useful focused views.

Then evaluated the new handoff command:

```sh
/Users/chris/projects/gander/target/release/gander handoff -b main -r @
/Users/chris/projects/gander/target/release/gander handoff --format json --only-open -b main -r @
/Users/chris/projects/gander/target/release/gander handoff -b main -r @ | head
```

The Markdown handoff begins with:

```markdown
# Human review handoff for a coding agent

You are a coding agent consuming a human Gander review. Prioritize the action items before using full hunks as reference.

- Repository: `/private/var/folders/.../fixture-w1-recheck`
- Target: `main` → `@`
- Session: Recheck CLI handoff review
- Action counts: 2 open task(s), 2 unresolved comment(s)
```

It then lists tasks and comments first, with excerpts under comments, followed by walkthrough and full hunks. This is prompt-ready enough to paste to a coding agent. `handoff | head` also behaved cleanly: no backtrace or broken-pipe noise.

# Findings

## major — `handoff --format json` is not structured enough for ideal agent automation

Concrete repro:

```sh
/Users/chris/projects/gander/target/release/gander handoff --format json --only-open -b main -r @
```

The JSON handoff is useful, but it is more like an envelope around rendered handoff content than the ideal structured action artifact. For an autonomous agent, I want first-class arrays of `action_items`, `comments`, `tasks`, `walkthrough_steps`, `anchors`, `excerpts`, `target`, and `full_hunks`, with stable ids and relationships. `export json --profile agent` is closer to that shape; `handoff --format json` should either reuse that complete schema or make its prompt-oriented action list structured too.

Impact: a coding agent can consume the Markdown very well, but automation that wants to prioritize, filter, or update tasks still has to parse prose/Markdown or use separate commands.

## minor — Markdown handoff action list omits task bodies/details

Concrete repro:

```sh
/Users/chris/projects/gander/target/release/gander tasks add -b main -r @ --title "Fix retry accounting and add regression coverage" --body "Preserve retry identity across re-enqueue or key attempts by a stable original job id; fix RetryPolicy boundary and add tests for max_retries=1 and permanently failing empty payload." --action fix --path src/worker.rs --line 34
/Users/chris/projects/gander/target/release/gander handoff -b main -r @
```

Actual action item:

```markdown
- [task][fix] Fix retry accounting and add regression coverage — `src/worker.rs`:34
```

The body text is absent from the top action list. That body contained the concrete expected fix and regression coverage. A later agent can infer much of it from the comments and hunks, but the task authoring affordance invites detail that the handoff then hides.

## minor — Linked task/comment relationships are easy to underuse and not fully surfaced

Concrete repro:

```sh
/Users/chris/projects/gander/target/release/gander comments add -b main -r @ --path src/worker.rs --line 34 --kind issue --action fix --body "..."
/Users/chris/projects/gander/target/release/gander tasks add -b main -r @ --title "Fix retry accounting and add regression coverage" --body "..." --action fix --path src/worker.rs --line 34
/Users/chris/projects/gander/target/release/gander tasks list -b main -r @
```

Unless I explicitly pass `--comment <id>`, an anchored task near a comment is not linked. The handoff shows linked comments for the second task only, but does not show a reciprocal `related tasks` line under the comment excerpt. A polished handoff would make relationships hard to miss and perhaps suggest/comment-link matching by same file+line.

## minor — CLI inspection is still JSON-first; good for machines, less pleasant for humans

Concrete repro:

```sh
/Users/chris/projects/gander/target/release/gander hunks show 'src/worker.rs:0' -b main -r @
```

The JSON is accurate and scriptable, but as a human reviewer I still have to visually parse objects for every line. There is no obvious `hunks show --format diff` or `hunks list --summary` in help. The handoff/full Markdown diff is readable, but the primary review-inspection commands remain machine-readable rather than human-friendly.

## papercut — Help remains terse in important subcommands

Concrete repro:

```sh
/Users/chris/projects/gander/target/release/gander comments add --help
/Users/chris/projects/gander/target/release/gander tasks add --help
/Users/chris/projects/gander/target/release/gander walkthrough add-step --help
```

The usage and flags are present, but command summaries are blank or minimal. For a first-time evaluator, examples would help a lot, especially for linking tasks to comments and choosing `kind`/`action` values.

## papercut — `hunks list | head` is quiet but not very useful

Concrete repro:

```sh
/Users/chris/projects/gander/target/release/gander hunks list -b main -r @ | head
```

This no longer showed the baseline broken-pipe backtrace. However, the output is just partial pretty JSON. That is technically composable, but a common CLI pattern like `| head` would be more valuable with JSONL or a terse table/summary mode.

# Scores

- **Discoverability: 4/5.** Before/after versus baseline: **3 → 4**. The top-level `handoff` command is discoverable and named exactly for the agent use case, and `handoff --help` exposes `--format`, `--only-open`, `--output`, and `--copy`. Remaining friction: I still guessed `session` before finding `reviews`, subcommand help lacks examples, and human-readable hunk modes are not obvious.
- **Output quality for humans: 4/5.** Before/after versus baseline: **4 → 4**. The file/hunk data is accurate, anchors are rich, and Markdown handoff/walkthrough output is readable. Human hunk inspection is still JSON-heavy, and `head` gives partial JSON rather than a useful summary, so I would not raise this to 5.
- **Output quality for agents: 5/5.** Before/after versus baseline: **3 → 5**. `export json --profile agent` now contains session metadata, target, files, full hunks, comments with anchors and excerpts, tasks, and walkthroughs. Markdown agent export/handoff is action-first and includes excerpts plus full hunks. The JSON handoff shape could be better, but the agent profile artifact itself meets the rubric's single structured artifact bar.
- **Handoff readiness: 4/5.** Before/after versus baseline: **2 → 4**. The new `handoff` command is prompt-ready and directly actionable: target/session metadata, counts, open tasks, unresolved comments, excerpts, walkthrough order, and full hunk context are bundled. It misses a 5 because task bodies/details and richer structured relationships are not surfaced as well as they should be, and JSON handoff is not the ideal structured action model.

Regressions found: **none obvious**. The previous blocker, broken-pipe backtrace on `| head`, did not reproduce for `hunks list | head` or `handoff | head`.

New friction found: the presence of both `export --profile agent` and `handoff` creates some ambiguity about which is canonical for agents; `handoff` is better prompt Markdown, while `export json --profile agent` is better structured data.

# Top proposals

1. **Make `handoff --format json` a first-class structured agent artifact.** Include explicit arrays for action items, comments, tasks, walkthrough steps, full hunks, anchors, excerpts, target, session, and links. Avoid making agents parse rendered Markdown.

2. **Render task bodies and relationships in Markdown handoff.** Under each task, include body, status, action, anchor, linked comment ids, and any same-line related comments. Under each comment, include linked/open task ids.

3. **Add human-friendly hunk formats.** Examples: `hunks list --summary`, `hunks show --format diff`, or JSONL output so `| head` and other Unix composition are meaningful, not just quiet.

4. **Improve help with examples and next-step cues.** Add examples for the main loop: create review, list files/hunks, show a hunk, add a comment, add a linked task, add walkthrough, export handoff.

5. **Clarify canonical agent flows.** Help text should say when to use `handoff` versus `export --profile agent`, e.g. “Use `handoff` for prompt-ready Markdown; use `export json --profile agent` for structured automation.”
