//! Durable walkthrough-step popup state.

use crate::app::ReviewSession;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WalkthroughListState {
    pub(super) step_ids: Vec<String>,
    pub(super) selected: usize,
}

impl WalkthroughListState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        let mut state = Self {
            step_ids: walkthrough_step_ids(session),
            selected: 0,
        };
        state.clamp();
        state
    }

    pub(super) fn refresh(&mut self, session: &ReviewSession) {
        let selected = self.selected_step_id().map(str::to_owned);
        self.step_ids = walkthrough_step_ids(session);
        if let Some(id) = selected
            && let Some(index) = self.step_ids.iter().position(|candidate| candidate == &id)
        {
            self.selected = index;
        }
        self.clamp();
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.step_ids.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.step_ids.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_step_id(&self) -> Option<&str> {
        self.step_ids.get(self.selected).map(String::as_str)
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.step_ids.len().saturating_sub(1));
    }
}

fn walkthrough_step_ids(session: &ReviewSession) -> Vec<String> {
    session
        .active_durable_session()
        .into_iter()
        .flat_map(|durable| durable.walkthroughs.iter())
        .flat_map(|walkthrough| walkthrough.steps.iter())
        .map(|step| step.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Focus, review, state::WalkthroughStep, tui::test_support::snapshot_session};

    #[test]
    fn selection_moves_and_refreshes_after_delete() {
        let mut session =
            snapshot_session("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n");
        session.focus = Focus::Diff;
        let durable = super::super::ensure_tui_review_session(&mut session);
        review::add_walkthrough_step(
            durable,
            WalkthroughStep {
                id: "a".into(),
                ..Default::default()
            },
        );
        review::add_walkthrough_step(
            durable,
            WalkthroughStep {
                id: "b".into(),
                ..Default::default()
            },
        );
        let mut list = WalkthroughListState::new(&session);
        list.move_selection(1);
        assert_eq!(list.selected, 1);
        let id = list.selected_step_id().unwrap().to_owned();
        let durable = session.durable_sessions_mut().first_mut().unwrap();
        review::remove_walkthrough_step(durable, &id).unwrap();
        list.refresh(&session);
        assert_eq!(list.step_ids.len(), 1);
        assert_eq!(list.selected, 0);
    }

    #[test]
    fn reorder_preserves_selected_id() {
        let mut session =
            snapshot_session("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-a\n+b\n");
        let durable = super::super::ensure_tui_review_session(&mut session);
        review::add_walkthrough_step(
            durable,
            WalkthroughStep {
                id: "a".into(),
                ..Default::default()
            },
        );
        review::add_walkthrough_step(
            durable,
            WalkthroughStep {
                id: "b".into(),
                ..Default::default()
            },
        );
        let mut list = WalkthroughListState::new(&session);
        list.selected = 1;
        review::move_walkthrough_step(session.durable_sessions_mut().first_mut().unwrap(), "b", 0)
            .unwrap();
        list.refresh(&session);
        assert_eq!(list.selected_step_id(), Some("b"));
    }
}
