//! Agent draft comments awaiting human triage: accept, edit-then-accept, or
//! discard. Dispositions are written back to the agent overlay so agents can
//! observe the outcome (two-way feedback).

use crate::{agent::AgentDraft, app::ReviewSession};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DraftListState {
    /// Pending drafts only; accepted/discarded drafts leave the list.
    pub(super) drafts: Vec<AgentDraft>,
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

    pub(super) fn selected_draft(&self) -> Option<&AgentDraft> {
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
    use crate::agent::{AgentOverlay, DraftState};
    use crate::tui::test_support::snapshot_session;

    fn draft(id: &str, state: DraftState) -> AgentDraft {
        AgentDraft {
            id: id.to_owned(),
            path: "a.rs".to_owned(),
            line: Some(1),
            body: "note".to_owned(),
            state,
            accepted_comment_id: None,
        }
    }

    #[test]
    fn lists_only_pending_drafts() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            drafts: vec![
                draft("p1", DraftState::Pending),
                draft("done", DraftState::Accepted),
                draft("p2", DraftState::Pending),
            ],
            ..Default::default()
        });

        let state = DraftListState::new(&session);

        let ids: Vec<&str> = state.drafts.iter().map(|draft| draft.id.as_str()).collect();
        assert_eq!(ids, ["p1", "p2"]);
    }

    #[test]
    fn remove_clamps_selection_and_reports_empty() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            drafts: vec![
                draft("p1", DraftState::Pending),
                draft("p2", DraftState::Pending),
            ],
            ..Default::default()
        });
        let mut state = DraftListState::new(&session);
        state.move_selection(1);

        assert!(!state.remove("p2"));
        assert_eq!(state.selected_draft().unwrap().id, "p1");
        assert!(state.remove("p1"));
    }
}
