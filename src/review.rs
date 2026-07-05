//! Interface-agnostic review service over durable [`ReviewState`].
//!
//! Per `docs/vision.md`, this layer mutates only local Gander review state so
//! the CLI, TUI, and MCP can share the same lifecycle semantics.

use color_eyre::eyre::{Result, eyre};
use serde::Serialize;

use crate::state::{
    ActionIntent, Comment, CommentKind, CommentState, ReviewSession, ReviewSessionStatus,
    ReviewState, ReviewTarget, ReviewTask, ReviewTaskStatus, Walkthrough, WalkthroughStep,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTargetSpec {
    pub repo: Option<String>,
    pub base: Option<String>,
    pub revision: Option<String>,
    pub revset: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: Option<String>,
    pub target: ReviewTarget,
    pub status: ReviewSessionStatus,
    pub task_count: usize,
    pub walkthrough_count: usize,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ListedTask {
    pub id: String,
    pub source: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub status: ReviewTaskStatus,
    pub action: Option<ActionIntent>,
    pub target: Option<ReviewTarget>,
    pub source_comment_id: Option<String>,
    pub resolution: Option<String>,
}

pub fn ensure_session<'a>(
    state: &'a mut ReviewState,
    target: &SessionTargetSpec,
    title: Option<&str>,
) -> &'a mut ReviewSession {
    if let Some(index) = state.sessions.iter().position(|session| {
        session.status == ReviewSessionStatus::Open && target_matches(&session.target, target)
    }) {
        return &mut state.sessions[index];
    }
    let now = chrono::Utc::now();
    state.sessions.push(ReviewSession {
        id: uuid::Uuid::new_v4().to_string(),
        title: title.map(str::to_owned),
        target: ReviewTarget {
            repo: target.repo.clone(),
            base: target.base.clone(),
            revision: target.revision.clone(),
            revset: target.revset.clone(),
            ..ReviewTarget::default()
        },
        status: ReviewSessionStatus::Open,
        created_at: Some(now),
        updated_at: Some(now),
        ..ReviewSession::default()
    });
    state.sessions.last_mut().unwrap()
}

fn target_matches(actual: &ReviewTarget, spec: &SessionTargetSpec) -> bool {
    actual.repo == spec.repo && actual.base == spec.base && actual.revision == spec.revision
}

pub fn list_sessions(state: &ReviewState) -> Vec<SessionSummary> {
    state.sessions.iter().map(session_summary).collect()
}

pub fn session_summary(session: &ReviewSession) -> SessionSummary {
    SessionSummary {
        id: session.id.clone(),
        title: session.title.clone(),
        target: session.target.clone(),
        status: session.status,
        task_count: session.tasks.len(),
        walkthrough_count: session.walkthroughs.len(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

pub fn find_session<'a>(state: &'a ReviewState, prefix: &str) -> Result<&'a ReviewSession> {
    let matches = state
        .sessions
        .iter()
        .filter(|session| session.id.starts_with(prefix))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [session] => Ok(session),
        [] => Err(eyre!("unknown review session `{prefix}`")),
        _ => Err(eyre!("ambiguous review session prefix `{prefix}`")),
    }
}

#[allow(dead_code)]
pub fn find_session_mut<'a>(
    state: &'a mut ReviewState,
    prefix: &str,
) -> Result<&'a mut ReviewSession> {
    let matches = state
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, session)| session.id.starts_with(prefix))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [index] => Ok(&mut state.sessions[*index]),
        [] => Err(eyre!("unknown review session `{prefix}`")),
        _ => Err(eyre!("ambiguous review session prefix `{prefix}`")),
    }
}

/// A new durable comment to add through the service layer.
///
/// Grouping the fields keeps the service call sites readable as optional
/// metadata (kind/action) grows; see docs/vision.md.
#[derive(Debug, Clone, Default)]
pub struct NewComment {
    pub path: String,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub anchor: Option<crate::anchor::CommentAnchor>,
    pub body: String,
    pub kind: Option<CommentKind>,
    pub action: Option<ActionIntent>,
}

pub fn add_comment(
    session: &mut ReviewSession,
    comments: &mut Vec<Comment>,
    new: NewComment,
) -> Comment {
    let comment = Comment {
        id: uuid::Uuid::new_v4().to_string(),
        path: new.path,
        line: new.line,
        end_line: new.end_line,
        anchor: new.anchor,
        body: new.body,
        kind: new.kind,
        action: new.action,
        state: CommentState::Draft,
        created_at: chrono::Utc::now(),
    };
    comments.push(comment.clone());
    touch(session);
    comment
}

pub fn set_comment_state(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    id: &str,
    new_state: CommentState,
) -> Result<Comment> {
    let comment = comments
        .iter_mut()
        .find(|comment| comment.id.starts_with(id))
        .ok_or_else(|| eyre!("unknown comment `{id}`"))?;
    comment.state = new_state;
    touch(session);
    Ok(comment.clone())
}

pub fn resolve_comment(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    id: &str,
) -> Result<Comment> {
    set_comment_state(session, comments, id, CommentState::Resolved)
}

pub fn add_task(
    session: &mut ReviewSession,
    title: String,
    body: Option<String>,
    action: ActionIntent,
    source_comment_id: Option<String>,
    target: Option<ReviewTarget>,
) -> ReviewTask {
    let now = chrono::Utc::now();
    let task = ReviewTask {
        id: uuid::Uuid::new_v4().to_string(),
        title,
        body,
        target,
        action,
        status: ReviewTaskStatus::Open,
        source_comment_id,
        resolution: None,
        created_at: Some(now),
        updated_at: Some(now),
    };
    session.tasks.push(task.clone());
    touch(session);
    task
}

pub fn complete_task(
    session: &mut ReviewSession,
    id: &str,
    summary: Option<String>,
) -> Result<ReviewTask> {
    let task = session
        .tasks
        .iter_mut()
        .find(|task| task.id.starts_with(id))
        .ok_or_else(|| eyre!("unknown task `{id}`"))?;
    task.status = ReviewTaskStatus::Done;
    task.resolution = summary;
    task.updated_at = Some(chrono::Utc::now());
    let out = task.clone();
    touch(session);
    Ok(out)
}

pub fn reopen_task(session: &mut ReviewSession, id: &str) -> Result<ReviewTask> {
    let task = session
        .tasks
        .iter_mut()
        .find(|task| task.id.starts_with(id))
        .ok_or_else(|| eyre!("unknown task `{id}`"))?;
    task.status = ReviewTaskStatus::Open;
    task.resolution = None;
    task.updated_at = Some(chrono::Utc::now());
    let out = task.clone();
    touch(session);
    Ok(out)
}

pub fn list_tasks(session: &ReviewSession, comments: &[Comment]) -> Vec<ListedTask> {
    let mut tasks = session
        .tasks
        .iter()
        .map(|task| ListedTask {
            id: task.id.clone(),
            source: "session".to_owned(),
            title: Some(task.title.clone()),
            body: task.body.clone(),
            status: task.status,
            action: Some(task.action),
            target: task.target.clone(),
            source_comment_id: task.source_comment_id.clone(),
            resolution: task.resolution.clone(),
        })
        .collect::<Vec<_>>();
    tasks.extend(
        comments
            .iter()
            .filter(|c| c.state == CommentState::Todo)
            .map(|c| ListedTask {
                id: c.id.clone(),
                source: "comment".to_owned(),
                title: None,
                body: Some(c.body.clone()),
                status: ReviewTaskStatus::Open,
                action: c.action,
                target: Some(ReviewTarget {
                    file: Some(c.path.clone()),
                    line: c.line,
                    end_line: c.end_line,
                    ..ReviewTarget::default()
                }),
                source_comment_id: Some(c.id.clone()),
                resolution: None,
            }),
    );
    tasks
}

pub fn add_walkthrough_step(session: &mut ReviewSession, step: WalkthroughStep) -> WalkthroughStep {
    if session.walkthroughs.is_empty() {
        session.walkthroughs.push(Walkthrough {
            id: uuid::Uuid::new_v4().to_string(),
            title: Some("Walkthrough".to_owned()),
            steps: Vec::new(),
        });
    }
    let mut step = step;
    if step.id.is_empty() {
        step.id = uuid::Uuid::new_v4().to_string();
    }
    session.walkthroughs[0].steps.push(step.clone());
    touch(session);
    step
}

pub fn remove_walkthrough_step(
    session: &mut ReviewSession,
    step_id: &str,
) -> Result<WalkthroughStep> {
    for walkthrough in &mut session.walkthroughs {
        if let Some(index) = walkthrough
            .steps
            .iter()
            .position(|step| step.id.starts_with(step_id))
        {
            let step = walkthrough.steps.remove(index);
            touch(session);
            return Ok(step);
        }
    }
    Err(eyre!("unknown walkthrough step `{step_id}`"))
}

pub fn move_walkthrough_step(
    session: &mut ReviewSession,
    step_id: &str,
    new_index: usize,
) -> Result<WalkthroughStep> {
    for walkthrough in &mut session.walkthroughs {
        if let Some(index) = walkthrough
            .steps
            .iter()
            .position(|step| step.id.starts_with(step_id))
        {
            let step = walkthrough.steps.remove(index);
            let to = new_index.min(walkthrough.steps.len());
            walkthrough.steps.insert(to, step.clone());
            touch(session);
            return Ok(step);
        }
    }
    Err(eyre!("unknown walkthrough step `{step_id}`"))
}

fn touch(session: &mut ReviewSession) {
    session.updated_at = Some(chrono::Utc::now());
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec() -> SessionTargetSpec {
        SessionTargetSpec {
            repo: Some("r".into()),
            base: Some("b".into()),
            revision: Some("h".into()),
            revset: Some("@".into()),
        }
    }

    #[test]
    fn ensure_list_and_find_session_by_prefix() {
        let mut state = ReviewState::default();
        let id = ensure_session(&mut state, &spec(), Some("Review"))
            .id
            .clone();
        assert_eq!(ensure_session(&mut state, &spec(), None).id, id);
        assert_eq!(list_sessions(&state)[0].title.as_deref(), Some("Review"));
        assert_eq!(find_session(&state, &id[..8]).unwrap().id, id);
    }

    #[test]
    fn complete_task_records_resolution() {
        let mut session = ReviewSession::default();
        let task = add_task(
            &mut session,
            "Fix".into(),
            None,
            ActionIntent::Fix,
            None,
            None,
        );
        let done = complete_task(&mut session, &task.id, Some("patched".into())).unwrap();
        assert_eq!(done.status, ReviewTaskStatus::Done);
        assert_eq!(done.resolution.as_deref(), Some("patched"));
        assert_eq!(
            reopen_task(&mut session, &task.id).unwrap().status,
            ReviewTaskStatus::Open
        );
    }

    #[test]
    fn walkthrough_add_remove_move() {
        let mut session = ReviewSession::default();
        let a = add_walkthrough_step(
            &mut session,
            WalkthroughStep {
                title: Some("a".into()),
                ..WalkthroughStep::default()
            },
        );
        let b = add_walkthrough_step(
            &mut session,
            WalkthroughStep {
                title: Some("b".into()),
                ..WalkthroughStep::default()
            },
        );
        move_walkthrough_step(&mut session, &b.id, 0).unwrap();
        assert_eq!(session.walkthroughs[0].steps[0].id, b.id);
        assert_eq!(
            remove_walkthrough_step(&mut session, &a.id).unwrap().id,
            a.id
        );
        assert_eq!(session.walkthroughs[0].steps.len(), 1);
    }
}
