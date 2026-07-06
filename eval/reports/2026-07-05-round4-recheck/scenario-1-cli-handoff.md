# Summary

Scenario 1 (CLI-only review + agent handoff) on fixture `/tmp/gander-eval/round4-s1`, gander 0.5.0 release binary.

The core loop works and the JSON handoff artifact is genuinely strong: one command (`gander handoff --format json --only-open`) yields session metadata, all open tasks and unresolved comments as first-class action items with anchors, line-fingerprinted excerpts, bidirectional task↔comment links, the walkthrough, and reference hunks last. `hunks show --format diff` is excellent, hunk ids (`path:index`) compose well, piping through `head` is SIGPIPE-clean, and the anchor echo on `comments add` (returned `line_text`) caught a wrongly anchored comment of mine immediately.

The most important product gaps:

1. **`gander comments set-state` panics on every invocation** (clap arg-id collision between the subcommand's `--state <draft|todo|resolved>` and the global `--state <STATE-FILE>`). The comment triage verb is 100% unusable. Blocker.
2. **Unknown-id errors on all state-mutating commands dump color-eyre backtraces** (`comments resolve`, `tasks complete`, `tasks reopen`, `walkthrough remove-step`), while `hunks show` prints a clean one-liner. The "user errors print cleanly" goal is only half-landed.
3. **Markdown handoff misclassifies action items**: unresolved comments whose kind isn't `issue` (a `question`/`follow-up` and a `note`/`test`, both with explicit reviewer-set actions) are demoted to "Other comments", while the JSON handoff correctly includes all four as `action_items`. The markdown header even says "4 unresolved comment(s)" and then lists two.
4. `gander handoff` (markdown, the default format) is **byte-identical** to `gander export markdown --profile agent`, despite `--help` examples positioning them as different tools.

# Step log

All commands run from `/tmp/gander-eval/round4-s1` with `GANDER=/Users/chris/projects/gander/target/release/gander` spelled out in full; `--base main --rev @` throughout.

1. `gander --help` — clear top-level map; `handoff` vs `export` distinction described; new `briefs`/`drafts`/`chunks` groups discoverable with good spec examples in their help.
2. `gander reviews create --base main --rev @ --title "Priority queue scheduling + retry policy extraction review"` — returned session JSON with id `96946c8a`. Friction: `reviews create/list/show` have **no help descriptions at all** (blank lines in `gander reviews --help`), same for `comments add/resolve/set-state`, `tasks add/complete/reopen`, `walkthrough add-step/remove-step/move-step/show`, and per-flag docs (`--path`, `--line`, `--body`, `--title` are undocumented; line-number side/indexing semantics are guesswork).
3. `gander files` (bare) — usage error demanding a subcommand; `gander files list` returned useful JSON (status, hunk_count, additions/deletions, fingerprint, viewed). No human/table format; a human doing CLI-only review lives in `jq`.
4. `gander hunks list` piped through `head -3` — clean truncation, no SIGPIPE panic, nothing on stderr, exit 0. No friction.
5. `gander hunks show "src/queue.rs:1" --format diff` and 5 more hunks (queue, retry, worker×2, config, priority, tests) — unified diff output is exactly right for review. Bad ids (`src/queue.rs:9`, `bogus`) print a clean `error: unknown hunk id`, exit 1. 
6. `gander comments add --path src/retry.rs --line 21 --kind issue --action fix --body ...` — the returned anchor showed `line_text: "    }"`, exposing my off-by-one (target was line 20). **There is no `comments edit` or `comments delete`**, so the botched comment can only be `resolve`d; it still appears (as resolved) in exports unless `--only-open`. Re-added at line 20; added a range comment on `src/worker.rs:31-34` (retry id-churn bug), a `question`/`follow-up` on `src/queue.rs:46-48`, and a `note`/`test` on `tests/basic.rs:21-27`.
7. `gander tasks add --title "Fix retry loop..." --action fix --comment c158caa3-... --path src/worker.rs --line 34 --body ...` — comment-linked, file/line-anchored task worked first try. Second task linked to the queue question comment.
8. `gander walkthrough add-step` ×2 (priority model first, then the worker/retry path) with `--why` and `--body` — worked; `walkthrough export` renders ordered steps with locations, why, and body.
9. Role-switch to consuming agent:
   - `gander export markdown --profile agent` — 427 lines: action items → walkthrough → other comments → full reference hunks. Task bodies and task↔comment links present.
   - `gander export json --profile agent -o ...` — full session artifact: files (with per-hunk lines), 5 comments with rich anchors (per-line fingerprints, hunk headers, row indexes), 2 tasks with `linked_comment_ids`, walkthroughs.
   - `gander tasks list` — complete, but emits `"action": "followup"`; feeding that back (`tasks add --action followup`) is rejected (`possible values: fix, explain, test, follow-up`). The error tip ("a similar value exists: 'follow-up'") is nice, but the round-trip mismatch is a trap for agents.
   - `gander comments list` — all 5 comments with state/kind/action/anchors.
   - `gander walkthrough export` — clean markdown.
   - `gander handoff --format json --only-open -o ...` — the standout artifact: `{session, action_items[6], walkthrough[2], reference.hunks[7]}`; resolved comment correctly filtered; comments carry `linked_task_ids` and tasks `linked_comment_ids`; every action item has an `excerpt`.
   - `gander handoff -o ...` (markdown) — **identical bytes to `export markdown --profile agent`** (`diff` confirms). Its "Action items" lists only the 2 tasks + 2 `issue` comments; the `question` and `note` comments (with explicit `follow-up`/`test` actions) are buried under "Other comments".
10. Error-path probes: `tasks complete not-a-real-id` → color-eyre BACKTRACE wall (30+ useless `__mh_execute_header` frames), exit 1. Same for `comments resolve`, `tasks reopen`, `walkthrough remove-step`. `comments set-state <valid-id> --state todo` → **panic, exit 101**: `Mismatch between definition and access of 'state'. Could not downcast to TypeId(...)` from `clap_builder-4.6.0/src/parser/error.rs:32`. Note `comments set-state --help` silently drops the global `--state <STATE-FILE>` row — the subcommand enum arg shadows it.
11. Incidental (run in a different repo with old review state, no session): `gander handoff --format json` exits 0 and emits an artifact with `session.id: null`, `title: null` and whatever stray draft comments exist. No warning that no review session exists.

# Findings

1. **blocker — `comments set-state` panics on every invocation (clap arg-id collision).**
   Repro: `{GANDER_BIN} comments set-state <any-comment-id> --state todo --base main --rev @` → `The application panicked (crashed). Message: Mismatch between definition and access of 'state'...`, exit 101. Also panics with invalid ids and regardless of value. Root cause visible from the CLI surface: the subcommand's `--state <draft|todo|resolved>` collides with the global `--state <STATE-FILE>` (which vanishes from `comments set-state --help`). The draft→todo→resolved triage loop — which the handoff artifact's `state` field is built around — cannot be driven from the CLI at all.

2. **major — state-mutating commands print color-eyre backtraces for plain user errors.**
   Repro: `{GANDER_BIN} tasks complete not-a-real-id --base main --rev @` → `Error: 0: unknown task 'not-a-real-id'` followed by a ~30-frame `BACKTRACE` of `__mh_execute_header <unknown>` plus `COLORBT_SHOW_HIDDEN`/`RUST_BACKTRACE` tips. Identical wall for `comments resolve nope`, `tasks reopen nope`, `walkthrough remove-step nope`. Contrast: `hunks show bogus` prints a clean one-line `error: unknown hunk id 'bogus'`. The clean-error treatment landed for read paths but not mutation paths.

3. **major — markdown handoff/export drops actionable comments from "Action items" based on kind, contradicting its own header and the JSON artifact.**
   Repro: add a `--kind question --action follow-up` comment and a `--kind note --action test` comment; run `{GANDER_BIN} handoff`. Header says "Action counts: 2 open task(s), 4 unresolved comment(s)" but "## Action items" lists only the two `issue` comments; the question/note comments sit under "## Other comments" after the walkthrough. `handoff --format json` includes all 4 unresolved comments in `action_items`. An agent told to "prioritize the action items" (the document's own preamble) will skip reviewer-requested test coverage and follow-ups.

4. **major — no way to edit or delete a comment from the CLI.**
   Repro: `comments add` with a wrong `--line`, then look for a fix: subcommands are only `list`, `add`, `resolve`, `set-state`. The only mitigation is `resolve` (and `set-state` is broken, see finding 1), so the mistake permanently pollutes `export`/`handoff` output unless `--only-open` is passed. For agent-authored review state this makes anchoring mistakes irreversible.

5. **minor — `handoff` markdown is byte-identical to `export markdown --profile agent`.**
   Repro: `diff <({GANDER_BIN} handoff) <({GANDER_BIN} export markdown --profile agent)` → empty. The `--help` examples explicitly position handoff as "one-shot actionable prompt" vs export as "full session artifact", but in markdown the only real differentiators are `--only-open` and `--copy`. The "prompt-ready" handoff for this tiny 7-file review is 427 lines, ~two-thirds of which is the full reference diff an implementer agent could fetch itself.

6. **minor — action-value round-trip mismatch between output and input.**
   Repro: `{GANDER_BIN} tasks list` emits `"action": "followup"`; `{GANDER_BIN} tasks add --title x --action followup` → `error: invalid value 'followup' ... [possible values: fix, explain, test, follow-up]`. Markdown renders `[follow-up]`, JSON `followup`. Any agent echoing observed values back fails parsing.

7. **minor — help text is skeletal exactly where first-time users need it.**
   Repro: `{GANDER_BIN} reviews --help`, `comments add --help`, `tasks add --help`, `walkthrough add-step --help` — subcommands and most flags (`--title`, `--path`, `--line`, `--body`, `--kind` semantics, whether `--line` is a new-side 1-indexed line) have empty descriptions. Global flags are interleaved with command flags in an arbitrary order (`--path`, `--repo`, `--line`, `-r`, `-b`, `--end-line`, `--body`...), making the signal hard to scan. Top-level and `handoff`/`export`/`chunks`/`briefs`/`drafts` help is much better — the polish is uneven.

8. **minor — `handoff` silently succeeds with no review session.**
   Repro: in a repo with review state but no created session, `{GANDER_BIN} handoff --format json` exits 0 with `"session": {"id": null, "title": null, ...}` and whatever stray comments exist. An orchestrator can ship an empty/wrong handoff with no diagnostic. A warning on stderr (or a `--require-session` flag) would prevent this.

9. **papercut — "Other comments" render duplicates diff excerpts.**
   Repro: `{GANDER_BIN} handoff`, look at the `src/queue.rs`:range:46-48 comment: an anchor excerpt block (lines 46-48) is followed by a second wider context block repeating the same lines, which mixes old-side numbering (`-  28  self.jobs.pop_front()`) into new-side numbering (`44, 45, 46...`) with no column legend.

10. **papercut — everything is JSON; no human-oriented plain/table output for `files list`, `hunks list`, `tasks list`, `comments list`.**
    Repro: `{GANDER_BIN} files list` → pretty JSON only. CLI-only human review means `jq` for every glance. (`hunks show --format diff` shows the right pattern; `--format` should exist on the list commands too.)

Positives worth keeping: SIGPIPE-clean piping; `hunks show --format diff`; composable `path:index` hunk ids; `comments add` echoing `line_text`/fingerprints (caught my own anchoring error); handoff JSON's bidirectional `linked_task_ids`/`linked_comment_ids`; `--only-open` filtering works correctly.

# Scores

- **Discoverability: 3/5.** The top-level help, the handoff/export cross-referencing examples, and the spec examples in `briefs`/`chunks`/`drafts` help are genuinely good, and I completed the whole scenario without source-diving. But the session/comment/task/walkthrough subcommands — the heart of this scenario — have empty descriptions and undocumented flags, `--line` semantics are guesswork, and the `--state` collision means one advertised verb doesn't work at all. Matches the "learnable from help, but exact flags require lookups and retries" anchor.
- **Output quality for humans: 3/5.** `hunks show --format diff` and the markdown export/walkthrough are strong; everything else is JSON-only, so a human doing CLI review assembles context through `jq` and repeated `show` calls. Accurate and usable, but with manual assembly — a 3.
- **Output quality for agents: 4/5.** The handoff JSON artifact is close to the rubric's 5: one command gives target metadata, all action items with anchors, per-line fingerprints, excerpts, links, walkthrough, and reference hunks. Deductions: `followup`/`follow-up` round-trip failure, the markdown/JSON disagreement about what counts as an action item, and silent `session: null` artifacts.
- **Handoff readiness: 3/5.** The JSON payload bundles nearly everything an implementer needs (I could have fixed the retry bugs from `action_items` alone, without re-reading the diff). But the default markdown handoff buries two of four reviewer-flagged items, action items carry no explicit ordering/priority, the comment-state triage loop is broken (`set-state` panic), and wrongly anchored comments can't be cleaned up. The pieces are there; the default path still needs stitching and trust repairs.

# Top proposals

1. **Fix the `comments set-state` clap collision (rename the subcommand flag or scope arg ids) and add a regression test that actually invokes every mutating subcommand once.** A panicking verb in the review-state surface is the single worst trust signal for agent automation.
2. **Route all expected user errors (unknown comment/task/step ids) through the same clean one-line formatter `hunks show` uses.** No color-eyre backtrace for anything reachable by a typo'd id; keep backtraces for genuine bugs.
3. **Make markdown "Action items" match handoff JSON `action_items`:** include every unresolved comment (or at least every comment with an `action`), and make the "Action counts" header agree with the list below it. The document instructs agents to prioritize this section; it must be complete.
4. **Differentiate `handoff` markdown from `export markdown --profile agent`:** lead with an ordered, compact action plan (tasks first, linked comments inlined, excerpts only), and gate the full reference-hunks appendix behind a flag (e.g. `--with-reference`). Today the "prompt-ready" output is mostly raw diff.
5. **Add `comments edit`/`comments delete` (or `comments retarget --line`)** so anchoring mistakes are correctable, plus serialize enums exactly as the CLI accepts them (`follow-up`), so observed values round-trip.
6. **Fill in the empty subcommand/flag help strings** for `reviews`, `comments`, `tasks`, and `walkthrough`, including explicit `--line` semantics (new-file side, 1-indexed) — the anchor echo proves the data model is precise; the help should say so.
