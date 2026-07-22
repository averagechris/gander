//! Attention-map glance board state. Rows come from the current review stream,
//! effective attention map, and durable fingerprint-guarded progress.

use crate::{
    app::ReviewSession,
    attention::{self, SkimFoldSummary},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GlanceBoardState {
    pub(super) rows: Vec<SkimFoldSummary>,
    pub(super) selected: usize,
}

impl GlanceBoardState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        Self {
            rows: rows(session),
            selected: 0,
        }
    }

    pub(super) fn selected(&self) -> Option<&SkimFoldSummary> {
        self.rows.get(self.selected)
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected =
            (self.selected as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize;
    }

    pub(super) fn refresh(&mut self, session: &ReviewSession) {
        let selected_id = self.selected().map(|row| row.id.clone());
        self.rows = rows(session);
        self.selected = selected_id
            .and_then(|id| self.rows.iter().position(|row| row.id == id))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }
}

fn rows(session: &ReviewSession) -> Vec<SkimFoldSummary> {
    let files = session
        .files
        .iter()
        .map(|file| file.diff.clone())
        .collect::<Vec<_>>();
    let durable = session
        .active_durable_session()
        .cloned()
        .unwrap_or_default();
    attention::list_skim_folds(&durable, &files, true)
}
