# Summary

Dogfooding `gander -b main -r @ tui` on the fixture shows that the core review TUI is usable for normal file/diff review, but zen mode only becomes meaningfully useful after an agent supplies explicit narrative and spotlight chunks. The uncurated tour mostly re-packages normal file browsing as full-screen cards. Curated zen was much closer to a guided review: the chapter card explained the risky retry change, the first spotlight landed directly on the off-by-one, and the next stop explained the larger retry-identity problem.

The major UX gap is that zen's fallback lacks intent, risk, dependency relationships, and review questions. It answers “what file is next?” rather than “why should I care about these lines?” The ACP curation path can provide that missing layer, but it is awkward and easy to get subtly wrong, especially with stack change IDs and per-change line numbering.

# Keybinding discoverability notes

- Pressing `?` did open a substantial grouped help overlay. It was sufficient to discover the basics: focus toggle, file navigation, comments, stack stepping, and zen.
- The overlay reflected my configured/active movement keys as `n/e`, not the README's `j/k` defaults. That was good for accuracy but surprising because many footer hints also emphasized `n/e`; a first-time user may not know whether this is project default or personal config.
- Help showed `>/< step through stack`, which was enough to find the feature. In practice I initially pressed `>` from `main..@` and got `info: already at the top of the stack`; `<` moved me to an earlier stack change. The mental model of “top” vs “next” is not obvious.
- The comment workflow was discoverable enough (`c`, type, `ctrl-s`) but the save key is easy to miss because the editor hint is not visible until the mode is active.
- Zen's footer was helpful: `n next (marks viewed)`, `p back`, `tab full diff`, `g glance`, `c comment`, `esc end`. The “marks viewed” side effect is important and well surfaced.

Helpful captured excerpt:

```text
general                              comments
 tab  switch focus between files...        c  comment at cursor
 ...
 targets & jj
 t  compare trunk()..@                    >/<  step through stack
 agent
 S  agent review chunks                    T  zen briefing (focus stops + glance)
```

# Plain-review findings

## Normal review flow

- The split file tree + diff pane worked well at 200x50. I could navigate to `src/retry.rs`, switch focus, move to a line, and add a line-anchored draft comment.
- Marking a file viewed with Enter immediately updated the tree and advanced attention. This is fast once learned.
- Stack stepping works, but `>`/`<` direction was not self-explanatory from `main..@`; `>` said “already at the top of the stack”, while `<` moved to `stack 4/5`.
- The code issue was discoverable in normal review: `RetryPolicy::should_retry` uses `attempts <= self.max_retries`, allowing one extra retry. A second, more serious issue appears in `Worker`: failed jobs are re-enqueued through `queue.enqueue_with_priority`, which assigns a fresh job id while attempts are keyed by `job.id`; an always-failing job can reset its attempt count indefinitely.

Comment added in TUI:

```text
↳ [draft] This should be '< max_retries'; as written it permits one extra retry.
Also worker re-enqueues with a new id, so attempts keyed by id can reset forever
for always-failing jobs.
```

## Friction

- I wanted a quick “current target” explanation while stack stepping: am I reviewing the whole stack, one change against parent, or trunk? The footer gives `main..@`, but after pressing stack keys the semantic transition needs more explanation.
- Help is dense. It is complete, but not task-oriented: “first review loop”, “stack review loop”, and “agent/zen loop” would be easier to learn than a long key map.
- The file tree sorted viewed files down, which is useful, but after prior actions persisted state made a fresh restart look partially reviewed. Accurate, but for evaluation it felt like hidden state; a visible state path/reset hint would help.

# Zen-without-agent stop-by-stop log + verdict

I entered zen without setting agent chunks. It produced a chapter card and one stop per file.

Opening card excerpt:

```text
zen · chapter 1/1
chapter 1/1 · trunk()..@
(no description)
8 file(s) · +234 −0
n tours the 8 stop(s) in this chapter
what this change does
(no change brief from the agent — @ summons one to tell this change's story)
```

Stop-by-stop:

1. `src/config.rs`: showed the file excerpt from the top of the file. It did not explain that `for_tests()` was related to later test coverage. No extra value beyond browsing.
2. `src/lib.rs`: showed module exports. No extra value; a normal file tree/diff gives the same information with more context.
3. `src/priority.rs`: showed the new enum. Mild value as a “new concept” card, but no risk or API implications.
4. `src/queue.rs`: showed the top of the queue change. It did not call out the important BTreeMap ascending/rev behavior or FIFO within bands.
5. `src/retry.rs`: showed the file, but did not call attention to the off-by-one line. This is the clearest example of the maintainer complaint: the tour displayed the relevant file but did not present the useful information.
6. `src/worker.rs`: showed the start of the worker change. It did not connect `attempts.entry(job.id)` to `enqueue_with_priority` assigning a new id.
7. `tests/basic.rs`: showed the tests, but not what they missed.
8. `Cargo.toml`: shown as another file stop. This felt like bookkeeping, not review guidance.

Verdict: uncurated zen is a pleasant full-screen file slideshow, not a useful review briefing. It removes visual clutter, but it also removes the file tree's comparative context and does not add intent, risk, or reviewer tasks. It did not tell me anything I would not get from normal file browsing except progress dots and a “mark viewed as you go” flow.

# ACP curation friction log

- The docs are clear that JSON-RPC is line-delimited and that `gander acp` bridges to the live TUI. That part worked.
- The base/revision behavior is easy to trip over. When the live TUI had been retargeted with `t`, `gander -b main -r @ acp` still reported the live session target (`trunk()..@`) because the bridge took precedence. This is documented in spirit, but surprising when command flags appear to be ignored.
- `review/stack_changes` included the `main` base change and an empty current change in addition to the three meaningful stack changes. As a curator I had to infer which change IDs to use.
- `review/set_chunks` replaces all chunks. This is simple but unforgiving; incremental updates would be easier while iterating.
- Line numbers for chunk parts anchored to `change_id` must come from `review/change_diff`, not the main diff. The protocol says this, but it is a high-friction requirement in a manual JSON-RPC flow.
- I attempted a multi-part chunk spanning `src/worker.rs` and `src/queue.rs` under the retry-policy change. Because `src/queue.rs` was not part of that change's diff, the rendered second part showed the wrong excerpt (`src/worker.rs` lines) while the title referenced `src/queue.rs`. This is a major footgun: invalid parts should be rejected or visibly flagged.
- Hand-writing JSON strings for multi-sentence explanations is cumbersome. A CLI helper accepting YAML/TOML/Markdown would be much more humane.
- Drafting a comment via ACP worked and surfaced as a draft, but line anchoring again requires knowing which target's line space is active.

ACP commands used conceptually:

```text
review/set_change_briefs: summaries for lzuvzsxknltv, wntwqruxxumq, xtmnmtplwwrz
review/set_chunks: 3 spotlight chunks + 1 glance chunk
review/draft_comment: src/worker.rs:34 retry id reset issue
```

# Zen-with-agent comparison

Curated zen was substantially better.

Chapter card excerpt:

```text
chapter 1/2 · change wntwqruxxumq
refactor: extract retry policy from worker
3 file(s) · +52 −6
what this change does
Extracts retry decisions into RetryPolicy and teaches Worker to re-enqueue failed jobs.
This is the risky behavior change: retry counting and job identity determine whether
failures eventually drop or loop.
```

Improvements:

- The tour started at the risky retry-policy chapter instead of walking boilerplate first.
- Stop 1 was genuinely useful: it centered lines 16-20 and explained the off-by-one.
- Stop 2 explained the non-obvious cross-line bug: attempts keyed by old id but retry uses a new queued job.
- The chapter split made the stack more understandable: retry refactor first, tests second.
- The glance board separated mechanical priority-queue code and uncovered files from spotlight review.

Remaining awkwardness:

- The invalid chunk part was not rejected; it produced a misleading stop where the title said `src/queue.rs` but the excerpt was still worker code.
- Long titles/paths were truncated on cards, e.g. `tests/b...`, reducing confidence in exact location.
- The glance board had duplicate entries for one glance chunk with two parts. That may be technically correct, but conceptually I expected one “Priority queue mechanics” item that could expand to locations.
- Glance items for uncovered files (`src/config.rs`, `src/lib.rs`) lacked rationale, so they still felt empty.
- Pacing is close, but multi-part spotlight chunks currently become separate numbered stops. For a single conceptual issue spanning two locations, I wanted a card that explicitly says “part 1/2” and “part 2/2” with a cross-reference diagram or combined explanation.

Curated stop excerpt:

```text
Retry limit is off by one · [wntwqruxxumq] src/retry.rs:16-20
16 + pub fn should_retry(&self, attempts: u32) -> bool {
...
20 +     attempts <= self.max_retries
why this matters
The comment says attempts already performed should be compared to the retry budget,
but the implementation uses <=. With max_retries=3, attempt count 3 still returns true...
```

Glance excerpt:

```text
4 spotlight stop(s) toured. 4 remaining item(s) below — skim, then `a` marks them all viewed.
› • Priority queue mechanics [lzuvzsxknltv] src/priority.rs:1-25 — Mechanical BTreeMap banding...
  • Priority queue mechanics [lzuvzsxknltv] src/queue.rs:1-58 — Mechanical BTreeMap banding...
  • src/config.rs src/config.rs +11 -0
  • src/lib.rs src/lib.rs
```

# Findings list

## Major: uncurated zen does not present review insight

Concrete repro: start fixture TUI with `-b main -r @`, press `T` before any ACP chunks, press `n` through stops. Observe one card per file with no risk, intent, or suggested questions. `src/retry.rs` displays the file but does not spotlight `attempts <= self.max_retries`.

## Major: ACP accepts or mis-renders invalid change-anchored chunk parts

Concrete repro: call `review/set_chunks` with a chunk anchored to `wntwqruxxumq` and include a part for `src/queue.rs`, which is not in that change diff. Enter zen and step to the second part. The card title references `src/queue.rs`, but the excerpt shown is from the worker diff. Expected: reject the chunk part with a JSON-RPC error, or render an explicit “part not in change diff” error.

## Major: live ACP bridge can make explicit `-b/-r` flags appear ignored

Concrete repro: retarget the running TUI to `trunk()..@` with `t`; run `gander -b main -r @ acp` in the same workspace; `review/summary`/`review/stack_changes` reflects the live TUI target. This is powerful but surprising. The ACP response should include a clear `live_session: true` and active target notice, or the CLI should warn on stderr.

## Minor: stack stepping direction is hard to reason about

Concrete repro: on `main..@`, press `>`; footer says already at top. Press `<`; it moves to an earlier stack change. The labels “next/previous” and “top” need a visible stack position model.

## Minor: glance board duplicates multi-part glance chunks

Concrete repro: set one glance chunk with two parts (`src/priority.rs`, `src/queue.rs`). The glance board shows two lines with the same title/rationale. This is skim-friendly by location but weak as a conceptual checklist.

## Papercut: help is complete but not task-oriented

Concrete repro: press `?`. The grouped keymap is dense and accurate, but it does not tell a new user the recommended review loop: choose target, navigate files, switch to diff, comment, mark viewed, step stack, optionally zen.

# Scores

- TUI review ergonomics: **4/5**. File navigation, diff focus, comments, viewed state, and stack stepping are all present and mostly fast. Main deductions: stack target mental model and dense help.
- Zen usefulness uncurated: **2/5**. It is visually calm and can mark files viewed, but it does not add insight beyond normal browsing.
- Zen usefulness curated: **4/5**. With change briefs and spotlight explanations, it became a real briefing and directly improved review quality. Deductions: invalid chunk footgun, truncation, and glance-board rough edges.
- Curation-protocol ergonomics: **2/5**. The protocol is capable and documented, but manual line-delimited JSON-RPC with change-specific line spaces is brittle. It needs validation and friendlier authoring tools.

# Top 3 concrete proposals to make the zen tour present genuinely useful information

1. **Make every zen stop answer “why this matters” even without an agent.** Use heuristics to label files/stops: new public API, tests, retry/error handling, queue ordering, config, exports. For fallback stops, show generated prompts like “Review question: does this condition enforce the documented retry budget?” or “Risk: this re-enqueues failed work; verify identity and termination.”

2. **Validate and preview agent chunks before zen.** `review/set_chunks` should reject parts that are not in the anchored change diff, out-of-range lines, or files absent from that target. Add an `review/validate_chunks` or return per-chunk warnings. In the TUI, `S` should show invalid chunks before the user starts zen.

3. **Turn zen into a stack narrative, not a file slideshow.** Chapter cards should summarize dependencies between changes, changed public APIs, tests added/missing, and known reviewer tasks. The glance board should group by concept with expandable locations and require a rationale for uncovered files (“boilerplate export”, “test helper”, “mechanical priority enum”) so the reviewer can confidently bulk-mark them viewed.
