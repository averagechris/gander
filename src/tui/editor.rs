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
}
