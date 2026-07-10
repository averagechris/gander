//! Interface-agnostic review service over durable [`ReviewState`].
//!
//! Per `docs/vision.md`, this layer mutates only local Gander review state so
//! the CLI, TUI, and MCP can share the same lifecycle semantics.

use color_eyre::eyre::{Result, eyre};
use serde::Serialize;
use std::path::Path;

use crate::ids::{resolve_unique_prefix, shortest_unique_prefix};
use crate::state::{
    ActionIntent, ActionItem, ActionItemStatus, ClosedDisposition, Comment, CommentKind,
    CommentReply, CommentState, ExternalTicket, ReviewSession, ReviewSessionStatus, ReviewState,
    ReviewTarget, Walkthrough, WalkthroughStep,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTargetSpec {
    pub repo: Option<String>,
    pub base: Option<String>,
    pub revision: Option<String>,
    pub revset: Option<String>,
}

pub fn validate_body(body: &str, label: &str) -> Result<()> {
    if body.trim().is_empty() {
        return Err(eyre!("{label} must contain non-whitespace text"));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub selector: String,
    pub title: Option<String>,
    pub target: ReviewTarget,
    pub status: ReviewSessionStatus,
    pub action_item_count: usize,
    pub walkthrough_count: usize,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ListedActionItem {
    pub id: String,
    pub selector: String,
    pub title: String,
    pub body: Option<String>,
    pub status: ActionItemStatus,
    pub action: Option<ActionIntent>,
    pub target: Option<ReviewTarget>,
    pub comment_ids: Vec<String>,
    pub external_tickets: Vec<ExternalTicket>,
    pub disposition: Option<ClosedDisposition>,
    pub outcome: Option<String>,
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
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

pub fn canonical_repo_identity(repo: &Path) -> String {
    repo.canonicalize()
        .unwrap_or_else(|_| repo.to_path_buf())
        .display()
        .to_string()
}

/// Locate the active durable session for an already-loaded review. All
/// renderers and TUI open-work surfaces must use the same repo/base/revision
/// identity so a same-shaped target from another repository cannot leak in.
pub fn active_session_for_loaded_review<'a>(
    sessions: &'a [ReviewSession],
    repo: &Path,
    base: &str,
    revision: &str,
) -> Option<&'a ReviewSession> {
    let repo = canonical_repo_identity(repo);
    sessions.iter().find(|session| {
        session.status == ReviewSessionStatus::Open
            && session.target.repo.as_deref() == Some(repo.as_str())
            && session.target.base.as_deref() == Some(base)
            && session.target.revision.as_deref() == Some(revision)
    })
}

pub fn list_sessions(state: &ReviewState) -> Vec<SessionSummary> {
    state.sessions.iter().map(session_summary).collect()
}

pub fn session_summary(session: &ReviewSession) -> SessionSummary {
    SessionSummary {
        id: session.id.clone(),
        selector: shortest_unique_prefix(&session.id, &[session.id.as_str()]),
        title: session.title.clone(),
        target: session.target.clone(),
        status: session.status,
        action_item_count: session.action_items.len(),
        walkthrough_count: session.walkthroughs.len(),
        created_at: session.created_at,
        updated_at: session.updated_at,
    }
}

pub fn find_session<'a>(state: &'a ReviewState, prefix: &str) -> Result<&'a ReviewSession> {
    resolve_unique_prefix(&state.sessions, prefix, "review session", |s| s.id.as_str())
}

pub fn resolve_comment_id(comments: &[Comment], prefix: &str) -> Result<String> {
    Ok(
        resolve_unique_prefix(comments, prefix, "comment", |c| c.id.as_str())?
            .id
            .clone(),
    )
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
#[derive(Debug, Clone)]
pub struct NewComment {
    /// The durable session that will own the new comment.
    pub session_id: String,
    /// `None` creates a general, session-level comment.
    pub path: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub anchor: Option<crate::anchor::CommentAnchor>,
    /// Immutable evidence of the already-loaded review scope at creation.
    pub observation: Option<crate::provenance::CommentObservation>,
    pub body: String,
    pub kind: Option<CommentKind>,
    pub action: Option<ActionIntent>,
    /// New comments may start as private drafts or actionable todos.
    pub state: CommentState,
}

#[derive(Debug, Clone, Default)]
pub struct CommentEdits {
    pub path: Option<String>,
    pub line: Option<Option<usize>>,
    pub end_line: Option<Option<usize>>,
    pub anchor: Option<Option<crate::anchor::CommentAnchor>>,
    pub body: Option<String>,
    pub kind: Option<Option<CommentKind>>,
    pub action: Option<Option<ActionIntent>>,
}

pub fn add_comment(
    session: &mut ReviewSession,
    comments: &mut Vec<Comment>,
    new: NewComment,
) -> Result<Comment> {
    validate_body(&new.body, "comment body")?;
    if new.session_id != session.id {
        return Err(eyre!(
            "comment session `{}` does not match active session `{}`",
            new.session_id,
            session.id
        ));
    }
    if !matches!(new.state, CommentState::Draft | CommentState::Todo) {
        return Err(eyre!(
            "new comment state must be draft or todo, not {}",
            new.state.label()
        ));
    }
    let now = chrono::Utc::now();
    let comment = Comment {
        id: uuid::Uuid::new_v4().to_string(),
        session_id: Some(new.session_id),
        path: new.path,
        line: new.line,
        end_line: new.end_line,
        anchor: new.anchor,
        observation: new.observation,
        body: new.body,
        kind: new.kind,
        action: new.action,
        state: new.state,
        replies: Vec::new(),
        created_at: now,
        updated_at: Some(now),
    };
    comment.validate()?;
    comments.push(comment.clone());
    touch_at(session, now);
    Ok(comment)
}

/// Counts returned by an atomic draft-readiness operation.
#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct ReadyCommentsResult {
    /// Draft comments changed to todo.
    pub readied: usize,
    /// Explicitly selected comments that were already todo.
    pub already_ready: usize,
}

/// Atomically mark comments ready for implementation.
///
/// `Some(selectors)` resolves every id prefix before making any mutation. An
/// unknown, ambiguous, out-of-session, or resolved selection rejects the whole
/// operation. `None` means all drafts belonging to the active session; scoped
/// comments from other sessions are simply excluded. Legacy unscoped comments
/// belong to every session for compatibility.
pub fn ready_comments(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    selectors: Option<&[String]>,
) -> Result<ReadyCommentsResult> {
    let (indices, already_ready) = if let Some(selectors) = selectors {
        let mut indices = Vec::with_capacity(selectors.len());
        for selector in selectors {
            let canonical_id = resolve_comment_id(comments, selector)?;
            let index = comments
                .iter()
                .position(|comment| comment.id == canonical_id)
                .expect("resolved comment id must exist");
            let comment = &comments[index];
            if !comment.belongs_to_session(&session.id) {
                return Err(eyre!(
                    "comment `{}` does not belong to active session `{}`",
                    comment.id,
                    session.id
                ));
            }
            if comment.state == CommentState::Resolved {
                return Err(eyre!("comment `{}` is already resolved", comment.id));
            }
            if !indices.contains(&index) {
                indices.push(index);
            }
        }
        let already_ready = indices
            .iter()
            .filter(|index| comments[**index].state == CommentState::Todo)
            .count();
        (indices, already_ready)
    } else {
        (
            comments
                .iter()
                .enumerate()
                .filter(|(_, comment)| {
                    comment.state == CommentState::Draft && comment.belongs_to_session(&session.id)
                })
                .map(|(index, _)| index)
                .collect(),
            0,
        )
    };

    let draft_indices = indices
        .into_iter()
        .filter(|index| comments[*index].state == CommentState::Draft)
        .collect::<Vec<_>>();
    if draft_indices.is_empty() {
        return Ok(ReadyCommentsResult {
            readied: 0,
            already_ready,
        });
    }

    let now = chrono::Utc::now();
    for index in &draft_indices {
        comments[*index].state = CommentState::Todo;
        comments[*index].updated_at = Some(now);
    }
    touch_at(session, now);
    Ok(ReadyCommentsResult {
        readied: draft_indices.len(),
        already_ready,
    })
}

pub fn ready_selected_comments(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    selectors: &[String],
) -> Result<ReadyCommentsResult> {
    ready_comments(session, comments, Some(selectors))
}

pub fn ready_all_drafts(
    session: &mut ReviewSession,
    comments: &mut [Comment],
) -> Result<ReadyCommentsResult> {
    ready_comments(session, comments, None)
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
    ensure_comment_belongs_to_session(comment, session)?;
    let now = chrono::Utc::now();
    comment.state = new_state;
    comment.updated_at = Some(now);
    touch_at(session, now);
    Ok(comment.clone())
}

pub fn resolve_comment(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    id: &str,
) -> Result<Comment> {
    set_comment_state(session, comments, id, CommentState::Resolved)
}

pub fn reply_to_comment(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    id: &str,
    body: String,
    snapshot: crate::provenance::SnapshotEvidence,
) -> Result<Comment> {
    reply_and_maybe_resolve_comment(session, comments, id, body, false, snapshot)
}

pub fn reply_and_maybe_resolve_comment(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    id: &str,
    body: String,
    resolve: bool,
    snapshot: crate::provenance::SnapshotEvidence,
) -> Result<Comment> {
    validate_body(&body, "reply body")?;
    let canonical_id = resolve_comment_id(comments, id)?;
    let comment = comments
        .iter_mut()
        .find(|comment| comment.id == canonical_id)
        .expect("resolved comment id must exist");
    ensure_comment_belongs_to_session(comment, session)?;
    let now = chrono::Utc::now();
    let result = crate::provenance::CommentReplyResult::compare(
        comment.id.clone(),
        comment.observation.as_ref(),
        comment.path.as_deref(),
        snapshot,
    );
    comment.replies.push(CommentReply {
        id: uuid::Uuid::new_v4().to_string(),
        body,
        created_at: now,
        result: Some(result),
    });
    if resolve {
        comment.state = CommentState::Resolved;
    }
    comment.updated_at = Some(now);
    touch_at(session, now);
    Ok(comment.clone())
}

pub fn edit_comment(
    session: &mut ReviewSession,
    comments: &mut [Comment],
    id: &str,
    edits: CommentEdits,
) -> Result<Comment> {
    let canonical_id = resolve_comment_id(comments, id)?;
    let index = comments
        .iter()
        .position(|comment| comment.id == canonical_id)
        .expect("resolved comment id must exist");
    let mut comment = comments[index].clone();
    ensure_comment_belongs_to_session(&comment, session)?;
    if let Some(path) = edits.path {
        comment.path = Some(path);
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
        validate_body(&body, "comment body")?;
        comment.body = body;
    }
    if let Some(kind) = edits.kind {
        comment.kind = kind;
    }
    if let Some(action) = edits.action {
        comment.action = action;
    }
    comment.validate()?;
    let now = chrono::Utc::now();
    comment.updated_at = Some(now);
    comments[index] = comment.clone();
    touch_at(session, now);
    Ok(comment)
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
    ensure_comment_belongs_to_session(&comments[index], session)?;
    let comment = comments.remove(index);
    touch(session);
    Ok(comment)
}

fn ensure_comment_belongs_to_session(comment: &Comment, session: &ReviewSession) -> Result<()> {
    if comment.belongs_to_session(&session.id) {
        Ok(())
    } else {
        Err(eyre!(
            "comment `{}` does not belong to active session `{}`",
            comment.id,
            session.id
        ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct NewActionItem {
    pub title: String,
    pub body: Option<String>,
    pub target: Option<ReviewTarget>,
    pub action: Option<ActionIntent>,
    pub comment_selectors: Vec<String>,
    pub external_tickets: Vec<NewExternalTicket>,
}

#[derive(Debug, Clone, Default)]
pub struct ActionItemEdits {
    pub title: Option<String>,
    pub body: Option<Option<String>>,
    pub target: Option<Option<ReviewTarget>>,
    pub action: Option<Option<ActionIntent>>,
}

#[derive(Debug, Clone)]
pub struct NewExternalTicket {
    pub tracker: String,
    pub reference: String,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OpenActionItem {
    pub item: ActionItem,
    pub linked_todo_comments: Vec<Comment>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct OpenWork {
    pub action_items: Vec<OpenActionItem>,
    pub remaining_todo_comments: Vec<Comment>,
}

pub fn list_action_items(session: &ReviewSession) -> Vec<ListedActionItem> {
    let ids = session
        .action_items
        .iter()
        .map(|item| item.id.as_str())
        .collect::<Vec<_>>();
    session
        .action_items
        .iter()
        .map(|item| ListedActionItem {
            id: item.id.clone(),
            selector: shortest_unique_prefix(&item.id, &ids),
            title: item.title.clone(),
            body: item.body.clone(),
            status: item.status,
            action: item.action,
            target: item.target.clone(),
            comment_ids: item.comment_ids.clone(),
            external_tickets: item.external_tickets.clone(),
            disposition: item.disposition,
            outcome: item.outcome.clone(),
            closed_at: item.closed_at,
            created_at: item.created_at,
            updated_at: item.updated_at,
        })
        .collect()
}

pub fn add_action_item(
    session: &mut ReviewSession,
    comments: &[Comment],
    new: NewActionItem,
) -> Result<ActionItem> {
    validate_body(&new.title, "action item title")?;
    if let Some(body) = new.body.as_deref() {
        validate_body(body, "action item body")?;
    }
    let comment_ids = resolve_link_comment_ids(session, comments, None, &new.comment_selectors)?;
    validate_new_tickets(&new.external_tickets, &[])?;
    let now = chrono::Utc::now();
    let item = ActionItem {
        id: uuid::Uuid::new_v4().to_string(),
        title: new.title,
        body: new.body,
        target: new.target,
        action: new.action,
        comment_ids,
        external_tickets: new
            .external_tickets
            .into_iter()
            .map(|ticket| ExternalTicket {
                tracker: ticket.tracker,
                reference: ticket.reference,
                url: ticket.url,
                created_at: now,
                updated_at: now,
            })
            .collect(),
        status: ActionItemStatus::Open,
        disposition: None,
        outcome: None,
        closed_at: None,
        created_at: Some(now),
        updated_at: Some(now),
    };
    session.action_items.push(item.clone());
    touch_at(session, now);
    Ok(item)
}

pub fn resolve_action_item_id(session: &ReviewSession, selector: &str) -> Result<String> {
    Ok(
        resolve_unique_prefix(&session.action_items, selector, "action item", |item| {
            item.id.as_str()
        })?
        .id
        .clone(),
    )
}

pub fn edit_action_item(
    session: &mut ReviewSession,
    selector: &str,
    edits: ActionItemEdits,
) -> Result<ActionItem> {
    if let Some(title) = edits.title.as_deref() {
        validate_body(title, "action item title")?;
    }
    if let Some(Some(body)) = edits.body.as_ref() {
        validate_body(body, "action item body")?;
    }
    let id = resolve_action_item_id(session, selector)?;
    let item = session
        .action_items
        .iter_mut()
        .find(|item| item.id == id)
        .expect("resolved action item must exist");
    if let Some(title) = edits.title {
        item.title = title;
    }
    if let Some(body) = edits.body {
        item.body = body;
    }
    if let Some(target) = edits.target {
        item.target = target;
    }
    if let Some(action) = edits.action {
        item.action = action;
    }
    let now = chrono::Utc::now();
    item.updated_at = Some(now);
    let item = item.clone();
    touch_at(session, now);
    Ok(item)
}

pub fn link_comment(
    session: &mut ReviewSession,
    comments: &[Comment],
    item_selector: &str,
    comment_selectors: &[String],
) -> Result<ActionItem> {
    let item_id = resolve_action_item_id(session, item_selector)?;
    let resolved =
        resolve_link_comment_ids(session, comments, Some(item_id.as_str()), comment_selectors)?;
    let item = session
        .action_items
        .iter_mut()
        .find(|item| item.id == item_id)
        .expect("resolved action item must exist");
    let mut changed = false;
    for comment_id in resolved {
        if !item.comment_ids.contains(&comment_id) {
            item.comment_ids.push(comment_id);
            changed = true;
        }
    }
    if changed {
        let now = chrono::Utc::now();
        item.updated_at = Some(now);
        let item = item.clone();
        touch_at(session, now);
        Ok(item)
    } else {
        Ok(item.clone())
    }
}

fn resolve_link_comment_ids(
    session: &ReviewSession,
    comments: &[Comment],
    target_item_id: Option<&str>,
    selectors: &[String],
) -> Result<Vec<String>> {
    let target_is_open = target_item_id.is_none_or(|id| {
        session
            .action_items
            .iter()
            .find(|item| item.id == id)
            .is_none_or(|item| item.status == ActionItemStatus::Open)
    });
    let mut resolved = Vec::with_capacity(selectors.len());
    for selector in selectors {
        let id = resolve_comment_id(comments, selector)?;
        let comment = comments
            .iter()
            .find(|comment| comment.id == id)
            .expect("resolved comment must exist");
        ensure_comment_belongs_to_session(comment, session)?;
        if target_is_open
            && session.action_items.iter().any(|item| {
                item.status == ActionItemStatus::Open
                    && Some(item.id.as_str()) != target_item_id
                    && item.comment_ids.contains(&id)
            })
        {
            return Err(eyre!(
                "comment `{id}` is linked to another open action item"
            ));
        }
        if !resolved.contains(&id) {
            resolved.push(id);
        }
    }
    Ok(resolved)
}

pub fn unlink_comment(
    session: &mut ReviewSession,
    comments: &[Comment],
    item_selector: &str,
    comment_selectors: &[String],
) -> Result<ActionItem> {
    let item_id = resolve_action_item_id(session, item_selector)?;
    let mut ids = Vec::with_capacity(comment_selectors.len());
    for selector in comment_selectors {
        let id = resolve_comment_id(comments, selector)?;
        let comment = comments
            .iter()
            .find(|comment| comment.id == id)
            .expect("resolved comment must exist");
        ensure_comment_belongs_to_session(comment, session)?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let item = session
        .action_items
        .iter_mut()
        .find(|item| item.id == item_id)
        .expect("resolved action item must exist");
    let old_len = item.comment_ids.len();
    item.comment_ids.retain(|id| !ids.contains(id));
    if item.comment_ids.len() != old_len {
        let now = chrono::Utc::now();
        item.updated_at = Some(now);
        let item = item.clone();
        touch_at(session, now);
        Ok(item)
    } else {
        Ok(item.clone())
    }
}

pub fn add_ticket(
    session: &mut ReviewSession,
    item_selector: &str,
    new: NewExternalTicket,
) -> Result<ActionItem> {
    let item_id = resolve_action_item_id(session, item_selector)?;
    let item = session
        .action_items
        .iter_mut()
        .find(|item| item.id == item_id)
        .expect("resolved action item must exist");
    validate_new_tickets(std::slice::from_ref(&new), &item.external_tickets)?;
    let now = chrono::Utc::now();
    item.external_tickets.push(ExternalTicket {
        tracker: new.tracker,
        reference: new.reference,
        url: new.url,
        created_at: now,
        updated_at: now,
    });
    item.updated_at = Some(now);
    let item = item.clone();
    touch_at(session, now);
    Ok(item)
}

fn validate_new_tickets(new: &[NewExternalTicket], existing: &[ExternalTicket]) -> Result<()> {
    let mut references = existing
        .iter()
        .map(|ticket| ticket.reference.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for ticket in new {
        validate_body(&ticket.tracker, "ticket tracker")?;
        validate_body(&ticket.reference, "ticket reference")?;
        if !references.insert(ticket.reference.clone()) {
            return Err(eyre!(
                "ticket reference `{}` is already linked",
                ticket.reference
            ));
        }
    }
    Ok(())
}

pub fn remove_ticket(
    session: &mut ReviewSession,
    item_selector: &str,
    reference_prefix: &str,
) -> Result<ActionItem> {
    let item_id = resolve_action_item_id(session, item_selector)?;
    let item_index = session
        .action_items
        .iter()
        .position(|item| item.id == item_id)
        .expect("resolved action item must exist");
    let matches = session.action_items[item_index]
        .external_tickets
        .iter()
        .enumerate()
        .filter(|(_, ticket)| ticket.reference.starts_with(reference_prefix))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let ticket_index = match matches.as_slice() {
        [index] => *index,
        [] => return Err(eyre!("unknown external ticket `{reference_prefix}`")),
        _ => {
            return Err(eyre!(
                "ambiguous external ticket prefix `{reference_prefix}`"
            ));
        }
    };
    let selected = &session.action_items[item_index];
    if selected.status == ActionItemStatus::Closed
        && selected.disposition == Some(ClosedDisposition::Deferred)
        && selected.external_tickets.len() == 1
    {
        return Err(eyre!("deferred action item requires an external ticket"));
    }
    let now = chrono::Utc::now();
    let item = &mut session.action_items[item_index];
    item.external_tickets.remove(ticket_index);
    item.updated_at = Some(now);
    let item = item.clone();
    touch_at(session, now);
    Ok(item)
}

pub fn close_action_item(
    session: &mut ReviewSession,
    selector: &str,
    disposition: ClosedDisposition,
    outcome: Option<String>,
) -> Result<ActionItem> {
    if let Some(outcome) = outcome.as_deref() {
        validate_body(outcome, "action item outcome")?;
    }
    let id = resolve_action_item_id(session, selector)?;
    let item = session
        .action_items
        .iter_mut()
        .find(|item| item.id == id)
        .expect("resolved action item must exist");
    if disposition == ClosedDisposition::Deferred && item.external_tickets.is_empty() {
        return Err(eyre!("deferred action item requires an external ticket"));
    }
    let now = chrono::Utc::now();
    item.status = ActionItemStatus::Closed;
    item.disposition = Some(disposition);
    item.outcome = outcome;
    item.closed_at = Some(now);
    item.updated_at = Some(now);
    let item = item.clone();
    touch_at(session, now);
    Ok(item)
}

pub fn reopen_action_item(session: &mut ReviewSession, selector: &str) -> Result<ActionItem> {
    let id = resolve_action_item_id(session, selector)?;
    let reopening = session
        .action_items
        .iter()
        .find(|item| item.id == id)
        .expect("resolved action item must exist");
    if let Some(comment_id) = reopening.comment_ids.iter().find(|comment_id| {
        session.action_items.iter().any(|item| {
            item.id != id
                && item.status == ActionItemStatus::Open
                && item.comment_ids.contains(comment_id)
        })
    }) {
        return Err(eyre!(
            "comment `{comment_id}` is linked to another open action item"
        ));
    }
    let item = session
        .action_items
        .iter_mut()
        .find(|item| item.id == id)
        .expect("resolved action item must exist");
    let now = chrono::Utc::now();
    item.status = ActionItemStatus::Open;
    item.disposition = None;
    item.outcome = None;
    item.closed_at = None;
    item.updated_at = Some(now);
    let item = item.clone();
    touch_at(session, now);
    Ok(item)
}

pub fn delete_action_item(session: &mut ReviewSession, selector: &str) -> Result<ActionItem> {
    let id = resolve_action_item_id(session, selector)?;
    let index = session
        .action_items
        .iter()
        .position(|item| item.id == id)
        .expect("resolved action item must exist");
    let now = chrono::Utc::now();
    let mut item = session.action_items.remove(index);
    item.updated_at = Some(now);
    touch_at(session, now);
    Ok(item)
}

pub fn open_work(session: &ReviewSession, comments: &[Comment]) -> OpenWork {
    let active_todos = comments
        .iter()
        .filter(|comment| {
            comment.state == CommentState::Todo && comment.belongs_to_session(&session.id)
        })
        .collect::<Vec<_>>();
    let mut linked_ids = std::collections::BTreeSet::new();
    let action_items = session
        .action_items
        .iter()
        .filter(|item| item.status == ActionItemStatus::Open)
        .map(|item| {
            let linked_todo_comments = active_todos
                .iter()
                .filter(|comment| item.comment_ids.contains(&comment.id))
                .map(|comment| {
                    linked_ids.insert(comment.id.clone());
                    (*comment).clone()
                })
                .collect();
            OpenActionItem {
                item: item.clone(),
                linked_todo_comments,
            }
        })
        .collect();
    let remaining_todo_comments = active_todos
        .into_iter()
        .filter(|comment| !linked_ids.contains(&comment.id))
        .cloned()
        .collect();
    OpenWork {
        action_items,
        remaining_todo_comments,
    }
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
    let (walkthrough_index, index) = resolve_walkthrough_step_index(session, step_id)?;
    let walkthrough = &mut session.walkthroughs[walkthrough_index];
    let mut step = walkthrough.steps.remove(index);
    step.updated_at = Some(chrono::Utc::now());
    walkthrough.updated_at = step.updated_at;
    touch(session);
    Ok(step)
}

pub fn move_walkthrough_step(
    session: &mut ReviewSession,
    step_id: &str,
    new_index: usize,
) -> Result<WalkthroughStep> {
    let (walkthrough_index, index) = resolve_walkthrough_step_index(session, step_id)?;
    let walkthrough = &mut session.walkthroughs[walkthrough_index];
    let mut step = walkthrough.steps.remove(index);
    step.updated_at = Some(chrono::Utc::now());
    let to = new_index.min(walkthrough.steps.len());
    walkthrough.steps.insert(to, step.clone());
    walkthrough.updated_at = step.updated_at;
    touch(session);
    Ok(step)
}

fn resolve_walkthrough_step_index(
    session: &ReviewSession,
    step_id: &str,
) -> Result<(usize, usize)> {
    let matches = session
        .walkthroughs
        .iter()
        .enumerate()
        .flat_map(|(walkthrough_index, walkthrough)| {
            walkthrough
                .steps
                .iter()
                .enumerate()
                .filter_map(move |(step_index, step)| {
                    step.id
                        .starts_with(step_id)
                        .then_some((walkthrough_index, step_index))
                })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [only] => Ok(*only),
        [] => Err(eyre!("unknown walkthrough step `{step_id}`")),
        _ => Err(eyre!("ambiguous walkthrough step prefix `{step_id}`")),
    }
}

fn touch(session: &mut ReviewSession) {
    touch_at(session, chrono::Utc::now());
}

fn touch_at(session: &mut ReviewSession, now: chrono::DateTime<chrono::Utc>) {
    session.updated_at = Some(now);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> crate::provenance::SnapshotEvidence {
        crate::provenance::SnapshotEvidence::capture(
            chrono::Utc::now(),
            "session",
            ReviewTarget::default(),
            std::iter::empty(),
        )
    }
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
    fn close_and_reopen_action_item_records_lifecycle() {
        let mut session = ReviewSession::default();
        let item = add_action_item(
            &mut session,
            &[],
            NewActionItem {
                title: "Fix".into(),
                action: Some(ActionIntent::Fix),
                ..Default::default()
            },
        )
        .unwrap();
        let closed = close_action_item(
            &mut session,
            &item.id,
            ClosedDisposition::Completed,
            Some("patched".into()),
        )
        .unwrap();
        assert_eq!(closed.status, ActionItemStatus::Closed);
        assert_eq!(closed.disposition, Some(ClosedDisposition::Completed));
        assert_eq!(closed.outcome.as_deref(), Some("patched"));
        assert!(closed.closed_at.is_some());

        let reopened = reopen_action_item(&mut session, &item.id).unwrap();
        assert_eq!(reopened.status, ActionItemStatus::Open);
        assert_eq!(reopened.disposition, None);
        assert_eq!(reopened.outcome, None);
        assert_eq!(reopened.closed_at, None);
    }

    #[test]
    fn resolve_action_item_id_rejects_ambiguous_prefixes() {
        let session = ReviewSession {
            action_items: vec![
                ActionItem {
                    id: "abcdef00-0000".into(),
                    ..Default::default()
                },
                ActionItem {
                    id: "abc12300-0000".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            resolve_action_item_id(&session, "abcdef").unwrap(),
            "abcdef00-0000"
        );
        assert_eq!(
            resolve_action_item_id(&session, "abc")
                .unwrap_err()
                .to_string(),
            "ambiguous action item prefix `abc`"
        );
    }

    #[test]
    fn edit_action_item_patches_fields() {
        let mut session = ReviewSession::default();
        let item = add_action_item(
            &mut session,
            &[],
            NewActionItem {
                title: "fix".into(),
                action: Some(ActionIntent::Fix),
                target: Some(ReviewTarget {
                    file: Some("src/lib.rs".into()),
                    line: Some(10),
                    ..ReviewTarget::default()
                }),
                ..Default::default()
            },
        )
        .unwrap();

        let edited = edit_action_item(
            &mut session,
            &item.id,
            ActionItemEdits {
                title: None,
                body: None,
                action: None,
                target: Some(Some(ReviewTarget {
                    file: Some("src/main.rs".into()),
                    line: Some(10),
                    ..ReviewTarget::default()
                })),
            },
        )
        .unwrap();
        assert_eq!(
            edited.target.as_ref().unwrap().file.as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(edited.target.as_ref().unwrap().line, Some(10));
    }

    #[test]
    fn edit_comment_updates_kind_and_clears_action() {
        let mut session = ReviewSession::default();
        let mut comments = vec![Comment {
            id: "abcdef00".into(),
            body: "body".into(),
            kind: Some(CommentKind::Note),
            action: Some(ActionIntent::Fix),
            ..Default::default()
        }];

        let edited = edit_comment(
            &mut session,
            &mut comments,
            "abc",
            CommentEdits {
                kind: Some(Some(CommentKind::Issue)),
                action: Some(None),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(edited.kind, Some(CommentKind::Issue));
        assert_eq!(edited.action, None);
    }

    #[test]
    fn resolve_comment_id_canonicalizes_prefix_and_rejects_bad_links() {
        fn comment(id: &str) -> Comment {
            Comment {
                id: id.into(),
                path: Some("a.txt".into()),
                line: None,
                end_line: None,
                anchor: None,
                body: String::new(),
                kind: None,
                action: None,
                state: CommentState::Draft,
                created_at: chrono::Utc::now(),
                ..Default::default()
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

    #[test]
    fn replies_append_and_can_resolve_with_timestamp() {
        let mut session = ReviewSession::default();
        let mut comments = vec![Comment {
            id: "abcdef00".into(),
            ..Default::default()
        }];

        let replied = reply_to_comment(
            &mut session,
            &mut comments,
            "abc",
            "done".into(),
            snapshot(),
        )
        .unwrap();
        assert_eq!(replied.replies.len(), 1);
        assert_eq!(replied.state, CommentState::Draft);
        assert!(replied.updated_at.is_some());
        let legacy_result = replied.replies[0].result.as_ref().unwrap();
        assert_eq!(legacy_result.observation_aggregate_fingerprint, None);
        assert_eq!(legacy_result.portable_patch_changed, None);
        assert_eq!(
            legacy_result.related,
            crate::provenance::RelatedTransition::NotInDiff { path: None }
        );

        let resolved = reply_and_maybe_resolve_comment(
            &mut session,
            &mut comments,
            "abc",
            "fixed".into(),
            true,
            snapshot(),
        )
        .unwrap();
        assert_eq!(resolved.replies.len(), 2);
        assert_eq!(resolved.state, CommentState::Resolved);
        assert!(
            reply_to_comment(&mut session, &mut comments, "abc", "  ".into(), snapshot()).is_err()
        );
    }

    fn scoped_comment(id: &str, session_id: Option<&str>, state: CommentState) -> Comment {
        Comment {
            id: id.into(),
            session_id: session_id.map(str::to_owned),
            path: Some("src/lib.rs".into()),
            body: format!("comment {id}"),
            state,
            ..Comment::default()
        }
    }

    #[test]
    fn add_comment_stores_general_scope_and_explicit_initial_state() {
        let mut session = ReviewSession {
            id: "session-a".into(),
            ..ReviewSession::default()
        };
        let mut comments = Vec::new();

        let comment = add_comment(
            &mut session,
            &mut comments,
            NewComment {
                session_id: "session-a".into(),
                path: None,
                line: None,
                end_line: None,
                anchor: None,
                observation: None,
                body: "Overall concern".into(),
                kind: Some(CommentKind::Issue),
                action: Some(ActionIntent::Explain),
                state: CommentState::Todo,
            },
        )
        .unwrap();

        assert_eq!(comment.session_id.as_deref(), Some("session-a"));
        assert!(comment.is_general());
        assert_eq!(comment.state, CommentState::Todo);
        assert_eq!(comments, vec![comment.clone()]);
        assert_eq!(session.updated_at, comment.updated_at);
    }

    #[test]
    fn add_comment_rejects_invalid_scope_location_and_initial_state_without_mutation() {
        fn new(session_id: &str, state: CommentState) -> NewComment {
            NewComment {
                session_id: session_id.into(),
                path: None,
                line: None,
                end_line: None,
                anchor: None,
                observation: None,
                body: "body".into(),
                kind: None,
                action: None,
                state,
            }
        }

        let mut session = ReviewSession {
            id: "session-a".into(),
            updated_at: None,
            ..ReviewSession::default()
        };
        let mut comments = Vec::new();

        assert!(
            add_comment(
                &mut session,
                &mut comments,
                new("session-a", CommentState::Resolved)
            )
            .unwrap_err()
            .to_string()
            .contains("must be draft or todo")
        );
        assert!(
            add_comment(
                &mut session,
                &mut comments,
                new("session-b", CommentState::Draft)
            )
            .unwrap_err()
            .to_string()
            .contains("does not match active session")
        );
        let mut invalid_location = new("session-a", CommentState::Draft);
        invalid_location.line = Some(1);
        assert!(
            add_comment(&mut session, &mut comments, invalid_location)
                .unwrap_err()
                .to_string()
                .contains("general comment cannot")
        );
        assert!(comments.is_empty());
        assert!(session.updated_at.is_none());
    }

    #[test]
    fn ready_selected_comments_is_atomic_timestamped_and_idempotent() {
        let old_touch = chrono::Utc::now() - chrono::Duration::hours(1);
        let mut session = ReviewSession {
            id: "session-a".into(),
            updated_at: Some(old_touch),
            ..ReviewSession::default()
        };
        let mut comments = vec![
            scoped_comment("draft-a-0000", Some("session-a"), CommentState::Draft),
            scoped_comment("todo-a-0000", Some("session-a"), CommentState::Todo),
            scoped_comment("resolved-a-0000", Some("session-a"), CommentState::Resolved),
            scoped_comment("draft-b-0000", Some("session-b"), CommentState::Draft),
        ];
        let todo_updated_at = comments[1].updated_at;

        let result = ready_selected_comments(
            &mut session,
            &mut comments,
            &["draft-a".into(), "todo-a".into()],
        )
        .unwrap();
        assert_eq!(
            result,
            ReadyCommentsResult {
                readied: 1,
                already_ready: 1
            }
        );
        assert_eq!(comments[0].state, CommentState::Todo);
        assert_eq!(comments[0].updated_at, session.updated_at);
        assert_eq!(comments[1].updated_at, todo_updated_at);
        let first_touch = session.updated_at;

        let repeated = ready_selected_comments(
            &mut session,
            &mut comments,
            &["draft-a".into(), "todo-a".into(), "draft-a".into()],
        )
        .unwrap();
        assert_eq!(repeated.readied, 0);
        assert_eq!(repeated.already_ready, 2);
        assert_eq!(session.updated_at, first_touch);

        let snapshot = comments.clone();
        assert!(
            ready_selected_comments(
                &mut session,
                &mut comments,
                &["draft-a".into(), "missing".into()]
            )
            .is_err()
        );
        assert_eq!(comments, snapshot);
        assert_eq!(session.updated_at, first_touch);

        assert!(ready_selected_comments(&mut session, &mut comments, &["draft-b".into()]).is_err());
        assert!(
            ready_selected_comments(&mut session, &mut comments, &["resolved-a".into()]).is_err()
        );
        assert_eq!(comments, snapshot);
        assert_eq!(session.updated_at, first_touch);
    }

    #[test]
    fn ready_all_drafts_scopes_session_and_includes_legacy_comments() {
        let mut session = ReviewSession {
            id: "session-a".into(),
            ..ReviewSession::default()
        };
        let mut comments = vec![
            scoped_comment("scoped-a", Some("session-a"), CommentState::Draft),
            scoped_comment("legacy", None, CommentState::Draft),
            scoped_comment("scoped-b", Some("session-b"), CommentState::Draft),
            scoped_comment("todo-a", Some("session-a"), CommentState::Todo),
        ];

        let result = ready_all_drafts(&mut session, &mut comments).unwrap();
        assert_eq!(result.readied, 2);
        assert_eq!(result.already_ready, 0);
        assert_eq!(comments[0].state, CommentState::Todo);
        assert_eq!(comments[1].state, CommentState::Todo);
        assert_eq!(comments[2].state, CommentState::Draft);
        assert_eq!(comments[3].state, CommentState::Todo);
        assert_eq!(comments[0].updated_at, comments[1].updated_at);
        assert_eq!(comments[0].updated_at, session.updated_at);
        let first_touch = session.updated_at;

        let repeated = ready_all_drafts(&mut session, &mut comments).unwrap();
        assert_eq!(repeated, ReadyCommentsResult::default());
        assert_eq!(session.updated_at, first_touch);
    }

    #[test]
    fn comment_mutations_reject_foreign_session_scope() {
        let mut session = ReviewSession {
            id: "session-a".into(),
            ..ReviewSession::default()
        };
        let foreign = scoped_comment("foreign-comment", Some("session-b"), CommentState::Draft);
        let mut comments = vec![foreign.clone()];

        assert!(
            set_comment_state(&mut session, &mut comments, "foreign-", CommentState::Todo).is_err()
        );
        assert!(
            edit_comment(
                &mut session,
                &mut comments,
                "foreign-",
                CommentEdits {
                    body: Some("changed".into()),
                    ..Default::default()
                }
            )
            .is_err()
        );
        assert!(
            reply_to_comment(
                &mut session,
                &mut comments,
                "foreign-",
                "reply".into(),
                snapshot(),
            )
            .is_err()
        );
        assert!(delete_comment(&mut session, &mut comments, "foreign-").is_err());
        assert_eq!(comments, vec![foreign]);
        assert!(session.updated_at.is_none());
    }

    fn item(id: &str, status: ActionItemStatus, comment_ids: &[&str]) -> ActionItem {
        ActionItem {
            id: id.into(),
            title: id.into(),
            status,
            comment_ids: comment_ids.iter().map(|id| (*id).into()).collect(),
            ..ActionItem::default()
        }
    }

    #[test]
    fn multi_comment_link_is_atomic_deduped_owned_and_one_open_item_only() {
        let mut session = ReviewSession {
            id: "session-a".into(),
            action_items: vec![
                item("item-open-a", ActionItemStatus::Open, &[]),
                item("item-open-b", ActionItemStatus::Open, &[]),
                item("item-closed", ActionItemStatus::Closed, &[]),
            ],
            ..ReviewSession::default()
        };
        let comments = vec![
            scoped_comment("comment-a-0000", Some("session-a"), CommentState::Todo),
            scoped_comment("comment-b-0000", Some("session-a"), CommentState::Todo),
            scoped_comment("comment-foreign", Some("session-b"), CommentState::Todo),
        ];

        let linked = link_comment(
            &mut session,
            &comments,
            "item-open-a",
            &["comment-a".into(), "comment-a".into()],
        )
        .unwrap();
        assert_eq!(linked.comment_ids, ["comment-a-0000"]);
        assert_eq!(comments[0].state, CommentState::Todo);

        let snapshot = session.clone();
        assert!(
            link_comment(
                &mut session,
                &comments,
                "item-open-a",
                &["comment-b".into(), "missing".into()]
            )
            .is_err()
        );
        assert_eq!(session, snapshot);
        assert!(
            link_comment(
                &mut session,
                &comments,
                "item-open-b",
                &["comment-a".into()]
            )
            .unwrap_err()
            .to_string()
            .contains("another open action item")
        );
        assert!(
            link_comment(
                &mut session,
                &comments,
                "item-open-a",
                &["comment-b".into(), "comment-foreign".into()]
            )
            .is_err()
        );
        assert_eq!(session, snapshot);

        let historical = link_comment(
            &mut session,
            &comments,
            "item-closed",
            &["comment-a".into()],
        )
        .unwrap();
        assert_eq!(historical.comment_ids, ["comment-a-0000"]);
    }

    #[test]
    fn add_list_edit_unlink_and_delete_share_canonical_durable_state() {
        let mut session = ReviewSession {
            id: "session-a".into(),
            ..ReviewSession::default()
        };
        let comments = vec![scoped_comment(
            "comment-a-0000",
            Some("session-a"),
            CommentState::Todo,
        )];
        let added = add_action_item(
            &mut session,
            &comments,
            NewActionItem {
                title: "Coordinate fix".into(),
                body: Some("Original body".into()),
                comment_selectors: vec!["comment-a".into(), "comment-a".into()],
                external_tickets: vec![NewExternalTicket {
                    tracker: "SourceHut".into(),
                    reference: "todo/42".into(),
                    url: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(added.comment_ids, ["comment-a-0000"]);
        assert_eq!(added.external_tickets.len(), 1);
        assert_eq!(added.updated_at, session.updated_at);
        assert_eq!(list_action_items(&session)[0].id, added.id);

        let edited = edit_action_item(
            &mut session,
            &added.id[..8],
            ActionItemEdits {
                title: Some("Coordinate final fix".into()),
                body: Some(None),
                action: Some(Some(ActionIntent::FollowUp)),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(edited.body, None);
        assert_eq!(edited.action, Some(ActionIntent::FollowUp));
        let unlinked = unlink_comment(
            &mut session,
            &comments,
            &added.id[..8],
            &["comment-a".into(), "comment-a".into()],
        )
        .unwrap();
        assert!(unlinked.comment_ids.is_empty());
        assert_eq!(comments[0].state, CommentState::Todo);

        let deleted = delete_action_item(&mut session, &added.id[..8]).unwrap();
        assert_eq!(deleted.id, added.id);
        assert_eq!(deleted.updated_at, session.updated_at);
        assert!(session.action_items.is_empty());
    }

    #[test]
    fn deferred_requires_ticket_and_reopen_clears_closure() {
        let mut session = ReviewSession {
            action_items: vec![item("item-abcdef", ActionItemStatus::Open, &[])],
            ..ReviewSession::default()
        };
        let snapshot = session.clone();
        assert!(
            close_action_item(
                &mut session,
                "item-a",
                ClosedDisposition::Deferred,
                Some("tracked elsewhere".into())
            )
            .is_err()
        );
        assert_eq!(session, snapshot);

        add_ticket(
            &mut session,
            "item-a",
            NewExternalTicket {
                tracker: "Linear".into(),
                reference: "GAN-123".into(),
                url: Some("https://linear.example/GAN-123".into()),
            },
        )
        .unwrap();
        add_ticket(
            &mut session,
            "item-a",
            NewExternalTicket {
                tracker: "Linear".into(),
                reference: "GAN-124".into(),
                url: None,
            },
        )
        .unwrap();
        assert!(remove_ticket(&mut session, "item-a", "GAN-12").is_err());
        assert_eq!(
            remove_ticket(&mut session, "item-a", "GAN-124")
                .unwrap()
                .external_tickets
                .len(),
            1
        );

        let closed = close_action_item(
            &mut session,
            "item-a",
            ClosedDisposition::Deferred,
            Some("tracked elsewhere".into()),
        )
        .unwrap();
        assert_eq!(closed.disposition, Some(ClosedDisposition::Deferred));
        assert!(closed.closed_at.is_some());
        assert!(remove_ticket(&mut session, "item-a", "GAN-123").is_err());
        let reopened = reopen_action_item(&mut session, "item-a").unwrap();
        assert_eq!(reopened.status, ActionItemStatus::Open);
        assert_eq!(reopened.disposition, None);
        assert_eq!(reopened.outcome, None);
        assert_eq!(reopened.closed_at, None);
        assert_eq!(reopened.updated_at, session.updated_at);
    }

    #[test]
    fn open_work_folds_linked_todo_evidence_and_scopes_remaining_comments() {
        let session = ReviewSession {
            id: "session-a".into(),
            action_items: vec![
                item("open", ActionItemStatus::Open, &["linked-open"]),
                item("closed", ActionItemStatus::Closed, &["linked-closed"]),
            ],
            ..ReviewSession::default()
        };
        let comments = vec![
            scoped_comment("linked-open", Some("session-a"), CommentState::Todo),
            scoped_comment("linked-closed", Some("session-a"), CommentState::Todo),
            scoped_comment("remaining", None, CommentState::Todo),
            scoped_comment("resolved", Some("session-a"), CommentState::Resolved),
            scoped_comment("foreign", Some("session-b"), CommentState::Todo),
        ];

        let work = open_work(&session, &comments);
        assert_eq!(work.action_items.len(), 1);
        assert_eq!(work.action_items[0].item.id, "open");
        assert_eq!(
            work.action_items[0].linked_todo_comments[0].id,
            "linked-open"
        );
        assert_eq!(
            work.remaining_todo_comments
                .iter()
                .map(|comment| comment.id.as_str())
                .collect::<Vec<_>>(),
            ["linked-closed", "remaining"]
        );
        assert_eq!(list_action_items(&session).len(), 2);
    }
}
