//! Base/tip target picker: jj change list, fuzzy filtering, and selection state.

use crate::jj::{JjChangeSummary, ReviewTarget};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TargetChooserState {
    pub(super) rows: Vec<JjChangeSummary>,
    pub(super) filtered: Vec<usize>,
    pub(super) selected: usize,
    pub(super) query: String,
    pub(super) current_base: String,
    pub(super) current_tip: String,
    pub(super) selecting: TargetPickerSide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TargetPickerSide {
    Base,
    Tip,
}

impl TargetChooserState {
    pub(super) fn new(rows: Vec<JjChangeSummary>, current_base: &str, current_tip: &str) -> Self {
        let selected = rows
            .iter()
            .position(|row| row.matches_rev(current_base))
            .unwrap_or(0);
        let filtered = (0..rows.len()).collect();
        Self {
            rows,
            filtered,
            selected,
            query: String::new(),
            current_base: current_base.to_owned(),
            current_tip: current_tip.to_owned(),
            selecting: TargetPickerSide::Base,
        }
    }

    pub(super) fn target(&self) -> Option<ReviewTarget> {
        let selected = self.rows.get(*self.filtered.get(self.selected)?)?;
        Some(match self.selecting {
            TargetPickerSide::Base => {
                ReviewTarget::new(selected.change_id.clone(), self.current_tip.clone())
            }
            TargetPickerSide::Tip => {
                ReviewTarget::new(self.current_base.clone(), selected.change_id.clone())
            }
        })
    }

    pub(super) fn toggle_side(&mut self) {
        self.selecting = match self.selecting {
            TargetPickerSide::Base => TargetPickerSide::Tip,
            TargetPickerSide::Tip => TargetPickerSide::Base,
        };
        let current = match self.selecting {
            TargetPickerSide::Base => &self.current_base,
            TargetPickerSide::Tip => &self.current_tip,
        };
        if let Some(selected) = self.selected_index_for_rev(current) {
            self.selected = selected;
        }
    }

    fn selected_index_for_rev(&self, rev: &str) -> Option<usize> {
        if rev == "@" && !self.filtered.is_empty() {
            return Some(0);
        }
        self.filtered
            .iter()
            .position(|row_index| self.rows[*row_index].matches_rev(rev))
    }

    pub(super) fn tip_matches_row(&self, row: &JjChangeSummary, row_index: usize) -> bool {
        if self.current_tip == "@" {
            row_index == 0
        } else {
            row.matches_rev(&self.current_tip)
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.filtered.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn select_first(&mut self) {
        self.selected = 0;
    }

    pub(super) fn select_last(&mut self) {
        self.selected = self.filtered.len().saturating_sub(1);
    }

    pub(super) fn push_query_char(&mut self, ch: char) {
        self.query.push(ch);
        self.apply_filter();
    }

    pub(super) fn pop_query_char(&mut self) {
        self.query.pop();
        self.apply_filter();
    }

    fn apply_filter(&mut self) {
        self.filtered = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| fuzzy_matches(row, &self.query).then_some(index))
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
    }
}

impl TargetPickerSide {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Tip => "tip",
        }
    }
}

impl JjChangeSummary {
    pub(crate) fn matches_rev(&self, rev: &str) -> bool {
        self.change_id == rev
            || self
                .bookmarks
                .split_whitespace()
                .any(|bookmark| bookmark.trim_end_matches('*') == rev)
    }
}

fn fuzzy_matches(row: &JjChangeSummary, query: &str) -> bool {
    let query = query.trim();
    if query.is_empty() {
        return true;
    }
    let haystack = format!("{} {} {}", row.change_id, row.bookmarks, row.description);
    fuzzy_contains(&haystack.to_ascii_lowercase(), &query.to_ascii_lowercase())
}

fn fuzzy_contains(haystack: &str, needle: &str) -> bool {
    let mut haystack_chars = haystack.chars();
    needle.chars().all(|needle_char| {
        haystack_chars
            .by_ref()
            .any(|haystack_char| haystack_char == needle_char)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_chooser_selects_current_base_when_visible() {
        let chooser = TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "abc".to_owned(),
                    bookmarks: String::new(),
                    description: String::new(),
                },
                JjChangeSummary {
                    change_id: "def".to_owned(),
                    bookmarks: "trunk".to_owned(),
                    description: String::new(),
                },
            ],
            "trunk",
            "@",
        );

        assert_eq!(chooser.selected, 1);
        assert_eq!(chooser.target(), Some(ReviewTarget::new("def", "@")));
    }

    #[test]
    fn target_chooser_can_switch_to_tip_selection() {
        let mut chooser = TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "base".to_owned(),
                    bookmarks: String::new(),
                    description: String::new(),
                },
                JjChangeSummary {
                    change_id: "tip".to_owned(),
                    bookmarks: "feature".to_owned(),
                    description: String::new(),
                },
            ],
            "base",
            "feature",
        );

        chooser.toggle_side();

        assert_eq!(chooser.selecting, TargetPickerSide::Tip);
        assert_eq!(chooser.selected, 1);
        assert_eq!(chooser.target(), Some(ReviewTarget::new("base", "tip")));
    }

    #[test]
    fn target_chooser_fuzzy_filters_rows() {
        let mut chooser = TargetChooserState::new(
            vec![
                JjChangeSummary {
                    change_id: "abc".to_owned(),
                    bookmarks: "main".to_owned(),
                    description: "feature work".to_owned(),
                },
                JjChangeSummary {
                    change_id: "def".to_owned(),
                    bookmarks: "topic".to_owned(),
                    description: "bug fix".to_owned(),
                },
            ],
            "abc",
            "@",
        );

        for ch in "tp".chars() {
            chooser.push_query_char(ch);
        }

        assert_eq!(chooser.filtered, vec![1]);
        assert_eq!(chooser.target(), Some(ReviewTarget::new("def", "@")));
    }
}
