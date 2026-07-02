//! Changed-symbol outline overlay: lists changed functions/classes/modules in
//! the selected file and jumps to the chosen one.

use crate::app::{ChangedSymbolTarget, ReviewSession};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SymbolOutlineState {
    pub(super) targets: Vec<ChangedSymbolTarget>,
    pub(super) selected: usize,
}

impl SymbolOutlineState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        let targets = session.changed_symbol_targets();
        // Start on the symbol at or before the diff cursor so the outline
        // opens "where you are".
        let selected = targets
            .iter()
            .rposition(|target| target.row_index <= session.diff_cursor)
            .unwrap_or(0);
        Self { targets, selected }
    }

    pub(super) fn selected_row_index(&self) -> Option<usize> {
        self.targets
            .get(self.selected)
            .map(|target| target.row_index)
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.targets.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.targets.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::ReviewSession, diff::DiffSet, jj::ReviewTarget, state::ReviewState};

    fn rust_session() -> ReviewSession {
        let diff = DiffSet::parse(
            r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,8 +1,8 @@
 fn alpha() {
-    old_alpha();
+    new_alpha();
 }
 
 fn beta() {
-    old_beta();
+    new_beta();
 }
"#,
        )
        .unwrap();
        ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        )
    }

    #[test]
    fn outline_lists_changed_symbols_in_order() {
        let session = rust_session();

        let outline = SymbolOutlineState::new(&session);

        let labels: Vec<_> = outline
            .targets
            .iter()
            .map(|target| target.label.as_str())
            .collect();
        assert_eq!(labels, ["fn alpha", "fn beta"]);
    }

    #[test]
    fn outline_selection_moves_and_resolves_rows() {
        let session = rust_session();
        let mut outline = SymbolOutlineState::new(&session);

        outline.move_selection(1);

        let row_index = outline.selected_row_index().unwrap();
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[row_index].text.trim(), "new_beta();");
    }
}
