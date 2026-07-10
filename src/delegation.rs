use std::collections::{BTreeMap, BTreeSet};

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    diff::{DiffLineKind, DiffSet, FileDiff},
    state::{
        ActionIntent, Comment, CommentKind, CommentState, ReviewSession, ReviewState, ReviewTarget,
        ReviewTask, ReviewTaskStatus, Walkthrough, WalkthroughStep,
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
    pub path: String,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub body: String,
    pub action: Option<ActionIntent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DelegatedWalkthroughStep {
    pub walkthrough_id: String,
    pub step_id: String,
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
        action_items: selected,
        walkthrough: walkthrough_context(&session.walkthroughs),
        reference_hunks: reference_hunks(diff, &paths, spec.hunk_context_lines),
        return_contract: return_contract(&session.id),
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
                one(
                    tasks.iter().filter(|t| t.id.starts_with(s)).collect(),
                    "task",
                    s,
                )
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
                one(
                    comments.iter().filter(|c| c.id.starts_with(s)).collect(),
                    "comment",
                    s,
                )
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
        path: c.path.clone(),
        line: c.line,
        end_line: c.end_line,
        body: c.body.clone(),
        action: c.action,
    }
}
fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("TODO").trim().to_owned()
}
fn item_key(i: &DelegatedActionItem) -> (String, usize, String) {
    (
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
    let mut out = ws
        .iter()
        .flat_map(|w| {
            w.steps
                .iter()
                .map(move |s: &WalkthroughStep| DelegatedWalkthroughStep {
                    walkthrough_id: w.id.clone(),
                    step_id: s.id.clone(),
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
fn return_contract(session_id: &str) -> ReviewStateReturnContract {
    ReviewStateReturnContract {
        summary_required: true,
        allowed_task_statuses: vec!["open".into(), "done".into(), "dismissed".into()],
        allowed_comment_states: vec!["todo".into(), "resolved".into()],
        commands: vec![
            ReturnCommand {
                purpose: "mark a delegated task complete".into(),
                command: format!(
                    "gander review task done {session_id} <task-id> --resolution '<summary>'"
                ),
            },
            ReturnCommand {
                purpose: "resolve a source TODO comment".into(),
                command: "gander comment resolve <comment-id>".into(),
            },
            ReturnCommand {
                purpose: "add a follow-up comment".into(),
                command: format!(
                    "gander review comment add {session_id} <path> --line <line> --kind issue --action follow-up --body '<body>'"
                ),
            },
        ],
    }
}

pub fn render_delegation_markdown(packet: &DelegationPacket) -> String {
    let mut out = format!(
        "# Gander Delegation Packet\n\nSchema version: `{}`\nKind: `{:?}`\nSession: `{}`\n\n",
        packet.schema_version, packet.kind, packet.session.id
    );
    if let Some(r) = &packet.brief.recipient {
        out.push_str(&format!("Recipient: {r}\n\n"));
    }
    out.push_str(&format!("## Objective\n\n{}\n\n", packet.brief.objective));
    list(
        &mut out,
        "Repeated constraints",
        &packet.brief.repeated_constraints,
    );
    list(
        &mut out,
        "Acceptance criteria",
        &packet.brief.acceptance_criteria,
    );
    list(
        &mut out,
        "Requested verification (inert; do not execute unless you choose to)",
        &packet.brief.requested_verification,
    );
    out.push_str("## Action items\n\n");
    for i in &packet.action_items {
        out.push_str(&format!("- `{}` {:?}: {}\n", i.id, i.source, i.title));
        for e in &i.evidence_comments {
            out.push_str(&format!(
                "  - evidence comment `{}` at `{}`: {}\n",
                e.id,
                e.path,
                first_line(&e.body)
            ));
        }
    }
    out.push_str("\n## Reference hunks\n\n");
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
            created_at: chrono::DateTime::UNIX_EPOCH,
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
        assert!(md.contains("gander review task done sess <task-id>"));
        assert!(md.contains("gander comment resolve <comment-id>"));
    }
}
