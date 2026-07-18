//! Durable agent-authored draft comments awaiting human triage: accept,
//! edit-then-accept, or discard.

use crate::{app::ReviewSession, state::Comment};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DraftListState {
    /// Pending drafts only; accepted/discarded drafts leave the list.
    pub(super) drafts: Vec<Comment>,
    pub(super) selected: usize,
}

impl DraftListState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        Self {
            drafts: session.pending_agent_drafts(),
            selected: 0,
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.drafts.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.drafts.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_draft(&self) -> Option<&Comment> {
        self.drafts.get(self.selected)
    }

    /// Drop a draft that was just accepted or discarded. Returns true when
    /// no pending drafts remain (the popup should close).
    pub(super) fn remove(&mut self, draft_id: &str) -> bool {
        self.drafts.retain(|draft| draft.id != draft_id);
        self.selected = self.selected.min(self.drafts.len().saturating_sub(1));
        self.drafts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AuthorKind, Channel, CommentState, Identity};
    use crate::tui::test_support::snapshot_session;

    fn draft(id: &str, state: CommentState) -> Comment {
        Comment {
            id: id.to_owned(),
            path: Some("a.rs".to_owned()),
            line: Some(1),
            body: "note".to_owned(),
            state,
            author: Identity {
                kind: AuthorKind::Agent,
                name: "review-agent".to_owned(),
            },
            channel: Channel::Onboarding,
            ..Default::default()
        }
    }

    fn install_drafts(session: &mut ReviewSession, mut drafts: Vec<Comment>) {
        let seeded = session
            .add_agent_draft("a.rs".into(), Some(1), "seed".into())
            .unwrap();
        for draft in &mut drafts {
            draft.session_id = seeded.session_id.clone();
        }
        session.comments = drafts;
    }

    #[test]
    fn lists_only_pending_drafts() {
        let mut session = snapshot_session("");
        install_drafts(
            &mut session,
            vec![
                draft("p1", CommentState::Draft),
                draft("done", CommentState::Resolved),
                draft("p2", CommentState::Draft),
            ],
        );

        let state = DraftListState::new(&session);

        let ids: Vec<&str> = state.drafts.iter().map(|draft| draft.id.as_str()).collect();
        assert_eq!(ids, ["p1", "p2"]);
    }

    #[test]
    fn remove_clamps_selection_and_reports_empty() {
        let mut session = snapshot_session("");
        install_drafts(
            &mut session,
            vec![
                draft("p1", CommentState::Draft),
                draft("p2", CommentState::Draft),
            ],
        );
        let mut state = DraftListState::new(&session);
        state.move_selection(1);

        assert!(!state.remove("p2"));
        assert_eq!(state.selected_draft().unwrap().id, "p1");
        assert!(state.remove("p1"));
    }

    #[test]
    fn triage_lists_and_mutates_only_active_session_agent_drafts() {
        let mut session = snapshot_session("");
        let active = session
            .add_agent_draft("a.rs".into(), Some(1), "active".into())
            .unwrap();
        session.comments[0].id = "active".into();
        session.sessions.push(crate::state::ReviewSession {
            id: "foreign-session".into(),
            target: crate::state::ReviewTarget {
                repo: Some(crate::review::canonical_repo_identity(&session.repo)),
                base: Some(session.target.base.clone()),
                revision: Some("other".into()),
                ..Default::default()
            },
            status: crate::state::ReviewSessionStatus::Open,
            ..Default::default()
        });
        session.comments.push(Comment {
            id: "foreign".into(),
            session_id: Some("foreign-session".into()),
            path: Some("a.rs".into()),
            body: "foreign".into(),
            state: CommentState::Draft,
            author: Identity::agent(),
            channel: Channel::Onboarding,
            ..Default::default()
        });

        let list = DraftListState::new(&session);

        assert_eq!(active.session_id, session.comments[0].session_id);
        assert_eq!(list.drafts.len(), 1);
        assert_eq!(list.drafts[0].id, "active");
        assert!(!crate::tui::accept_agent_draft(
            &mut session,
            &mut crate::tui::TuiState::default(),
            "foreign",
            None,
        ));
        assert!(!session.discard_agent_draft("foreign"));
        assert!(
            session
                .comments
                .iter()
                .any(|comment| comment.id == "foreign")
        );
    }
}
