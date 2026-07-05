# Summary

I dogfooded `gander` as a CLI-only reviewer on the fixture stack and then as an agent trying to consume the resulting review. The core CLI is usable for structured review state: I could create a session, inspect files/hunks, add comments with kind/action tags, create a task, add walkthrough steps, and export agent-oriented artifacts. I found a real fixture bug: failed jobs are re-enqueued with a new id, so retry accounting keyed by `job.id` resets forever for permanent failures when `max_retries >= 1`.

The biggest UX gap is handoff readiness. The exported Markdown is readable but omits tasks, walkthroughs, kind/action tags, and code excerpts. The exported JSON contains raw hunks but, in this run, comments did not include the documented `excerpt`/`anchor` fields, and it also omitted tasks and walkthroughs. Agents can act, but only by combining several commands and often re-reading diff context.

# What I did

Brief transcript of key commands, all from `/var/folders/vc/ngphl55953z3y1wcb363syjc0000gn/T/opencode/gander-eval/fixture-cli` with `alias gander=/Users/chris/projects/gander/target/release/gander`:

```sh
jj status
gander --repo . -b main -r @ reviews create --title 'CLI handoff UX eval'
gander --repo . -b main -r @ files list
gander --repo . -b main -r @ hunks list | head
gander --repo . -b main -r @ hunks list
gander --repo . -b main -r @ hunks show src/queue.rs:0
gander --repo . -b main -r @ hunks show src/queue.rs:1
gander --repo . -b main -r @ hunks show src/worker.rs:0
gander --repo . -b main -r @ hunks show src/retry.rs:0
gander --repo . -b main -r @ comments add --path src/worker.rs --line 34 --kind issue --action fix --body '...'
gander --repo . -b main -r @ tasks add --title 'Fix retry requeue preserving job identity' --action fix --comment <comment-id> --path src/worker.rs --line 34
gander --repo . -b main -r @ comments add --path src/retry.rs --line 20 --kind issue --action test --body '...'
gander --repo . -b main -r @ walkthrough add-step --title 'Start with the public queue API changes' --file src/queue.rs --line 26 --why '...' --body '...'
gander --repo . -b main -r @ walkthrough add-step --title 'Then inspect worker retry behavior' --file src/worker.rs --line 31 --why '...' --body '...'
gander --repo . -b main -r @ export markdown --profile agent
gander --repo . -b main -r @ export json --profile agent
gander --repo . -b main -r @ tasks list
gander --repo . -b main -r @ comments list
gander --repo . -b main -r @ walkthrough export
```

Useful output examples:

- `files list` gave a compact review map: `7 files ... +134/-12`, including `src/config.rs`, `src/priority.rs`, `src/queue.rs`, `src/retry.rs`, `src/worker.rs`, and tests.
- `hunks list` gave stable hunk ids like `src/worker.rs:0`, which made `hunks show` straightforward after I knew the syntax from docs.
- Piping to `head` produced noisy failure output:

```text
Error:
   0: Broken pipe (os error 32)

Location:
   src/main.rs:791

  ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━ BACKTRACE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
```

# Bugs found in fixture code

1. **Infinite retry / never dropped for permanent failures** (`src/worker.rs:34`) — `Worker::run` stores retry attempts in `HashMap<u64, u32>` keyed by `job.id`, but on failure it calls `queue.enqueue_with_priority(job.payload.clone(), job.priority)`. That allocates a new id. The next dequeue sees a different `job.id`, so `attempts.entry(job.id)` starts at zero again. With `max_retries >= 1`, a permanently failing job such as an empty payload is requeued forever and never increments `dropped`.

2. **Retry boundary likely off by one and untested** (`src/retry.rs:20`) — `RetryPolicy::should_retry` returns `attempts <= self.max_retries`. The code comment itself says this yields one extra retry. Tests do not assert exact retry count, and because of bug 1 the empty-payload test would hang instead of proving the policy.

# Findings

## blocker — Broken pipe prints a backtrace when stdout is closed

Concrete repro:

```sh
gander --repo . -b main -r @ hunks list | head
```

Actual output begins with valid JSON, then emits:

```text
Error:
   0: Broken pipe (os error 32)
Location:
   src/main.rs:791
... BACKTRACE ...
```

This is hostile to CLI composition and makes `gander | head`, pagers that close early, and scripts noisy. Expected behavior: silently exit or treat EPIPE as success/no diagnostic.

## major — No single handoff command/export contains all actionable review state

Concrete repro:

```sh
gander --repo . -b main -r @ export markdown --profile agent
gander --repo . -b main -r @ export json --profile agent
gander --repo . -b main -r @ tasks list
gander --repo . -b main -r @ walkthrough export
```

Markdown export contained files and comments only. JSON export contained files/hunks/comments only. Tasks and walkthroughs required separate commands. For an agent, the actionable unit is the combination of comments, tasks, walkthrough order, target rev/base, and local diff context. Today that requires stitching at least four outputs together.

## major — Agent JSON did not include documented comment excerpts/anchors in this run

Concrete repro:

```sh
gander --repo . -b main -r @ export json --profile agent
```

The docs say agent profile adds `comments[].excerpt`, and the schema shows `anchor`. Actual comment object was:

```json
{
  "id": "def57888-3721-4b31-bde3-f2dfcf1aa27a",
  "path": "src/worker.rs",
  "line": 34,
  "body": "Re-enqueueing a failed job...",
  "kind": "issue",
  "action": "fix",
  "state": "draft"
}
```

It did not include nearby code or anchor metadata. The raw file hunks are elsewhere in the same JSON, so an agent can recover context, but not from the comment object alone.

## minor — Markdown agent export loses action metadata and code context

Concrete repro:

```sh
gander --repo . -b main -r @ export markdown --profile agent
```

The Markdown comments rendered as headings plus body and `Status: draft`, but omitted `kind: issue`, `action: fix/test`, related task id/title, and code excerpt. Example:

```markdown
### `src/worker.rs`:34

Status: draft

Re-enqueueing a failed job through enqueue_with_priority allocates a fresh job id...
```

This is pleasant for humans, but less agent-ready than `comments list` because the action intent is missing.

## minor — Command discovery depends on docs; `--help` hierarchy was not obvious enough from memory

Concrete repro: starting from the scenario, I expected either `gander diff`, `gander review files`, or `gander comments add --file`. The actual nouns are `files list`, `hunks list/show`, `comments add --path`, and `walkthrough add-step --file`. The CLI is consistent after reading `docs/cli.md`, but flags use both `--path` and `--file` for file anchors across command groups.

## papercut — Hunk ids are practical but require shell care

Concrete repro:

```sh
gander --repo . -b main -r @ hunks show src/queue.rs:1
```

The `path:index` id is easy to copy from `hunks list`, but it is not self-describing, and paths containing `:` would be ambiguous unless escaped or otherwise handled. A JSON `--id` field or accepting separate `--path --index` would be more script-friendly.

## papercut — Review session id is not surfaced in normal export/handoff flows

Concrete repro:

```sh
gander --repo . -b main -r @ reviews create --title 'CLI handoff UX eval'
gander --repo . -b main -r @ export json --profile agent
```

The create command returns a session id, but the artifact export does not visibly tie back to that session or include session title/tasks/walkthroughs. This makes durable multi-session review state harder to reason about for agents.

# Scores

- **Discoverability: 3/5.** The command groups are logical once read, and JSON output makes automation approachable. I still had to consult `docs/cli.md` for exact verbs and flags (`hunks show <path:index>`, `comments add --path`, `walkthrough add-step --file`). Some expected shortcuts such as a direct `diff` or `handoff` command do not exist.
- **Output quality for humans: 4/5.** `files list`, `hunks list`, and Markdown walkthrough export are clear. The raw JSON hunk output is verbose but accurate. The main human-facing problem is the broken-pipe backtrace/noise and the lack of prettier CLI diff rendering for quick reading.
- **Output quality for agents: 3/5.** JSON agent export includes full hunks, so an agent can reconstruct the diff without running `jj`. However, actionable state is fragmented: tasks and walkthroughs are not in export, Markdown drops action tags, and JSON comments lacked documented excerpts/anchors in my run.
- **Handoff readiness: 2/5.** There is no one-shot command that says “here is what the human wants fixed, with ordered context, code excerpts, and exact anchors.” A coding agent needs to run and merge `export json --profile agent`, `tasks list`, `comments list`, and `walkthrough export`, or else re-read large parts of the diff.

# Top improvement proposals

1. **Add a dedicated `gander handoff` command or make `export --profile agent` truly complete.** It should include: review/session metadata, base/rev/repo, open tasks, comments with kind/action/state, linked task/comment ids, walkthrough steps, and compact code excerpts around each actionable anchor. Ideally support `--format markdown|json` and `--only-open`.

2. **Add clipboard and file handoff ergonomics.** Concrete flags: `gander handoff --copy` to put Markdown on the clipboard, `gander handoff --output .gander/handoff.md` for a stable file agents can read/watch, and `gander handoff --print-path` so harnesses can avoid copy/pasting stdout.

3. **Make the agent profile more prompt-like and self-contained.** Markdown should start with a short preamble such as “You are a coding agent consuming a human Gander review. Prioritize open tasks and issue/fix comments; preserve base main/rev @.” Then list action items first, each with file:line, kind/action, linked task, and a 5-10 line excerpt. Put the full raw hunks after the action list as optional reference.

4. **Handle EPIPE cleanly.** Treat broken pipe as normal CLI behavior and suppress color backtraces unless explicitly requested. This is a high-impact fix for Unix composability.
