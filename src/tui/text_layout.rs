//! Terminal-cell-aware visual layout for UTF-8 text.
//!
//! This module owns soft wrapping and byte-cursor mapping independently of any
//! ratatui widget. Rows always end on extended grapheme-cluster boundaries, so
//! callers can render the returned slices directly without asking a widget to
//! wrap them a second time.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisualPosition {
    pub(super) row: usize,
    pub(super) column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VisualRow {
    byte_range: Range<usize>,
    pub(super) display_width: usize,
}

/// A visual-row layout for one immutable text value at one terminal width.
#[derive(Debug, Clone)]
pub(super) struct VisualTextLayout<'a> {
    text: &'a str,
    width: usize,
    rows: Vec<VisualRow>,
}

impl<'a> VisualTextLayout<'a> {
    /// Lay out editable text. An exactly-full final row gets an additional
    /// empty row so the insertion point remains representable on screen.
    pub(super) fn new(text: &'a str, width: usize) -> Self {
        Self::build(text, width, true)
    }

    /// Lay out read-only text such as a diff line. Unlike [`Self::new`], this
    /// never manufactures a trailing cursor row.
    pub(super) fn read_only(text: &'a str, width: usize) -> Self {
        Self::build(text, width, false)
    }

    fn build(text: &'a str, width: usize, cursor_row: bool) -> Self {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut row_start = 0;
        let mut row_width = 0;

        for (index, grapheme) in text.grapheme_indices(true) {
            if grapheme.ends_with('\n') {
                rows.push(VisualRow {
                    byte_range: row_start..index,
                    display_width: row_width,
                });
                row_start = index + grapheme.len();
                row_width = 0;
                continue;
            }

            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if row_width > 0 && row_width + grapheme_width > width {
                rows.push(VisualRow {
                    byte_range: row_start..index,
                    display_width: row_width,
                });
                row_start = index;
                row_width = 0;
            }
            row_width += grapheme_width;
        }

        rows.push(VisualRow {
            byte_range: row_start..text.len(),
            display_width: row_width,
        });

        // A terminal cursor cannot occupy column `width`. Keep the insertion
        // point after an exactly-full final row representable at column zero.
        if cursor_row && row_width >= width && row_start < text.len() {
            rows.push(VisualRow {
                byte_range: text.len()..text.len(),
                display_width: 0,
            });
        }

        Self { text, width, rows }
    }

    pub(super) fn rows(&self) -> &[VisualRow] {
        &self.rows
    }

    pub(super) fn row_text(&self, row: usize) -> &'a str {
        &self.text[self.rows[row].byte_range.clone()]
    }

    pub(super) fn row_byte_range(&self, row: usize) -> Range<usize> {
        self.rows[row].byte_range.clone()
    }

    /// Map a grapheme-boundary UTF-8 byte cursor to its visual insertion point.
    pub(super) fn cursor_position(&self, cursor: usize) -> VisualPosition {
        let cursor = cursor.min(self.text.len());
        debug_assert!(self.text.is_char_boundary(cursor));
        debug_assert!(is_grapheme_boundary(self.text, cursor));

        // At a soft-wrap boundary the same byte is both the prior row's end and
        // the next row's start. Searching from the bottom makes the next row
        // authoritative, matching where insertion occurs on screen.
        for (row_index, row) in self.rows.iter().enumerate().rev() {
            if cursor >= row.byte_range.start && cursor <= row.byte_range.end {
                let column = UnicodeWidthStr::width(&self.text[row.byte_range.start..cursor]);
                if column >= self.width && row_index + 1 < self.rows.len() {
                    return VisualPosition {
                        row: row_index + 1,
                        column: 0,
                    };
                }
                return VisualPosition {
                    row: row_index,
                    column,
                };
            }
        }

        // The only bytes excluded from row ranges are newline bytes. A valid
        // insertion point before a newline belongs to the preceding row.
        let row_index = self
            .rows
            .iter()
            .rposition(|row| row.byte_range.end <= cursor)
            .unwrap_or(0);
        let row = &self.rows[row_index];
        VisualPosition {
            row: row_index,
            column: row.display_width,
        }
    }

    /// Find the nearest valid byte cursor on `row` to a display-cell column.
    ///
    /// A byte at a soft-wrap boundary is owned by the following visual row, so
    /// candidates are filtered through `cursor_position` before selection.
    pub(super) fn byte_at_column(&self, row: usize, target_column: usize) -> usize {
        let row = row.min(self.rows.len().saturating_sub(1));
        let visual_row = &self.rows[row];
        let slice = &self.text[visual_row.byte_range.clone()];
        let mut candidates = Vec::new();
        candidates.push(visual_row.byte_range.start);
        candidates.extend(
            slice
                .grapheme_indices(true)
                .map(|(offset, grapheme)| visual_row.byte_range.start + offset + grapheme.len()),
        );

        candidates
            .into_iter()
            .filter_map(|byte| {
                let position = self.cursor_position(byte);
                (position.row == row).then_some((
                    position.column.abs_diff(target_column),
                    position.column,
                    byte,
                ))
            })
            .min()
            .map(|(_, _, byte)| byte)
            .unwrap_or(visual_row.byte_range.start)
    }
}

pub(super) fn is_grapheme_boundary(text: &str, byte: usize) -> bool {
    byte == text.len()
        || (text.is_char_boundary(byte)
            && text
                .grapheme_indices(true)
                .any(|(boundary, _)| boundary == byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_texts<'a>(layout: &'a VisualTextLayout<'a>) -> Vec<&'a str> {
        (0..layout.rows().len())
            .map(|row| layout.row_text(row))
            .collect()
    }

    #[test]
    fn lays_out_soft_wraps_newlines_empty_lines_and_trailing_lines() {
        let layout = VisualTextLayout::new("abcd\n\nz\n", 3);

        assert_eq!(row_texts(&layout), ["abc", "d", "", "z", ""]);
        assert_eq!(
            layout.cursor_position(3),
            VisualPosition { row: 1, column: 0 }
        );
        assert_eq!(
            layout.cursor_position(4),
            VisualPosition { row: 1, column: 1 }
        );
        assert_eq!(
            layout.cursor_position(5),
            VisualPosition { row: 2, column: 0 }
        );
        assert_eq!(
            layout.cursor_position(8),
            VisualPosition { row: 4, column: 0 }
        );
    }

    #[test]
    fn exact_width_final_line_has_a_cursor_row() {
        let layout = VisualTextLayout::new("abc", 3);

        assert_eq!(row_texts(&layout), ["abc", ""]);
        assert_eq!(
            layout.cursor_position(3),
            VisualPosition { row: 1, column: 0 }
        );
    }

    #[test]
    fn read_only_exact_width_has_no_synthetic_row() {
        let layout = VisualTextLayout::read_only("abc", 3);

        assert_eq!(row_texts(&layout), ["abc"]);
        assert_eq!(layout.row_byte_range(0), 0..3);
    }

    #[test]
    fn uses_display_width_without_splitting_graphemes() {
        let combining = "e\u{301}";
        let family = "👨‍👩‍👧‍👦";
        let text = format!("界{combining}{family}x");
        let layout = VisualTextLayout::new(&text, 3);

        assert_eq!(
            row_texts(&layout),
            [
                format!("界{combining}"),
                format!("{family}x"),
                String::new()
            ]
        );
        assert_eq!(layout.rows()[0].display_width, 3);
        assert_eq!(layout.rows()[1].display_width, 3);
        assert_eq!(
            layout.cursor_position("界".len()),
            VisualPosition { row: 0, column: 2 }
        );
        assert_eq!(
            layout.cursor_position(format!("界{combining}").len()),
            VisualPosition { row: 1, column: 0 }
        );
        assert!(is_grapheme_boundary(&text, "界".len()));
        assert!(!is_grapheme_boundary(&text, "界e".len()));
    }

    #[test]
    fn nearest_column_never_returns_the_middle_of_a_wide_grapheme() {
        let layout = VisualTextLayout::new("界a\n123", 8);

        assert_eq!(layout.byte_at_column(0, 1), 0);
        assert_eq!(layout.byte_at_column(0, 2), "界".len());
        assert_eq!(layout.byte_at_column(1, 2), "界a\n12".len());
        for row in 0..layout.rows().len() {
            for column in 0..=8 {
                assert!(is_grapheme_boundary(
                    layout.text,
                    layout.byte_at_column(row, column)
                ));
            }
        }
    }
}
