use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    diff::{DiffLineKind, DiffSet, FileDiff},
    ids::shortest_unique_prefix,
    state::{
        ActionIntent, ActionItem, ActionItemStatus, Comment, CommentReply, CommentState,
        ExternalTicket, ReviewSession, ReviewState, ReviewTarget, Walkthrough, WalkthroughStep,
    },
};

pub const DELEGATION_SCHEMA_VERSION: u8 = 4;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegationSpec {
    pub recipient: Option<String>,
    pub objective: String,
    pub repeated_constraints: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub requested_verification: Vec<String>,
    pub action_item_selectors: Vec<String>,
    pub comment_selectors: Vec<String>,
    pub hunk_context_lines: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegationPacket {
    pub schema_version: u8,
    pub kind: DelegationPacketKind,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub session: SessionMeta,
    pub target: ReviewTarget,
    pub source: SourceMeta,
    pub fingerprints: PacketFingerprints,
    pub brief: DelegationBrief,
    pub action_items: Vec<DelegatedActionItem>,
    pub walkthrough: Vec<DelegatedWalkthroughStep>,
    pub reference_hunks: Vec<ReferenceHunk>,
    pub return_contract: ReviewStateReturnContract,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DelegationPacketKind {
    GanderDelegation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub selector: String,
    pub title: Option<String>,
    pub status: String,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceMeta {
    pub diff_file_count: usize,
    pub action_item_count: usize,
    pub comment_count: usize,
    pub walkthrough_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PacketFingerprints {
    pub diff: String,
    pub files: Vec<FileFingerprint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileFingerprint {
    pub path: String,
    pub old_path: Option<String>,
    pub status: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegationBrief {
    pub recipient: Option<String>,
    pub objective: String,
    pub repeated_constraints: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub requested_verification: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegatedActionItem {
    pub id: String,
    pub selector: String,
    pub source: ActionSource,
    pub title: String,
    pub body: Option<String>,
    pub action: Option<ActionIntent>,
    pub target: Option<ReviewTarget>,
    pub comment_ids: Vec<String>,
    pub external_tickets: Vec<ExternalTicket>,
    pub evidence_comments: Vec<CommentEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionSource {
    ActionItem,
    Comment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentEvidence {
    pub id: String,
    pub selector: String,
    pub path: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub body: String,
    pub action: Option<ActionIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<crate::provenance::CommentObservation>,
    pub replies: Vec<CommentReply>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegatedWalkthroughStep {
    pub walkthrough_id: String,
    pub walkthrough_selector: String,
    pub step_id: String,
    pub step_selector: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub why: Option<String>,
    pub target: ReviewTarget,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReferenceHunk {
    pub path: String,
    pub hunk_header: String,
    pub fingerprint: String,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewStateReturnContract {
    pub summary_required: bool,
    pub allowed_action_item_statuses: Vec<String>,
    pub allowed_closed_dispositions: Vec<String>,
    pub allowed_comment_states: Vec<String>,
    pub commands: Vec<ReturnCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReturnCommand {
    pub purpose: String,
    pub command: String,
}

pub fn build_delegation_packet(
    state: &ReviewState,
    session: &ReviewSession,
    diff: &DiffSet,
    spec: &DelegationSpec,
) -> Result<DelegationPacket> {
    let session_comments = state
        .comments
        .iter()
        .filter(|comment| comment.belongs_to_session(&session.id))
        .collect::<Vec<_>>();
    let selected = with_action_selectors(select_action_items(session, &session_comments, spec)?);
    let return_contract = return_contract(&session.target, &selected);
    let paths = selected
        .iter()
        .filter_map(|item| item.target.as_ref()?.file.clone())
        .chain(selected.iter().flat_map(|item| {
            item.evidence_comments
                .iter()
                .filter_map(|comment| comment.path.clone())
        }))
        .collect::<BTreeSet<_>>();
    Ok(DelegationPacket {
        schema_version: DELEGATION_SCHEMA_VERSION,
        kind: DelegationPacketKind::GanderDelegation,
        generated_at: chrono::Utc::now(),
        session: SessionMeta {
            id: session.id.clone(),
            selector: shortest_unique_prefix(
                &session.id,
                &state
                    .sessions
                    .iter()
                    .map(|session| session.id.as_str())
                    .collect::<Vec<_>>(),
            ),
            title: session.title.clone(),
            status: format!("{:?}", session.status).to_lowercase(),
            created_at: session.created_at,
            updated_at: session.updated_at,
        },
        target: session.target.clone(),
        source: SourceMeta {
            diff_file_count: diff.files.len(),
            action_item_count: session.action_items.len(),
            comment_count: session_comments.len(),
            walkthrough_count: session.walkthroughs.len(),
        },
        fingerprints: fingerprints(diff),
        brief: DelegationBrief {
            recipient: spec.recipient.clone(),
            objective: spec.objective.clone(),
            repeated_constraints: spec.repeated_constraints.clone(),
            acceptance_criteria: spec.acceptance_criteria.clone(),
            requested_verification: spec.requested_verification.clone(),
        },
        action_items: selected,
        walkthrough: walkthrough_context(&session.walkthroughs),
        reference_hunks: reference_hunks(diff, &paths, spec.hunk_context_lines),
        return_contract,
    })
}

fn select_action_items(
    session: &ReviewSession,
    comments: &[&Comment],
    spec: &DelegationSpec,
) -> Result<Vec<DelegatedActionItem>> {
    let comments_by_id = comments
        .iter()
        .filter(|comment| comment.state == CommentState::Todo)
        .map(|comment| (comment.id.as_str(), *comment))
        .collect::<BTreeMap<_, _>>();
    let explicit = !spec.action_item_selectors.is_empty() || !spec.comment_selectors.is_empty();
    let selected_items =
        resolve_action_items(&session.action_items, &spec.action_item_selectors, explicit)?;
    let selected_ids = selected_items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();

    // The shared query owns the implicit folding policy. Explicit selection
    // narrows that query without changing how linked todo evidence is nested.
    let open_work = crate::review::open_work(
        session,
        &comments
            .iter()
            .map(|comment| (*comment).clone())
            .collect::<Vec<_>>(),
    );
    let mut items = open_work
        .action_items
        .iter()
        .filter(|entry| selected_ids.contains(entry.item.id.as_str()))
        .map(|entry| {
            action_item_entry(
                &entry.item,
                entry
                    .linked_todo_comments
                    .iter()
                    .map(comment_evidence)
                    .collect(),
            )
        })
        .collect::<Vec<_>>();

    let selected_linked_comments = selected_items
        .iter()
        .flat_map(|item| item.comment_ids.iter())
        .filter(|id| comments_by_id.contains_key(id.as_str()))
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let selected_comments = resolve_comments(comments, &spec.comment_selectors, explicit)?;
    if explicit {
        for comment in selected_comments {
            if !selected_linked_comments.contains(comment.id.as_str()) {
                items.push(comment_entry(comment));
            }
        }
    } else {
        items.extend(open_work.remaining_todo_comments.iter().map(comment_entry));
    }
    items.sort_by_key(item_key);
    if items.is_empty() {
        return Err(eyre!("delegation selection is empty"));
    }
    Ok(items)
}

fn resolve_action_items<'a>(
    items: &'a [ActionItem],
    selectors: &[String],
    explicit: bool,
) -> Result<Vec<&'a ActionItem>> {
    if !selectors.is_empty() {
        selectors
            .iter()
            .map(|selector| {
                let item = one(
                    items
                        .iter()
                        .filter(|item| item.id.starts_with(selector))
                        .collect(),
                    "action item",
                    selector,
                )?;
                if item.status != ActionItemStatus::Open {
                    return Err(eyre!("action item `{}` is not open", item.id));
                }
                Ok(item)
            })
            .collect()
    } else if explicit {
        Ok(Vec::new())
    } else {
        Ok(items
            .iter()
            .filter(|item| item.status == ActionItemStatus::Open)
            .collect())
    }
}

fn resolve_comments<'a>(
    comments: &[&'a Comment],
    selectors: &[String],
    explicit: bool,
) -> Result<Vec<&'a Comment>> {
    if !selectors.is_empty() {
        selectors
            .iter()
            .map(|selector| {
                let comment = one(
                    comments
                        .iter()
                        .copied()
                        .filter(|comment| comment.id.starts_with(selector))
                        .collect(),
                    "comment",
                    selector,
                )?;
                if comment.state != CommentState::Todo {
                    return Err(match comment.state {
                        CommentState::Draft => eyre!(
                            "comment `{}` is still draft; run `gander comments ready {}` before delegating it",
                            comment.id,
                            selector
                        ),
                        CommentState::Resolved => {
                            eyre!("comment `{}` is already resolved", comment.id)
                        }
                        CommentState::Todo => unreachable!(),
                    });
                }
                Ok(comment)
            })
            .collect()
    } else if explicit {
        Ok(Vec::new())
    } else {
        Ok(comments
            .iter()
            .copied()
            .filter(|comment| comment.state == CommentState::Todo)
            .collect())
    }
}

fn one<'a, T>(matches: Vec<&'a T>, kind: &str, selector: &str) -> Result<&'a T> {
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(eyre!("unknown {kind} selector `{selector}`")),
        _ => Err(eyre!("ambiguous {kind} selector `{selector}`")),
    }
}

fn action_item_entry(
    item: &ActionItem,
    evidence_comments: Vec<CommentEvidence>,
) -> DelegatedActionItem {
    DelegatedActionItem {
        id: item.id.clone(),
        selector: item.id.clone(),
        source: ActionSource::ActionItem,
        title: item.title.clone(),
        body: item.body.clone(),
        action: item.action,
        target: item.target.clone(),
        comment_ids: item.comment_ids.clone(),
        external_tickets: item.external_tickets.clone(),
        evidence_comments,
    }
}

fn comment_entry(comment: &Comment) -> DelegatedActionItem {
    DelegatedActionItem {
        id: comment.id.clone(),
        selector: comment.id.clone(),
        source: ActionSource::Comment,
        title: first_line(&comment.body),
        body: Some(comment.body.clone()),
        action: comment.action,
        target: comment.path.as_ref().map(|path| ReviewTarget {
            file: Some(path.clone()),
            line: comment.line,
            end_line: comment.end_line,
            ..ReviewTarget::default()
        }),
        comment_ids: vec![comment.id.clone()],
        external_tickets: Vec::new(),
        evidence_comments: vec![comment_evidence(comment)],
    }
}

fn comment_evidence(comment: &Comment) -> CommentEvidence {
    CommentEvidence {
        id: comment.id.clone(),
        selector: comment.id.clone(),
        path: comment.path.clone(),
        line: comment.line,
        end_line: comment.end_line,
        body: comment.body.clone(),
        action: comment.action,
        observation: comment.observation.clone(),
        replies: comment.replies.clone(),
    }
}

fn with_action_selectors(mut items: Vec<DelegatedActionItem>) -> Vec<DelegatedActionItem> {
    let id_strings = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    let ids = id_strings.iter().map(String::as_str).collect::<Vec<_>>();
    let evidence_id_strings = items
        .iter()
        .flat_map(|item| {
            item.evidence_comments
                .iter()
                .map(|comment| comment.id.clone())
        })
        .collect::<Vec<_>>();
    let evidence_ids = evidence_id_strings
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    for item in &mut items {
        item.selector = shortest_unique_prefix(&item.id, &ids);
        for comment in &mut item.evidence_comments {
            comment.selector = shortest_unique_prefix(&comment.id, &evidence_ids);
        }
    }
    items
}

fn first_line(body: &str) -> String {
    body.lines().next().unwrap_or("TODO").trim().to_owned()
}

fn item_key(item: &DelegatedActionItem) -> (u8, String, usize, String) {
    (
        match item.action {
            Some(ActionIntent::Fix) => 0,
            Some(ActionIntent::Test) => 1,
            Some(ActionIntent::FollowUp) => 2,
            Some(ActionIntent::Explain) => 3,
            Some(ActionIntent::None) | None => 4,
        },
        item.target
            .as_ref()
            .and_then(|target| target.file.clone())
            .unwrap_or_default(),
        item.target
            .as_ref()
            .and_then(|target| target.line)
            .unwrap_or(0),
        item.id.clone(),
    )
}

fn fingerprints(diff: &DiffSet) -> PacketFingerprints {
    let mut files = diff
        .files
        .iter()
        .map(|file| FileFingerprint {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            status: file.status.to_string(),
            fingerprint: file.fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    // Preserve the delegation v3 aggregate contract. Comment provenance has
    // its own independently versioned review-scope aggregate.
    let mut digest = Sha256::new();
    for file in &files {
        digest.update(file.path.as_bytes());
        digest.update(file.fingerprint.as_bytes());
    }
    PacketFingerprints {
        diff: format!("{:x}", digest.finalize()),
        files,
    }
}

fn walkthrough_context(walkthroughs: &[Walkthrough]) -> Vec<DelegatedWalkthroughStep> {
    let walkthrough_ids = walkthroughs
        .iter()
        .map(|walkthrough| walkthrough.id.as_str())
        .collect::<Vec<_>>();
    let step_ids = walkthroughs
        .iter()
        .flat_map(|walkthrough| walkthrough.steps.iter().map(|step| step.id.as_str()))
        .collect::<Vec<_>>();
    let mut output = walkthroughs
        .iter()
        .flat_map(|walkthrough| {
            let walkthrough_selector = shortest_unique_prefix(&walkthrough.id, &walkthrough_ids);
            let step_ids = &step_ids;
            walkthrough
                .steps
                .iter()
                .map(move |step: &WalkthroughStep| DelegatedWalkthroughStep {
                    walkthrough_id: walkthrough.id.clone(),
                    walkthrough_selector: walkthrough_selector.clone(),
                    step_id: step.id.clone(),
                    step_selector: shortest_unique_prefix(&step.id, step_ids),
                    title: step.title.clone(),
                    body: step.body.clone(),
                    why: step.why.clone(),
                    target: step.target.clone(),
                })
        })
        .collect::<Vec<_>>();
    output.sort_by(|left, right| {
        left.walkthrough_id
            .cmp(&right.walkthrough_id)
            .then(left.step_id.cmp(&right.step_id))
    });
    output
}

fn reference_hunks(diff: &DiffSet, paths: &BTreeSet<String>, limit: usize) -> Vec<ReferenceHunk> {
    let mut output = Vec::new();
    for file in sorted_files(&diff.files) {
        if !paths.contains(&file.path) {
            continue;
        }
        for hunk in &file.hunks {
            output.push(ReferenceHunk {
                path: file.path.clone(),
                hunk_header: hunk.header.clone(),
                fingerprint: hunk.content_fingerprint(),
                lines: hunk
                    .lines
                    .iter()
                    .take(limit.max(1))
                    .map(|line| {
                        format!(
                            "{}{}",
                            match line.kind {
                                DiffLineKind::Context => ' ',
                                DiffLineKind::Added => '+',
                                DiffLineKind::Removed => '-',
                                DiffLineKind::Meta => '\\',
                            },
                            line.text
                        )
                    })
                    .collect(),
            });
        }
    }
    output
}

fn sorted_files(files: &[FileDiff]) -> Vec<&FileDiff> {
    let mut files = files.iter().collect::<Vec<_>>();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    files
}

fn return_contract(
    target: &ReviewTarget,
    items: &[DelegatedActionItem],
) -> ReviewStateReturnContract {
    let globals = return_globals(target);
    let mut commands = Vec::new();
    let mut comment_ids = BTreeSet::new();
    for item in items {
        if item.source == ActionSource::ActionItem {
            commands.push(ReturnCommand {
                purpose: format!("close delegated action item {}", item.selector),
                command: format!(
                    "gander {globals} action-items close {} --disposition completed --outcome '<what changed and what was actually checked>'",
                    item.selector
                ),
            });
        }
        comment_ids.extend(
            item.evidence_comments
                .iter()
                .map(|comment| comment.id.clone()),
        );
    }
    let comment_ids = comment_ids.into_iter().collect::<Vec<_>>();
    let all_comment_ids = comment_ids.iter().map(String::as_str).collect::<Vec<_>>();
    commands.extend(comment_ids.iter().map(|id| {
        let selector = shortest_unique_prefix(id, &all_comment_ids);
        ReturnCommand {
            purpose: format!("reply to and resolve addressed comment {selector}"),
            command: format!(
                "gander {globals} comments resolve {selector} --reply '<what changed and what was actually checked>'"
            ),
        }
    }));
    commands.push(ReturnCommand {
        purpose: "add a follow-up comment".into(),
        command: format!(
            "gander {globals} comments add --path <path> --line <line> --kind issue --action follow-up --body '<body>'"
        ),
    });
    ReviewStateReturnContract {
        summary_required: true,
        allowed_action_item_statuses: vec!["open".into(), "closed".into()],
        allowed_closed_dispositions: vec![
            "completed".into(),
            "dismissed".into(),
            "deferred".into(),
        ],
        allowed_comment_states: vec!["todo".into(), "resolved".into()],
        commands,
    }
}

fn return_globals(target: &ReviewTarget) -> String {
    let mut args = Vec::new();
    if let Some(repo) = &target.repo {
        args.push(format!("--repo {}", shell_quote(repo)));
    }
    if let Some(base) = &target.base {
        args.push(format!("--base {}", shell_quote(base)));
    }
    if let Some(revision) = &target.revision {
        args.push(format!("--rev {}", shell_quote(revision)));
    }
    args.join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn render_delegation_markdown(packet: &DelegationPacket) -> String {
    let mut output = format!(
        "# Gander Delegation Packet\n\nSchema version: `{}`\nKind: `gander_delegation`\nSession: `{}`\n\n",
        packet.schema_version, packet.session.id
    );
    if let Some(recipient) = &packet.brief.recipient {
        output.push_str(&format!("Recipient: {recipient}\n\n"));
    }
    output.push_str("## Source snapshot\n\n");
    if let Some(repo) = &packet.target.repo {
        output.push_str(&format!("- Repository: `{repo}`\n"));
    }
    if let Some(base) = &packet.target.base {
        output.push_str(&format!("- Base: `{base}`\n"));
    }
    if let Some(revision) = &packet.target.revision {
        output.push_str(&format!("- Revision: `{revision}`\n"));
    }
    output.push_str(&format!(
        "- Diff fingerprint: `{}`\n- Durable action items: {}\n- Comments: {}\n\n",
        packet.fingerprints.diff, packet.source.action_item_count, packet.source.comment_count
    ));
    output.push_str(&format!("## Objective\n\n{}\n\n", packet.brief.objective));
    list(
        &mut output,
        "Constraints",
        &packet.brief.repeated_constraints,
    );
    list(
        &mut output,
        "Acceptance criteria",
        &packet.brief.acceptance_criteria,
    );
    list(
        &mut output,
        "Requested verification (not executed by Gander)",
        &packet.brief.requested_verification,
    );
    output.push_str("## Action items\n\n");
    for (index, item) in packet.action_items.iter().enumerate() {
        output.push_str(&format!(
            "### {}. {} [`{:?}` / `{}`]\n\nID: `{}`\n",
            index + 1,
            item.title,
            item.source,
            item.action
                .map(|action| format!("{action:?}").to_lowercase())
                .unwrap_or_else(|| "none".into()),
            item.id
        ));
        if let Some(target) = &item.target {
            output.push_str(&format!("Location: {}\n", target_label(target)));
        }
        if let Some(body) = &item.body
            && body.trim() != item.title.trim()
        {
            output.push_str(&format!("\n{}\n", body.trim()));
        }
        for ticket in &item.external_tickets {
            output.push_str(&format!(
                "\nExternal ticket: `{}` `{}`{}\n",
                ticket.tracker,
                ticket.reference,
                ticket
                    .url
                    .as_deref()
                    .map(|url| format!(" ({url})"))
                    .unwrap_or_default()
            ));
        }
        for evidence in &item.evidence_comments {
            output.push_str(&format!("\nReviewer comment `{}`", evidence.id));
            if let Some(path) = &evidence.path {
                output.push_str(" at `");
                output.push_str(&path.replace('`', "\\`"));
                output.push('`');
            }
            output.push_str(":\n\n> ");
            output.push_str(&evidence.body.trim().replace('\n', "\n> "));
            output.push('\n');
            match &evidence.observation {
                Some(observation) => output.push_str(&format!(
                    "\nObservation snapshot: `{}` (scope v{}).\n",
                    observation.snapshot.scope.aggregate, observation.snapshot.scope.version
                )),
                None => output.push_str(
                    "\nObservation snapshot unavailable (legacy comment; target labels are not proof).\n",
                ),
            }
            for reply in &evidence.replies {
                output.push_str(&format!("\nReply `{}`: {}\n", reply.id, reply.body.trim()));
                match &reply.result {
                    Some(result) => output.push_str(&format!(
                        "Result snapshot: `{}`; {}; relation: {}; portable patch changed: {}.\n",
                        result.snapshot.scope.aggregate,
                        result
                            .observation_aggregate_fingerprint
                            .as_deref()
                            .map(|fingerprint| format!("against observation: `{fingerprint}`"))
                            .unwrap_or_else(|| {
                                "against observation unavailable (legacy comment)".into()
                            }),
                        match &result.related {
                            crate::provenance::RelatedTransition::SamePath { path } =>
                                format!("same_path `{path}`"),
                            crate::provenance::RelatedTransition::RenamedFrom {
                                old_path,
                                path,
                            } => format!("renamed_from `{old_path}` to `{path}`"),
                            crate::provenance::RelatedTransition::NotInDiff { path } => path
                                .as_deref()
                                .map(|path| format!("not_in_diff `{path}`"))
                                .unwrap_or_else(|| "not_in_diff (general)".into()),
                        },
                        result
                            .portable_patch_changed
                            .map(|changed| if changed { "yes" } else { "no" })
                            .unwrap_or("unknown")
                    )),
                    None => output.push_str("Result snapshot unavailable (legacy reply).\n"),
                }
            }
        }
        output.push('\n');
    }
    if !packet.walkthrough.is_empty() {
        output.push_str("## Walkthrough context\n\n");
        for step in &packet.walkthrough {
            output.push_str(&format!(
                "- `{}` {} — {}\n",
                step.step_id,
                step.title.as_deref().unwrap_or("Untitled step"),
                target_label(&step.target)
            ));
            if let Some(why) = &step.why {
                output.push_str(&format!("  Why: {}\n", why.trim()));
            }
        }
        output.push('\n');
    }
    output.push_str("## Reference hunks\n\n");
    for hunk in &packet.reference_hunks {
        output.push_str(&format!(
            "### `{}` {}\n\n```diff\n{}\n```\n\n",
            hunk.path,
            hunk.hunk_header,
            hunk.lines.join("\n")
        ));
    }
    output.push_str("## Return contract\n\n");
    for command in &packet.return_contract.commands {
        output.push_str(&format!("- {}: `{}`\n", command.purpose, command.command));
    }
    output
}

fn target_label(target: &ReviewTarget) -> String {
    let mut label = target
        .file
        .as_deref()
        .map(|file| format!("`{file}`"))
        .unwrap_or_else(|| "review target".to_owned());
    if let Some(line) = target.line {
        label.push_str(&format!(":{line}"));
        if let Some(end_line) = target.end_line
            && end_line != line
        {
            label.push_str(&format!("-{end_line}"));
        }
    }
    if let Some(symbol) = &target.symbol {
        label.push_str(&format!(" (`{symbol}`)"));
    }
    label
}

fn list(output: &mut String, title: &str, values: &[String]) {
    if !values.is_empty() {
        output.push_str(&format!("## {title}\n\n"));
        for value in values {
            output.push_str(&format!("- {value}\n"));
        }
        output.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ClosedDisposition, CommentKind, ReviewSessionStatus, StepImportance};

    fn comment(id: &str, state: CommentState, path: Option<&str>) -> Comment {
        Comment {
            id: id.into(),
            path: path.map(str::to_owned),
            session_id: Some("session".into()),
            line: path.map(|_| 10),
            body: format!("feedback {id}\nmore"),
            kind: Some(CommentKind::Issue),
            action: Some(ActionIntent::Fix),
            state,
            ..Default::default()
        }
    }

    fn action_item(
        id: &str,
        status: ActionItemStatus,
        comment_ids: &[&str],
        path: &str,
    ) -> ActionItem {
        ActionItem {
            id: id.into(),
            title: format!("action {id}"),
            body: Some("body".into()),
            target: Some(ReviewTarget {
                file: Some(path.into()),
                line: Some(2),
                ..Default::default()
            }),
            action: Some(ActionIntent::Fix),
            status,
            disposition: (status == ActionItemStatus::Closed)
                .then_some(ClosedDisposition::Completed),
            comment_ids: comment_ids.iter().map(|id| (*id).to_owned()).collect(),
            ..Default::default()
        }
    }

    fn fixture() -> (ReviewState, ReviewSession, DiffSet) {
        let comments = vec![
            comment("standalone", CommentState::Todo, Some("b.rs")),
            comment("linked", CommentState::Todo, Some("a.rs")),
            comment("draft", CommentState::Draft, Some("a.rs")),
        ];
        let session = ReviewSession {
            id: "session".into(),
            title: Some("Review".into()),
            status: ReviewSessionStatus::Open,
            target: ReviewTarget {
                repo: Some("/repo".into()),
                base: Some("main".into()),
                revision: Some("@".into()),
                ..Default::default()
            },
            action_items: vec![
                action_item("open-action", ActionItemStatus::Open, &["linked"], "a.rs"),
                action_item("closed-action", ActionItemStatus::Closed, &[], "z.rs"),
            ],
            walkthroughs: vec![Walkthrough {
                id: "walk".into(),
                steps: vec![WalkthroughStep {
                    id: "step".into(),
                    title: Some("Read this".into()),
                    importance: StepImportance::Glance,
                    target: ReviewTarget {
                        file: Some("a.rs".into()),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let state = ReviewState {
            comments,
            sessions: vec![session.clone()],
            ..Default::default()
        };
        let diff = DiffSet::parse(
            "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-olda\n+newa\n",
        )
        .unwrap();
        (state, session, diff)
    }

    #[test]
    fn implicit_selection_folds_linked_todo_evidence() {
        let (state, session, diff) = fixture();
        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();

        assert_eq!(packet.action_items.len(), 2);
        let durable = packet
            .action_items
            .iter()
            .find(|item| item.source == ActionSource::ActionItem)
            .unwrap();
        assert_eq!(durable.id, "open-action");
        assert_eq!(durable.evidence_comments[0].id, "linked");
        assert!(packet.action_items.iter().all(|item| item.id != "linked"));
        assert!(
            packet
                .action_items
                .iter()
                .any(|item| item.id == "standalone")
        );
    }

    #[test]
    fn delegation_four_carries_provenance_and_honest_missing_language() {
        let (mut state, session, diff) = fixture();
        let snapshot = crate::provenance::SnapshotEvidence::capture(
            chrono::DateTime::UNIX_EPOCH,
            "session",
            session.target.clone(),
            diff.files.iter(),
        );
        let observation = crate::provenance::CommentObservation::new(
            snapshot.clone(),
            Some(crate::anchor::CommentAnchor::File {
                path: "a.rs".into(),
                old_path: None,
                diff_fingerprint: diff
                    .files
                    .iter()
                    .find(|file| file.path == "a.rs")
                    .unwrap()
                    .fingerprint
                    .clone(),
            }),
        );
        let linked = state
            .comments
            .iter_mut()
            .find(|comment| comment.id == "linked")
            .unwrap();
        linked.observation = Some(observation.clone());
        linked.replies.push(CommentReply {
            id: "reply".into(),
            body: "addressed".into(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            result: Some(crate::provenance::CommentReplyResult::compare(
                "linked",
                Some(&observation),
                Some("a.rs"),
                snapshot,
            )),
        });
        linked.replies.push(CommentReply {
            id: "legacy-a".into(),
            body: "legacy observation".into(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            result: Some(crate::provenance::CommentReplyResult::compare(
                "linked",
                None,
                Some("a.rs"),
                crate::provenance::SnapshotEvidence::capture(
                    chrono::DateTime::UNIX_EPOCH,
                    "session",
                    session.target.clone(),
                    diff.files.iter(),
                ),
            )),
        });

        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();
        assert_eq!(packet.schema_version, 4);
        let json = serde_json::to_value(&packet).unwrap();
        assert!(json.to_string().contains("portable_patch_changed"));
        let markdown = render_delegation_markdown(&packet);
        assert!(markdown.contains("Observation snapshot:"));
        assert!(markdown.contains("Result snapshot:"));
        assert!(markdown.contains("Observation snapshot unavailable (legacy comment"));
        assert!(markdown.contains("against observation unavailable (legacy comment)"));
    }

    #[test]
    fn explicit_action_item_selection_rejects_closed_and_unknown() {
        let (state, session, diff) = fixture();
        let closed = DelegationSpec {
            action_item_selectors: vec!["closed".into()],
            ..Default::default()
        };
        assert!(
            build_delegation_packet(&state, &session, &diff, &closed)
                .unwrap_err()
                .to_string()
                .contains("not open")
        );
        let unknown = DelegationSpec {
            action_item_selectors: vec!["missing".into()],
            ..Default::default()
        };
        assert!(
            build_delegation_packet(&state, &session, &diff, &unknown)
                .unwrap_err()
                .to_string()
                .contains("unknown action item")
        );
    }

    #[test]
    fn explicit_comment_selection_preserves_readiness_errors() {
        let (state, session, diff) = fixture();
        let draft = DelegationSpec {
            comment_selectors: vec!["draft".into()],
            ..Default::default()
        };
        let error = build_delegation_packet(&state, &session, &diff, &draft)
            .unwrap_err()
            .to_string();
        assert!(error.contains("still draft"));
        assert!(error.contains("comments ready"));
    }

    #[test]
    fn schema_four_uses_action_item_vocabulary_and_close_contract() {
        let (state, session, diff) = fixture();
        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();
        let value = serde_json::to_value(&packet).unwrap();
        let json = serde_json::to_string(&value).unwrap();
        let markdown = render_delegation_markdown(&packet);

        assert_eq!(value["schema_version"], DELEGATION_SCHEMA_VERSION);
        assert_eq!(value["source"]["action_item_count"], 2);
        assert_eq!(value["action_items"][0]["source"], "action_item");
        assert!(value["return_contract"]["allowed_action_item_statuses"].is_array());
        assert!(json.contains("action-items close"));
        assert!(markdown.contains("action-items close"));
        assert!(markdown.contains("Reviewer comment `linked`"));
    }

    #[test]
    fn schema_four_preserves_top_level_diff_fingerprint_algorithm() {
        let (state, session, diff) = fixture();
        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();
        let mut files = diff.files.iter().collect::<Vec<_>>();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let mut digest = Sha256::new();
        for file in files {
            digest.update(file.path.as_bytes());
            digest.update(file.fingerprint.as_bytes());
        }

        assert_eq!(packet.fingerprints.diff, format!("{:x}", digest.finalize()));
    }

    #[test]
    fn delegation_three_reply_without_provenance_deserializes() {
        let (mut state, session, diff) = fixture();
        state
            .comments
            .iter_mut()
            .find(|comment| comment.id == "linked")
            .unwrap()
            .replies
            .push(CommentReply {
                id: "legacy-reply".into(),
                body: "legacy".into(),
                created_at: chrono::DateTime::UNIX_EPOCH,
                result: None,
            });
        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();
        let mut value = serde_json::to_value(packet).unwrap();
        value["schema_version"] = serde_json::json!(3);
        for item in value["action_items"].as_array_mut().unwrap() {
            for evidence in item["evidence_comments"].as_array_mut().unwrap() {
                evidence.as_object_mut().unwrap().remove("observation");
                for reply in evidence["replies"].as_array_mut().unwrap() {
                    reply.as_object_mut().unwrap().remove("result");
                }
            }
        }

        let legacy: DelegationPacket = serde_json::from_value(value).unwrap();

        assert_eq!(legacy.schema_version, 3);
        let evidence = legacy
            .action_items
            .iter()
            .flat_map(|item| &item.evidence_comments)
            .find(|comment| comment.id == "linked")
            .unwrap();
        assert!(evidence.observation.is_none());
        assert!(evidence.replies[0].result.is_none());
    }

    #[test]
    fn compact_selector_is_used_by_return_contract() {
        let (mut state, mut session, diff) = fixture();
        session.action_items[0].id = "12345678-aaaa-bbbb-cccc-000000000000".into();
        state.sessions = vec![session.clone()];
        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();
        let item = packet
            .action_items
            .iter()
            .find(|item| item.source == ActionSource::ActionItem)
            .unwrap();

        assert_eq!(item.selector, "12345678");
        assert!(packet.return_contract.commands.iter().any(|command| {
            command.command.contains("action-items close 12345678")
                && !command.command.contains(&item.id)
        }));
    }

    #[test]
    fn linked_evidence_paths_contribute_reference_hunks() {
        let (state, mut session, diff) = fixture();
        session.action_items[0].target = None;
        let mut state = state;
        state.sessions = vec![session.clone()];
        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();

        assert!(
            packet
                .reference_hunks
                .iter()
                .any(|hunk| hunk.path == "a.rs")
        );
        assert!(
            packet
                .reference_hunks
                .iter()
                .any(|hunk| hunk.path == "b.rs")
        );
    }

    #[test]
    fn general_todo_has_no_target_or_reference_hunks() {
        let (mut state, mut session, diff) = fixture();
        session.action_items.clear();
        state.comments = vec![comment("general", CommentState::Todo, None)];
        state.sessions = vec![session.clone()];
        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();

        assert_eq!(packet.action_items.len(), 1);
        assert!(packet.action_items[0].target.is_none());
        assert!(packet.action_items[0].evidence_comments[0].path.is_none());
        assert!(packet.reference_hunks.is_empty());
    }

    #[test]
    fn comments_are_scoped_to_delegated_session_with_legacy_visible() {
        let (mut state, mut session, diff) = fixture();
        session.action_items.clear();
        let mut legacy = comment("legacy", CommentState::Todo, Some("a.rs"));
        legacy.session_id = None;
        let matching = comment("matching", CommentState::Todo, Some("b.rs"));
        let mut foreign = comment("foreign", CommentState::Todo, Some("a.rs"));
        foreign.session_id = Some("other".into());
        state.comments = vec![legacy, matching, foreign];
        state.sessions = vec![session.clone()];

        let packet =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();
        let ids = packet
            .action_items
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["legacy", "matching"]);
        assert_eq!(packet.source.comment_count, 2);
    }

    #[test]
    fn packet_build_is_deterministic_and_does_not_mutate_state() {
        let (state, session, diff) = fixture();
        let before = state.clone();
        let first =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();
        let second =
            build_delegation_packet(&state, &session, &diff, &DelegationSpec::default()).unwrap();

        assert_eq!(first.fingerprints, second.fingerprints);
        assert_eq!(state, before);
    }
}
