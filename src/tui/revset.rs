//! Free-form revset input overlay: type arbitrary base/tip revsets and load
//! them as the review target.

use crate::jj::ReviewTarget;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RevsetInputState {
    pub(super) base: String,
    pub(super) tip: String,
    pub(super) editing: RevsetField,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RevsetField {
    Base,
    Tip,
}

impl RevsetInputState {
    pub(super) fn new(base: &str, tip: &str) -> Self {
        Self {
            base: base.to_owned(),
            tip: tip.to_owned(),
            editing: RevsetField::Base,
        }
    }

    pub(super) fn toggle_field(&mut self) {
        self.editing = match self.editing {
            RevsetField::Base => RevsetField::Tip,
            RevsetField::Tip => RevsetField::Base,
        };
    }

    fn editing_field_mut(&mut self) -> &mut String {
        match self.editing {
            RevsetField::Base => &mut self.base,
            RevsetField::Tip => &mut self.tip,
        }
    }

    pub(super) fn push_char(&mut self, ch: char) {
        self.editing_field_mut().push(ch);
    }

    pub(super) fn pop_char(&mut self) {
        self.editing_field_mut().pop();
    }

    /// The target described by the current input, if both revsets are
    /// non-empty after trimming.
    pub(super) fn target(&self) -> Option<ReviewTarget> {
        let base = self.base.trim();
        let tip = self.tip.trim();
        if base.is_empty() || tip.is_empty() {
            return None;
        }
        Some(ReviewTarget::new(base, tip))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_base_then_tip_after_toggle() {
        let mut input = RevsetInputState::new("trunk()", "@");

        for _ in 0.."trunk()".len() {
            input.pop_char();
        }
        for ch in "main..".chars() {
            input.push_char(ch);
        }
        input.toggle_field();
        input.push_char('-');

        assert_eq!(input.base, "main..");
        assert_eq!(input.tip, "@-");
    }

    #[test]
    fn target_requires_both_fields_non_empty() {
        let mut input = RevsetInputState::new("trunk()", "@");
        assert_eq!(input.target(), Some(ReviewTarget::new("trunk()", "@")));

        for _ in 0.."trunk()".len() {
            input.pop_char();
        }
        assert_eq!(input.target(), None);

        input.push_char(' ');
        assert_eq!(input.target(), None);
    }

    #[test]
    fn accepts_arbitrary_revset_syntax() {
        let mut input = RevsetInputState::new("", "");
        for ch in "ancestors(@, 3) & ~empty()".chars() {
            input.push_char(ch);
        }
        input.toggle_field();
        for ch in "bookmarks() | @".chars() {
            input.push_char(ch);
        }

        assert_eq!(
            input.target(),
            Some(ReviewTarget::new(
                "ancestors(@, 3) & ~empty()",
                "bookmarks() | @"
            ))
        );
    }
}
