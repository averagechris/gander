# Scenario 2: TUI and zen evaluation — round 4 recheck

- Binary: `/Users/chris/projects/gander/target/release/gander`
- Fixture: `/tmp/gander-eval/round4-s2` (jj stack on `main`: `chore: initial task queue library` → `feat: priority scheduling` → `refactor: extract retry policy` → `test: cover priority ordering and retry drops` → empty `@`; `main` bookmark sits on the chore change)
- Launch: `gander -b main -r @ tui` in tmux session `gander-r4-s2` (200x50)

# Summary

The core review loop is in good shape: navigation, viewed tracking, stack stepping, and commenting all worked first try, and the comment editor now shows its anchor (`comment · src/priority.rs:4`) plus `ctrl-s save · esc cancel` hints. Curated zen is the standout — chapter cards render agent briefs under "what this change does", stops carry titles/explanations/artifacts with change-scoped anchors, and the CLI curation path (`gander chunks` / `briefs` / `drafts`) is real parity and materially easier than raw JSON-RPC, with strict all-or-nothing validation and live TUI pickup ("agent suggestions updated" within a second).

The important gaps are all on the uncurated/derived side and in line-space plumbing:

1. **Zen and stack stepping ignore the session base.** With `-b main`, zen still tours the chore change that *is* `main` (chapter 1/4), and `<`/`>` count it in the stack. You are guided through code that is not under review.
2. **Uncurated "why this matters" facts are computed from the wrong diff.** Stops anchored to the chore change cite churn numbers and a `pub fn dropped()` "signature change" that belong to later changes; the chore chapter card says "no tests touched" while the change adds `tests/basic.rs`.
3. **`gander chunks lines` reports ranges the validator then rejects.** For the refactor's `worker.rs` it prints hunks `1–41` and `28–58` (old-side start mixed with new-side end, overlapping), yet `chunks update` rejects lines 42–44 that sit inside the reported 28–58 range. The one tool that exists to define the "valid line space" contradicts the validator.

# Step log

All tmux interaction via `nix shell nixpkgs#tmux --command tmux ...`, session `gander-r4-s2`, killed at the end.

## Setup and normal review

- `tmux new-session -d -s gander-r4-s2 -x 200 -y 50 -c /tmp/gander-eval/round4-s2 -- '<gander> -b main -r @ tui'` → files tree + diff pane, footer `7 files (0/7 viewed ...), +134/-12, 0 comments · following @` and a key legend line. Matches `main..@` (feat+refactor+test), verified against `jj diff --stat`.
- `?` → full help overlay. It leads with a **"first review loop"** section (`/` open, `]` next unviewed, `enter` mark viewed and advance, `c` comment, `T` zen, `ctrl-y` handoff) — genuinely task-oriented for a first-timer, followed by dense reference columns. Keys are contextual and occasionally collide across modes (`e` = move in tree / edit comment / open artifacts; `p` = zen back / compare `@-..@`; `a` = mark all viewed / acknowledge glance), which the columns disambiguate but a newcomer must read carefully.
- File navigation with `n`, diff follows selection. `>` at `@` → `info: already at the top of the stack`. `<` → `info: stack 3/4: test: cover priority ordering and retry drops`, file pane narrows to that change's 2 files (+30/−1). `<` again → `stack 3/5: refactor: extract retry policy from worker` (note the denominator jumped 4→5 and index 3 repeated), `<` → `stack 2/5: feat: priority scheduling for the job queue`.
- `Enter` on `lib.rs` in the feat scope → `✓`, advances to `priority.rs`. `Tab` to diff, cursor to line 4, `c` → editor popup titled `comment · src/priority.rs:4` with footer `ctrl-s save · esc cancel`. Typed a comment about derived `Ord` on the enum, `C-s` → rendered inline as `↳ 9abb3c09 [draft] Derive Ord on a c-style enum couples priority order...`, footer `1 comment`. (Why is a human-authored comment labeled `[draft]`? No visible way to "finalize" it.)
- `t` → back to launch target.

## Uncurated zen stop log

`T` → `zen: touring 5 file(s)`, header `zen · chapter 1/4 · then 2 at a glance`.

| Stop | Anchor | Added information beyond browsing? |
| --- | --- | --- |
| Chapter 1/4 card | `change otzqkssqtoyx · main` — "chore: initial task queue library", `6 file(s) · +112 −0` | **Misleading.** This change is `main` itself, i.e. the review base. Card facts contradict themselves: header `+112 −0` but `stops: churn: +54 −11 · no tests touched` (the change adds `tests/basic.rs` +11) and lists `fn enqueue_with_priority` — a symbol introduced by the *feat* change. Honest line: "No agent brief for this change — the facts above are derived from the diff." |
| 1/5 `worker.rs:1-41` [chore] | pure-add excerpt | Excerpt fine; facts wrong diff: "largest hunk +25 −5" (the file is +35 −0 here) and "public API change: `pub fn dropped(&self) -> u64` signature changed" — `dropped()` doesn't exist until the refactor change. Review question is boilerplate. |
| 2/5 `queue.rs:24-58` [chore] | pure-add excerpt | Same wrong-diff mixing: claims `fn enqueue_with_priority` and "+18 −4" in a chore-scoped all-add view. |
| Chapter 2/4 card [feat] | commit message + body | **Useful** — the commit body ("Jobs now carry a Priority... FIFO within a band") is real intent. Hard-wrapped mid-sentence ("keeps a / band"). |
| 3/5 `priority.rs:1-25` [feat] | new enum | Churn correct (+25 −0); but "pub enum Priority **signature changed**" for a brand-new enum, plus the same canned "do callers handle the new signature?" |
| Chapter 3/4 card [refactor] | commit body | Useful intent, same wrapping. |
| 4/5 `retry.rs:1-22` [refactor] | new struct | Correct churn; "pub struct RetryPolicy signature changed" again wrong phrasing for an addition. |
| Chapter 4/4 card [test] | commit body | Useful; but the chapter's single stop is `src/config.rs` (a helper) — `tests/basic.rs` (+19 −1, the substance of a test-titled change) is demoted to glance. |
| 5/5 `config.rs:6-22` [test] | `for_tests()` | "pub fn for_tests() -> Self signature changed" — new fn. |
| Glance board | `src/lib.rs — exports only`, `tests/basic.rs +19 -1 — tests` | Reasonable, except tests being glance for a test change. |

Verdict on the recent changes: **verified** — uncurated zen does build one chapter per stack change with the commit message (title + body) as intent, and the honest "No agent brief... derived from the diff / press @ to summon" line is exactly the right tone. But the derived facts under stops/cards are frequently computed from the wrong diff, which makes them worse than saying nothing.

`Esc` → `zen ended`, session back at 7 files.

## Curation (ACP + CLI)

ACP via `gander -b main -r @ acp` from the fixture (bridged to the live TUI: `"live_session":true`).

- `initialize`, `review/summary`, `review/stack_changes` — stack includes the chore change (bookmarked `main`) as a reviewable entry, confirming the base-vs-trunk() issue also exists at the protocol level.
- `review/set_change_briefs` with briefs for feat/refactor/test → `{"briefs":3,"warnings":[...]}` — three advisory `brief for change X has no spotlight chunk yet...` warnings. **Verified**: warnings fire, are advisory, and the write still lands.
- `gander chunks --help` — good: shows an inline JSON example, all subcommands (`list/lines/set/update/remove/clear`), stdin convention. Discoverable without reading docs.
- `gander -b main -r @ chunks lines --change vpwnnvwnwrtt` — hunk ranges with `first_line`/`last_line` excerpts; genuinely useful for authoring. **Verified the new subcommand works.** But:
  - Without `-b main` it silently uses `trunk()` and reports an 8-file line space (includes `Cargo.toml`) different from the live session's 7 files. No warning, despite a live registered TUI instance on the workspace.
  - For `--change osxwxnurqoox --path src/worker.rs` it reports hunks `(1,41)` and `(28,58)` — overlapping, because hunk 2's header is `@@ -25,11 +45,14 @@` (start 28 looks old-side-derived, end 58 new-side).
- Probe: `chunks set` with `start_line:200` and a nonexistent file → single clear all-or-nothing error naming each chunk/part and reason; exit 1. Good. (It doesn't tell you the valid ranges or mention `chunks lines`.)
- Probe: `chunks update` with `worker.rs:42-44` (inside the reported 28–58 space, between real hunks) → **rejected**: "line range outside diff line space". `chunks lines` and the validator disagree.
- `chunks set --file chunks.json` → `Set 4 chunks` (3 spotlight anchored to feat/refactor/test + 1 glance, with rationale/explanation/one artifact).
- Incremental: `chunks update` (retitled the glance chunk + added a probe) → `Updated 1 chunks, added 1; total 5`; `chunks remove --id probe-...` → `Removed 1; remaining 4`; `chunks remove --id nope-not-real` → strict error, exit 1. All as documented.
- ACP `review/draft_comment` (`worker.rs:50`, retry-starvation note) → id returned; `review/overlay` shows 4 chunks, 3 briefs, 1 pending draft. `gander briefs list` / `gander drafts list` mirror it — **CLI parity verified**.
- TUI footer showed `info: agent suggestions updated` unprompted. `D` → agent drafts panel with `[pending ] src/worker.rs:50 ...` and `enter/a accept · e edit then accept · x discard`; accepted → `accepted agent draft as comment`, 2 comments.

## Curated zen and comparison

`T` → `zen: 3 chapter(s), 5 focus stop(s), 3 at a glance`. The chore chapter is gone (nothing curated referenced it), which accidentally fixes the base problem — but only because curation skipped it.

- Chapter cards: commit message + body, file/churn stats, then **"what this change does"** with my brief verbatim. Night-and-day versus "No agent brief".
- Stops: chunk title + `part 1/2` + change-scoped anchor (`[vpwnnvwnwrtt] src/queue.rs:1-20`), my explanation under "why this matters", `e 1 artifact(s)` in the footer. The artifact card (`dequeue order · example`) opens inline and scrolls; underlying diff text fragments bleed around its border.
- Multi-part chunks repeat the identical explanation on every part (read the same paragraph on stops 3/5 and 4/5).
- Glance board mixes curated glance chunks (with rationale shown) and uncovered files: `Priority enum (Ord = declaration order) ... — Derived Ord couples semantics...`, `src/config.rs`, `src/lib.rs`. `a` → `zen complete — 3 glance item(s) marked viewed`, 6/7 viewed.

Uncurated vs curated: uncurated gives structure (chapters, commit intent, pacing) but its derived facts range from boilerplate to wrong; curated zen is a genuinely good guided briefing — accurate anchors, real review questions, artifacts, and a glance lane that respects what was already spotlighted.

`q` to quit the TUI, then `tmux kill-session -t gander-r4-s2` (verified gone).

# Findings

1. **major — zen/stack stepping ignore the session base (`-b main`)**
   Repro: fixture has `main` on the chore change; launch `gander -b main -r @ tui`, press `T`. Chapter 1/4 is `change otzqkssqtoyx · main` — the base itself, 6 files/+112 the reviewer already owns as merged context. `<`/`>` similarly report `stack 2/5`-style positions counting the base change (and ACP `review/stack_changes` returns it). Zen should tour `base..@`, or at minimum mark base-side chapters as out of review scope.

2. **major — uncurated stop/chapter facts computed from the wrong diff**
   Repro: same launch, `T`, look at chapter 1 card and stops 1–2. Chapter header says `+112 −0` but `stops: churn: +54 −11 · no tests touched` (the change adds `tests/basic.rs`); stop 1 shows chore's `worker.rs:1-41` pure-add excerpt while claiming `largest hunk +25 −5` and `pub fn dropped(&self) -> u64 signature changed` — `dropped()` is introduced by the refactor change two chapters later. Facts appear derived from the session diff while the excerpt is change-scoped. Actively misleading in the exact mode meant to build trust.

3. **major — `gander chunks lines` reports ranges the validator rejects (old/new side mixing)**
   Repro: `gander -b main -r @ chunks lines --change osxwxnurqoox --path src/worker.rs` → hunks `start_line:1,end_line:41` and `start_line:28,end_line:58` (overlapping; header `@@ -25,11 +45,14 @@`, so 28 is old-side, 58 new-side). Then `chunks update` a part `src/worker.rs 42-44` (inside the reported 28–58): rejected with "line range outside diff line space". The authoring aid and the validator disagree about the one thing the aid exists to define.

4. **minor — uncurated stop selection demotes the substance of a test change**
   Repro: uncurated zen, chapter 4/4 (`test: cover priority ordering and retry drops`): its only spotlight stop is `src/config.rs:6-22` (a test-config helper) while `tests/basic.rs` (+19 −1, the actual coverage) is glance-only with the blanket `— tests` role. For a test-titled change the tests are the review target.

5. **minor — formulaic and wrong "public API change" phrasing for additions**
   Repro: every uncurated stop with a new pub symbol says "`pub enum Priority` signature changed / `pub struct RetryPolicy` signature changed / `pub fn for_tests() -> Self` signature changed · review question: do callers handle the new signature?" — nothing changed, they're new, and there are no callers. Also truncation: "pub fn enqueue_with_priority( signature changed". Boilerplate on 5/5 stops trains the reviewer to skip the section.

6. **minor — stack position indicator inconsistent while stepping**
   Repro: from `@`, press `<` → `stack 3/4: test: ...`; press `<` again → `stack 3/5: refactor: ...` (denominator 4→5, index 3 repeated), then `stack 2/5: feat`. The first step under-counts and mislabels the position.

7. **minor — chunks/briefs CLI silently uses `trunk()` when `-b` is omitted, diverging from the live session**
   Repro: with the TUI live on `main..@`, run `gander chunks lines` (no flags) → 8-file line space including `Cargo.toml` (trunk()..@) vs the session's 7 files. No warning that a live instance on the same workspace has a different target; an agent authoring session-scoped chunks from that output writes anchors against the wrong diff.

8. **papercut — validation errors don't include valid ranges**
   Repro: the `start_line:200` rejection says "line range outside diff line space for src/queue.rs" but not what the space is, nor "see `gander chunks lines`". One sentence would close the loop.

9. **papercut — multi-part chunks repeat the identical explanation per part**
   Repro: curated zen stops 3/5 and 4/5 (`RetryPolicy extraction`, parts 1/2 and 2/2) show the same full "why this matters" paragraph twice.

10. **papercut — hard-wrapped commit bodies/briefs render ragged on chapter cards**
    Repro: chapter 2/4 card shows "The queue keeps a / band per priority" — source line breaks preserved instead of reflowed to card width.

11. **papercut — human-authored comment displays as `[draft]` with no visible path to non-draft**
    Repro: `c`, type, `ctrl-s` → inline `↳ 9abb3c09 [draft] ...`. If all comments are drafts until export, the label is noise; if not, the promotion action is undiscoverable.

12. **papercut — artifact overlay bleeds underlying diff text at its border**
    Repro: curated zen stop 1/5, press `e` — fragments of the covered focus card ("thin") remain visible along the artifact card's right edge.

# Scores

- **TUI review ergonomics: 4/5.** Navigation, viewed-and-advance, change-scoped stack stepping, anchored comment editor with save/cancel hints, and the agent-draft triage panel all worked first try; help opens with a task-oriented loop. Held back by the stack indicator glitch (finding 6), the `[draft]` label confusion (11), and heavy context-dependent key overloading that demands the help screen.
- **Zen uncurated: 2/5.** The structure is right — one chapter per change, commit-message intent, honest "no agent brief" framing, pacing, glance lane — and that's better than raw browsing. But the derived facts are wrong often enough to mislead (findings 1, 2, 5), and stop selection can skip the substance of a change (4). A tour that confidently states wrong churn numbers and phantom signature changes scores below "calm but limited".
- **Zen curated: 4/5.** With briefs + chunks + a draft, zen becomes a genuinely useful guided briefing: accurate change-scoped anchors, per-stop teaching text, artifacts on demand, curated glance items with rationale, and correct exclusion of uncovered-but-spotlighted files. Repetition across parts (9), ragged wrapping (10), and artifact-border bleed (12) keep it from 5.
- **Curation protocol ergonomics: 3.5/5.** The CLI path is discoverable (`chunks --help` with inline example), materially easier than raw JSON-RPC, supports incremental `update`/`remove` with strict all-or-nothing validation and clear per-part errors, brief warnings are advisory as documented, and writes surface in the live TUI within a second. Docked for the `chunks lines` old/new-side contradiction (3 — the flagship authoring aid can't be trusted at face value), the silent `trunk()` default divergence (7), and errors that don't state the valid space (8).

# Top proposals

1. **Make zen and stack navigation respect the session base.** Chapters and `review/stack_changes` should cover `base..@` (or explicitly flag base-side changes as context, not review targets). This is the single biggest trust issue in uncurated zen.
2. **Fix derived-fact provenance in uncurated stops.** Compute churn, symbols, and API deltas from the same change-scoped diff the stop displays; never blend session-diff facts into a chore/feat chapter. Drop or soften the "signature changed" claim for newly added symbols ("new public API: `pub enum Priority`" + "review question: is this the right surface?").
3. **Unify the line-space model between `chunks lines` and the validator.** Report new-side ranges (or explicit `{side, start, end}`), never mix old-side starts with new-side ends, and include the valid ranges (or a pointer to `chunks lines`) in rejection messages.
4. **Warn on target divergence in the curation CLIs.** When a live instance is registered for the workspace with a different base/rev than the CLI invocation resolves to, print one advisory line — this turns a silent wrong-line-space footgun into a non-event.
5. **Prioritize test files as spotlights in test-titled changes** (or generally, weight stop selection by the commit type/intent), and de-duplicate multi-part chunk explanations (full text on part 1, "continued" on later parts).
