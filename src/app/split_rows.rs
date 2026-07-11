//! Side-by-side projection over the unified diff rows
//! (docs/focused-diff-ux.md §4). The unified row list stays the single
//! source of truth: the cursor, comments, anchors, and range selection all
//! keep indexing unified rows, and this module only decides which unified
//! row lands in which cell of the split layout.

use crate::diff::DiffLineKind;

use super::{DiffRow, DiffRowKind};

/// One display row of the side-by-side view. Cell values are indices into
/// the unified row list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitRow {
    /// Rendered across the full pane width (headers, folds, placeholders,
    /// meta lines).
    Full(usize),
    /// Removed/context cell on the left, added/context cell on the right.
    Pair {
        left: Option<usize>,
        right: Option<usize>,
    },
}

impl SplitRow {
    /// Whether this display row contains the given unified row index.
    #[cfg(test)]
    pub fn contains(&self, row_index: usize) -> bool {
        match self {
            Self::Full(index) => *index == row_index,
            Self::Pair { left, right } => *left == Some(row_index) || *right == Some(row_index),
        }
    }
}

/// Project unified rows into side-by-side display rows.
///
/// Context lines occupy both cells; within a change block, removed line *i*
/// pairs with added line *i* (the same alignment the word-level diff uses,
/// so intra-line emphasis lines up across the divider) and leftovers get a
/// blank opposite cell.
pub fn split_rows(rows: &[DiffRow]) -> Vec<SplitRow> {
    let mut result = Vec::with_capacity(rows.len());
    let mut index = 0;
    while index < rows.len() {
        match rows[index].kind {
            DiffRowKind::DiffLine(DiffLineKind::Context) => {
                result.push(SplitRow::Pair {
                    left: Some(index),
                    right: Some(index),
                });
                index += 1;
            }
            DiffRowKind::DiffLine(DiffLineKind::Removed) => {
                let removed_start = index;
                while index < rows.len()
                    && rows[index].kind == DiffRowKind::DiffLine(DiffLineKind::Removed)
                {
                    index += 1;
                }
                let added_start = index;
                while index < rows.len()
                    && rows[index].kind == DiffRowKind::DiffLine(DiffLineKind::Added)
                {
                    index += 1;
                }
                let removed = added_start - removed_start;
                let added = index - added_start;
                for offset in 0..removed.max(added) {
                    result.push(SplitRow::Pair {
                        left: (offset < removed).then(|| removed_start + offset),
                        right: (offset < added).then(|| added_start + offset),
                    });
                }
            }
            DiffRowKind::DiffLine(DiffLineKind::Added) => {
                result.push(SplitRow::Pair {
                    left: None,
                    right: Some(index),
                });
                index += 1;
            }
            _ => {
                result.push(SplitRow::Full(index));
                index += 1;
            }
        }
    }
    result
}

/// Index of the split row containing a unified row, for cursor/scroll
/// mapping. Falls back to the last split row.
#[cfg(test)]
pub fn split_index_of(split: &[SplitRow], row_index: usize) -> usize {
    split
        .iter()
        .position(|row| row.contains(row_index))
        .unwrap_or_else(|| split.len().saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::{app::ReviewSession, diff::DiffSet, jj::ReviewTarget, state::ReviewState};

    fn rows_for(diff_text: &str) -> Vec<DiffRow> {
        let diff = DiffSet::parse(diff_text).unwrap();
        let mut session = ReviewSession::new(
            PathBuf::from("."),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.syntax = crate::syntax::SyntaxConfig {
            enabled: false,
            ..crate::syntax::SyntaxConfig::default()
        };
        session.diff_rows_for_selected_file().as_ref().clone()
    }

    #[test]
    fn pairs_change_blocks_and_spreads_context() {
        let rows = rows_for(
            r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,4 +1,5 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        );

        let split = split_rows(&rows);

        // file header + hunk header render full width.
        assert_eq!(split[0], SplitRow::Full(0));
        assert_eq!(split[1], SplitRow::Full(1));
        // context occupies both cells.
        assert_eq!(
            split[2],
            SplitRow::Pair {
                left: Some(2),
                right: Some(2)
            }
        );
        // removed pairs with the first added; the second added is unpaired.
        assert_eq!(
            split[3],
            SplitRow::Pair {
                left: Some(3),
                right: Some(4)
            }
        );
        assert_eq!(
            split[4],
            SplitRow::Pair {
                left: None,
                right: Some(5)
            }
        );
        assert_eq!(
            split[5],
            SplitRow::Pair {
                left: Some(6),
                right: Some(6)
            }
        );
        assert_eq!(split.len(), 6);
    }

    #[test]
    fn unpaired_removals_get_blank_right_cells() {
        let rows = rows_for(
            r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,3 +1,1 @@
-alpha();
-beta();
 gamma();
"#,
        );

        let split = split_rows(&rows);

        assert_eq!(
            split[2],
            SplitRow::Pair {
                left: Some(2),
                right: None
            }
        );
        assert_eq!(
            split[3],
            SplitRow::Pair {
                left: Some(3),
                right: None
            }
        );
    }

    #[test]
    fn split_index_maps_unified_rows_to_display_rows() {
        let rows = rows_for(
            r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
-old();
+new();
 done();
"#,
        );
        let split = split_rows(&rows);

        // Removed (2) and added (3) share one display row.
        assert_eq!(split_index_of(&split, 2), split_index_of(&split, 3));
        // The trailing context has its own display row.
        assert!(split_index_of(&split, 4) > split_index_of(&split, 3));
    }
}
