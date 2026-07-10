//! Review-task overlay: actionable comments for human/agent handoff.

use crate::{app::ReviewSession, state::CommentState};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TaskListState {
    /// Comment ids matching the actionable task filter, in comment order.
    pub(super) comment_ids: Vec<String>,
    pub(super) selected: usize,
}

impl TaskListState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        let mut state = Self {
            comment_ids: actionable_comment_ids(session),
            selected: 0,
        };
        state.clamp(session);
        state
    }

    pub(super) fn refresh(&mut self, session: &ReviewSession) {
        let selected_id = self.selected_comment_id().map(str::to_owned);
        self.comment_ids = actionable_comment_ids(session);
        if let Some(id) = selected_id
            && let Some(index) = self
                .comment_ids
                .iter()
                .position(|candidate| candidate == &id)
        {
            self.selected = index;
        }
        self.clamp(session);
    }

    pub(super) fn clamp(&mut self, _session: &ReviewSession) {
        self.selected = self.selected.min(self.comment_ids.len().saturating_sub(1));
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.comment_ids.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.comment_ids.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_comment_id(&self) -> Option<&str> {
        self.comment_ids.get(self.selected).map(String::as_str)
    }
}

fn actionable_comment_ids(session: &ReviewSession) -> Vec<String> {
    session
        .comments
        .iter()
        .filter(|comment| comment.state == CommentState::Todo)
        .map(|comment| comment.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{state::ActionIntent, tui::test_support::snapshot_session};

    fn task_session() -> ReviewSession {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        session.comment_initial_state = CommentState::Draft;
        session.add_comment("plain".into());
        session.add_comment("todo".into());
        let todo = session.comments[1].id.clone();
        session.cycle_comment_state(&todo);
        session.add_comment("fix me".into());
        session.comments[2].action = Some(ActionIntent::Fix);
        session
    }

    #[test]
    fn tasks_include_only_todo_comments() {
        let session = task_session();
        let state = TaskListState::new(&session);

        assert_eq!(state.comment_ids, vec![session.comments[1].id.clone()]);
    }

    #[test]
    fn selection_moves_within_bounds() {
        let session = task_session();
        let mut state = TaskListState::new(&session);

        state.move_selection(1);
        assert_eq!(state.selected, 0);
        state.move_selection(5);
        assert_eq!(state.selected, 0);
        state.move_selection(-5);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn refresh_clamps_after_task_stops_matching() {
        let mut session = task_session();
        let mut state = TaskListState::new(&session);
        session.comments[1].state = CommentState::Draft;

        state.refresh(&session);

        assert_eq!(state.selected, 0);
        assert!(state.comment_ids.is_empty());
    }
}
