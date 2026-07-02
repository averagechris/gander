//! Comment list overlay: browse every recorded comment, jump to its anchor,
//! cycle its draft/todo/resolved state, or delete it.

use crate::app::ReviewSession;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CommentListState {
    pub(super) selected: usize,
}

impl CommentListState {
    pub(super) fn clamp(&mut self, session: &ReviewSession) {
        self.selected = self.selected.min(session.comments.len().saturating_sub(1));
    }

    pub(super) fn move_selection(&mut self, delta: isize, session: &ReviewSession) {
        if session.comments.is_empty() {
            self.selected = 0;
            return;
        }
        let max = session.comments.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_comment_id(&self, session: &ReviewSession) -> Option<String> {
        session
            .comments
            .get(self.selected)
            .map(|comment| comment.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{state::CommentState, tui::test_support::snapshot_session};

    fn commented_session() -> ReviewSession {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        session.add_comment("first".into());
        session.add_comment("second".into());
        session
    }

    #[test]
    fn selection_moves_within_bounds() {
        let session = commented_session();
        let mut list = CommentListState::default();

        list.move_selection(1, &session);
        assert_eq!(list.selected, 1);
        list.move_selection(5, &session);
        assert_eq!(list.selected, 1);
        list.move_selection(-5, &session);
        assert_eq!(list.selected, 0);
    }

    #[test]
    fn clamp_recovers_after_deletion() {
        let mut session = commented_session();
        let mut list = CommentListState { selected: 1 };
        let id = list.selected_comment_id(&session).unwrap();
        session.delete_comment(&id);

        list.clamp(&session);

        assert_eq!(list.selected, 0);
        assert!(list.selected_comment_id(&session).is_some());
    }

    #[test]
    fn cycling_state_walks_draft_todo_resolved() {
        let mut session = commented_session();
        let list = CommentListState::default();
        let id = list.selected_comment_id(&session).unwrap();

        assert_eq!(session.cycle_comment_state(&id), Some(CommentState::Todo));
        assert_eq!(
            session.cycle_comment_state(&id),
            Some(CommentState::Resolved)
        );
        assert_eq!(session.cycle_comment_state(&id), Some(CommentState::Draft));
    }
}
