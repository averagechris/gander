use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    diff::{DiffLineKind, DiffSet, FileDiff},
    ids::shortest_unique_prefix,
    state::{
        ActionIntent, Comment, CommentKind, CommentReply, CommentState, ReviewSession, ReviewState,
        ReviewTarget, ReviewTask, ReviewTaskStatus, Walkthrough, WalkthroughStep,
    },
};

pub const DELEGATION_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegationSpec {
    pub recipient: Option<String>,
    pub objective: String,
    pub repeated_constraints: Vec<String>,
    pub acceptance_criteria: Vec<String>,
    pub requested_verification: Vec<String>,
    pub task_selectors: Vec<String>,
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
    pub task_count: usize,
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
    pub action: ActionIntent,
    pub target: Option<ReviewTarget>,
    pub evidence_comments: Vec<CommentEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionSource {
    Task,
    Comment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommentEvidence {
    pub id: String,
    pub selector: String,
    pub path: String,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub body: String,
    pub action: Option<ActionIntent>,
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
    pub allowed_task_statuses: Vec<String>,
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
    let selected = select_action_items(session, &state.comments, spec)?;
    let return_contract = return_contract(&session.target, &selected);
    let paths = selected
        .iter()
        .filter_map(|i| i.target.as_ref()?.file.clone())
        .collect::<BTreeSet<_>>();
    Ok(DelegationPacket {
        schema_version: DELEGATION_SCHEMA_VERSION,
        kind: DelegationPacketKind::GanderDelegation,
        generated_at: chrono::Utc::now(),
        session: SessionMeta {
            id: session.id.clone(),
            selector: shortest_unique_prefix(&session.id, &[session.id.as_str()]),
            title: session.title.clone(),
            status: format!("{:?}", session.status).to_lowercase(),
            created_at: session.created_at,
            updated_at: session.updated_at,
        },
        target: session.target.clone(),
        source: SourceMeta {
            diff_file_count: diff.files.len(),
            task_count: session.tasks.len(),
            comment_count: state.comments.len(),
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
        action_items: with_action_selectors(selected),
        walkthrough: walkthrough_context(&session.walkthroughs),
        reference_hunks: reference_hunks(diff, &paths, spec.hunk_context_lines),
        return_contract,
    })
}

fn select_action_items(
    session: &ReviewSession,
    comments: &[Comment],
    spec: &DelegationSpec,
) -> Result<Vec<DelegatedActionItem>> {
    let comments_by_id = comments
        .iter()
        .map(|c| (c.id.as_str(), c))
        .collect::<BTreeMap<_, _>>();
    let mut items = Vec::new();
    let explicit = !spec.task_selectors.is_empty() || !spec.comment_selectors.is_empty();
    let tasks = resolve_tasks(&session.tasks, &spec.task_selectors, explicit)?;
    let selected_task_comments = tasks
        .iter()
        .filter_map(|t| t.source_comment_id.as_deref())
        .collect::<BTreeSet<_>>();
    for task in tasks {
        let mut evidence = Vec::new();
        if let Some(id) = task
            .source_comment_id
            .as_deref()
            .and_then(|id| comments_by_id.get(id).copied())
        {
            evidence.push(comment_evidence(id));
        }
        items.push(task_item(task, evidence));
    }
    for comment in resolve_comments(comments, &spec.comment_selectors, explicit)? {
        if selected_task_comments.contains(comment.id.as_str()) {
            continue;
        }
        items.push(comment_item(comment));
    }
    items.sort_by_key(item_key);
    if items.is_empty() {
        return Err(eyre!("delegation selection is empty"));
    }
    Ok(items)
}

fn resolve_tasks<'a>(
    tasks: &'a [ReviewTask],
    selectors: &[String],
    explicit: bool,
) -> Result<Vec<&'a ReviewTask>> {
    if !selectors.is_empty() {
        selectors
            .iter()
            .map(|s| {
                let task = one(
                    tasks.iter().filter(|t| t.id.starts_with(s)).collect(),
                    "task",
                    s,
                )?;
                if task.status != ReviewTaskStatus::Open {
                    return Err(eyre!("task `{}` is not open", task.id));
                }
                Ok(task)
            })
            .collect()
    } else if explicit {
        Ok(vec![])
    } else {
        Ok(tasks
            .iter()
            .filter(|t| t.status == ReviewTaskStatus::Open)
            .collect())
    }
}
fn resolve_comments<'a>(
    comments: &'a [Comment],
    selectors: &[String],
    explicit: bool,
) -> Result<Vec<&'a Comment>> {
    if !selectors.is_empty() {
        selectors
            .iter()
            .map(|s| {
                let comment = one(
                    comments.iter().filter(|c| c.id.starts_with(s)).collect(),
                    "comment",
                    s,
                )?;
                if comment.state == CommentState::Resolved {
                    return Err(eyre!("comment `{}` is already resolved", comment.id));
                }
                Ok(comment)
            })
            .collect()
    } else if explicit {
        Ok(vec![])
    } else {
        Ok(comments
            .iter()
            .filter(|c| {
                c.state == CommentState::Todo
                    && c.kind != Some(CommentKind::Praise)
                    && c.action.unwrap_or(ActionIntent::Fix) != ActionIntent::None
            })
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
fn task_item(task: &ReviewTask, evidence_comments: Vec<CommentEvidence>) -> DelegatedActionItem {
    DelegatedActionItem {
        id: task.id.clone(),
        selector: task.id.clone(),
        source: ActionSource::Task,
        title: task.title.clone(),
        body: task.body.clone(),
        action: task.action,
        target: task.target.clone(),
        evidence_comments,
    }
}
fn comment_item(c: &Comment) -> DelegatedActionItem {
    DelegatedActionItem {
        id: c.id.clone(),
        selector: c.id.clone(),
        source: ActionSource::Comment,
        title: first_line(&c.body),
        body: Some(c.body.clone()),
        action: c.action.unwrap_or(ActionIntent::Fix),
        target: Some(ReviewTarget {
            file: Some(c.path.clone()),
            line: c.line,
            end_line: c.end_line,
            ..ReviewTarget::default()
        }),
        evidence_comments: vec![comment_evidence(c)],
    }
}
fn comment_evidence(c: &Comment) -> CommentEvidence {
    CommentEvidence {
        id: c.id.clone(),
        selector: c.id.clone(),
        path: c.path.clone(),
        line: c.line,
        end_line: c.end_line,
        body: c.body.clone(),
        action: c.action,
        replies: c.replies.clone(),
    }
}

fn with_action_selectors(mut items: Vec<DelegatedActionItem>) -> Vec<DelegatedActionItem> {
    let id_strings = items.iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    let ids = id_strings.iter().map(String::as_str).collect::<Vec<_>>();
    for item in &mut items {
        item.selector = shortest_unique_prefix(&item.id, &ids);
        let comment_id_strings = item
            .evidence_comments
            .iter()
            .map(|comment| comment.id.clone())
            .collect::<Vec<_>>();
        let comment_ids = comment_id_strings
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        for comment in &mut item.evidence_comments {
            comment.selector = shortest_unique_prefix(&comment.id, &comment_ids);
        }
    }
    items
}
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("TODO").trim().to_owned()
}
fn item_key(i: &DelegatedActionItem) -> (u8, String, usize, String) {
    (
        match i.action {
            ActionIntent::Fix => 0,
            ActionIntent::Test => 1,
            ActionIntent::FollowUp => 2,
            ActionIntent::Explain => 3,
            ActionIntent::None => 4,
        },
        i.target
            .as_ref()
            .and_then(|t| t.file.clone())
            .unwrap_or_default(),
        i.target.as_ref().and_then(|t| t.line).unwrap_or(0),
        i.id.clone(),
    )
}

fn fingerprints(diff: &DiffSet) -> PacketFingerprints {
    let mut files = diff
        .files
        .iter()
        .map(|f| FileFingerprint {
            path: f.path.clone(),
            old_path: f.old_path.clone(),
            status: f.status.to_string(),
            fingerprint: f.fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut h = Sha256::new();
    for f in &files {
        h.update(f.path.as_bytes());
        h.update(f.fingerprint.as_bytes());
    }
    PacketFingerprints {
        diff: format!("{:x}", h.finalize()),
        files,
    }
}
fn walkthrough_context(ws: &[Walkthrough]) -> Vec<DelegatedWalkthroughStep> {
    let walkthrough_ids = ws.iter().map(|w| w.id.as_str()).collect::<Vec<_>>();
    let step_ids = ws
        .iter()
        .flat_map(|w| w.steps.iter().map(|s| s.id.as_str()))
        .collect::<Vec<_>>();
    let mut out = ws
        .iter()
        .flat_map(|w| {
            let walkthrough_selector = shortest_unique_prefix(&w.id, &walkthrough_ids);
            let step_ids = &step_ids;
            w.steps
                .iter()
                .map(move |s: &WalkthroughStep| DelegatedWalkthroughStep {
                    walkthrough_id: w.id.clone(),
                    walkthrough_selector: walkthrough_selector.clone(),
                    step_id: s.id.clone(),
                    step_selector: shortest_unique_prefix(&s.id, step_ids),
                    title: s.title.clone(),
                    body: s.body.clone(),
                    why: s.why.clone(),
                    target: s.target.clone(),
                })
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| {
        a.walkthrough_id
            .cmp(&b.walkthrough_id)
            .then(a.step_id.cmp(&b.step_id))
    });
    out
}
fn reference_hunks(diff: &DiffSet, paths: &BTreeSet<String>, limit: usize) -> Vec<ReferenceHunk> {
    let include_all = paths.is_empty();
    let mut out = Vec::new();
    for f in sorted_files(&diff.files) {
        if !include_all && !paths.contains(&f.path) {
            continue;
        }
        for h in &f.hunks {
            out.push(ReferenceHunk {
                path: f.path.clone(),
                hunk_header: h.header.clone(),
                fingerprint: h.content_fingerprint(),
                lines: h
                    .lines
                    .iter()
                    .take(limit.max(1))
                    .map(|l| {
                        format!(
                            "{}{}",
                            match l.kind {
                                DiffLineKind::Context => ' ',
                                DiffLineKind::Added => '+',
                                DiffLineKind::Removed => '-',
                                DiffLineKind::Meta => '\\',
                            },
                            l.text
                        )
                    })
                    .collect(),
            });
        }
    }
    out
}
fn sorted_files(files: &[FileDiff]) -> Vec<&FileDiff> {
    let mut v = files.iter().collect::<Vec<_>>();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}
fn return_contract(
    target: &ReviewTarget,
    items: &[DelegatedActionItem],
) -> ReviewStateReturnContract {
    let globals = return_globals(target);
    let mut commands = Vec::new();
    let mut comment_ids = BTreeSet::new();
    for item in items {
        match item.source {
            ActionSource::Task => commands.push(ReturnCommand {
                purpose: format!("mark delegated task {} complete", item.selector),
                command: format!(
                    "gander {globals} tasks complete {} --summary '<what changed and what was actually checked>'",
                    item.selector
                ),
            }),
            ActionSource::Comment => {
                comment_ids.insert(item.id.clone());
            }
        }
        comment_ids.extend(
            item.evidence_comments
                .iter()
                .map(|comment| comment.id.clone()),
        );
    }
    let comment_ids = comment_ids.into_iter().collect::<Vec<_>>();
    commands.extend(comment_ids.iter().map(|id| {
        let all_comment_ids = comment_ids.iter().map(String::as_str).collect::<Vec<_>>();
        let selector = shortest_unique_prefix(id, &all_comment_ids);
        ReturnCommand {
        purpose: format!("reply to and resolve addressed comment {selector}"),
        command: format!(
            "gander {globals} comments resolve {selector} --reply '<what changed and what was actually checked>'"
        ),
    }}));
    commands.push(ReturnCommand {
        purpose: "add a follow-up comment".into(),
        command: format!("gander {globals} comments add --path <path> --line <line> --kind issue --action follow-up --body '<body>'"),
    });

    ReviewStateReturnContract {
        summary_required: true,
        allowed_task_statuses: vec!["open".into(), "done".into(), "dismissed".into()],
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
    let mut out = format!(
        "# Gander Delegation Packet\n\nSchema version: `{}`\nKind: `gander_delegation`\nSession: `{}`\n\n",
        packet.schema_version, packet.session.id
    );
    if let Some(r) = &packet.brief.recipient {
        out.push_str(&format!("Recipient: {r}\n\n"));
    }
    out.push_str("## Source snapshot\n\n");
    if let Some(repo) = &packet.target.repo {
        out.push_str(&format!("- Repository: `{repo}`\n"));
    }
    if let Some(base) = &packet.target.base {
        out.push_str(&format!("- Base: `{base}`\n"));
    }
    if let Some(revision) = &packet.target.revision {
        out.push_str(&format!("- Revision: `{revision}`\n"));
    }
    out.push_str(&format!(
        "- Diff fingerprint: `{}`\n\n",
        packet.fingerprints.diff
    ));
    out.push_str(&format!("## Objective\n\n{}\n\n", packet.brief.objective));
    list(&mut out, "Constraints", &packet.brief.repeated_constraints);
    list(
        &mut out,
        "Acceptance criteria",
        &packet.brief.acceptance_criteria,
    );
    list(
        &mut out,
        "Requested verification (not executed by Gander)",
        &packet.brief.requested_verification,
    );
    out.push_str("## Action items\n\n");
    for (index, i) in packet.action_items.iter().enumerate() {
        out.push_str(&format!(
            "### {}. {} [`{:?}` / `{:?}`]\n\nID: `{}`\n",
            index + 1,
            i.title,
            i.source,
            i.action,
            i.id
        ));
        if let Some(target) = &i.target {
            out.push_str(&format!("Location: {}\n", target_label(target)));
        }
        if let Some(body) = &i.body
            && body.trim() != i.title.trim()
        {
            out.push_str(&format!("\n{}\n", body.trim()));
        }
        for e in &i.evidence_comments {
            out.push_str(&format!(
                "\nReviewer comment `{}` at `{}`:\n\n> {}\n",
                e.id,
                e.path,
                e.body.trim().replace('\n', "\n> ")
            ));
            for reply in &e.replies {
                out.push_str(&format!("\nReply `{}`: {}\n", reply.id, reply.body.trim()));
            }
        }
        out.push('\n');
    }
    if !packet.walkthrough.is_empty() {
        out.push_str("## Walkthrough context\n\n");
        for step in &packet.walkthrough {
            out.push_str(&format!(
                "- `{}` {} — {}\n",
                step.step_id,
                step.title.as_deref().unwrap_or("Untitled step"),
                target_label(&step.target)
            ));
            if let Some(why) = &step.why {
                out.push_str(&format!("  Why: {}\n", why.trim()));
            }
        }
        out.push('\n');
    }
    out.push_str("## Reference hunks\n\n");
    for h in &packet.reference_hunks {
        out.push_str(&format!(
            "### `{}` {}\n\n```diff\n{}\n```\n\n",
            h.path,
            h.hunk_header,
            h.lines.join("\n")
        ));
    }
    out.push_str("## Return contract\n\n");
    for c in &packet.return_contract.commands {
        out.push_str(&format!("- {}: `{}`\n", c.purpose, c.command));
    }
    out
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
fn list(out: &mut String, title: &str, xs: &[String]) {
    if !xs.is_empty() {
        out.push_str(&format!("## {title}\n\n"));
        for x in xs {
            out.push_str(&format!("- {x}\n"));
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{ReviewSessionStatus, StepImportance};
    fn comment(
        id: &str,
        state: CommentState,
        kind: Option<CommentKind>,
        action: Option<ActionIntent>,
        path: &str,
    ) -> Comment {
        Comment {
            id: id.into(),
            path: path.into(),
            line: Some(10),
            end_line: None,
            anchor: None,
            body: format!("fix {id}\nmore"),
            kind,
            action,
            state,
            replies: Vec::new(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            updated_at: None,
        }
    }
    fn task(
        id: &str,
        status: ReviewTaskStatus,
        source_comment_id: Option<&str>,
        path: &str,
    ) -> ReviewTask {
        ReviewTask {
            id: id.into(),
            title: format!("task {id}"),
            body: Some("body".into()),
            target: Some(ReviewTarget {
                file: Some(path.into()),
                line: Some(2),
                ..Default::default()
            }),
            action: ActionIntent::Fix,
            status,
            source_comment_id: source_comment_id.map(str::to_owned),
            ..Default::default()
        }
    }
    fn fixture() -> (ReviewState, ReviewSession, DiffSet) {
        let comments = vec![
            comment(
                "c-todo",
                CommentState::Todo,
                Some(CommentKind::Issue),
                Some(ActionIntent::Fix),
                "b.rs",
            ),
            comment(
                "c-praise",
                CommentState::Todo,
                Some(CommentKind::Praise),
                Some(ActionIntent::Fix),
                "a.rs",
            ),
            comment(
                "c-draft",
                CommentState::Draft,
                Some(CommentKind::Issue),
                Some(ActionIntent::Fix),
                "a.rs",
            ),
            comment(
                "c-linked",
                CommentState::Todo,
                Some(CommentKind::Issue),
                Some(ActionIntent::Fix),
                "a.rs",
            ),
        ];
        let session = ReviewSession {
            id: "sess".into(),
            title: Some("T".into()),
            status: ReviewSessionStatus::Open,
            tasks: vec![
                task("t-open", ReviewTaskStatus::Open, Some("c-linked"), "a.rs"),
                task("t-done", ReviewTaskStatus::Done, None, "z.rs"),
            ],
            walkthroughs: vec![Walkthrough {
                id: "w".into(),
                steps: vec![WalkthroughStep {
                    id: "s".into(),
                    title: Some("step".into()),
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
        let diff = DiffSet::parse("diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-olda\n+newa\n").unwrap();
        (state, session, diff)
    }
    #[test]
    fn default_filters_and_folds_comments() {
        let (s, sess, d) = fixture();
        let p = build_delegation_packet(&s, &sess, &d, &DelegationSpec::default()).unwrap();
        assert_eq!(
            p.action_items
                .iter()
                .map(|i| i.id.as_str())
                .collect::<Vec<_>>(),
            vec!["t-open", "c-todo"]
        );
        assert_eq!(p.action_items[0].evidence_comments[0].id, "c-linked");
    }
    #[test]
    fn explicit_selection_and_errors() {
        let (s, sess, d) = fixture();
        let spec = DelegationSpec {
            comment_selectors: vec!["c-to".into()],
            ..Default::default()
        };
        assert_eq!(
            build_delegation_packet(&s, &sess, &d, &spec)
                .unwrap()
                .action_items[0]
                .id,
            "c-todo"
        );
        let bad = DelegationSpec {
            task_selectors: vec!["missing".into()],
            ..Default::default()
        };
        assert!(
            build_delegation_packet(&s, &sess, &d, &bad)
                .unwrap_err()
                .to_string()
                .contains("unknown task")
        );
        let amb = DelegationSpec {
            comment_selectors: vec!["c-".into()],
            ..Default::default()
        };
        assert!(
            build_delegation_packet(&s, &sess, &d, &amb)
                .unwrap_err()
                .to_string()
                .contains("ambiguous comment")
        );
    }
    #[test]
    fn deterministic_fingerprints_and_no_mutation() {
        let (s, sess, d) = fixture();
        let before = s.clone();
        let p1 = build_delegation_packet(&s, &sess, &d, &DelegationSpec::default()).unwrap();
        let p2 = build_delegation_packet(&s, &sess, &d, &DelegationSpec::default()).unwrap();
        assert_eq!(p1.fingerprints, p2.fingerprints);
        assert_eq!(s, before);
        assert_eq!(
            p1.fingerprints
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            vec!["a.rs", "b.rs"]
        );
    }
    #[test]
    fn json_shape_version_and_markdown_parity() {
        let (s, sess, d) = fixture();
        let spec = DelegationSpec {
            recipient: Some("agent".into()),
            objective: "do it".into(),
            requested_verification: vec!["cargo test".into()],
            ..Default::default()
        };
        let p = build_delegation_packet(&s, &sess, &d, &spec).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["kind"], "gander_delegation");
        let md = render_delegation_markdown(&p);
        assert!(md.contains("do it"));
        assert!(md.contains("cargo test"));
        assert!(md.contains("tasks complete t-open --summary"));
        assert!(md.contains("comments resolve c-linked --reply"));
        assert!(md.contains("comments resolve c-todo --reply"));
    }

    #[test]
    fn explicit_selection_rejects_closed_items() {
        let (mut state, session, diff) = fixture();
        state.comments.push(comment(
            "c-resolved",
            CommentState::Resolved,
            Some(CommentKind::Issue),
            Some(ActionIntent::Fix),
            "a.rs",
        ));

        let done = DelegationSpec {
            task_selectors: vec!["t-done".into()],
            ..Default::default()
        };
        assert!(
            build_delegation_packet(&state, &session, &diff, &done)
                .unwrap_err()
                .to_string()
                .contains("not open")
        );

        let resolved = DelegationSpec {
            comment_selectors: vec!["c-resolved".into()],
            ..Default::default()
        };
        assert!(
            build_delegation_packet(&state, &session, &diff, &resolved)
                .unwrap_err()
                .to_string()
                .contains("already resolved")
        );
    }
}
