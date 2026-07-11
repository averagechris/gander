//! Multiline comment editor state backed by the shared visual text layout.

use unicode_segmentation::UnicodeSegmentation;

#[cfg(test)]
use super::text_layout::VisualPosition;
use super::text_layout::{VisualTextLayout, is_grapheme_boundary};

const DEFAULT_VIEWPORT_WIDTH: usize = 80;

#[derive(Debug, Clone)]
pub(super) struct CommentEditor {
    pub(super) text: String,
    pub(super) cursor: usize,
    viewport_width: usize,
    viewport_height: usize,
    scroll: usize,
    preferred_column: Option<usize>,
}

impl Default for CommentEditor {
    fn default() -> Self {
        Self::new(String::new())
    }
}

impl CommentEditor {
    pub(super) fn new(text: String) -> Self {
        let cursor = text.len();
        Self {
            text,
            cursor,
            viewport_width: DEFAULT_VIEWPORT_WIDTH,
            viewport_height: 1,
            scroll: 0,
            preferred_column: None,
        }
    }

    #[cfg(test)]
    fn with_cursor(text: &str, cursor: usize) -> Self {
        assert!(is_grapheme_boundary(text, cursor));
        Self {
            text: text.to_owned(),
            cursor,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(super) fn with_cursor_for_test(text: &str, cursor: usize) -> Self {
        Self::with_cursor(text, cursor)
    }

    /// Reflow at the current popup geometry without changing text or its byte
    /// cursor. This is called before every draw, including after a resize.
    pub(super) fn resize(&mut self, width: usize, height: usize) {
        let width = width.max(1);
        let height = height.max(1);
        if self.viewport_width != width {
            self.preferred_column = None;
        }
        self.viewport_width = width;
        self.viewport_height = height;
        self.ensure_cursor_visible();
    }

    pub(super) fn layout(&self, width: usize) -> VisualTextLayout<'_> {
        VisualTextLayout::new(&self.text, width)
    }

    pub(super) fn visible_scroll(&self, width: usize, height: usize) -> usize {
        Self::scroll_for_cursor(&self.layout(width), self.cursor, self.scroll, height.max(1))
    }

    #[cfg(test)]
    fn cursor_position(&self, width: usize) -> VisualPosition {
        self.layout(width).cursor_position(self.cursor)
    }

    pub(super) fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.cursor = next_grapheme_boundary(&self.text, self.cursor);
        self.after_non_vertical_action();
    }

    pub(super) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(super) fn backspace(&mut self) {
        let previous = previous_grapheme_boundary(&self.text, self.cursor);
        if previous == self.cursor {
            return;
        }
        self.text.drain(previous..self.cursor);
        self.cursor = previous;
        self.after_non_vertical_action();
    }

    pub(super) fn delete_forward(&mut self) {
        let next = next_grapheme_boundary(&self.text, self.cursor.saturating_add(1));
        if next == self.cursor {
            return;
        }
        self.text.drain(self.cursor..next);
        self.after_non_vertical_action();
    }

    pub(super) fn move_left(&mut self) {
        self.cursor = previous_grapheme_boundary(&self.text, self.cursor);
        self.after_non_vertical_action();
    }

    pub(super) fn move_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.text, self.cursor.saturating_add(1));
        self.after_non_vertical_action();
    }

    pub(super) fn move_to_line_start(&mut self) {
        self.cursor = self.current_line_start();
        self.after_non_vertical_action();
    }

    pub(super) fn move_to_line_end(&mut self) {
        self.cursor = self.current_line_end();
        self.after_non_vertical_action();
    }

    pub(super) fn move_word_left(&mut self) {
        self.cursor = self.previous_word_start();
        self.after_non_vertical_action();
    }

    pub(super) fn move_word_right(&mut self) {
        self.cursor = self.next_word_end();
        self.after_non_vertical_action();
    }

    pub(super) fn delete_to_line_start(&mut self) {
        let start = self.current_line_start();
        self.text.drain(start..self.cursor);
        self.cursor = start;
        self.after_non_vertical_action();
    }

    pub(super) fn delete_to_line_end(&mut self) {
        let end = self.current_line_end();
        self.text.drain(self.cursor..end);
        self.after_non_vertical_action();
    }

    pub(super) fn delete_previous_word(&mut self) {
        let start = self.previous_word_start();
        self.text.drain(start..self.cursor);
        self.cursor = start;
        self.after_non_vertical_action();
    }

    fn current_line_start(&self) -> usize {
        self.text[..self.cursor]
            .rfind('\n')
            .map(|index| index + '\n'.len_utf8())
            .unwrap_or(0)
    }

    fn current_line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map(|offset| self.cursor + offset)
            .unwrap_or(self.text.len())
    }

    fn previous_word_start(&self) -> usize {
        let mut seen_word = false;
        for (index, grapheme) in self.text[..self.cursor].grapheme_indices(true).rev() {
            if grapheme.chars().all(char::is_whitespace) {
                if seen_word {
                    return index + grapheme.len();
                }
            } else {
                seen_word = true;
            }
        }
        0
    }

    fn next_word_end(&self) -> usize {
        let mut seen_word = false;
        for (offset, grapheme) in self.text[self.cursor..].grapheme_indices(true) {
            if grapheme.chars().all(char::is_whitespace) {
                if seen_word {
                    return self.cursor + offset;
                }
            } else {
                seen_word = true;
            }
        }
        self.text.len()
    }

    pub(super) fn move_up(&mut self) {
        self.move_vertical(-1);
    }

    pub(super) fn move_down(&mut self) {
        self.move_vertical(1);
    }

    fn move_vertical(&mut self, delta: isize) {
        let layout = VisualTextLayout::new(&self.text, self.viewport_width);
        let position = layout.cursor_position(self.cursor);
        let preferred = *self.preferred_column.get_or_insert(position.column);
        let target_row = position.row.saturating_add_signed(delta);
        if target_row < layout.rows().len() && target_row != position.row {
            self.cursor = layout.byte_at_column(target_row, preferred);
        }
        self.ensure_cursor_visible();
    }

    fn after_non_vertical_action(&mut self) {
        if !is_grapheme_boundary(&self.text, self.cursor) {
            // Removing or inserting text can cause the graphemes on either
            // side of the edit to combine (regional indicators are a common
            // example). There is no valid insertion point inside the newly
            // combined cluster, so advance to its next boundary.
            self.cursor = next_grapheme_boundary(&self.text, self.cursor);
        }
        debug_assert!(is_grapheme_boundary(&self.text, self.cursor));
        self.preferred_column = None;
        self.ensure_cursor_visible();
    }

    fn ensure_cursor_visible(&mut self) {
        let layout = VisualTextLayout::new(&self.text, self.viewport_width);
        self.scroll =
            Self::scroll_for_cursor(&layout, self.cursor, self.scroll, self.viewport_height);
    }

    fn scroll_for_cursor(
        layout: &VisualTextLayout<'_>,
        cursor: usize,
        mut scroll: usize,
        height: usize,
    ) -> usize {
        let height = height.max(1);
        let cursor_row = layout.cursor_position(cursor).row;
        if cursor_row < scroll {
            scroll = cursor_row;
        } else if cursor_row >= scroll.saturating_add(height) {
            scroll = cursor_row + 1 - height;
        }
        scroll.min(layout.rows().len().saturating_sub(height))
    }

    pub(super) fn into_text(self) -> String {
        self.text
    }
}

fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(cursor)
}

fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .find(|index| *index >= cursor)
        .unwrap_or(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inserts_at_edges_and_across_explicit_newlines() {
        let mut editor = CommentEditor::default();
        for ch in "hello".chars() {
            editor.insert_char(ch);
        }
        editor.insert_newline();
        for ch in "world".chars() {
            editor.insert_char(ch);
        }
        editor.cursor = 0;
        editor.insert_char('>');
        editor.cursor = editor.text.len();
        editor.insert_char('<');

        assert_eq!(editor.text, ">hello\nworld<");
        assert_eq!(editor.cursor, editor.text.len());
    }

    #[test]
    fn backspace_deletes_graphemes_and_merges_lines() {
        let mut editor = CommentEditor::with_cursor("hello\ne\u{301}界", "hello\ne\u{301}界".len());

        editor.backspace();
        assert_eq!(editor.text, "hello\ne\u{301}");
        editor.backspace();
        assert_eq!(editor.text, "hello\n");
        editor.backspace();

        assert_eq!(editor.text, "hello");
        assert_eq!(editor.cursor, editor.text.len());
    }

    #[test]
    fn delete_forward_removes_one_grapheme_or_newline() {
        let family = "👨‍👩‍👧‍👦";
        let mut editor = CommentEditor::with_cursor(&format!("e\u{301}{family}\n界"), 0);

        editor.delete_forward();
        assert_eq!(editor.text, format!("{family}\n界"));
        assert_eq!(editor.cursor, 0);
        editor.delete_forward();
        assert_eq!(editor.text, "\n界");
        editor.delete_forward();
        assert_eq!(editor.text, "界");
        editor.delete_forward();
        assert!(editor.text.is_empty());
        editor.delete_forward();
        assert_eq!(editor.cursor, 0);
    }

    #[test]
    fn edits_that_resegment_neighboring_text_leave_cursor_on_a_grapheme_boundary() {
        let mut editor = CommentEditor::with_cursor("🇺X🇸", "🇺X".len());

        editor.backspace();

        assert_eq!(editor.text, "🇺🇸");
        assert_eq!(editor.cursor, editor.text.len());
        assert!(is_grapheme_boundary(&editor.text, editor.cursor));

        let mut combining = CommentEditor::new("e".to_owned());
        combining.insert_char('\u{301}');
        assert_eq!(combining.text, "e\u{301}");
        assert_eq!(combining.cursor, combining.text.len());
        combining.backspace();
        assert!(combining.text.is_empty());
    }

    #[test]
    fn line_and_word_edits_preserve_unicode_boundaries() {
        let mut editor = CommentEditor::with_cursor(
            "alpha e\u{301}ta\ngamma δelta\nomega",
            "alpha e\u{301}ta\ngamma δ".len(),
        );

        editor.delete_to_line_start();
        assert_eq!(editor.text, "alpha e\u{301}ta\nelta\nomega");
        editor.cursor = "alpha e\u{301}ta\nel".len();
        editor.delete_to_line_end();
        assert_eq!(editor.text, "alpha e\u{301}ta\nel\nomega");

        editor.cursor = "alpha e\u{301}ta".len();
        editor.move_word_left();
        assert_eq!(editor.cursor, "alpha ".len());
        editor.move_word_right();
        assert_eq!(editor.cursor, "alpha e\u{301}ta".len());
    }

    #[test]
    fn deleting_previous_word_preserves_complete_graphemes() {
        let mut editor = CommentEditor::new("alpha 👩🏽‍💻 e\u{301}ta".to_owned());

        editor.delete_previous_word();
        assert_eq!(editor.text, "alpha 👩🏽‍💻 ");
        editor.delete_previous_word();
        assert_eq!(editor.text, "alpha ");
        assert!(is_grapheme_boundary(&editor.text, editor.cursor));
    }

    #[test]
    fn left_and_right_move_by_extended_grapheme_cluster() {
        let family = "👨‍👩‍👧‍👦";
        let text = format!("a{family}e\u{301}界");
        let mut editor = CommentEditor::new(text.clone());

        editor.move_left();
        assert_eq!(editor.cursor, text.len() - "界".len());
        editor.move_left();
        assert_eq!(editor.cursor, "a".len() + family.len());
        editor.move_left();
        assert_eq!(editor.cursor, "a".len());
        editor.move_right();
        assert_eq!(editor.cursor, "a".len() + family.len());
    }

    #[test]
    fn vertical_movement_follows_wrapped_rows_with_preferred_display_column() {
        let mut editor = CommentEditor::with_cursor("ab界d\n123456\nxy", "ab界".len());
        editor.resize(5, 4);

        assert_eq!(
            editor.cursor_position(5),
            VisualPosition { row: 0, column: 4 }
        );
        editor.move_down();
        assert_eq!(editor.cursor, "ab界d\n1234".len());
        editor.move_down();
        assert_eq!(editor.cursor, "ab界d\n123456".len());
        editor.move_down();
        assert_eq!(editor.cursor, "ab界d\n123456\nxy".len());
        editor.move_up();
        assert_eq!(editor.cursor, "ab界d\n123456".len());
    }

    #[test]
    fn cursor_after_soft_wrap_and_newline_is_exact() {
        let mut editor = CommentEditor::with_cursor("abcDEF\n界x", 3);
        editor.resize(3, 5);
        assert_eq!(
            editor.cursor_position(3),
            VisualPosition { row: 1, column: 0 }
        );

        editor.cursor = "abcDEF\n".len();
        assert_eq!(
            editor.cursor_position(3),
            VisualPosition { row: 2, column: 0 }
        );
        editor.move_right();
        assert_eq!(
            editor.cursor_position(3),
            VisualPosition { row: 2, column: 2 }
        );
    }

    #[test]
    fn scrolling_keeps_cursor_visible_in_both_directions() {
        let mut editor = CommentEditor::new("0\n1\n2\n3\n4\n5".to_owned());
        editor.resize(10, 3);
        assert_eq!(editor.visible_scroll(10, 3), 3);

        editor.move_up();
        editor.move_up();
        editor.move_up();
        assert_eq!(editor.visible_scroll(10, 3), 2);
        editor.move_up();
        editor.move_up();
        assert_eq!(editor.visible_scroll(10, 3), 0);
    }

    #[test]
    fn resize_reflows_without_changing_text_or_byte_cursor() {
        let text = "prefix 界 e\u{301} 👩🏽‍💻 suffix\ntrailing\n";
        let cursor = "prefix 界 e\u{301} 👩🏽‍💻".len();
        let mut editor = CommentEditor::with_cursor(text, cursor);

        editor.resize(20, 4);
        let wide_position = editor.cursor_position(20);
        editor.resize(7, 2);
        let narrow_position = editor.cursor_position(7);

        assert_eq!(editor.text, text);
        assert_eq!(editor.cursor, cursor);
        assert_ne!(wide_position, narrow_position);
        assert!(narrow_position.row >= wide_position.row);
        let scroll = editor.visible_scroll(7, 2);
        assert!(narrow_position.row >= scroll && narrow_position.row < scroll + 2);
    }
}
