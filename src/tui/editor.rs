//! Minimal multiline text editor state for the comment popup.

#[derive(Debug, Clone, Default)]
pub(super) struct CommentEditor {
    pub(super) text: String,
    pub(super) cursor: usize,
}

impl CommentEditor {
    pub(super) fn insert_char(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub(super) fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    pub(super) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let Some((previous, ch)) = self.text[..self.cursor].char_indices().last() else {
            return;
        };
        self.text.drain(previous..self.cursor);
        self.cursor -= ch.len_utf8();
    }

    pub(super) fn move_left(&mut self) {
        if let Some((previous, _)) = self.text[..self.cursor].char_indices().last() {
            self.cursor = previous;
        }
    }

    pub(super) fn move_right(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let ch = self.text[self.cursor..].chars().next().unwrap();
        self.cursor += ch.len_utf8();
    }

    pub(super) fn move_to_line_start(&mut self) {
        self.cursor = self.current_line_start();
    }

    pub(super) fn move_to_line_end(&mut self) {
        self.cursor = self.current_line_end();
    }

    pub(super) fn move_word_left(&mut self) {
        self.cursor = self.previous_word_start();
    }

    pub(super) fn move_word_right(&mut self) {
        self.cursor = self.next_word_end();
    }

    pub(super) fn delete_to_line_start(&mut self) {
        let start = self.current_line_start();
        self.text.drain(start..self.cursor);
        self.cursor = start;
    }

    pub(super) fn delete_to_line_end(&mut self) {
        let end = self.current_line_end();
        self.text.drain(self.cursor..end);
    }

    pub(super) fn delete_previous_word(&mut self) {
        let start = self.previous_word_start();
        self.text.drain(start..self.cursor);
        self.cursor = start;
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
        for (index, ch) in self.text[..self.cursor].char_indices().rev() {
            if ch.is_whitespace() {
                if seen_word {
                    return index + ch.len_utf8();
                }
            } else {
                seen_word = true;
            }
        }
        0
    }

    fn next_word_end(&self) -> usize {
        let mut seen_word = false;
        for (offset, ch) in self.text[self.cursor..].char_indices() {
            if ch.is_whitespace() {
                if seen_word {
                    return self.cursor + offset;
                }
            } else {
                seen_word = true;
            }
        }
        self.text.len()
    }

    pub(super) fn line_col(&self) -> (usize, usize) {
        let before = &self.text[..self.cursor];
        let line = before.chars().filter(|ch| *ch == '\n').count();
        let col = before
            .rsplit_once('\n')
            .map(|(_, tail)| tail.chars().count())
            .unwrap_or_else(|| before.chars().count());
        (line, col)
    }

    fn set_line_col(&mut self, target_line: usize, target_col: usize) {
        let mut line = 0;
        let mut col = 0;
        for (index, ch) in self.text.char_indices() {
            if line == target_line && col == target_col {
                self.cursor = index;
                return;
            }
            if ch == '\n' {
                if line == target_line {
                    self.cursor = index;
                    return;
                }
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        self.cursor = self.text.len();
    }

    pub(super) fn move_up(&mut self) {
        let (line, col) = self.line_col();
        if line > 0 {
            self.set_line_col(line - 1, col);
        }
    }

    pub(super) fn move_down(&mut self) {
        let (line, col) = self.line_col();
        if line + 1 < self.text.lines().count().max(1) {
            self.set_line_col(line + 1, col);
        }
    }

    pub(super) fn into_text(self) -> String {
        self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_editor_inserts_multiline_text() {
        let mut editor = CommentEditor::default();
        for ch in "hello".chars() {
            editor.insert_char(ch);
        }
        editor.insert_newline();
        for ch in "world".chars() {
            editor.insert_char(ch);
        }

        assert_eq!(editor.text, "hello\nworld");
        assert_eq!(editor.line_col(), (1, 5));
    }

    #[test]
    fn comment_editor_backspace_merges_lines() {
        let mut editor = CommentEditor {
            text: "hello\nworld".to_owned(),
            cursor: "hello\n".len(),
        };

        editor.backspace();

        assert_eq!(editor.text, "helloworld");
        assert_eq!(editor.line_col(), (0, 5));
    }

    #[test]
    fn comment_editor_deletes_to_current_line_boundaries() {
        let mut editor = CommentEditor {
            text: "alpha βeta\ngamma δelta\nomega".to_owned(),
            cursor: "alpha βeta\ngamma δ".len(),
        };

        editor.delete_to_line_start();

        assert_eq!(editor.text, "alpha βeta\nelta\nomega");
        assert_eq!(editor.line_col(), (1, 0));

        editor.cursor = "alpha βeta\nel".len();
        editor.delete_to_line_end();

        assert_eq!(editor.text, "alpha βeta\nel\nomega");
        assert_eq!(editor.line_col(), (1, 2));
    }

    #[test]
    fn comment_editor_moves_to_current_line_boundaries() {
        let mut editor = CommentEditor {
            text: "one\ntwo three\nfour".to_owned(),
            cursor: "one\ntwo th".len(),
        };

        editor.move_to_line_start();
        assert_eq!(editor.cursor, "one\n".len());

        editor.move_to_line_end();
        assert_eq!(editor.cursor, "one\ntwo three".len());
    }

    #[test]
    fn comment_editor_deletes_previous_word_on_char_boundaries() {
        let mut editor = CommentEditor {
            text: "hello  βeta world".to_owned(),
            cursor: "hello  βeta ".len(),
        };

        editor.delete_previous_word();

        assert_eq!(editor.text, "hello  world");
        assert_eq!(editor.cursor, "hello  ".len());
    }

    #[test]
    fn comment_editor_moves_by_words() {
        let mut editor = CommentEditor {
            text: "alpha  βeta gamma".to_owned(),
            cursor: "alpha  βeta".len(),
        };

        editor.move_word_left();
        assert_eq!(editor.cursor, "alpha  ".len());

        editor.move_word_right();
        assert_eq!(editor.cursor, "alpha  βeta".len());
    }
}
