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
        ActionIntent, Comment, CommentKind, CommentState, ReviewState, ReviewTarget,
        ReviewTaskStatus, StepArtifact, StepImportance, StepKind,
    },
};

#[derive(Clone, Copy, Debug)]
pub enum ArtifactFormat {
    Json,
    Markdown,
}

/// Who the artifact is for. The agent profile adds raw diff hunks per file
/// and raw excerpts around each comment so tools can reason about the change
/// without re-running jj.
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

/// Diff context lines included on each side of a comment excerpt.
const EXCERPT_CONTEXT_LINES: usize = 3;

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
    pub tasks: Vec<TaskArtifact<'a>>,
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
    /// Raw hunks, agent profile only.
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
    pub linked_task_ids: Vec<&'a str>,
    /// Raw diff lines around the comment anchor, agent profile only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpt: Option<Vec<ExcerptLine<'a>>>,
}

#[derive(Debug, Serialize)]
pub struct TaskArtifact<'a> {
    pub id: &'a str,
    pub title: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<&'a str>,
    pub status: ReviewTaskStatus,
    pub action: ActionIntent,
    pub linked_comment_ids: Vec<&'a str>,
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
            version: 6,
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
        let durable_session = active_durable_session(session);
        Self {
            version: 6,
            generated_at: Utc::now(),
            repo: &session.repo,
            base: &session.target.base,
            revision: &session.target.rev,
            profile: agent.then_some("agent"),
            summary: session.summary_line(),
            session: durable_session.map(|durable| SessionArtifact {
                id: &durable.id,
                title: durable.title.as_deref(),
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
                    comment_belongs_to_session(
                        comment,
                        durable_session.map(|durable| durable.id.as_str()),
                    )
                })
                .filter(|comment| !options.only_open || comment.state == CommentState::Todo)
                .map(|comment| CommentArtifact {
                    comment,
                    linked_task_ids: linked_task_ids_for_comment(session, &comment.id),
                    excerpt: agent.then(|| comment_excerpt(session, comment)).flatten(),
                })
                .collect(),
            tasks: durable_session
                .into_iter()
                .flat_map(|durable| durable.tasks.iter())
                .filter(|task| !options.only_open || task.status == ReviewTaskStatus::Open)
                .map(|task| TaskArtifact {
                    id: &task.id,
                    title: &task.title,
                    body: task.body.as_deref(),
                    status: task.status,
                    action: task.action,
                    linked_comment_ids: task
                        .source_comment_id
                        .as_deref()
                        .filter(|comment_id| {
                            !options.only_open
                                || session.comments.iter().any(|comment| {
                                    comment.id == *comment_id
                                        && comment_belongs_to_session(
                                            comment,
                                            durable_session.map(|durable| durable.id.as_str()),
                                        )
                                        && comment.state == CommentState::Todo
                                })
                        })
                        .into_iter()
                        .collect(),
                    target: task.target.as_ref().map(target_artifact),
                })
                .collect(),
            walkthroughs: durable_session
                .into_iter()
                .flat_map(|durable| durable.walkthroughs.iter())
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
    let task_ids = artifact
        .tasks
        .iter()
        .map(|task| task.id)
        .collect::<Vec<_>>();
    let comment_ids = artifact
        .comments
        .iter()
        .map(|comment| comment.comment.id.as_str())
        .collect::<Vec<_>>();
    let mut items = Vec::new();
    for item in ordered_action_items(&artifact) {
        match item {
            OrderedActionItem::Task(task) => {
                let linked = task.linked_comment_ids.clone();
                items.push(serde_json::json!({
                    "id": task.id, "selector": shortest_unique_prefix(task.id, &task_ids), "short_id": shortest_unique_prefix(task.id, &task_ids), "source": "task", "kind": null, "action": task.action,
                    "path": task.target.as_ref().and_then(|t| t.file), "line": task.target.as_ref().and_then(|t| t.line), "end_line": task.target.as_ref().and_then(|t| t.end_line),
                    "excerpt": null, "body": task.body.unwrap_or(task.title), "title": task.title,
                    "state": task.status, "linked_comment_ids": linked, "linked_task_ids": Vec::<&str>::new(),
                }));
            }
            OrderedActionItem::Comment(comment) => {
                let linked_tasks = artifact
                    .tasks
                    .iter()
                    .filter(|task| {
                        task.linked_comment_ids
                            .iter()
                            .any(|id| *id == comment.comment.id)
                    })
                    .map(|task| task.id)
                    .collect::<Vec<_>>();
                items.push(serde_json::json!({
                    "id": comment.comment.id, "selector": shortest_unique_prefix(&comment.comment.id, &comment_ids), "short_id": shortest_unique_prefix(&comment.comment.id, &comment_ids), "source": "comment", "kind": comment.comment.kind, "action": comment.comment.action,
                    "path": comment.comment.path, "line": comment.comment.line, "end_line": comment.comment.end_line, "excerpt": comment.excerpt, "body": comment.comment.body,
                    "state": comment.comment.state, "linked_comment_ids": Vec::<&str>::new(), "linked_task_ids": linked_tasks,
                }));
            }
        }
    }
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
    let referenced_paths = referenced_hunk_paths(&artifact);
    let hunks = artifact
        .files
        .iter()
        .filter(|file| referenced_paths.contains(file.path))
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
    let value = serde_json::json!({
        "session": session_meta,
        "action_items": items,
        "walkthrough": walkthrough,
        "reference": { "hunks": hunks },
    });
    Ok(serde_json::to_string_pretty(&value)?)
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
    session.sessions.iter().find(|durable| {
        durable.status == crate::state::ReviewSessionStatus::Open
            && durable.target.base.as_deref() == Some(session.target.base.as_str())
            && durable.target.revision.as_deref() == Some(session.target.rev.as_str())
    })
}

fn comment_belongs_to_session(comment: &Comment, session_id: Option<&str>) -> bool {
    session_id.map_or(comment.session_id.is_none(), |id| {
        comment.belongs_to_session(id)
    })
}

fn linked_task_ids_for_comment<'a>(session: &'a ReviewSession, comment_id: &str) -> Vec<&'a str> {
    active_durable_session(session)
        .into_iter()
        .flat_map(|durable| durable.tasks.iter())
        .filter(|task| task.source_comment_id.as_deref() == Some(comment_id))
        .map(|task| task.id.as_str())
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

/// Raw diff lines around a comment anchor: the anchored line(s) plus
/// [`EXCERPT_CONTEXT_LINES`] context lines on each side within the hunk.
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
            let in_first_hunk = lines
                .iter()
                .filter(|line| line.hunk_index == first.hunk_index);
            let last = in_first_hunk
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

/// File-level comments excerpt the first hunk, which is usually enough for a
/// tool to orient itself.
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
            for reply in &comment.replies {
                if !existing
                    .replies
                    .iter()
                    .any(|existing_reply| existing_reply.id == reply.id)
                {
                    existing.replies.push(reply.clone());
                }
            }
            existing
                .replies
                .sort_by_key(|reply| (reply.created_at, reply.id.clone()));
            if comment.updated_at > existing.updated_at {
                let replies = existing.replies.clone();
                *existing = comment.clone();
                existing.replies = replies;
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
    action_item_tasks(artifact).count() + action_item_comments(artifact).count()
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
        return to_agent_markdown(artifact);
    }
    to_human_markdown(artifact)
}

fn to_human_markdown(artifact: &ReviewArtifact<'_>) -> String {
    let mut out = String::new();
    out.push_str("# jj change review\n\n");
    out.push_str(&format!("- Revision: `{}`\n", artifact.revision));
    out.push_str(&format!("- Base: `{}`\n", artifact.base));
    out.push_str(&format!("- Repository: `{}`\n", artifact.repo.display()));
    out.push_str(&format!("- Generated: `{}`\n", artifact.generated_at));
    out.push_str(&format!("- Summary: {}\n\n", artifact.summary));

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
            out.push_str(&format!("Status: {}\n\n", comment.comment.state.label()));
            out.push_str(comment.comment.body.trim());
            out.push_str("\n\n");
            write_comment_replies(&mut out, comment.comment);
        }
    }

    out.push_str("\n## Tasks\n\n");
    if artifact.tasks.is_empty() {
        out.push_str("No tasks recorded.\n");
    } else {
        for task in &artifact.tasks {
            out.push_str(&format!(
                "- [{}] `{}` — {:?} ({:?})",
                match task.status {
                    ReviewTaskStatus::Done => "x",
                    _ => " ",
                },
                task.id,
                task.title,
                task.action
            ));
            if let Some(target) = &task.target {
                write_target_suffix(&mut out, target);
            }
            if !task.linked_comment_ids.is_empty() {
                out.push_str(&format!(
                    "; comments: {}",
                    task.linked_comment_ids.join(", ")
                ));
            }
            out.push('\n');
        }
    }

    out.push_str("\n## Walkthrough\n\n");
    if artifact.walkthroughs.is_empty() {
        out.push_str("No walkthrough steps recorded.\n");
    } else {
        for walkthrough in &artifact.walkthroughs {
            if let Some(title) = walkthrough.title {
                out.push_str(&format!("### {}\n\n", title));
            }
            for (index, step) in walkthrough.steps.iter().enumerate() {
                out.push_str(&format!("{}. {}", index + 1, step.title.unwrap_or(step.id)));
                write_target_suffix(&mut out, &step.target);
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

    out
}

fn to_agent_markdown(artifact: &ReviewArtifact<'_>) -> String {
    let mut out = String::new();
    out.push_str("# Review session export (agent profile)\n\n");
    out.push_str("**Export artifact (agent profile).**\n\n");
    out.push_str("Complete session artifact for archive/import/tooling. It includes all comments/tasks (open tasks and ready comments first, draft/resolved/done items below) plus full reference hunks. For a one-shot implementer prompt, use `gander handoff`.\n\n");
    write_agent_header(artifact, &mut out);
    write_action_items(artifact, &mut out);

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
    out.push_str("One-shot actionable prompt: implement these action items first, then use the walkthrough and trimmed reference hunks as supporting context. Use `gander export markdown --profile agent` for the complete session artifact.\n\n");
    write_agent_header(artifact, &mut out);
    write_action_items(artifact, &mut out);

    out.push_str("\n## Walkthrough\n\n");
    write_walkthroughs(&mut out, artifact);

    out.push_str("\n## Reference: action/walkthrough hunks\n\n");
    write_limited_hunks(artifact, &mut out);
    out
}

fn write_agent_header(artifact: &ReviewArtifact<'_>, out: &mut String) {
    out.push_str(&format!("- Repository: `{}`\n", artifact.repo.display()));
    out.push_str(&format!(
        "- Target: `{}` → `{}`\n",
        artifact.base, artifact.revision
    ));
    if let Some(session) = &artifact.session
        && let Some(title) = session.title
    {
        out.push_str(&format!("- Session: {}\n", title));
    }
    out.push_str(&format!(
        "- Action items: {} item(s) ({} open task(s), {} ready comment(s))\n\n",
        action_item_count(artifact),
        action_item_tasks(artifact).count(),
        action_item_comments(artifact).count()
    ));
}

fn handoff_globals(artifact: &ReviewArtifact<'_>) -> String {
    [
        Some(format!(
            "--repo {}",
            shell_quote(&artifact.repo.display().to_string())
        )),
        Some(format!("--base {}", shell_quote(artifact.base))),
        Some(format!("--rev {}", shell_quote(artifact.revision))),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_action_items(artifact: &ReviewArtifact<'_>, out: &mut String) {
    out.push_str("## Action items\n\n");
    out.push_str(
        "Ordered by action priority (fix, test, follow-up, other), then path and line.\n\n",
    );
    let mut wrote = false;
    let globals = handoff_globals(artifact);
    let task_ids = artifact
        .tasks
        .iter()
        .map(|task| task.id)
        .collect::<Vec<_>>();
    let comment_ids = artifact
        .comments
        .iter()
        .map(|comment| comment.comment.id.as_str())
        .collect::<Vec<_>>();
    for item in ordered_action_items(artifact) {
        match item {
            OrderedActionItem::Task(task) => {
                wrote = true;
                out.push_str(&format!(
                    "- [task][{}][{}] {}",
                    action_label(task.action),
                    shortest_unique_prefix(task.id, &task_ids),
                    task.title
                ));
                if let Some(target) = &task.target {
                    write_target_suffix(out, target);
                }
                if !task.linked_comment_ids.is_empty() {
                    out.push_str("; ");
                    write_linked_comments(out, artifact, &task.linked_comment_ids);
                }
                out.push('\n');
                if let Some(body) = task.body.filter(|body| !body.trim().is_empty()) {
                    out.push_str(&format!("  Body: {}\n", body.trim()));
                }
            }
            OrderedActionItem::Comment(comment) => {
                wrote = true;
                out.push_str(&format!(
                    "- [comment][{}][{}] ",
                    kind_label(comment.comment.kind.unwrap_or(CommentKind::Note)),
                    action_label(comment.comment.action.unwrap_or(ActionIntent::None))
                ));
                if comment.comment.path.is_some() {
                    write_comment_location_inline(out, comment.comment);
                    out.push_str(" — ");
                }
                out.push_str(comment.comment.body.trim());
                let linked_tasks = linked_tasks_for_comment(artifact, comment.comment.id.as_str());
                if !linked_tasks.is_empty() {
                    out.push_str("; linked to task ");
                    out.push_str(&linked_tasks.join(", "));
                }
                let selector = shortest_unique_prefix(&comment.comment.id, &comment_ids);
                out.push_str(&format!(
                    "\n  Selector: `{selector}`; reply: `gander {globals} comments reply {selector} --body <text>`; resolve: `gander {globals} comments resolve {selector} --reply <text>`\n"
                ));
                write_comment_replies(out, comment.comment);
                if comment.comment.path.is_some() {
                    write_excerpt(out, comment.excerpt.as_deref());
                }
            }
        }
    }
    if !wrote {
        out.push_str("No open tasks or ready comments.\n");
    }
}

fn write_other_comments(artifact: &ReviewArtifact<'_>, out: &mut String) {
    let mut other = false;
    for comment in &artifact.comments {
        if comment.comment.state == CommentState::Todo {
            continue;
        }
        other = true;
        write_comment_heading(out, comment.comment);
        out.push_str(&format!(
            "Status: {}; kind: {}; action: {}\n\n",
            comment.comment.state.label(),
            kind_label(comment.comment.kind.unwrap_or(CommentKind::Note)),
            action_label(comment.comment.action.unwrap_or(ActionIntent::None))
        ));
        out.push_str(comment.comment.body.trim());
        out.push_str("\n\n");
        write_comment_replies(out, comment.comment);
        write_excerpt(out, comment.excerpt.as_deref());
    }
    if !other {
        out.push_str("No other comments recorded.\n");
    }
}

fn write_full_hunks(artifact: &ReviewArtifact<'_>, out: &mut String) {
    for file in &artifact.files {
        if let Some(hunks) = &file.hunks {
            out.push_str(&format!("### `{}`\n\n", file.path));
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
    }
}

fn write_limited_hunks(artifact: &ReviewArtifact<'_>, out: &mut String) {
    let paths = referenced_hunk_paths(artifact);
    if paths.is_empty() {
        out.push_str("No action-item or walkthrough file hunks recorded.\n");
        return;
    }
    let mut wrote = false;
    for file in &artifact.files {
        if !paths.contains(file.path) {
            continue;
        }
        if let Some(hunks) = &file.hunks {
            wrote = true;
            out.push_str(&format!("### `{}`\n\n", file.path));
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
    }
    if !wrote {
        out.push_str("No action-item or walkthrough file hunks recorded.\n");
    }
}

fn referenced_hunk_paths<'a>(artifact: &'a ReviewArtifact<'a>) -> BTreeSet<&'a str> {
    let mut paths = BTreeSet::new();
    for task in action_item_tasks(artifact) {
        if let Some(file) = task.target.as_ref().and_then(|target| target.file) {
            paths.insert(file);
        }
        for comment_id in &task.linked_comment_ids {
            if let Some(comment) = artifact
                .comments
                .iter()
                .find(|comment| comment.comment.id == *comment_id)
                && let Some(path) = comment.comment.path.as_deref()
            {
                paths.insert(path);
            }
        }
    }
    for comment in action_item_comments(artifact) {
        if let Some(path) = comment.comment.path.as_deref() {
            paths.insert(path);
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

fn action_item_tasks<'a>(
    artifact: &'a ReviewArtifact<'a>,
) -> impl Iterator<Item = &'a TaskArtifact<'a>> {
    artifact
        .tasks
        .iter()
        .filter(|task| task.status == ReviewTaskStatus::Open)
}

fn action_item_comments<'a>(
    artifact: &'a ReviewArtifact<'a>,
) -> impl Iterator<Item = &'a CommentArtifact<'a>> {
    artifact
        .comments
        .iter()
        .filter(|comment| comment.comment.state == CommentState::Todo)
}

enum OrderedActionItem<'a> {
    Task(&'a TaskArtifact<'a>),
    Comment(&'a CommentArtifact<'a>),
}

fn action_priority(action: ActionIntent) -> u8 {
    match action {
        ActionIntent::Fix => 0,
        ActionIntent::Test => 1,
        ActionIntent::FollowUp => 2,
        ActionIntent::Explain | ActionIntent::None => 3,
    }
}

fn ordered_action_items<'a>(artifact: &'a ReviewArtifact<'a>) -> Vec<OrderedActionItem<'a>> {
    let mut indexed = Vec::new();
    for (idx, task) in action_item_tasks(artifact).enumerate() {
        indexed.push((
            action_priority(task.action),
            task.target.as_ref().and_then(|t| t.file).unwrap_or("~"),
            task.target
                .as_ref()
                .and_then(|t| t.line)
                .unwrap_or(usize::MAX),
            idx,
            OrderedActionItem::Task(task),
        ));
    }
    let offset = indexed.len();
    for (idx, comment) in action_item_comments(artifact).enumerate() {
        indexed.push((
            action_priority(comment.comment.action.unwrap_or(ActionIntent::None)),
            comment.comment.path.as_deref().unwrap_or("~"),
            comment.comment.line.unwrap_or(usize::MAX),
            offset + idx,
            OrderedActionItem::Comment(comment),
        ));
    }
    indexed.sort_by_key(|(priority, path, line, stable, _)| {
        (*priority, *path == "~", *path, *line, *stable)
    });
    indexed.into_iter().map(|(_, _, _, _, item)| item).collect()
}

fn linked_tasks_for_comment(artifact: &ReviewArtifact<'_>, comment_id: &str) -> Vec<String> {
    artifact
        .tasks
        .iter()
        .filter(|task| task.linked_comment_ids.contains(&comment_id))
        .map(|task| task.id.to_owned())
        .collect()
}

fn write_linked_comments(out: &mut String, artifact: &ReviewArtifact<'_>, ids: &[&str]) {
    let labels = ids
        .iter()
        .map(|id| {
            if let Some(comment) = artifact
                .comments
                .iter()
                .find(|comment| comment.comment.id == *id)
            {
                let line = comment
                    .comment
                    .line
                    .map(|line| format!(":{line}"))
                    .unwrap_or_default();
                match comment.comment.path.as_deref() {
                    Some(path) => format!("linked to comment {id} ({path}{line})"),
                    None => format!("linked to general comment {id}"),
                }
            } else {
                format!("linked to comment {id}")
            }
        })
        .collect::<Vec<_>>();
    out.push_str(&labels.join(", "));
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
    let Some(lines) = excerpt.filter(|lines| !lines.is_empty()) else {
        return;
    };
    out.push_str("\n```diff\n");
    for line in lines {
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
    out.push_str("```\n");
}

fn write_comment_location_inline(out: &mut String, comment: &crate::state::Comment) {
    let Some(path) = comment.path.as_deref() else {
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

fn write_comment_heading(out: &mut String, comment: &crate::state::Comment) {
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
                out.push_str(&format!("{:>4} ", line.line));
                out.push_str(&line.line_text);
                out.push('\n');
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

fn write_comment_replies(out: &mut String, comment: &crate::state::Comment) {
    if comment.replies.is_empty() {
        return;
    }
    out.push_str("Replies:\n");
    for reply in &comment.replies {
        out.push_str(&format!(
            "- `{}` at `{}`: {}\n",
            reply.id,
            reply.created_at,
            reply.body.trim()
        ));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::ReviewSession,
        diff::DiffSet,
        jj::ReviewTarget,
        state::{FileState, ReviewState, ReviewTask},
    };

    #[test]
    fn markdown_contains_files_and_comments() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("Looks good".into());
        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);
        assert!(markdown.contains("`a.txt`"));
        assert!(markdown.contains("Looks good"));
    }

    #[test]
    fn render_artifact_outputs_json_comments() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("Looks good".into());

        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();

        assert!(json.contains("\"comments\""));
        assert!(json.contains("Looks good"));
    }

    #[test]
    fn write_artifact_to_appends_newline() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        let mut out = Vec::new();

        write_artifact_to(
            &session,
            ArtifactFormat::Json,
            ArtifactProfile::Human,
            &mut out,
        )
        .unwrap();

        assert!(out.ends_with(b"\n"));
    }

    #[test]
    fn markdown_includes_comment_state() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("Needs work".into());
        let id = session.comments[0].id.clone();
        session.cycle_comment_state(&id);

        let markdown = render_artifact(&session, ArtifactFormat::Markdown).unwrap();
        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();

        assert!(markdown.contains("Status: todo"));
        assert!(json.contains("\"state\": \"todo\""));
    }

    #[test]
    fn agent_profile_includes_raw_hunks_and_comment_excerpts() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,5 +1,5 @@
 first
 second
-old
+new
 fourth
 fifth
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.toggle_focus();
        session.move_diff_cursor(2);
        session.add_comment("Line note".into());

        let json =
            render_artifact_with_profile(&session, ArtifactFormat::Json, ArtifactProfile::Agent)
                .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["profile"], "agent");
        let hunk = &value["files"][0]["hunks"][0];
        assert_eq!(hunk["header"], "@@ -1,5 +1,5 @@");
        assert_eq!(hunk["lines"].as_array().unwrap().len(), 6);
        assert_eq!(hunk["lines"][2]["kind"], "removed");
        assert_eq!(hunk["lines"][2]["text"], "old");

        let excerpt = value["comments"][0]["excerpt"].as_array().unwrap();
        assert!(!excerpt.is_empty());
        assert!(
            excerpt
                .iter()
                .any(|line| line["kind"] == "added" && line["text"] == "new")
        );
        // Comment fields stay flattened alongside the excerpt.
        assert_eq!(value["comments"][0]["body"], "Line note");
    }

    #[test]
    fn human_profile_omits_hunks_and_excerpts() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.add_comment("File note".into());

        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(value.get("profile").is_none());
        assert!(value["files"][0].get("hunks").is_none());
        assert!(value["comments"][0].get("excerpt").is_none());
    }

    #[test]
    fn artifact_version_is_6_for_optional_comment_paths() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );

        let artifact = ReviewArtifact::from(&session);

        assert_eq!(artifact.version, 6);
        assert_eq!(artifact.profile, None);
    }

    #[test]
    fn agent_markdown_orders_action_items_before_reference_hunks_and_mentions_target() {
        let mut session = ReviewSession::new(
            "/repo".into(),
            ReviewTarget::new("main", "@"),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        session.add_comment("Please fix this".into());
        session.comments[0].kind = Some(CommentKind::Issue);
        session.comments[0].action = Some(ActionIntent::Fix);
        session.comments[0].state = CommentState::Todo;

        let markdown = render_artifact_with_profile(
            &session,
            ArtifactFormat::Markdown,
            ArtifactProfile::Agent,
        )
        .unwrap();

        assert!(markdown.contains("`main` → `@`"));
        assert!(markdown.contains("## Action items"));
        assert!(markdown.contains("[comment][issue][fix]"));
        assert!(
            markdown.find("## Action items").unwrap()
                < markdown.find("## Reference: full hunks").unwrap()
        );
    }

    #[test]
    fn handoff_json_and_markdown_share_action_item_order() {
        let mut session = ReviewSession::new(
            "/repo".into(),
            ReviewTarget::new("main", "@"),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        session.sessions.push(crate::state::ReviewSession {
            id: "s".into(),
            target: crate::state::ReviewTarget {
                base: Some("main".into()),
                revision: Some("@".into()),
                ..Default::default()
            },
            tasks: vec![
                crate::state::ReviewTask {
                    id: "follow".into(),
                    title: "follow task".into(),
                    action: ActionIntent::FollowUp,
                    target: Some(crate::state::ReviewTarget {
                        file: Some("b.rs".into()),
                        line: Some(9),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                crate::state::ReviewTask {
                    id: "fix".into(),
                    title: "fix task".into(),
                    action: ActionIntent::Fix,
                    target: Some(crate::state::ReviewTarget {
                        file: Some("z.rs".into()),
                        line: Some(1),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                crate::state::ReviewTask {
                    id: "test".into(),
                    title: "test task".into(),
                    action: ActionIntent::Test,
                    target: Some(crate::state::ReviewTarget {
                        file: Some("a.rs".into()),
                        line: Some(5),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        session.add_comment("other comment".into());
        session.comments[0].action = Some(ActionIntent::None);
        session.comments[0].state = CommentState::Todo;
        session.comments[0].path = Some("a.rs".into());
        session.comments[0].line = Some(1);

        let json = render_handoff_json(&session, ArtifactBuildOptions::default()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let ids = value["action_items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            vec!["fix", "test", "follow", session.comments[0].id.as_str()]
        );

        let markdown = render_handoff_markdown(&session, ArtifactBuildOptions::default()).unwrap();
        assert!(markdown.find("fix task").unwrap() < markdown.find("test task").unwrap());
        assert!(markdown.find("test task").unwrap() < markdown.find("follow task").unwrap());
        assert!(markdown.find("follow task").unwrap() < markdown.find("other comment").unwrap());
    }

    #[test]
    fn only_open_excludes_resolved_comments_and_done_tasks() {
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "session-1".to_owned(),
            target: crate::state::ReviewTarget {
                base: Some("trunk()".to_owned()),
                revision: Some("@".to_owned()),
                ..crate::state::ReviewTarget::default()
            },
            tasks: vec![
                crate::state::ReviewTask {
                    id: "open-task".to_owned(),
                    title: "Keep me".to_owned(),
                    status: ReviewTaskStatus::Open,
                    ..crate::state::ReviewTask::default()
                },
                crate::state::ReviewTask {
                    id: "done-task".to_owned(),
                    title: "Drop me".to_owned(),
                    status: ReviewTaskStatus::Done,
                    ..crate::state::ReviewTask::default()
                },
            ],
            ..crate::state::ReviewSession::default()
        });
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        );
        session.add_comment("Keep comment".into());
        session.add_comment("Drop comment".into());
        session.comments[0].state = CommentState::Todo;
        session.comments[1].state = CommentState::Resolved;

        let json = render_artifact_with_options(
            &session,
            ArtifactFormat::Json,
            ArtifactProfile::Agent,
            ArtifactBuildOptions { only_open: true },
        )
        .unwrap();

        assert!(json.contains("Keep comment"));
        assert!(!json.contains("Drop comment"));
        assert!(json.contains("open-task"));
        assert!(!json.contains("done-task"));
    }

    #[test]
    fn artifact_json_includes_session_tasks_and_walkthroughs() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "session-1".to_owned(),
            title: Some("Review handoff".to_owned()),
            target: crate::state::ReviewTarget {
                base: Some("trunk()".to_owned()),
                revision: Some("@".to_owned()),
                ..crate::state::ReviewTarget::default()
            },
            tasks: vec![crate::state::ReviewTask {
                id: "task-1".to_owned(),
                title: "Fix parser".to_owned(),
                action: crate::state::ActionIntent::Fix,
                source_comment_id: Some("comment-1".to_owned()),
                target: Some(crate::state::ReviewTarget {
                    file: Some("a.txt".to_owned()),
                    line: Some(1),
                    ..crate::state::ReviewTarget::default()
                }),
                ..crate::state::ReviewTask::default()
            }],
            walkthroughs: vec![crate::state::Walkthrough {
                id: "walk-1".to_owned(),
                title: Some("Start here".to_owned()),
                steps: vec![crate::state::WalkthroughStep {
                    id: "step-1".to_owned(),
                    title: Some("Read parser".to_owned()),
                    why: Some("It changed".to_owned()),
                    body: Some("Check the replacement.".to_owned()),
                    target: crate::state::ReviewTarget {
                        file: Some("a.txt".to_owned()),
                        line: Some(1),
                        symbol: Some("parse".to_owned()),
                        ..crate::state::ReviewTarget::default()
                    },
                    ..crate::state::WalkthroughStep::default()
                }],
                ..crate::state::Walkthrough::default()
            }],
            ..crate::state::ReviewSession::default()
        });
        let session = ReviewSession::new(".".into(), ReviewTarget::trunk_to_current(), diff, state);

        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(value["session"]["id"], "session-1");
        assert_eq!(value["tasks"][0]["linked_comment_ids"][0], "comment-1");
        assert_eq!(value["tasks"][0]["target"]["file"], "a.txt");
        assert_eq!(value["walkthroughs"][0]["steps"][0]["why"], "It changed");
        assert_eq!(
            value["walkthroughs"][0]["steps"][0]["target"]["symbol"],
            "parse"
        );
    }

    #[test]
    fn handoff_json_is_structured_action_artifact() {
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "session-1".to_owned(),
            target: crate::state::ReviewTarget {
                base: Some("trunk()".to_owned()),
                revision: Some("@".to_owned()),
                ..crate::state::ReviewTarget::default()
            },
            tasks: vec![crate::state::ReviewTask {
                id: "task-1".to_owned(),
                title: "Fix parser".to_owned(),
                body: Some("Handle the edge case.".to_owned()),
                action: ActionIntent::Fix,
                source_comment_id: Some("comment-1".to_owned()),
                target: Some(crate::state::ReviewTarget {
                    file: Some("a.txt".to_owned()),
                    line: Some(1),
                    ..crate::state::ReviewTarget::default()
                }),
                ..crate::state::ReviewTask::default()
            }],
            walkthroughs: vec![crate::state::Walkthrough {
                id: "walk-1".to_owned(),
                steps: vec![crate::state::WalkthroughStep {
                    id: "step-1".to_owned(),
                    title: Some("Read parser".to_owned()),
                    target: crate::state::ReviewTarget {
                        file: Some("a.txt".to_owned()),
                        line: Some(1),
                        ..crate::state::ReviewTarget::default()
                    },
                    ..crate::state::WalkthroughStep::default()
                }],
                ..crate::state::Walkthrough::default()
            }],
            ..crate::state::ReviewSession::default()
        });
        let mut session = ReviewSession::new(
            "/repo".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        );
        session.add_comment("Please fix".into());
        session.comments[0].id = "comment-1".to_owned();
        session.comments[0].kind = Some(CommentKind::Issue);
        session.comments[0].state = CommentState::Todo;

        let json = render_handoff_json(&session, ArtifactBuildOptions::default()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["session"]["base"], "trunk()");
        assert_eq!(value["action_items"][0]["source"], "task");
        assert_eq!(value["action_items"][0]["body"], "Handle the edge case.");
        assert_eq!(value["action_items"][1]["linked_task_ids"][0], "task-1");
        assert_eq!(value["walkthrough"][0]["id"], "step-1");
        assert_eq!(value["reference"]["hunks"][0]["path"], "a.txt");
    }

    #[test]
    fn handoff_markdown_action_items_match_json_and_header_count() {
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "session-1".to_owned(),
            target: crate::state::ReviewTarget {
                base: Some("trunk()".to_owned()),
                revision: Some("@".to_owned()),
                ..crate::state::ReviewTarget::default()
            },
            tasks: vec![
                ReviewTask {
                    id: "task-1".to_owned(),
                    title: "Open task one".to_owned(),
                    target: Some(crate::state::ReviewTarget {
                        file: Some("a.txt".to_owned()),
                        line: Some(1),
                        ..crate::state::ReviewTarget::default()
                    }),
                    ..ReviewTask::default()
                },
                ReviewTask {
                    id: "task-2".to_owned(),
                    title: "Open task two".to_owned(),
                    action: ActionIntent::Test,
                    ..ReviewTask::default()
                },
                ReviewTask {
                    id: "task-done".to_owned(),
                    title: "Done task".to_owned(),
                    status: ReviewTaskStatus::Done,
                    ..ReviewTask::default()
                },
            ],
            ..crate::state::ReviewSession::default()
        });
        let mut session = ReviewSession::new(
            "/repo".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        );
        for (id, body, kind, action) in [
            (
                "issue",
                "Fix it",
                Some(CommentKind::Issue),
                Some(ActionIntent::Fix),
            ),
            (
                "question",
                "Should this change?",
                Some(CommentKind::Question),
                Some(ActionIntent::FollowUp),
            ),
            (
                "note",
                "Add test",
                Some(CommentKind::Note),
                Some(ActionIntent::Test),
            ),
            ("praise", "Looks nice", Some(CommentKind::Praise), None),
        ] {
            session.add_comment(body.to_owned());
            let comment = session.comments.last_mut().unwrap();
            comment.id = id.to_owned();
            comment.kind = kind;
            comment.action = action;
            comment.state = CommentState::Todo;
        }
        session.comments[3].state = CommentState::Resolved;

        let json = render_handoff_json(&session, ArtifactBuildOptions::default()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let ids = value["action_items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();

        let markdown = render_handoff_markdown(&session, ArtifactBuildOptions::default()).unwrap();
        assert_eq!(ids, vec!["issue", "note", "task-2", "question", "task-1"]);
        assert!(
            markdown.contains("- Action items: 5 item(s) (2 open task(s), 3 ready comment(s))")
        );
        assert_eq!(markdown.matches("- [task]").count(), 2);
        assert_eq!(markdown.matches("- [comment]").count(), 3);
        assert!(markdown.contains("Open task one"));
        assert!(markdown.contains("Open task two"));
        assert!(!markdown.contains("Done task"));
        assert!(markdown.contains("Fix it"));
        assert!(markdown.contains("Should this change?"));
        assert!(markdown.contains("Add test"));
        assert!(markdown.contains("[comment][question][follow-up]"));
        assert!(markdown.contains("[comment][note][test]"));
        assert!(!markdown.contains("Looks nice"));
    }

    #[test]
    fn handoff_markdown_differs_from_agent_export_markdown() {
        let mut session = ReviewSession::new(
            "/repo".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old b\n+new b\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        session.add_comment("Action on a only".into());
        session.comments[0].path = Some("a.txt".to_owned());
        session.comments[0].kind = Some(CommentKind::Issue);
        session.comments[0].state = CommentState::Todo;
        let handoff = render_handoff_markdown(&session, ArtifactBuildOptions::default()).unwrap();
        let export = render_artifact_with_profile(
            &session,
            ArtifactFormat::Markdown,
            ArtifactProfile::Agent,
        )
        .unwrap();

        assert_ne!(handoff, export);
        assert!(handoff.contains("# Human review handoff for a coding agent"));
        assert!(handoff.contains("## Reference: action/walkthrough hunks"));
        assert!(!handoff.contains("## Reference: full hunks"));
        assert!(handoff.contains("### `a.txt`"));
        assert!(!handoff.contains("### `b.txt`"));
        assert!(export.contains("**Export artifact (agent profile).**"));
        assert!(export.contains("## Reference: full hunks"));
        assert!(export.contains("### `b.txt`"));
    }

    #[test]
    fn agent_markdown_includes_task_bodies_and_explicit_links() {
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "session-1".to_owned(),
            target: crate::state::ReviewTarget {
                base: Some("trunk()".to_owned()),
                revision: Some("@".to_owned()),
                ..crate::state::ReviewTarget::default()
            },
            tasks: vec![crate::state::ReviewTask {
                id: "task-1".to_owned(),
                title: "Fix parser".to_owned(),
                body: Some("Add regression coverage.".to_owned()),
                source_comment_id: Some("comment-1".to_owned()),
                ..crate::state::ReviewTask::default()
            }],
            ..crate::state::ReviewSession::default()
        });
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        );
        session.add_comment("Please fix".into());
        session.comments[0].id = "comment-1".to_owned();
        session.comments[0].kind = Some(CommentKind::Issue);
        session.comments[0].line = Some(1);
        session.comments[0].state = CommentState::Todo;

        let markdown = render_artifact_with_profile(
            &session,
            ArtifactFormat::Markdown,
            ArtifactProfile::Agent,
        )
        .unwrap();
        assert!(markdown.contains("Body: Add regression coverage."));
        assert!(markdown.contains("linked to comment comment-1 (a.txt:1)"));
        assert!(markdown.contains("linked to task task-1"));
    }

    #[test]
    fn markdown_contains_tasks_and_walkthrough_sections() {
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let artifact = ReviewArtifact::from(&session);

        let markdown = to_markdown(&artifact);

        assert!(markdown.contains("## Tasks"));
        assert!(markdown.contains("No tasks recorded."));
        assert!(markdown.contains("## Walkthrough"));
    }

    #[test]
    fn agent_markdown_deduplicates_default_walkthrough_heading() {
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "session-1".to_owned(),
            target: crate::state::ReviewTarget {
                base: Some("trunk()".to_owned()),
                revision: Some("@".to_owned()),
                ..crate::state::ReviewTarget::default()
            },
            walkthroughs: vec![crate::state::Walkthrough {
                id: "walk-1".to_owned(),
                title: Some("Walkthrough".to_owned()),
                steps: vec![crate::state::WalkthroughStep {
                    id: "step-1".to_owned(),
                    title: Some("Read parser".to_owned()),
                    target: crate::state::ReviewTarget {
                        file: Some("a.txt".to_owned()),
                        line: Some(1),
                        ..crate::state::ReviewTarget::default()
                    },
                    ..crate::state::WalkthroughStep::default()
                }],
                ..crate::state::Walkthrough::default()
            }],
            ..crate::state::ReviewSession::default()
        });
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        );

        let markdown = render_artifact_with_profile(
            &session,
            ArtifactFormat::Markdown,
            ArtifactProfile::Agent,
        )
        .unwrap();

        assert!(markdown.contains("## Walkthrough\n\n1. Read parser"));
        assert!(!markdown.contains("### Walkthrough"));
    }

    #[test]
    fn import_accepts_minimal_v4_artifact() {
        let mut state = ReviewState::default();
        state.files.insert(
            "src/lib.rs".to_owned(),
            FileState {
                fingerprint: "abc".to_owned(),
                viewed: false,
                ..Default::default()
            },
        );
        let artifact: OwnedReviewArtifact = serde_json::from_str(
            r#"{
  "version": 4,
  "base": "trunk()",
  "revision": "@",
  "files": [
    { "path": "src/lib.rs", "viewed": true, "fingerprint": "abc" }
  ],
  "comments": []
}"#,
        )
        .unwrap();

        let summary = import_json_artifact_into_state(&mut state, &artifact);

        assert_eq!(artifact.version, 4);
        assert_eq!(summary.viewed_files_imported, 1);
        assert!(state.files["src/lib.rs"].viewed);
    }

    #[test]
    fn acceptance_artifact_comments_are_scoped_to_active_session_with_legacy_visible() {
        let mut state = ReviewState::default();
        state.sessions.extend([
            crate::state::ReviewSession {
                id: "active".into(),
                target: crate::state::ReviewTarget {
                    base: Some("trunk()".into()),
                    revision: Some("@".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            crate::state::ReviewSession {
                id: "other".into(),
                ..Default::default()
            },
        ]);
        state.comments.extend([
            Comment {
                id: "legacy".into(),
                path: Some("a.txt".into()),
                session_id: None,
                body: "legacy comment".into(),
                state: CommentState::Draft,
                ..Default::default()
            },
            Comment {
                id: "matching".into(),
                path: Some("a.txt".into()),
                session_id: Some("active".into()),
                body: "matching comment".into(),
                state: CommentState::Todo,
                ..Default::default()
            },
            Comment {
                id: "foreign".into(),
                path: Some("a.txt".into()),
                session_id: Some("other".into()),
                body: "foreign comment".into(),
                state: CommentState::Resolved,
                ..Default::default()
            },
        ]);
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        );

        let value: serde_json::Value =
            serde_json::from_str(&render_artifact(&session, ArtifactFormat::Json).unwrap())
                .unwrap();
        let ids = value["comments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|comment| comment["id"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["legacy", "matching"]);
        assert_eq!(value["version"], 6);
    }

    #[test]
    fn acceptance_handoff_uses_todo_state_only_and_general_todo_has_no_location_or_hunks() {
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "active".into(),
            target: crate::state::ReviewTarget {
                base: Some("trunk()".into()),
                revision: Some("@".into()),
                ..Default::default()
            },
            tasks: vec![ReviewTask {
                id: "task-linked-to-draft".into(),
                title: "task remains actionable".into(),
                source_comment_id: Some("draft".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        state.comments.extend([
            Comment {
                id: "general-todo".into(),
                path: None,
                session_id: Some("active".into()),
                body: "general praise ready".into(),
                kind: Some(CommentKind::Praise),
                action: Some(ActionIntent::None),
                state: CommentState::Todo,
                ..Default::default()
            },
            Comment {
                id: "draft".into(),
                path: Some("a.txt".into()),
                session_id: Some("active".into()),
                body: "draft must stay private".into(),
                state: CommentState::Draft,
                ..Default::default()
            },
            Comment {
                id: "resolved".into(),
                path: Some("a.txt".into()),
                session_id: Some("active".into()),
                body: "resolved must stay out".into(),
                state: CommentState::Resolved,
                ..Default::default()
            },
        ]);
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .unwrap(),
            state,
        );

        let handoff_json = render_handoff_json(&session, ArtifactBuildOptions::default()).unwrap();
        let handoff: serde_json::Value = serde_json::from_str(&handoff_json).unwrap();
        let items = handoff["action_items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        let general = items
            .iter()
            .find(|item| item["id"] == "general-todo")
            .unwrap();
        assert!(general["path"].is_null());
        assert!(general["excerpt"].is_null());
        let task = items
            .iter()
            .find(|item| item["id"] == "task-linked-to-draft")
            .unwrap();
        assert!(task["linked_comment_ids"].as_array().unwrap().is_empty());
        assert!(handoff["reference"]["hunks"].as_array().unwrap().is_empty());

        let handoff_markdown =
            render_handoff_markdown(&session, ArtifactBuildOptions::default()).unwrap();
        assert!(handoff_markdown.contains("general praise ready"));
        assert!(!handoff_markdown.contains("draft must stay private"));
        assert!(!handoff_markdown.contains("resolved must stay out"));
        assert!(!handoff_markdown.contains("### `a.txt`"));

        let full = render_artifact(&session, ArtifactFormat::Json).unwrap();
        assert!(full.contains("general praise ready"));
        assert!(full.contains("draft must stay private"));
        assert!(full.contains("resolved must stay out"));
    }

    #[test]
    fn acceptance_import_deserializes_legacy_v5_comment_path_and_missing_session() {
        let artifact: OwnedReviewArtifact = serde_json::from_str(
            r#"{
  "version": 5,
  "base": "trunk()",
  "revision": "@",
  "comments": [{
    "id": "legacy-v5",
    "path": "src/lib.rs",
    "body": "legacy body",
    "state": "todo",
    "created_at": "2026-06-30T00:00:00Z"
  }]
}"#,
        )
        .unwrap();
        let mut state = ReviewState::default();

        let summary = import_json_artifact_into_state(&mut state, &artifact);

        assert_eq!(artifact.version, 5);
        assert_eq!(summary.comments_imported, 1);
        assert_eq!(state.comments[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(state.comments[0].session_id, None);
    }

    #[test]
    fn empty_state_exports_cleanly_with_empty_tasks_and_walkthroughs() {
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse("").unwrap(),
            ReviewState::default(),
        );

        let json = render_artifact(&session, ArtifactFormat::Json).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert!(value["files"].as_array().unwrap().is_empty());
        assert!(value["tasks"].as_array().unwrap().is_empty());
        assert!(value["walkthroughs"].as_array().unwrap().is_empty());
        assert!(value.get("session").is_none());
    }

    #[test]
    fn markdown_contains_line_anchor() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.toggle_focus();
        session.add_comment("Line note".into());

        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);

        assert!(markdown.contains("`a.txt`:old:1") || markdown.contains("`a.txt`:new:1"));
        assert!(markdown.contains("Anchor: `@@ -1 +1 @@`"));
        assert!(markdown.contains("Line note"));
    }

    #[test]
    fn artifact_includes_generated_metadata() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.annotate_generated_where(|file| file.path == "a.txt");

        let artifact = ReviewArtifact::from(&session);
        let json = serde_json::to_value(&artifact).unwrap();

        assert!(artifact.files[0].generated);
        assert_eq!(json["files"][0]["generated"], true);
    }

    #[test]
    fn markdown_marks_generated_files() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.annotate_generated_where(|file| file.path == "a.txt");

        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);

        assert!(markdown.contains("`a.txt` — mod [generated/noisy]"));
    }

    #[test]
    fn markdown_contains_range_anchor() {
        let diff = DiffSet::parse(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,3 @@
-old
+new
+extra
 same
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.toggle_focus();
        session.toggle_diff_range_selection();
        session.move_diff_cursor(2);
        session.add_comment("Range note".into());

        let artifact = ReviewArtifact::from(&session);
        let markdown = to_markdown(&artifact);

        assert!(markdown.contains(":range:"));
        assert!(markdown.contains("Range fingerprint"));
        assert!(markdown.contains("Range note"));
    }

    #[test]
    fn import_json_artifact_merges_matching_viewed_files_and_new_comments() {
        let mut state = ReviewState::default();
        state.files.insert(
            "src/main.rs".to_owned(),
            FileState {
                fingerprint: "abc".to_owned(),
                viewed: false,
                ..Default::default()
            },
        );
        state.files.insert(
            "src/stale.rs".to_owned(),
            FileState {
                fingerprint: "current".to_owned(),
                viewed: false,
                ..Default::default()
            },
        );
        state.comments.push(crate::state::Comment {
            id: "existing".to_owned(),
            path: Some("src/main.rs".to_owned()),
            line: None,
            end_line: None,
            anchor: None,
            body: "already here".to_owned(),
            kind: None,
            action: None,
            state: crate::state::CommentState::default(),
            created_at: chrono::DateTime::parse_from_rfc3339("2026-06-30T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
            ..Default::default()
        });
        let artifact: OwnedReviewArtifact = serde_json::from_str(
            r#"{
  "version": 3,
  "base": "trunk()",
  "revision": "@",
  "files": [
    { "path": "src/main.rs", "viewed": true, "fingerprint": "abc" },
    { "path": "src/stale.rs", "viewed": true, "fingerprint": "old" }
  ],
  "comments": [
    {
      "id": "existing",
      "path": "src/main.rs",
      "body": "duplicate",
      "created_at": "2026-06-30T00:00:00Z"
    },
    {
      "id": "new",
      "path": "src/main.rs",
      "body": "new comment",
      "created_at": "2026-06-30T00:00:00Z"
    }
  ]
}"#,
        )
        .unwrap();

        let summary = import_json_artifact_into_state(&mut state, &artifact);

        assert_eq!(summary.viewed_files_imported, 1);
        assert_eq!(summary.comments_imported, 1);
        assert_eq!(summary.duplicate_comments_skipped, 1);
        assert!(state.files["src/main.rs"].viewed);
        assert!(!state.files["src/stale.rs"].viewed);
        assert_eq!(state.comments.len(), 2);
    }
}
