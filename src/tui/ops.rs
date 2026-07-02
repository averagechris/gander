//! Prior-operation picker for incremental re-review: choose a jj operation
//! to compare the current diff against.

use crate::jj::JjOperationSummary;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OperationPickerState {
    pub(super) operations: Vec<JjOperationSummary>,
    pub(super) selected: usize,
}

impl OperationPickerState {
    pub(super) fn new(operations: Vec<JjOperationSummary>) -> Self {
        Self {
            operations,
            selected: 0,
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.operations.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.operations.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_operation(&self) -> Option<&JjOperationSummary> {
        self.operations.get(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation(id: &str) -> JjOperationSummary {
        JjOperationSummary {
            operation_id: id.to_owned(),
            time: "1 hour ago".to_owned(),
            description: "snapshot".to_owned(),
        }
    }

    #[test]
    fn selection_moves_and_clamps() {
        let mut picker = OperationPickerState::new(vec![operation("a"), operation("b")]);

        picker.move_selection(1);
        assert_eq!(picker.selected_operation().unwrap().operation_id, "b");

        picker.move_selection(5);
        assert_eq!(picker.selected_operation().unwrap().operation_id, "b");

        picker.move_selection(-5);
        assert_eq!(picker.selected_operation().unwrap().operation_id, "a");
    }

    #[test]
    fn empty_picker_is_safe() {
        let mut picker = OperationPickerState::new(Vec::new());
        picker.move_selection(1);
        assert_eq!(picker.selected_operation(), None);
    }
}
