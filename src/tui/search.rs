//! Fuzzy file search overlay: filters the visible review files and jumps to
//! the chosen one.

use crate::app::ReviewSession;
use crate::fuzzy::fuzzy_matches;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileSearchState {
    /// `(file_index, path, viewed)` for every searchable file, in session order.
    pub(super) files: Vec<FileSearchRow>,
    /// Indices into `files` that match the current query.
    pub(super) filtered: Vec<usize>,
    pub(super) selected: usize,
    pub(super) query: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FileSearchRow {
    pub(super) file_index: usize,
    pub(super) path: String,
    pub(super) viewed: bool,
}

impl FileSearchState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        let files: Vec<FileSearchRow> = session
            .visible_file_indices()
            .into_iter()
            .map(|file_index| {
                let file = &session.files[file_index];
                FileSearchRow {
                    file_index,
                    path: file.path.clone(),
                    viewed: file.viewed,
                }
            })
            .collect();
        let filtered = (0..files.len()).collect();
        Self {
            files,
            filtered,
            selected: 0,
            query: String::new(),
        }
    }

    pub(super) fn selected_file_index(&self) -> Option<usize> {
        self.filtered
            .get(self.selected)
            .map(|row| self.files[*row].file_index)
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.filtered.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
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
            .files
            .iter()
            .enumerate()
            .filter_map(|(index, row)| fuzzy_matches(&row.path, &self.query).then_some(index))
            .collect();
        self.selected = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::snapshot_session;

    fn search_session() -> ReviewSession {
        snapshot_session(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/tui/render.rs b/src/tui/render.rs
--- a/src/tui/render.rs
+++ b/src/tui/render.rs
@@ -1 +1 @@
-old
+new
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
"#,
        )
    }

    #[test]
    fn lists_all_visible_files_initially() {
        let session = search_session();
        let search = FileSearchState::new(&session);

        assert_eq!(search.files.len(), 3);
        assert_eq!(search.filtered.len(), 3);
        assert_eq!(search.selected_file_index(), Some(0));
    }

    #[test]
    fn filters_files_by_fuzzy_query() {
        let session = search_session();
        let mut search = FileSearchState::new(&session);

        for ch in "rend".chars() {
            search.push_query_char(ch);
        }

        assert_eq!(search.filtered.len(), 1);
        let selected = search.selected_file_index().unwrap();
        assert_eq!(session.files[selected].path, "src/tui/render.rs");
    }

    #[test]
    fn backspace_restores_matches_and_resets_selection() {
        let session = search_session();
        let mut search = FileSearchState::new(&session);
        for ch in "zzz".chars() {
            search.push_query_char(ch);
        }
        assert!(search.filtered.is_empty());
        assert_eq!(search.selected_file_index(), None);

        for _ in 0.."zzz".len() {
            search.pop_query_char();
        }

        assert_eq!(search.filtered.len(), 3);
        assert_eq!(search.selected, 0);
    }

    #[test]
    fn excludes_hidden_generated_files() {
        let mut session = search_session();
        session.annotate_generated_where(|file| file.path == "README.md");
        session.toggle_generated_visibility();

        let search = FileSearchState::new(&session);

        assert!(search.files.iter().all(|row| row.path != "README.md"));
    }
}
