# Annotation channels

Every annotation knows who wrote it and who it is for. Status: implemented in
roadmap milestone 17.

## Why

Gander annotations serve three different conversations, each with its own
lifecycle, consumer, and publication boundary:

1. an agent onboarding the reviewer to a change;
2. the reviewer directing feedback and questions at an agent;
3. humans collaborating with each other, including feedback destined for a
   forge review system someday.

Today the audience is implicit in storage buckets: agent drafts live in the
ephemeral overlay, `todo` state implies "for the agent", and everything else
is ambient. Making author and audience explicit gives each conversation the
right lifecycle and makes the privacy boundary structural instead of
conventional — without building three separate comment features.

## Model

Two fields on `Comment`, plus `author` on `CommentReply`:

```rust
pub struct Identity {
    pub kind: AuthorKind, // Human | Agent
    pub name: String,
}

pub enum Channel {
    Onboarding,    // agent → reviewer: narration, "look here", explanations
    Delegation,    // reviewer → agent: questions, todos, fix/explain/test
    Collaboration, // human ↔ human: review feedback, forge-bound someday
    Note,          // reviewer → self: private, never exported by default
}
```

Kind, state, action intent, anchors, and replies are unchanged and orthogonal.

| Channel       | Consumer                          | Lifecycle                        | Exit point                                    |
| ------------- | --------------------------------- | -------------------------------- | --------------------------------------------- |
| Onboarding    | the reviewer, in the TUI          | active → acknowledged / stale    | dissolves once understood; feeds attention map |
| Delegation    | agent harness via CLI/MCP         | draft → todo → resolved (+reply) | `gander comments list --channel delegation`    |
| Collaboration | teammate / future forge plugin    | draft → todo → resolved          | `gander export --profile team`                 |
| Note          | the reviewer only                 | freeform                         | never leaves the machine                       |

Threads do not mix channels. Today, composing a request from an onboarding
card creates a new delegation comment co-located on the same anchor and leaves
the onboarding comment unchanged. Exact durable source-comment linkage is
deferred: the current schema does not record which onboarding comment prompted
the delegation request. A post-M17 follow-up is tracked in the roadmap rather
than adding a premature linkage field here.

## Ergonomics

Channel selection must be zero-friction and impossible to get silently wrong.

**Gander infers the channel; the user overrides by exception.** Inference,
most-specific context first:

1. Replying in a thread → the thread's channel.
2. Composing on or inside an agent's onboarding card → `delegation`.
3. An agent is attached to the session (actual live harness contact or
   active-session agent-authored annotations) and the
   reviewed range is the reviewer's own → `delegation`. Merely configuring
   `[agent].name` is not attachment evidence.
4. Every non-empty change in the reviewed `base..rev` range has one consistent
   jj author that is not the configured identity (reviewing someone else's
   work) → `collaboration`.
5. Otherwise → `note`.

The fallback is deliberately the most private channel: a misfire leaks
nothing. Mixed authors, empty ranges, ambiguous author output, or missing
configured identity all fall back to `note`. Rule 4 compares the configured
`[identity]` name and optional email against the range's consistent jj author
name/email — trimmed and case-insensitively, per field — and treats the work
as the reviewer's own when either field matches, so benign drift like
"chris" vs "Chris Ericson" does not misread your own work as a teammate's.
When neither pair is comparable, inference never guesses and stays `note`.
`[comments] default-channel` pins
a fixed default for people who prefer no inference.

**One quiet indicator, which is also the control.** The comment editor's
border takes the channel color and the title carries a compact chip —
`╭ comment → agent ╮`, `→ team`, `→ note` — nothing else. One key cycles the
channel while composing; border and chip update live. No dialogs, no extra
prompt, no separate mode.

**One color language everywhere.** Editor border, inline card border, gutter
mark, and comment-list rows use the same four theme colors: onboarding =
accent, delegation = warning, collaboration = info, note = muted.

Inline presentation uses one annotation-card view model for durable comments
(including agent-authored drafts) and walkthrough narration. Cards project the
existing durable types; they do not add another persisted annotation schema.
Walkthrough steps carry an optional author identity: new TUI/CLI steps stamp the
configured human, MCP/agent-authored steps stamp the configured agent, and
legacy missing authors stay visibly neutral (`walkthrough`) rather than being
guessed as agent-authored.
Walkthrough artifacts expand in place with `E` while the cursor owns the card.
That expansion is ephemeral TUI state, so it does not require a CLI command or
change exported review state.

## Unification effects

- **Agent drafts stop being a separate type.** A draft is a comment with
  `author.kind = Agent` and `state = draft` awaiting triage; accepting flips
  state, with channel resolved by the same inference at accept time. The
  overlay draft bucket goes away.
- **Todo comments** become explicitly `channel = delegation` instead of
  "todo state implies agent-directed".
- **Replies carry authors**, so a delegation thread is a durable
  conversation: the reviewer asks, the agent answers with the resolution.
- **Publication is structural**: only `collaboration` comments in eligible
  states appear in team-facing exports. A future forge plugin cannot post
  delegation chatter or private notes by accident.

## Forge readiness (no forge code today)

The primitives a future GitHub/GitLab/SourceHut plugin needs, built now:

1. **Anchor round-tripping.** `CommentAnchor` already carries path, side,
   old/new line, hunk header, and fingerprints — a superset of forge review
   APIs. The artifact schema documents the mapping guarantee and includes
   fingerprints so a plugin can detect drift before posting.
2. **`--profile team` export**: JSON is the canonical forge-mappable contract:
   session metadata, target change ids, collaboration threads with authors and
   structured anchors/fingerprints, and an optional session-level disposition
   (`comment | approve | request-changes`). Team Markdown/HTML are filtered
   human summaries over the same public projection.
3. **Identity config**: `[identity] name` (plus optional `[identity] email`
   for ownership matching) for humans; agent identities from
   the agent config. Authorship is stamped now so exports are attributable
   later.
4. **Import as the reverse direction**: a teammate's exported collaboration
   comments land locally with their authorship intact. Two humans can review
   over an artifact file today, validating the model before any plugin
   exists.

## Migration

Serde-defaulted, one release of leniency, consistent with prior state
migrations: comments without a channel deserialize as `delegation` when
`state = todo`, else `note`; missing authors default to a local human
identity. Raw state deserialization uses the deterministic legacy identity
`human:local` because configuration is intentionally unavailable there; new
agent-authored annotations use `[agent].name`, falling back to `agent`. Pending
overlay drafts fold into onboarding-channel durable draft comments on first
load. Accepted and discarded overlay history is consumed without recreating
comments.
