//! Open-work overlay state: durable action items and actionable feedback.

use std::collections::BTreeSet;

use crate::{
    app::ReviewSession,
    review,
    state::{ActionIntent, CommentState, ReviewTarget},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OpenWorkRow {
    ActionItem {
        id: String,
        title: String,
        action: Option<ActionIntent>,
        target: Box<Option<ReviewTarget>>,
    },
    EvidenceComment {
        id: String,
    },
    TodoComment {
        id: String,
    },
}

impl OpenWorkRow {
    pub(super) fn comment_id(&self) -> Option<&str> {
        match self {
            Self::EvidenceComment { id } | Self::TodoComment { id } => Some(id),
            Self::ActionItem { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenWorkListState {
    pub(super) rows: Vec<OpenWorkRow>,
    pub(super) selected: usize,
}

impl OpenWorkListState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        Self {
            rows: open_work_rows(session),
            selected: 0,
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_row(&self) -> Option<&OpenWorkRow> {
        self.rows.get(self.selected)
    }
}

fn open_work_rows(session: &ReviewSession) -> Vec<OpenWorkRow> {
    let durable = review::active_session_for_loaded_review(
        &session.sessions,
        &session.repo,
        &session.target.base,
        &session.target.rev,
    );
    let mut rows = Vec::new();
    let mut nested_comment_ids = BTreeSet::new();

    if let Some(durable) = durable {
        let open_work = review::open_work(durable, &session.comments);
        for open_item in open_work.action_items {
            let item = open_item.item;
            rows.push(OpenWorkRow::ActionItem {
                id: item.id,
                title: item.title,
                action: item.action,
                target: Box::new(item.target),
            });
            for comment in open_item.linked_todo_comments {
                if nested_comment_ids.insert(comment.id.clone()) {
                    rows.push(OpenWorkRow::EvidenceComment {
                        id: comment.id.clone(),
                    });
                }
            }
        }
        rows.extend(
            open_work
                .remaining_todo_comments
                .into_iter()
                .map(|comment| OpenWorkRow::TodoComment { id: comment.id }),
        );
        return rows;
    }

    rows.extend(
        session
            .comments
            .iter()
            .filter(|comment| {
                comment.state == CommentState::Todo
                    && !nested_comment_ids.contains(&comment.id)
                    && comment.session_id.is_none()
            })
            .map(|comment| OpenWorkRow::TodoComment {
                id: comment.id.clone(),
            }),
    );
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        state::{ActionItem, ActionItemStatus, CommentState},
        tui::test_support::snapshot_session,
    };

    fn open_work_session() -> ReviewSession {
        let mut session = snapshot_session(
            r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
"#,
        );
        session.add_comment("linked todo".into());
        session.add_comment("independent todo".into());
        session.add_comment("linked draft".into());
        session.comments[0].id = "linked-todo".into();
        session.comments[1].id = "independent-todo".into();
        session.comments[2].id = "linked-draft".into();
        session.comments[0].state = CommentState::Todo;
        session.comments[0].channel = crate::state::Channel::Delegation;
        session.comments[1].state = CommentState::Todo;
        session.comments[1].channel = crate::state::Channel::Delegation;
        session.comments[2].state = CommentState::Draft;
        let durable_id = session.comments[0].session_id.clone().unwrap();
        let durable = session
            .sessions
            .iter_mut()
            .find(|durable| durable.id == durable_id)
            .unwrap();
        durable.action_items.extend([
            ActionItem {
                id: "open-item".into(),
                title: "Open durable item".into(),
                action: Some(ActionIntent::Fix),
                comment_ids: vec!["linked-todo".into(), "linked-draft".into()],
                ..ActionItem::default()
            },
            ActionItem {
                id: "second-open-item".into(),
                title: "Another item sharing evidence".into(),
                comment_ids: vec!["linked-todo".into()],
                ..ActionItem::default()
            },
            ActionItem {
                id: "closed-item".into(),
                title: "Closed durable item".into(),
                status: ActionItemStatus::Closed,
                ..ActionItem::default()
            },
        ]);
        session
    }

    #[test]
    fn rows_nest_linked_todo_evidence_and_dedupe_it_from_feedback() {
        let session = open_work_session();
        let state = OpenWorkListState::new(&session);

        assert_eq!(
            state.rows,
            vec![
                OpenWorkRow::ActionItem {
                    id: "open-item".into(),
                    title: "Open durable item".into(),
                    action: Some(ActionIntent::Fix),
                    target: Box::new(None),
                },
                OpenWorkRow::EvidenceComment {
                    id: "linked-todo".into(),
                },
                OpenWorkRow::ActionItem {
                    id: "second-open-item".into(),
                    title: "Another item sharing evidence".into(),
                    action: None,
                    target: Box::new(None),
                },
                OpenWorkRow::TodoComment {
                    id: "independent-todo".into(),
                },
            ]
        );
    }

    #[test]
    fn open_work_requires_matching_repo_identity() {
        let mut session = open_work_session();
        let mut wrong = session.sessions[0].clone();
        wrong.id = "wrong-repo".into();
        wrong.target.repo = Some("/other-repo".into());
        wrong.action_items[0].title = "Wrong repo action".into();
        session.sessions.insert(0, wrong);

        let state = OpenWorkListState::new(&session);

        assert!(state.rows.iter().any(|row| matches!(
            row,
            OpenWorkRow::ActionItem { title, .. } if title == "Open durable item"
        )));
        assert!(!state.rows.iter().any(|row| matches!(
            row,
            OpenWorkRow::ActionItem { title, .. } if title == "Wrong repo action"
        )));
    }

    #[test]
    fn selection_moves_within_bounds() {
        let session = open_work_session();
        let mut state = OpenWorkListState::new(&session);

        state.move_selection(1);
        assert_eq!(state.selected, 1);
        state.move_selection(5);
        assert_eq!(state.selected, 3);
        state.move_selection(-5);
        assert_eq!(state.selected, 0);
    }

    #[test]
    fn todo_feedback_remains_visible_without_a_durable_session() {
        let mut session = open_work_session();
        session.sessions.clear();
        session
            .comments
            .retain(|comment| comment.id == "independent-todo");
        session.comments[0].session_id = None;

        let state = OpenWorkListState::new(&session);

        assert_eq!(
            state.rows,
            vec![OpenWorkRow::TodoComment {
                id: "independent-todo".into()
            }]
        );
    }
}
