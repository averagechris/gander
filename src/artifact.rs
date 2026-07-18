use std::{collections::BTreeSet, fs, io::Write, path::Path};

use chrono::Utc;
use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::{
    anchor::CommentAnchor,
    app::{ReviewFile, ReviewSession},
    diff::{DiffLineKind, Hunk},
    ids::shortest_unique_prefix,
    state::{
        ActionIntent, ActionItemStatus, ClosedDisposition, Comment, CommentKind, CommentState,
        ExternalTicket, ReviewState, ReviewTarget, StepArtifact, StepImportance, StepKind,
    },
};

#[derive(Clone, Copy, Debug)]
pub enum ArtifactFormat {
    Json,
    Markdown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArtifactProfile {
    #[default]
    Human,
    Agent,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ArtifactBuildOptions {
    pub only_open: bool,
}

const EXCERPT_CONTEXT_LINES: usize = 3;
pub const ARTIFACT_SCHEMA_VERSION: u8 = 9;

#[derive(Debug, Serialize)]
pub struct ReviewArtifact<'a> {
    pub version: u8,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub repo: &'a Path,
    pub base: &'a str,
    pub revision: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<&'static str>,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionArtifact<'a>>,
    pub files: Vec<FileArtifact<'a>>,
    pub comments: Vec<CommentArtifact<'a>>,
    pub action_items: Vec<ActionItemArtifact<'a>>,
    pub walkthroughs: Vec<WalkthroughArtifact<'a>>,
}

#[derive(Debug, Serialize)]
pub struct SessionArtifact<'a> {
    pub id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct FileArtifact<'a> {
    pub path: &'a str,
    pub old_path: Option<&'a str>,
    pub status: String,
    pub viewed: bool,
    pub generated: bool,
    pub additions: usize,
    pub deletions: usize,
    pub fingerprint: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hunks: Option<Vec<HunkArtifact<'a>>>,
}

#[derive(Debug, Serialize)]
pub struct HunkArtifact<'a> {
    pub header: &'a str,
    pub old_start: usize,
    pub old_len: usize,
    pub new_start: usize,
    pub new_len: usize,
    pub lines: Vec<ExcerptLine<'a>>,
}

#[derive(Debug, Serialize)]
pub struct ExcerptLine<'a> {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_line: Option<usize>,
    pub text: &'a str,
}

#[derive(Debug, Serialize)]
pub struct CommentArtifact<'a> {
    #[serde(flatten)]
    pub comment: &'a Comment,
    pub linked_action_item_ids: Vec<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<Vec<ExcerptLine<'a>>>,
}

#[derive(Debug, Serialize)]
pub struct ActionItemArtifact<'a> {
    pub id: &'a str,
    pub title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    pub status: ActionItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionIntent>,
    pub comment_ids: Vec<&'a str>,
    pub external_tickets: &'a [ExternalTicket],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<ClosedDisposition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<TargetArtifact<'a>>,
}

#[derive(Debug, Serialize)]
pub struct WalkthroughArtifact<'a> {
    pub id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    pub steps: Vec<WalkthroughStepArtifact<'a>>,
}

#[derive(Debug, Serialize)]
pub struct WalkthroughStepArtifact<'a> {
    pub id: &'a str,
    pub kind: StepKind,
    pub importance: StepImportance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    pub target: TargetArtifact<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<&'a StepArtifact>,
}

#[derive(Debug, Serialize)]
pub struct TargetArtifact<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct OwnedReviewArtifact {
    pub version: u8,
    pub base: String,
    pub revision: String,
    pub files: Vec<OwnedFileArtifact>,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct OwnedFileArtifact {
    pub path: String,
    pub viewed: bool,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportSummary {
    pub viewed_files_imported: usize,
    pub comments_imported: usize,
    pub duplicate_comments_skipped: usize,
}

impl Default for OwnedReviewArtifact {
    fn default() -> Self {
        Self {
            version: ARTIFACT_SCHEMA_VERSION,
            base: String::new(),
            revision: String::new(),
            files: Vec::new(),
            comments: Vec::new(),
        }
    }
}

impl<'a> From<&'a ReviewSession> for ReviewArtifact<'a> {
    fn from(session: &'a ReviewSession) -> Self {
        Self::build(session, ArtifactProfile::Human)
    }
}

impl<'a> ReviewArtifact<'a> {
    pub fn build(session: &'a ReviewSession, profile: ArtifactProfile) -> Self {
        Self::build_with_options(session, profile, ArtifactBuildOptions::default())
    }

    pub fn build_with_options(
        session: &'a ReviewSession,
        profile: ArtifactProfile,
        options: ArtifactBuildOptions,
    ) -> Self {
        let agent = profile == ArtifactProfile::Agent;
        let durable = active_durable_session(session);
        // Keep all outbound open-work adapters aligned with the shared review
        // query even though this complete artifact also carries closed history.
        let open_work = durable.map(|active| crate::review::open_work(active, &session.comments));
        let open_ids = open_work
            .iter()
            .flat_map(|work| work.action_items.iter().map(|entry| entry.item.id.as_str()))
            .collect::<BTreeSet<_>>();

        Self {
            version: ARTIFACT_SCHEMA_VERSION,
            generated_at: Utc::now(),
            repo: &session.repo,
            base: &session.target.base,
            revision: &session.target.rev,
            profile: agent.then_some("agent"),
            summary: session.summary_line(),
            session: durable.map(|active| SessionArtifact {
                id: &active.id,
                title: active.title.as_deref(),
            }),
            files: session
                .files
                .iter()
                .map(|file| FileArtifact {
                    path: &file.path,
                    old_path: file.old_path.as_deref(),
                    status: file.status.to_string(),
                    viewed: file.viewed,
                    generated: file.generated,
                    additions: file.additions,
                    deletions: file.deletions,
                    fingerprint: &file.fingerprint,
                    hunks: agent.then(|| file.diff.hunks.iter().map(hunk_artifact).collect()),
                })
                .collect(),
            comments: session
                .comments
                .iter()
                .filter(|comment| {
                    comment_belongs_to_session(comment, durable.map(|active| active.id.as_str()))
                })
                .filter(|comment| !options.only_open || comment.state == CommentState::Todo)
                .map(|comment| CommentArtifact {
                    comment,
                    linked_action_item_ids: linked_action_item_ids(session, &comment.id),
                    excerpt: agent.then(|| comment_excerpt(session, comment)).flatten(),
                })
                .collect(),
            action_items: durable
                .into_iter()
                .flat_map(|active| active.action_items.iter())
                .filter(|item| !options.only_open || open_ids.contains(item.id.as_str()))
                .map(|item| ActionItemArtifact {
                    id: &item.id,
                    title: &item.title,
                    body: item.body.as_deref(),
                    status: item.status,
                    action: item.action,
                    comment_ids: item.comment_ids.iter().map(String::as_str).collect(),
                    external_tickets: &item.external_tickets,
                    disposition: item.disposition,
                    outcome: item.outcome.as_deref(),
                    closed_at: item.closed_at,
                    target: item.target.as_ref().map(target_artifact),
                })
                .collect(),
            walkthroughs: durable
                .into_iter()
                .flat_map(|active| active.walkthroughs.iter())
                .map(|walkthrough| WalkthroughArtifact {
                    id: &walkthrough.id,
                    title: walkthrough.title.as_deref(),
                    steps: walkthrough
                        .steps
                        .iter()
                        .map(|step| WalkthroughStepArtifact {
                            id: &step.id,
                            kind: step.kind,
                            importance: step.importance,
                            change_id: step.change_id.as_deref(),
                            title: step.title.as_deref(),
                            why: step.why.as_deref(),
                            body: step.body.as_deref(),
                            target: target_artifact(&step.target),
                            artifacts: step.artifacts.iter().collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

pub fn render_handoff_json(
    session: &ReviewSession,
    mut options: ArtifactBuildOptions,
) -> Result<String> {
    options.only_open = true;
    let artifact = ReviewArtifact::build_with_options(session, ArtifactProfile::Agent, options);
    let item_ids = artifact
        .action_items
        .iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    let comment_ids = artifact
        .comments
        .iter()
        .map(|comment| comment.comment.id.as_str())
        .collect::<Vec<_>>();
    let items = ordered_open_work(&artifact)
        .into_iter()
        .map(|entry| match entry {
            OrderedOpenWork::Durable(item) => {
                let evidence = evidence_comments(&artifact, item)
                    .into_iter()
                    .map(comment_handoff_value)
                    .collect::<Vec<_>>();
                serde_json::json!({
                    "id": item.id,
                    "selector": shortest_unique_prefix(item.id, &item_ids),
                    "short_id": shortest_unique_prefix(item.id, &item_ids),
                    "source": "action_item",
                    "title": item.title,
                    "body": item.body,
                    "action": item.action,
                    "status": item.status,
                    "disposition": item.disposition,
                    "outcome": item.outcome,
                    "closed_at": item.closed_at,
                    "external_tickets": item.external_tickets,
                    "comment_ids": item.comment_ids,
                    "target": item.target,
                    "evidence_comments": evidence,
                })
            }
            OrderedOpenWork::Comment(comment) => serde_json::json!({
                "id": comment.comment.id,
                "selector": shortest_unique_prefix(&comment.comment.id, &comment_ids),
                "short_id": shortest_unique_prefix(&comment.comment.id, &comment_ids),
                "source": "comment",
                "kind": comment.comment.kind,
                "action": comment.comment.action,
                "path": comment.comment.path,
                "line": comment.comment.line,
                "end_line": comment.comment.end_line,
                "excerpt": comment.excerpt,
                "body": comment.comment.body,
                "state": comment.comment.state,
                "observation": comment.comment.observation,
                "replies": comment.comment.replies,
                "linked_action_item_ids": comment.linked_action_item_ids,
            }),
        })
        .collect::<Vec<_>>();
    let walkthrough = artifact
        .walkthroughs
        .iter()
        .flat_map(|walkthrough| {
            let step_ids = artifact
                .walkthroughs
                .iter()
                .flat_map(|walkthrough| walkthrough.steps.iter().map(|step| step.id))
                .collect::<Vec<_>>();
            walkthrough
                .steps
                .iter()
                .enumerate()
                .map(move |(index, step)| {
                    serde_json::json!({
                        "id": step.id,
                        "selector": shortest_unique_prefix(step.id, &step_ids),
                        "order": index + 1,
                        "kind": step.kind,
                        "importance": step.importance,
                        "change_id": step.change_id,
                        "title": step.title,
                        "why": step.why,
                        "body": step.body,
                        "artifacts": step.artifacts,
                        "target": step.target,
                    })
                })
        })
        .collect::<Vec<_>>();
    let paths = referenced_hunk_paths(&artifact);
    let hunks = artifact
        .files
        .iter()
        .filter(|file| paths.contains(file.path))
        .filter_map(|file| {
            file.hunks.as_ref().map(|hunks| {
                serde_json::json!({
                    "path": file.path,
                    "hunks": hunks,
                })
            })
        })
        .collect::<Vec<_>>();
    let mut session_meta = serde_json::json!({
        "repo": artifact.repo,
        "base": artifact.base,
        "rev": artifact.revision,
        "generated_at": artifact.generated_at,
    });
    if let Some(session) = &artifact.session {
        session_meta["id"] = serde_json::json!(session.id);
        if let Some(title) = session.title {
            session_meta["title"] = serde_json::json!(title);
        }
    }
    Ok(serde_json::to_string_pretty(&serde_json::json!({
        "session": session_meta,
        "action_items": items,
        "walkthrough": walkthrough,
        "reference": { "hunks": hunks },
    }))?)
}

fn comment_handoff_value(comment: &CommentArtifact<'_>) -> serde_json::Value {
    serde_json::json!({
        "id": comment.comment.id,
        "kind": comment.comment.kind,
        "action": comment.comment.action,
        "path": comment.comment.path,
        "line": comment.comment.line,
        "end_line": comment.comment.end_line,
        "body": comment.comment.body,
        "state": comment.comment.state,
        "observation": comment.comment.observation,
        "replies": comment.comment.replies,
        "excerpt": comment.excerpt,
    })
}

pub fn render_handoff_markdown(
    session: &ReviewSession,
    mut options: ArtifactBuildOptions,
) -> Result<String> {
    options.only_open = true;
    let artifact = ReviewArtifact::build_with_options(session, ArtifactProfile::Agent, options);
    Ok(to_handoff_markdown(&artifact))
}

fn active_durable_session(session: &ReviewSession) -> Option<&crate::state::ReviewSession> {
    crate::review::active_session_for_loaded_review(
        &session.sessions,
        &session.repo,
        &session.target.base,
        &session.target.rev,
    )
}

fn comment_belongs_to_session(comment: &Comment, session_id: Option<&str>) -> bool {
    session_id.map_or(comment.session_id.is_none(), |id| {
        comment.belongs_to_session(id)
    })
}

fn linked_action_item_ids<'a>(session: &'a ReviewSession, comment_id: &str) -> Vec<&'a str> {
    active_durable_session(session)
        .into_iter()
        .flat_map(|durable| durable.action_items.iter())
        .filter(|item| item.comment_ids.iter().any(|id| id == comment_id))
        .map(|item| item.id.as_str())
        .collect()
}

fn target_artifact(target: &ReviewTarget) -> TargetArtifact<'_> {
    TargetArtifact {
        file: target.file.as_deref(),
        line: target.line,
        end_line: target.end_line,
        symbol: target.symbol.as_deref(),
    }
}

fn hunk_artifact(hunk: &Hunk) -> HunkArtifact<'_> {
    HunkArtifact {
        header: &hunk.header,
        old_start: hunk.old_start,
        old_len: hunk.old_len,
        new_start: hunk.new_start,
        new_len: hunk.new_len,
        lines: hunk.lines.iter().map(excerpt_line).collect(),
    }
}

fn excerpt_line(line: &crate::diff::DiffLine) -> ExcerptLine<'_> {
    ExcerptLine {
        kind: match line.kind {
            DiffLineKind::Context => "context",
            DiffLineKind::Added => "added",
            DiffLineKind::Removed => "removed",
            DiffLineKind::Meta => "meta",
        },
        old_line: line.old_lineno,
        new_line: line.new_lineno,
        text: &line.text,
    }
}

fn comment_excerpt<'a>(
    session: &'a ReviewSession,
    comment: &Comment,
) -> Option<Vec<ExcerptLine<'a>>> {
    if comment.is_general() {
        return None;
    }
    let anchor = comment.anchor.as_ref()?;
    let file = session
        .files
        .iter()
        .find(|file| file.path == anchor.path())?;
    let (hunk_index, first_line, last_line) = match anchor {
        CommentAnchor::Line {
            hunk_index,
            line_index,
            ..
        } => (*hunk_index, *line_index, *line_index),
        CommentAnchor::Range { lines, .. } => {
            let first = lines.first()?;
            let last = lines
                .iter()
                .filter(|line| line.hunk_index == first.hunk_index)
                .map(|line| line.line_index)
                .max()
                .unwrap_or(first.line_index);
            (first.hunk_index, first.line_index, last)
        }
        CommentAnchor::File { .. } => return excerpt_for_file(file),
    };
    let hunk = file.diff.hunks.get(hunk_index)?;
    let start = first_line.saturating_sub(EXCERPT_CONTEXT_LINES);
    let end = (last_line + EXCERPT_CONTEXT_LINES + 1).min(hunk.lines.len());
    Some(hunk.lines[start..end].iter().map(excerpt_line).collect())
}

fn excerpt_for_file(file: &ReviewFile) -> Option<Vec<ExcerptLine<'_>>> {
    let hunk = file.diff.hunks.first()?;
    let end = (EXCERPT_CONTEXT_LINES * 2 + 1).min(hunk.lines.len());
    Some(hunk.lines[..end].iter().map(excerpt_line).collect())
}

pub fn write_artifact(
    session: &ReviewSession,
    format: ArtifactFormat,
    profile: ArtifactProfile,
    path: &Path,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(
        path,
        render_artifact_with_profile(session, format, profile)?,
    )?;
    Ok(())
}

pub fn import_json_artifact_into_state(
    state: &mut ReviewState,
    artifact: &OwnedReviewArtifact,
) -> ImportSummary {
    let mut summary = ImportSummary::default();
    for file in &artifact.files {
        let Some(existing) = state.files.get_mut(&file.path) else {
            continue;
        };
        if existing.fingerprint == file.fingerprint {
            let was_viewed = existing.viewed;
            existing.viewed = file.viewed;
            if file.viewed && !was_viewed {
                summary.viewed_files_imported += 1;
            }
        }
    }
    for comment in &artifact.comments {
        if let Some(existing) = state
            .comments
            .iter_mut()
            .find(|existing| existing.id == comment.id)
        {
            let before = existing.clone();
            crate::state::merge_comment_observation(existing, comment);
            crate::state::merge_comment_replies(existing, comment);
            if comment.updated_at > existing.updated_at {
                let replies = existing.replies.clone();
                let observation = existing.observation.clone();
                *existing = comment.clone();
                existing.replies = replies;
                existing.observation = observation;
            }
            if *existing == before {
                summary.duplicate_comments_skipped += 1;
            } else {
                summary.comments_imported += 1;
            }
        } else {
            state.comments.push(comment.clone());
            summary.comments_imported += 1;
        }
    }
    summary
}

#[cfg(test)]
pub fn render_artifact(session: &ReviewSession, format: ArtifactFormat) -> Result<String> {
    render_artifact_with_profile(session, format, ArtifactProfile::Human)
}

pub fn render_artifact_with_profile(
    session: &ReviewSession,
    format: ArtifactFormat,
    profile: ArtifactProfile,
) -> Result<String> {
    render_artifact_with_options(session, format, profile, ArtifactBuildOptions::default())
}

pub fn render_artifact_with_options(
    session: &ReviewSession,
    format: ArtifactFormat,
    profile: ArtifactProfile,
    options: ArtifactBuildOptions,
) -> Result<String> {
    let artifact = ReviewArtifact::build_with_options(session, profile, options);
    match format {
        ArtifactFormat::Json => Ok(serde_json::to_string_pretty(&artifact)?),
        ArtifactFormat::Markdown => Ok(to_markdown(&artifact)),
    }
}

pub fn action_item_count(artifact: &ReviewArtifact<'_>) -> usize {
    ordered_open_work(artifact).len()
}

pub fn write_artifact_to(
    session: &ReviewSession,
    format: ArtifactFormat,
    profile: ArtifactProfile,
    mut writer: impl Write,
) -> Result<()> {
    let body = render_artifact_with_profile(session, format, profile)?;
    writer.write_all(body.as_bytes())?;
    if !body.ends_with('\n') {
        writer.write_all(b"\n")?;
    }
    Ok(())
}

fn to_markdown(artifact: &ReviewArtifact<'_>) -> String {
    if artifact.profile == Some("agent") {
        to_agent_markdown(artifact)
    } else {
        to_human_markdown(artifact)
    }
}

fn to_human_markdown(artifact: &ReviewArtifact<'_>) -> String {
    let mut out = String::new();
    out.push_str("# jj change review\n\n");
    write_header(artifact, &mut out);
    out.push_str("## Files\n\n");
    for file in &artifact.files {
        out.push_str(&format!(
            "- [{}] `{}` — {}{} (+{}/-{})\n",
            if file.viewed { "x" } else { " " },
            file.path,
            file.status,
            if file.generated {
                " [generated/noisy]"
            } else {
                ""
            },
            file.additions,
            file.deletions
        ));
    }
    out.push_str("\n## Comments\n\n");
    if artifact.comments.is_empty() {
        out.push_str("No comments recorded.\n");
    } else {
        for comment in &artifact.comments {
            write_comment_heading(&mut out, comment.comment);
            write_comment_provenance(&mut out, comment.comment, "");
            out.push_str(&format!("Status: {}\n\n", comment.comment.state.label()));
            out.push_str(comment.comment.body.trim());
            out.push_str("\n\n");
            write_comment_replies(&mut out, comment.comment);
        }
    }
    out.push_str("\n## Action items\n\n");
    write_all_durable_items(artifact, &mut out);
    out.push_str("\n## Walkthrough\n\n");
    write_walkthroughs(&mut out, artifact);
    out
}

fn to_agent_markdown(artifact: &ReviewArtifact<'_>) -> String {
    let mut out = String::new();
    out.push_str("# Review session export (agent profile)\n\n");
    out.push_str("**Complete export artifact.** Open work is folded below; closed history and full reference hunks remain available for archival and tooling. For a one-shot implementation prompt, use `gander handoff`.\n\n");
    write_header(artifact, &mut out);
    write_open_work(artifact, &mut out);
    out.push_str("\n## Closed action items\n\n");
    let mut closed = false;
    for item in artifact
        .action_items
        .iter()
        .filter(|item| item.status == ActionItemStatus::Closed)
    {
        closed = true;
        write_durable_item(item, &mut out);
    }
    if !closed {
        out.push_str("No closed action items recorded.\n");
    }
    out.push_str("\n## Walkthrough\n\n");
    write_walkthroughs(&mut out, artifact);
    out.push_str("\n## Other comments\n\n");
    write_other_comments(artifact, &mut out);
    out.push_str("\n## Reference: full hunks\n\n");
    write_full_hunks(artifact, &mut out);
    out
}

fn to_handoff_markdown(artifact: &ReviewArtifact<'_>) -> String {
    let mut out = String::new();
    out.push_str("# Human review handoff for a coding agent\n\n");
    out.push_str("Implement the open action items below. Linked todo comments are supporting evidence nested beneath their durable action item; they are intentionally not repeated as standalone entries. Then use the walkthrough and trimmed reference hunks as context.\n\n");
    write_header(artifact, &mut out);
    write_open_work(artifact, &mut out);
    out.push_str("\n## Walkthrough\n\n");
    write_walkthroughs(&mut out, artifact);
    out.push_str("\n## Reference: action/walkthrough hunks\n\n");
    write_limited_hunks(artifact, &mut out);
    out
}

fn write_header(artifact: &ReviewArtifact<'_>, out: &mut String) {
    out.push_str(&format!("- Repository: `{}`\n", artifact.repo.display()));
    out.push_str(&format!(
        "- Target: `{}` → `{}`\n",
        artifact.base, artifact.revision
    ));
    out.push_str(&format!("- Generated: `{}`\n", artifact.generated_at));
    out.push_str(&format!("- Summary: {}\n", artifact.summary));
    if let Some(session) = &artifact.session
        && let Some(title) = session.title
    {
        out.push_str(&format!("- Session: {}\n", title));
    }
    out.push_str(&format!(
        "- Open work: {} item(s) ({} durable action item(s), {} standalone todo comment(s))\n\n",
        action_item_count(artifact),
        open_durable_items(artifact).count(),
        remaining_todo_comments(artifact).count()
    ));
}

fn write_all_durable_items(artifact: &ReviewArtifact<'_>, out: &mut String) {
    if artifact.action_items.is_empty() {
        out.push_str("No durable action items recorded.\n");
        return;
    }
    for item in &artifact.action_items {
        write_durable_item(item, out);
    }
}

fn write_durable_item(item: &ActionItemArtifact<'_>, out: &mut String) {
    out.push_str(&format!(
        "- [{}] `{}` — {}",
        status_mark(item.status),
        item.id,
        item.title
    ));
    if let Some(action) = item.action {
        out.push_str(&format!(" ({})", action_label(action)));
    }
    if let Some(target) = &item.target {
        write_target_suffix(out, target);
    }
    out.push('\n');
    if let Some(body) = item.body.filter(|body| !body.trim().is_empty()) {
        out.push_str(&format!("  Body: {}\n", body.trim()));
    }
    if let Some(disposition) = item.disposition {
        out.push_str(&format!("  Disposition: {disposition:?}\n"));
    }
    if let Some(outcome) = item.outcome {
        out.push_str(&format!("  Outcome: {}\n", outcome.trim()));
    }
    for ticket in item.external_tickets {
        out.push_str(&format!(
            "  External ticket: {} {}{}\n",
            ticket.tracker,
            ticket.reference,
            ticket
                .url
                .as_deref()
                .map(|url| format!(" ({url})"))
                .unwrap_or_default()
        ));
    }
}

fn status_mark(status: ActionItemStatus) -> &'static str {
    match status {
        ActionItemStatus::Open => " ",
        ActionItemStatus::Closed => "x",
    }
}

fn write_open_work(artifact: &ReviewArtifact<'_>, out: &mut String) {
    out.push_str("## Action items\n\n");
    out.push_str("Ordered by action priority, then path and line.\n\n");
    let globals = handoff_globals(artifact);
    let item_ids = artifact
        .action_items
        .iter()
        .map(|item| item.id)
        .collect::<Vec<_>>();
    let comment_ids = artifact
        .comments
        .iter()
        .map(|comment| comment.comment.id.as_str())
        .collect::<Vec<_>>();
    let ordered = ordered_open_work(artifact);
    if ordered.is_empty() {
        out.push_str("No open action items.\n");
        return;
    }
    for entry in ordered {
        match entry {
            OrderedOpenWork::Durable(item) => {
                let selector = shortest_unique_prefix(item.id, &item_ids);
                out.push_str(&format!(
                    "- [action-item][{}][{}] {}",
                    item.action.map(action_label).unwrap_or("none"),
                    selector,
                    item.title
                ));
                if let Some(target) = &item.target {
                    write_target_suffix(out, target);
                }
                out.push('\n');
                if let Some(body) = item.body.filter(|body| !body.trim().is_empty()) {
                    out.push_str(&format!("  Body: {}\n", body.trim()));
                }
                for comment in evidence_comments(artifact, item) {
                    out.push_str("  - Evidence comment ");
                    write_comment_location_inline(out, comment.comment);
                    out.push_str(": ");
                    out.push_str(comment.comment.body.trim());
                    out.push('\n');
                    write_comment_provenance(out, comment.comment, "    ");
                    write_comment_replies_indented(out, comment.comment, "    ");
                    write_excerpt_indented(out, comment.excerpt.as_deref(), "    ");
                }
                out.push_str(&format!(
                    "  Close: `gander {globals} action-items close {selector} --disposition completed --outcome <text>`\n"
                ));
            }
            OrderedOpenWork::Comment(comment) => {
                out.push_str(&format!(
                    "- [comment][{}][{}] ",
                    kind_label(comment.comment.kind.unwrap_or(CommentKind::Note)),
                    comment.comment.action.map(action_label).unwrap_or("none")
                ));
                if comment.comment.path.is_some() {
                    write_comment_location_inline(out, comment.comment);
                    out.push_str(" — ");
                }
                out.push_str(comment.comment.body.trim());
                let selector = shortest_unique_prefix(&comment.comment.id, &comment_ids);
                out.push_str(&format!(
                    "\n  Selector: `{selector}`; reply: `gander {globals} comments reply {selector} --body <text>`; resolve: `gander {globals} comments resolve {selector} --reply <text>`\n"
                ));
                write_comment_provenance(out, comment.comment, "  ");
                write_comment_replies(out, comment.comment);
                write_excerpt(out, comment.excerpt.as_deref());
            }
        }
    }
}

fn handoff_globals(artifact: &ReviewArtifact<'_>) -> String {
    [
        format!(
            "--repo {}",
            shell_quote(&artifact.repo.display().to_string())
        ),
        format!("--base {}", shell_quote(artifact.base)),
        format!("--rev {}", shell_quote(artifact.revision)),
    ]
    .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_other_comments(artifact: &ReviewArtifact<'_>, out: &mut String) {
    let nested_ids = nested_todo_comment_ids(artifact);
    let mut wrote = false;
    for comment in &artifact.comments {
        if comment.comment.state == CommentState::Todo
            && (nested_ids.contains(comment.comment.id.as_str())
                || comment.linked_action_item_ids.is_empty())
        {
            continue;
        }
        wrote = true;
        write_comment_heading(out, comment.comment);
        write_comment_provenance(out, comment.comment, "");
        out.push_str(&format!(
            "Status: {}; kind: {}; action: {}\n\n",
            comment.comment.state.label(),
            kind_label(comment.comment.kind.unwrap_or(CommentKind::Note)),
            comment.comment.action.map(action_label).unwrap_or("none")
        ));
        out.push_str(comment.comment.body.trim());
        out.push_str("\n\n");
        write_comment_replies(out, comment.comment);
        write_excerpt(out, comment.excerpt.as_deref());
    }
    if !wrote {
        out.push_str("No other comments recorded.\n");
    }
}

fn write_full_hunks(artifact: &ReviewArtifact<'_>, out: &mut String) {
    for file in &artifact.files {
        if let Some(hunks) = &file.hunks {
            write_file_hunks(out, file.path, hunks);
        }
    }
}

fn write_limited_hunks(artifact: &ReviewArtifact<'_>, out: &mut String) {
    let paths = referenced_hunk_paths(artifact);
    let mut wrote = false;
    for file in &artifact.files {
        if paths.contains(file.path)
            && let Some(hunks) = &file.hunks
        {
            wrote = true;
            write_file_hunks(out, file.path, hunks);
        }
    }
    if !wrote {
        out.push_str("No action-item or walkthrough file hunks recorded.\n");
    }
}

fn write_file_hunks(out: &mut String, path: &str, hunks: &[HunkArtifact<'_>]) {
    out.push_str(&format!("### `{path}`\n\n"));
    for hunk in hunks {
        out.push_str(&format!("#### {}\n\n```diff\n", hunk.header));
        for line in &hunk.lines {
            out.push_str(match line.kind {
                "added" => "+",
                "removed" => "-",
                _ => " ",
            });
            out.push_str(line.text);
            out.push('\n');
        }
        out.push_str("```\n\n");
    }
}

fn referenced_hunk_paths<'a>(artifact: &'a ReviewArtifact<'a>) -> BTreeSet<&'a str> {
    let mut paths = BTreeSet::new();
    for entry in ordered_open_work(artifact) {
        match entry {
            OrderedOpenWork::Durable(item) => {
                if let Some(file) = item.target.as_ref().and_then(|target| target.file) {
                    paths.insert(file);
                }
                for comment in evidence_comments(artifact, item) {
                    if let Some(path) = comment.comment.path.as_deref() {
                        paths.insert(path);
                    }
                }
            }
            OrderedOpenWork::Comment(comment) => {
                if let Some(path) = comment.comment.path.as_deref() {
                    paths.insert(path);
                }
            }
        }
    }
    for walkthrough in &artifact.walkthroughs {
        for step in &walkthrough.steps {
            if let Some(file) = step.target.file {
                paths.insert(file);
            }
        }
    }
    paths
}

fn open_durable_items<'a>(
    artifact: &'a ReviewArtifact<'a>,
) -> impl Iterator<Item = &'a ActionItemArtifact<'a>> {
    artifact
        .action_items
        .iter()
        .filter(|item| item.status == ActionItemStatus::Open)
}

fn nested_todo_comment_ids<'a>(artifact: &'a ReviewArtifact<'a>) -> BTreeSet<&'a str> {
    open_durable_items(artifact)
        .flat_map(|item| evidence_comments(artifact, item))
        .map(|comment| comment.comment.id.as_str())
        .collect()
}

fn remaining_todo_comments<'a>(
    artifact: &'a ReviewArtifact<'a>,
) -> impl Iterator<Item = &'a CommentArtifact<'a>> {
    let nested = nested_todo_comment_ids(artifact);
    artifact.comments.iter().filter(move |comment| {
        comment.comment.state == CommentState::Todo && !nested.contains(comment.comment.id.as_str())
    })
}

fn evidence_comments<'a>(
    artifact: &'a ReviewArtifact<'a>,
    item: &ActionItemArtifact<'_>,
) -> Vec<&'a CommentArtifact<'a>> {
    item.comment_ids
        .iter()
        .filter_map(|id| {
            artifact.comments.iter().find(|comment| {
                comment.comment.id == *id && comment.comment.state == CommentState::Todo
            })
        })
        .collect()
}

enum OrderedOpenWork<'a> {
    Durable(&'a ActionItemArtifact<'a>),
    Comment(&'a CommentArtifact<'a>),
}

fn action_priority(action: Option<ActionIntent>) -> u8 {
    match action {
        Some(ActionIntent::Fix) => 0,
        Some(ActionIntent::Test) => 1,
        Some(ActionIntent::FollowUp) => 2,
        Some(ActionIntent::Explain) => 3,
        Some(ActionIntent::None) | None => 4,
    }
}

fn ordered_open_work<'a>(artifact: &'a ReviewArtifact<'a>) -> Vec<OrderedOpenWork<'a>> {
    let mut indexed = Vec::new();
    for (index, item) in open_durable_items(artifact).enumerate() {
        indexed.push((
            action_priority(item.action),
            item.target
                .as_ref()
                .and_then(|target| target.file)
                .unwrap_or("~"),
            item.target
                .as_ref()
                .and_then(|target| target.line)
                .unwrap_or(usize::MAX),
            index,
            OrderedOpenWork::Durable(item),
        ));
    }
    let offset = indexed.len();
    for (index, comment) in remaining_todo_comments(artifact).enumerate() {
        indexed.push((
            action_priority(comment.comment.action),
            comment.comment.path.as_deref().unwrap_or("~"),
            comment.comment.line.unwrap_or(usize::MAX),
            offset + index,
            OrderedOpenWork::Comment(comment),
        ));
    }
    indexed.sort_by_key(|(priority, path, line, stable, _)| {
        (*priority, *path == "~", *path, *line, *stable)
    });
    indexed
        .into_iter()
        .map(|(_, _, _, _, entry)| entry)
        .collect()
}

fn write_walkthroughs(out: &mut String, artifact: &ReviewArtifact<'_>) {
    if artifact.walkthroughs.is_empty() {
        out.push_str("No walkthrough steps recorded.\n");
        return;
    }
    for walkthrough in &artifact.walkthroughs {
        if let Some(title) = walkthrough
            .title
            .filter(|title| !title.eq_ignore_ascii_case("walkthrough"))
        {
            out.push_str(&format!("### {}\n\n", title));
        }
        for (index, step) in walkthrough.steps.iter().enumerate() {
            out.push_str(&format!("{}. {}", index + 1, step.title.unwrap_or(step.id)));
            write_target_suffix(out, &step.target);
            out.push_str("\n\n");
            if let Some(why) = step.why {
                out.push_str(&format!("Why: {}\n\n", why.trim()));
            }
            if let Some(body) = step.body {
                out.push_str(body.trim());
                out.push_str("\n\n");
            }
        }
    }
}

fn write_excerpt(out: &mut String, excerpt: Option<&[ExcerptLine<'_>]>) {
    write_excerpt_indented(out, excerpt, "");
}

fn write_excerpt_indented(out: &mut String, excerpt: Option<&[ExcerptLine<'_>]>, indent: &str) {
    let Some(lines) = excerpt.filter(|lines| !lines.is_empty()) else {
        return;
    };
    out.push_str(&format!("{indent}```diff\n"));
    for line in lines {
        out.push_str(indent);
        out.push_str(match line.kind {
            "added" => "+",
            "removed" => "-",
            _ => " ",
        });
        if let Some(line_no) = line.new_line.or(line.old_line) {
            out.push_str(&format!("{:>4} ", line_no));
        }
        out.push_str(line.text);
        out.push('\n');
    }
    out.push_str(&format!("{indent}```\n"));
}

fn write_comment_location_inline(out: &mut String, comment: &Comment) {
    let Some(path) = comment.path.as_deref() else {
        out.push_str("general");
        return;
    };
    match comment.line {
        Some(line) => out.push_str(&format!("`{path}`:{line}")),
        None => out.push_str(&format!("`{path}`")),
    }
}

fn kind_label(kind: CommentKind) -> &'static str {
    match kind {
        CommentKind::Note => "note",
        CommentKind::Issue => "issue",
        CommentKind::Question => "question",
        CommentKind::Praise => "praise",
    }
}

fn action_label(action: ActionIntent) -> &'static str {
    match action {
        ActionIntent::None => "none",
        ActionIntent::Fix => "fix",
        ActionIntent::Explain => "explain",
        ActionIntent::Test => "test",
        ActionIntent::FollowUp => "follow-up",
    }
}

fn write_target_suffix(out: &mut String, target: &TargetArtifact<'_>) {
    if let Some(file) = target.file {
        out.push_str(&format!(" — `{file}`"));
        if let Some(line) = target.line {
            out.push_str(&format!(":{line}"));
            if let Some(end_line) = target.end_line.filter(|end| *end != line) {
                out.push_str(&format!("-{end_line}"));
            }
        }
    }
    if let Some(symbol) = target.symbol {
        out.push_str(&format!(" `{symbol}`"));
    }
}

fn write_comment_heading(out: &mut String, comment: &Comment) {
    out.push_str(&format!("<!-- comment-id: {} -->\n", comment.id));
    match comment.anchor.as_ref() {
        Some(CommentAnchor::Line {
            path,
            side,
            line,
            hunk_header,
            line_text,
            line_kind,
            diff_fingerprint,
            ..
        }) => {
            out.push_str(&format!("### `{path}`:{}:{line}\n\n", side.label()));
            out.push_str(&format!("Anchor: `{hunk_header}`  \n"));
            out.push_str(&format!("Diff fingerprint: `{diff_fingerprint}`\n\n"));
            out.push_str("```diff\n");
            out.push_str(match line_kind.as_str() {
                "added" => "+",
                "removed" => "-",
                _ => " ",
            });
            out.push_str(line_text);
            out.push_str("\n```\n\n");
        }
        Some(CommentAnchor::Range {
            path,
            start_line,
            end_line,
            lines,
            diff_fingerprint,
            range_fingerprint,
            ..
        }) => {
            out.push_str(&format!("### `{path}`:range:{start_line}-{end_line}\n\n"));
            out.push_str(&format!("Diff fingerprint: `{diff_fingerprint}`\n"));
            out.push_str(&format!("Range fingerprint: `{range_fingerprint}`\n\n"));
            out.push_str("```diff\n");
            for line in lines {
                out.push_str(match line.line_kind.as_str() {
                    "added" => "+",
                    "removed" => "-",
                    _ => " ",
                });
                out.push_str(&format!("{:>4} {}\n", line.line, line.line_text));
            }
            out.push_str("```\n\n");
        }
        Some(CommentAnchor::File { path, .. }) => out.push_str(&format!("### `{path}`\n\n")),
        None => match (comment.path.as_deref(), comment.line) {
            (Some(path), Some(line)) => out.push_str(&format!("### `{path}`:{line}\n\n")),
            (Some(path), None) => out.push_str(&format!("### `{path}`\n\n")),
            (None, _) => out.push_str("### General comment\n\n"),
        },
    }
}

fn write_comment_replies(out: &mut String, comment: &Comment) {
    write_comment_replies_indented(out, comment, "");
}

fn write_comment_replies_indented(out: &mut String, comment: &Comment, indent: &str) {
    if comment.replies.is_empty() {
        return;
    }
    out.push_str(&format!("{indent}Replies:\n"));
    for reply in &comment.replies {
        out.push_str(&format!(
            "{indent}- `{}` at `{}`: {}\n",
            reply.id,
            reply.created_at,
            reply.body.trim()
        ));
        match &reply.result {
            Some(result) => {
                let against = result
                    .observation_aggregate_fingerprint
                    .as_deref()
                    .unwrap_or("null (legacy observation unavailable)");
                out.push_str(&format!(
                    "{indent}  Result snapshot: `{}`; against: `{against}`; relation: {}; portable patch changed: {}.\n",
                    result.snapshot.scope.aggregate,
                    related_transition_label(&result.related),
                    result
                        .portable_patch_changed
                        .map(|changed| if changed { "yes" } else { "no" })
                        .unwrap_or("unknown"),
                ));
            }
            None => out.push_str(&format!(
                "{indent}  Result snapshot unavailable (legacy reply).\n"
            )),
        }
    }
    out.push('\n');
}

fn write_comment_provenance(out: &mut String, comment: &Comment, indent: &str) {
    match &comment.observation {
        Some(observation) => out.push_str(&format!(
            "{indent}Observation snapshot: `{}` (scope v{}, captured `{}`).\n",
            observation.snapshot.scope.aggregate,
            observation.snapshot.scope.version,
            observation.snapshot.captured_at,
        )),
        None => out.push_str(&format!(
            "{indent}Observation snapshot unavailable (legacy comment; target labels are context, not proof).\n"
        )),
    }
}

fn related_transition_label(related: &crate::provenance::RelatedTransition) -> String {
    match related {
        crate::provenance::RelatedTransition::SamePath { path } => format!("same_path `{path}`"),
        crate::provenance::RelatedTransition::RenamedFrom { old_path, path } => {
            format!("renamed_from `{old_path}` to `{path}`")
        }
        crate::provenance::RelatedTransition::NotInDiff { path } => path
            .as_deref()
            .map(|path| format!("not_in_diff `{path}`"))
            .unwrap_or_else(|| "not_in_diff (general comment)".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::ReviewSession,
        diff::DiffSet,
        jj::ReviewTarget as JjReviewTarget,
        state::{ActionItem, CommentReply, FileState, ReviewSessionStatus, Walkthrough},
    };

    fn fixture() -> ReviewSession {
        let mut state = ReviewState::default();
        state.sessions.extend([
            crate::state::ReviewSession {
                id: "active".into(),
                title: Some("Current review".into()),
                target: ReviewTarget {
                    repo: Some("/repo".into()),
                    base: Some("main".into()),
                    revision: Some("@".into()),
                    ..Default::default()
                },
                status: ReviewSessionStatus::Open,
                action_items: vec![ActionItem {
                    id: "action-open".into(),
                    title: "Fix parser behavior".into(),
                    body: Some("Cover every edge case.".into()),
                    target: Some(ReviewTarget {
                        file: Some("a.txt".into()),
                        line: Some(1),
                        ..Default::default()
                    }),
                    action: Some(ActionIntent::Fix),
                    comment_ids: vec!["linked".into(), "draft-linked".into()],
                    external_tickets: vec![ExternalTicket {
                        tracker: "linear".into(),
                        reference: "GAN-7".into(),
                        url: Some("https://example.test/GAN-7".into()),
                        created_at: chrono::DateTime::UNIX_EPOCH,
                        updated_at: chrono::DateTime::UNIX_EPOCH,
                    }],
                    ..Default::default()
                }],
                walkthroughs: vec![Walkthrough {
                    id: "walk".into(),
                    steps: vec![crate::state::WalkthroughStep {
                        id: "step".into(),
                        title: Some("Read parser".into()),
                        target: ReviewTarget {
                            file: Some("a.txt".into()),
                            line: Some(1),
                            ..Default::default()
                        },
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
            crate::state::ReviewSession {
                id: "foreign-session".into(),
                action_items: vec![ActionItem {
                    id: "foreign-action".into(),
                    title: "Must not render".into(),
                    ..Default::default()
                }],
                walkthroughs: vec![Walkthrough {
                    id: "foreign-walk".into(),
                    title: Some("Foreign walkthrough".into()),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ]);
        state.comments.extend([
            Comment {
                id: "linked".into(),
                session_id: Some("active".into()),
                path: Some("a.txt".into()),
                line: Some(1),
                body: "linked evidence body".into(),
                kind: Some(CommentKind::Issue),
                action: Some(ActionIntent::Fix),
                state: CommentState::Todo,
                replies: vec![CommentReply {
                    id: "reply".into(),
                    body: "extra evidence".into(),
                    author: crate::state::Identity::agent(),
                    created_at: chrono::DateTime::UNIX_EPOCH,
                    result: None,
                }],
                ..Default::default()
            },
            Comment {
                id: "draft-linked".into(),
                session_id: Some("active".into()),
                path: Some("a.txt".into()),
                body: "withheld draft".into(),
                state: CommentState::Draft,
                ..Default::default()
            },
            Comment {
                id: "standalone".into(),
                session_id: Some("active".into()),
                path: None,
                body: "standalone todo".into(),
                state: CommentState::Todo,
                ..Default::default()
            },
            Comment {
                id: "foreign-comment".into(),
                session_id: Some("foreign-session".into()),
                path: Some("a.txt".into()),
                body: "foreign comment body".into(),
                state: CommentState::Todo,
                ..Default::default()
            },
        ]);
        ReviewSession::new(
            "/repo".into(),
            JjReviewTarget::new("main", "@"),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        )
    }

    #[test]
    fn full_artifact_schema_nine_exposes_durable_action_item_fields_and_links() {
        let mut session = fixture();
        let closed = ActionItem {
            id: "action-closed".into(),
            title: "Historical result".into(),
            status: ActionItemStatus::Closed,
            disposition: Some(ClosedDisposition::Deferred),
            outcome: Some("Moved to the tracker".into()),
            closed_at: Some(chrono::DateTime::UNIX_EPOCH),
            external_tickets: vec![ExternalTicket {
                tracker: "sourcehut".into(),
                reference: "~user/project#1".into(),
                url: None,
                created_at: chrono::DateTime::UNIX_EPOCH,
                updated_at: chrono::DateTime::UNIX_EPOCH,
            }],
            comment_ids: vec!["linked".into()],
            ..Default::default()
        };
        session.sessions[0].action_items.push(closed);

        let value: serde_json::Value = serde_json::from_str(
            &render_artifact_with_profile(&session, ArtifactFormat::Json, ArtifactProfile::Agent)
                .unwrap(),
        )
        .unwrap();

        assert_eq!(value["version"], ARTIFACT_SCHEMA_VERSION);
        assert_eq!(value["action_items"].as_array().unwrap().len(), 2);
        assert_eq!(value["action_items"][0]["comment_ids"][0], "linked");
        assert!(value["action_items"][0].get("linked_comment_ids").is_none());
        assert!(value["action_items"][0].get("close_disposition").is_none());
        assert_eq!(
            value["comments"][0]["linked_action_item_ids"][0],
            "action-open"
        );
        assert_eq!(value["action_items"][1]["disposition"], "deferred");
        assert_eq!(value["action_items"][1]["outcome"], "Moved to the tracker");
        assert_eq!(
            value["action_items"][1]["external_tickets"][0]["reference"],
            "~user/project#1"
        );
        assert_eq!(
            value["action_items"][1]["closed_at"],
            "1970-01-01T00:00:00Z"
        );
        assert_eq!(value["walkthroughs"][0]["steps"][0]["kind"], "step");
        assert_eq!(
            value["walkthroughs"][0]["steps"][0]["importance"],
            "spotlight"
        );
        assert!(
            value["walkthroughs"][0]["steps"][0]
                .get("change_id")
                .is_none()
        );
    }

    #[test]
    fn handoff_json_folds_linked_todo_evidence_without_standalone_duplicate() {
        let session = fixture();
        let value: serde_json::Value = serde_json::from_str(
            &render_handoff_json(&session, ArtifactBuildOptions::default()).unwrap(),
        )
        .unwrap();
        let items = value["action_items"].as_array().unwrap();

        assert_eq!(items.len(), 2);
        let durable = items
            .iter()
            .find(|item| item["source"] == "action_item")
            .unwrap();
        assert_eq!(durable["id"], "action-open");
        assert_eq!(durable["evidence_comments"].as_array().unwrap().len(), 1);
        assert_eq!(durable["evidence_comments"][0]["id"], "linked");
        assert_eq!(
            durable["evidence_comments"][0]["replies"][0]["body"],
            "extra evidence"
        );
        assert!(items.iter().all(|item| item["id"] != "linked"));
        assert!(value.to_string().contains("standalone todo"));
        assert!(!value.to_string().contains("withheld draft"));
    }

    #[test]
    fn handoff_markdown_folds_evidence_and_uses_action_item_close_command() {
        let markdown =
            render_handoff_markdown(&fixture(), ArtifactBuildOptions::default()).unwrap();

        assert_eq!(markdown.matches("linked evidence body").count(), 1);
        assert!(markdown.contains("Evidence comment `a.txt`:1: linked evidence body"));
        assert!(markdown.contains("action-items close"));
        assert!(markdown.contains("--disposition completed"));
        assert!(markdown.contains("standalone todo"));
        assert!(!markdown.contains("withheld draft"));
        assert!(!markdown.contains("foreign comment body"));
    }

    #[test]
    fn only_open_excludes_closed_history_and_non_todo_comments() {
        let mut session = fixture();
        session.sessions[0].action_items.push(ActionItem {
            id: "closed".into(),
            title: "closed history".into(),
            status: ActionItemStatus::Closed,
            disposition: Some(ClosedDisposition::Completed),
            ..Default::default()
        });
        let value: serde_json::Value = serde_json::from_str(
            &render_artifact_with_options(
                &session,
                ArtifactFormat::Json,
                ArtifactProfile::Agent,
                ArtifactBuildOptions { only_open: true },
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(value["action_items"].as_array().unwrap().len(), 1);
        assert!(value.to_string().contains("linked evidence body"));
        assert!(!value.to_string().contains("withheld draft"));
        assert!(!value.to_string().contains("closed history"));
    }

    #[test]
    fn artifact_and_walkthroughs_are_scoped_to_active_session() {
        let value: serde_json::Value =
            serde_json::from_str(&render_artifact(&fixture(), ArtifactFormat::Json).unwrap())
                .unwrap();

        assert_eq!(value["session"]["id"], "active");
        assert_eq!(value["action_items"].as_array().unwrap().len(), 1);
        assert_eq!(value["walkthroughs"].as_array().unwrap().len(), 1);
        assert!(!value.to_string().contains("foreign-action"));
        assert!(!value.to_string().contains("Foreign walkthrough"));
        assert!(!value.to_string().contains("foreign comment body"));
    }

    #[test]
    fn artifact_active_session_requires_matching_repo_identity() {
        let mut session = fixture();
        let mut wrong = session.sessions[0].clone();
        wrong.id = "wrong-repo".into();
        wrong.target.repo = Some("/other-repo".into());
        wrong.title = Some("Wrong repo".into());
        wrong.action_items[0].title = "wrong repo action".into();
        session.sessions.insert(0, wrong);

        let value: serde_json::Value =
            serde_json::from_str(&render_artifact(&session, ArtifactFormat::Json).unwrap())
                .unwrap();

        assert_eq!(value["session"]["id"], "active");
        assert!(!value.to_string().contains("wrong repo action"));
    }

    #[test]
    fn agent_profile_contains_hunks_and_handoff_limits_general_comment_reference() {
        let session = fixture();
        let full: serde_json::Value = serde_json::from_str(
            &render_artifact_with_profile(&session, ArtifactFormat::Json, ArtifactProfile::Agent)
                .unwrap(),
        )
        .unwrap();
        assert!(full["files"][0]["hunks"].is_array());

        let handoff: serde_json::Value = serde_json::from_str(
            &render_handoff_json(&session, ArtifactBuildOptions::default()).unwrap(),
        )
        .unwrap();
        assert_eq!(handoff["reference"]["hunks"][0]["path"], "a.txt");
        let standalone = handoff["action_items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["id"] == "standalone")
            .unwrap();
        assert!(standalone["path"].is_null());
        assert!(standalone["excerpt"].is_null());
    }

    #[test]
    fn artifact_v8_import_defaults_legacy_authors_and_state_derived_channel() {
        let artifact: OwnedReviewArtifact = serde_json::from_str(
            r#"{
  "version": 8,
  "base": "trunk()",
  "revision": "@",
  "files": [{ "path": "src/lib.rs", "viewed": true, "fingerprint": "abc" }],
  "comments": [{
    "id": "legacy",
    "path": "src/lib.rs",
    "body": "legacy body",
    "state": "todo",
    "created_at": "2026-06-30T00:00:00Z",
    "replies": [{
      "id": "legacy-reply",
      "body": "legacy result",
      "created_at": "2026-06-30T00:01:00Z"
    }]
  }]
}"#,
        )
        .unwrap();
        let mut state = ReviewState::default();
        state.files.insert(
            "src/lib.rs".into(),
            FileState {
                fingerprint: "abc".into(),
                ..Default::default()
            },
        );

        let summary = import_json_artifact_into_state(&mut state, &artifact);

        assert_eq!(artifact.version, 8);
        assert_eq!(summary.viewed_files_imported, 1);
        assert_eq!(summary.comments_imported, 1);
        assert_eq!(state.comments[0].session_id, None);
        assert_eq!(
            state.comments[0].author,
            crate::state::Identity::local_human()
        );
        assert_eq!(state.comments[0].channel, crate::state::Channel::Delegation);
        assert_eq!(
            state.comments[0].replies[0].author,
            crate::state::Identity::local_human()
        );
        assert!(state.comments[0].observation.is_none());
        assert!(state.comments[0].replies[0].result.is_none());
    }

    #[test]
    fn artifact_v9_import_export_preserves_foreign_authors_and_channel() {
        let artifact: OwnedReviewArtifact = serde_json::from_str(
            r#"{
  "version": 9,
  "base": "trunk()",
  "revision": "@",
  "comments": [{
    "id": "foreign",
    "body": "team feedback",
    "state": "draft",
    "author": { "kind": "human", "name": "Alice" },
    "channel": "collaboration",
    "created_at": "2026-06-30T00:00:00Z",
    "replies": [{
      "id": "foreign-reply",
      "body": "acknowledged",
      "author": { "kind": "human", "name": "Bob" },
      "created_at": "2026-06-30T00:01:00Z"
    }]
  }]
}"#,
        )
        .unwrap();
        let mut state = ReviewState::default();

        let summary = import_json_artifact_into_state(&mut state, &artifact);

        assert_eq!(summary.comments_imported, 1);
        assert_eq!(state.comments[0].author.name, "Alice");
        assert_eq!(
            state.comments[0].channel,
            crate::state::Channel::Collaboration
        );
        assert_eq!(state.comments[0].replies[0].author.name, "Bob");

        let session = ReviewSession::new(
            ".".into(),
            crate::jj::ReviewTarget::trunk_to_current(),
            crate::diff::DiffSet::parse("").unwrap(),
            state,
        );
        let exported: serde_json::Value =
            serde_json::from_str(&render_artifact(&session, ArtifactFormat::Json).unwrap())
                .unwrap();
        let comment = exported["comments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|comment| comment["id"] == "foreign")
            .unwrap();
        assert_eq!(exported["version"], 9);
        assert_eq!(
            comment["author"],
            serde_json::json!({ "kind": "human", "name": "Alice" })
        );
        assert_eq!(comment["channel"], "collaboration");
        assert_eq!(
            comment["replies"][0]["author"],
            serde_json::json!({ "kind": "human", "name": "Bob" })
        );
    }

    #[test]
    fn artifact_handoff_and_markdown_carry_provenance_and_missing_language() {
        let mut session = fixture();
        let snapshot = crate::provenance::SnapshotEvidence::capture(
            chrono::DateTime::UNIX_EPOCH,
            "active",
            session.sessions[0].target.clone(),
            session.files.iter().map(|file| &file.diff),
        );
        let observation = crate::provenance::CommentObservation::new(
            snapshot.clone(),
            Some(crate::anchor::CommentAnchor::File {
                path: "a.txt".into(),
                old_path: None,
                diff_fingerprint: session.files[0].fingerprint.clone(),
            }),
        );
        session.comments[0].observation = Some(observation.clone());
        session.comments[0].replies[0].result =
            Some(crate::provenance::CommentReplyResult::compare(
                "linked",
                Some(&observation),
                Some("a.txt"),
                snapshot,
            ));

        let json: serde_json::Value =
            serde_json::from_str(&render_artifact(&session, ArtifactFormat::Json).unwrap())
                .unwrap();
        assert_eq!(json["version"], ARTIFACT_SCHEMA_VERSION);
        assert!(json["comments"][0]["observation"]["snapshot"]["scope"]["aggregate"].is_string());
        assert!(json["comments"][0]["replies"][0]["result"]["snapshot"].is_object());

        let handoff = render_handoff_json(&session, ArtifactBuildOptions::default()).unwrap();
        assert!(handoff.contains("observation_aggregate_fingerprint"));
        let markdown = render_handoff_markdown(&session, ArtifactBuildOptions::default()).unwrap();
        assert!(markdown.contains("Observation snapshot:"));
        assert!(markdown.contains("Result snapshot:"));
        assert!(markdown.contains("Observation snapshot unavailable (legacy comment"));
    }

    #[test]
    fn import_enriches_missing_same_id_reply_result() {
        let reply = CommentReply {
            id: "reply".into(),
            body: "done".into(),
            author: crate::state::Identity::local_human(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            result: None,
        };
        let comment = Comment {
            id: "comment".into(),
            replies: vec![reply],
            ..Default::default()
        };
        let mut state = ReviewState {
            comments: vec![comment.clone()],
            ..Default::default()
        };
        let mut imported = comment;
        let snapshot = crate::provenance::SnapshotEvidence::capture(
            chrono::DateTime::UNIX_EPOCH,
            "session",
            ReviewTarget::default(),
            std::iter::empty(),
        );
        imported.replies[0].result = Some(crate::provenance::CommentReplyResult::compare(
            "comment", None, None, snapshot,
        ));
        imported.observation = Some(crate::provenance::CommentObservation::new(
            imported.replies[0]
                .result
                .as_ref()
                .unwrap()
                .snapshot
                .clone(),
            None,
        ));
        let artifact = OwnedReviewArtifact {
            version: ARTIFACT_SCHEMA_VERSION,
            comments: vec![imported],
            ..Default::default()
        };

        let summary = import_json_artifact_into_state(&mut state, &artifact);

        assert_eq!(summary.comments_imported, 1);
        assert!(state.comments[0].replies[0].result.is_some());
        assert!(state.comments[0].observation.is_some());
    }

    #[test]
    fn import_equal_timestamp_reply_conflicts_converge_bidirectionally() {
        fn comment(body: &str, result_session: &str) -> Comment {
            let snapshot = crate::provenance::SnapshotEvidence::capture(
                chrono::DateTime::UNIX_EPOCH,
                result_session,
                ReviewTarget::default(),
                std::iter::empty(),
            );
            Comment {
                id: "comment".into(),
                replies: vec![CommentReply {
                    id: "reply".into(),
                    body: body.into(),
                    author: crate::state::Identity::agent(),
                    created_at: chrono::DateTime::UNIX_EPOCH,
                    result: Some(crate::provenance::CommentReplyResult::compare(
                        "comment", None, None, snapshot,
                    )),
                }],
                ..Default::default()
            }
        }
        let left = comment("z metadata", "left-result");
        let right = comment("a metadata", "right-result");
        let mut left_state = ReviewState {
            comments: vec![left.clone()],
            ..Default::default()
        };
        let mut right_state = ReviewState {
            comments: vec![right.clone()],
            ..Default::default()
        };

        import_json_artifact_into_state(
            &mut left_state,
            &OwnedReviewArtifact {
                version: ARTIFACT_SCHEMA_VERSION,
                comments: vec![right],
                ..Default::default()
            },
        );
        import_json_artifact_into_state(
            &mut right_state,
            &OwnedReviewArtifact {
                version: ARTIFACT_SCHEMA_VERSION,
                comments: vec![left],
                ..Default::default()
            },
        );

        assert_eq!(
            left_state.comments[0].replies,
            right_state.comments[0].replies
        );
        assert_eq!(left_state.comments[0].replies[0].body, "a metadata");
        assert!(left_state.comments[0].replies[0].result.is_some());
    }

    #[test]
    fn write_artifact_to_appends_newline() {
        let mut output = Vec::new();
        write_artifact_to(
            &fixture(),
            ArtifactFormat::Json,
            ArtifactProfile::Human,
            &mut output,
        )
        .unwrap();
        assert!(output.ends_with(b"\n"));
    }
}
