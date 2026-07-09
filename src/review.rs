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
    pub title: String,
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

pub fn find_session_for_target<'a>(
    state: &'a ReviewState,
    target: &SessionTargetSpec,
) -> Option<&'a ReviewSession> {
    state.sessions.iter().find(|session| {
        session.status == ReviewSessionStatus::Open && target_matches(&session.target, target)
    })
}

pub fn open_session_for_other_target<'a>(
    state: &'a ReviewState,
    target: &SessionTargetSpec,
) -> Option<&'a ReviewSession> {
    state.sessions.iter().find(|session| {
        session.status == ReviewSessionStatus::Open && !target_matches(&session.target, target)
    })
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

pub fn resolve_comment_id(comments: &[Comment], prefix: &str) -> Result<String> {
    let matches = comments
        .iter()
        .filter(|comment| comment.id.starts_with(prefix))
        .map(|comment| comment.id.as_str())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [id] => Ok((*id).to_owned()),
        [] => Err(eyre!("unknown comment `{prefix}`")),
        _ => Err(eyre!("ambiguous comment prefix `{prefix}`")),
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

#[derive(Debug, Clone, Default)]
pub struct CommentEdits {
    pub path: Option<String>,
    pub line: Option<Option<usize>>,
    pub end_line: Option<Option<usize>>,
    pub anchor: Option<Option<crate::anchor::CommentAnchor>>,
    pub body: Option<String>,
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
    let canonical_id = resolve_comment_id(comments, id)?;
    let comment = comments
        .iter_mut()
        .find(|comment| comment.id == canonical_id)
        .expect("resolved comment id must exist");
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

pub fn edit_comment(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    id: &str,
    edits: CommentEdits,
) -> Result<Comment> {
    let canonical_id = resolve_comment_id(comments, id)?;
    let comment = comments
        .iter_mut()
        .find(|comment| comment.id == canonical_id)
        .expect("resolved comment id must exist");
    if let Some(path) = edits.path {
        comment.path = path;
    }
    if let Some(line) = edits.line {
        comment.line = line;
    }
    if let Some(end_line) = edits.end_line {
        comment.end_line = end_line;
    }
    if let Some(anchor) = edits.anchor {
        comment.anchor = anchor;
    }
    if let Some(body) = edits.body {
        comment.body = body;
    }
    touch(session);
    Ok(comment.clone())
}

pub fn delete_comment(
    session: &mut ReviewSession,
    comments: &mut Vec<Comment>,
    id: &str,
) -> Result<Comment> {
    let canonical_id = resolve_comment_id(comments, id)?;
    let index = comments
        .iter()
        .position(|comment| comment.id == canonical_id)
        .expect("resolved comment id must exist");
    let comment = comments.remove(index);
    touch(session);
    Ok(comment)
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
    let canonical_id = resolve_task_id(session, id)?;
    let task = session
        .tasks
        .iter_mut()
        .find(|task| task.id == canonical_id)
        .unwrap();
    task.status = ReviewTaskStatus::Done;
    task.resolution = summary;
    task.updated_at = Some(chrono::Utc::now());
    let out = task.clone();
    touch(session);
    Ok(out)
}

pub fn reopen_task(session: &mut ReviewSession, id: &str) -> Result<ReviewTask> {
    let canonical_id = resolve_task_id(session, id)?;
    let task = session
        .tasks
        .iter_mut()
        .find(|task| task.id == canonical_id)
        .unwrap();
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
            title: task.title.clone(),
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
                title: title_from_comment(c),
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

fn title_from_comment(comment: &Comment) -> String {
    let first = comment.body.trim().lines().next().unwrap_or("").trim();
    if first.is_empty() {
        format!("comment {}", &comment.id[..comment.id.len().min(8)])
    } else if first.chars().count() > 72 {
        format!("{}…", first.chars().take(71).collect::<String>())
    } else {
        first.to_owned()
    }
}

pub fn resolve_task_id(session: &ReviewSession, id: &str) -> Result<String> {
    let matches = session
        .tasks
        .iter()
        .filter(|task| task.id.starts_with(id))
        .map(|task| task.id.clone())
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(eyre!("unknown task `{id}`")),
        [only] => Ok(only.clone()),
        _ => Err(eyre!("ambiguous task id prefix `{id}`")),
    }
}

pub struct TaskEdits {
    pub title: Option<String>,
    pub body: Option<String>,
    pub action: Option<ActionIntent>,
    pub source_comment_id: Option<String>,
    pub target: Option<ReviewTarget>,
}

pub fn edit_task(session: &mut ReviewSession, id: &str, edits: TaskEdits) -> Result<ReviewTask> {
    let canonical_id = resolve_task_id(session, id)?;
    let task = session
        .tasks
        .iter_mut()
        .find(|task| task.id == canonical_id)
        .unwrap();
    if let Some(title) = edits.title {
        task.title = title;
    }
    if edits.body.is_some() {
        task.body = edits.body;
    }
    if let Some(action) = edits.action {
        task.action = action;
    }
    if edits.source_comment_id.is_some() {
        task.source_comment_id = edits.source_comment_id;
    }
    if edits.target.is_some() {
        task.target = edits.target;
    }
    task.updated_at = Some(chrono::Utc::now());
    let out = task.clone();
    touch(session);
    Ok(out)
}

pub fn delete_task(session: &mut ReviewSession, id: &str) -> Result<ReviewTask> {
    let canonical_id = resolve_task_id(session, id)?;
    let index = session
        .tasks
        .iter()
        .position(|task| task.id == canonical_id)
        .unwrap();
    let task = session.tasks.remove(index);
    touch(session);
    Ok(task)
}

pub fn add_walkthrough_step(session: &mut ReviewSession, step: WalkthroughStep) -> WalkthroughStep {
    ensure_default_walkthrough(session);
    let step = prepare_walkthrough_step(step);
    session.walkthroughs[0].steps.push(step.clone());
    session.walkthroughs[0].updated_at = step.updated_at;
    touch(session);
    step
}

#[allow(dead_code)]
pub fn set_walkthrough(
    session: &mut ReviewSession,
    title: Option<String>,
    steps: Vec<WalkthroughStep>,
) -> Walkthrough {
    ensure_default_walkthrough(session);
    let now = chrono::Utc::now();
    let steps = steps.into_iter().map(prepare_walkthrough_step).collect();
    session.walkthroughs[0].title = title;
    session.walkthroughs[0].steps = steps;
    session.walkthroughs[0].updated_at = Some(now);
    touch(session);
    session.walkthroughs[0].clone()
}

pub fn set_walkthrough_preserve_ids(
    session: &mut ReviewSession,
    title: Option<String>,
    steps: Vec<WalkthroughStep>,
) -> Walkthrough {
    ensure_default_walkthrough(session);
    let prior = session.walkthroughs[0].steps.clone();
    let steps = preserve_walkthrough_step_ids(&prior, steps);
    set_walkthrough(session, title, steps)
}

pub fn preserve_walkthrough_step_ids(
    prior: &[WalkthroughStep],
    mut steps: Vec<WalkthroughStep>,
) -> Vec<WalkthroughStep> {
    for step in &mut steps {
        if !step.id.is_empty() && prior.iter().any(|old| old.id == step.id) {
            continue;
        }
        if let Some(old) = prior
            .iter()
            .find(|old| same_walkthrough_identity(old, step))
        {
            step.id = old.id.clone();
        } else if step.id.is_empty() {
            step.id = uuid::Uuid::new_v4().to_string();
        }
    }
    steps
}

fn same_walkthrough_identity(a: &WalkthroughStep, b: &WalkthroughStep) -> bool {
    a.kind == b.kind
        && a.title == b.title
        && a.target.file == b.target.file
        && a.target.line == b.target.line
}

#[allow(dead_code)]
pub fn add_chapter(
    session: &mut ReviewSession,
    change_id: String,
    summary: String,
    title: Option<String>,
) -> WalkthroughStep {
    add_walkthrough_step(
        session,
        WalkthroughStep {
            change_id: Some(change_id.clone()),
            title: title.or(Some(change_id)),
            body: Some(summary),
            kind: crate::state::StepKind::Chapter,
            ..Default::default()
        },
    )
}

fn ensure_default_walkthrough(session: &mut ReviewSession) {
    if session.walkthroughs.is_empty() {
        session.walkthroughs.push(Walkthrough {
            id: uuid::Uuid::new_v4().to_string(),
            title: Some("Walkthrough".to_owned()),
            steps: Vec::new(),
            updated_at: Some(chrono::Utc::now()),
        });
    }
}

fn prepare_walkthrough_step(mut step: WalkthroughStep) -> WalkthroughStep {
    if step.id.is_empty() {
        step.id = uuid::Uuid::new_v4().to_string();
    }
    step.updated_at = Some(chrono::Utc::now());
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
            let mut step = walkthrough.steps.remove(index);
            step.updated_at = Some(chrono::Utc::now());
            walkthrough.updated_at = step.updated_at;
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
            let mut step = walkthrough.steps.remove(index);
            step.updated_at = Some(chrono::Utc::now());
            let to = new_index.min(walkthrough.steps.len());
            walkthrough.steps.insert(to, step.clone());
            walkthrough.updated_at = step.updated_at;
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
    fn find_session_for_target_is_non_creating() {
        let mut state = ReviewState::default();
        assert!(find_session_for_target(&state, &spec()).is_none());
        assert!(state.sessions.is_empty());

        let id = ensure_session(&mut state, &spec(), Some("Review"))
            .id
            .clone();
        assert_eq!(find_session_for_target(&state, &spec()).unwrap().id, id);

        let mut other = spec();
        other.base = Some("main".into());
        assert!(find_session_for_target(&state, &other).is_none());
        assert_eq!(
            open_session_for_other_target(&state, &other).unwrap().id,
            id
        );
        assert_eq!(state.sessions.len(), 1);
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
    fn comment_backed_task_titles_are_synthesized() {
        fn comment(id: &str, body: &str) -> Comment {
            Comment {
                id: id.into(),
                path: "a.rs".into(),
                line: Some(1),
                end_line: None,
                anchor: None,
                body: body.into(),
                kind: None,
                action: None,
                state: CommentState::Todo,
                created_at: chrono::Utc::now(),
            }
        }
        let session = ReviewSession::default();
        let long = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz";
        let comments = vec![
            comment("abcdef00-0000", "first line\nsecond"),
            comment("bbbbbbbb-0000", long),
            comment("cccccccc-0000", "   "),
        ];
        let tasks = list_tasks(&session, &comments);
        assert_eq!(tasks[0].title, "first line");
        assert!(tasks[1].title.ends_with('…'));
        assert!(tasks[1].title.chars().count() <= 72);
        assert_eq!(tasks[2].title, "comment cccccccc");
    }

    #[test]
    fn resolve_task_id_rejects_ambiguous_prefixes() {
        let session = ReviewSession {
            tasks: vec![
                ReviewTask {
                    id: "abcdef00-0000".into(),
                    ..Default::default()
                },
                ReviewTask {
                    id: "abc12300-0000".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            resolve_task_id(&session, "abcdef").unwrap(),
            "abcdef00-0000"
        );
        assert_eq!(
            resolve_task_id(&session, "abc").unwrap_err().to_string(),
            "ambiguous task id prefix `abc`"
        );
    }

    #[test]
    fn resolve_comment_id_canonicalizes_prefix_and_rejects_bad_links() {
        fn comment(id: &str) -> Comment {
            Comment {
                id: id.into(),
                path: "a.txt".into(),
                line: None,
                end_line: None,
                anchor: None,
                body: String::new(),
                kind: None,
                action: None,
                state: CommentState::Draft,
                created_at: chrono::Utc::now(),
            }
        }
        let comments = vec![
            comment("abcdef00-0000-0000-0000-000000000000"),
            comment("abc12300-0000-0000-0000-000000000000"),
        ];

        assert_eq!(
            resolve_comment_id(&comments, "abcdef").unwrap(),
            "abcdef00-0000-0000-0000-000000000000"
        );
        assert_eq!(
            resolve_comment_id(&comments, "deadbeef")
                .unwrap_err()
                .to_string(),
            "unknown comment `deadbeef`"
        );
        assert_eq!(
            resolve_comment_id(&comments, "abc")
                .unwrap_err()
                .to_string(),
            "ambiguous comment prefix `abc`"
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
