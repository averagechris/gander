use std::{collections::BTreeMap, path::PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    diff::{DiffSet, FileDiff, FileStatus},
    file_tree::{FileTreeInput, FileTreeView},
    state::{Comment, FileState, ReviewState},
};

#[derive(Debug, Clone)]
pub struct ReviewSession {
    pub repo: PathBuf,
    pub revision: String,
    pub files: Vec<ReviewFile>,
    pub comments: Vec<Comment>,
    pub selected: usize,
    pub diff_scroll: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
    pub viewed: bool,
    pub fingerprint: String,
    pub diff: FileDiff,
}

impl ReviewSession {
    pub fn new(repo: PathBuf, revision: String, diff: DiffSet, state: ReviewState) -> Self {
        let ReviewState { files, comments } = state;
        let mut session = Self {
            repo,
            revision,
            files: diff
                .files
                .into_iter()
                .map(|file| ReviewFile {
                    path: file.path.clone(),
                    old_path: file.old_path.clone(),
                    status: file.status,
                    additions: file.additions,
                    deletions: file.deletions,
                    viewed: false,
                    fingerprint: file.fingerprint.clone(),
                    diff: file,
                })
                .collect(),
            comments,
            selected: 0,
            diff_scroll: 0,
        };
        session.apply_state_files(&files);
        session
    }

    pub fn apply_viewed_state(&mut self) {
        // Kept as an intentionally cheap hook for callers. State hydration happens in `new`.
    }

    fn apply_state_files(&mut self, files: &BTreeMap<String, FileState>) {
        for file in &mut self.files {
            if let Some(saved) = files.get(&file.path) {
                file.viewed = saved.viewed && saved.fingerprint == file.fingerprint;
            }
        }
    }

    pub fn selected_file(&self) -> Option<&ReviewFile> {
        self.files.get(self.selected)
    }

    pub fn selected_file_mut(&mut self) -> Option<&mut ReviewFile> {
        self.files.get_mut(self.selected)
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        if let Some(next) = self.file_tree().next_file_index(self.selected, delta) {
            self.selected = next;
            self.diff_scroll = 0;
        }
    }

    pub fn file_tree(&self) -> FileTreeView {
        let inputs: Vec<_> = self
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| FileTreeInput {
                index,
                path: &file.path,
                viewed: file.viewed,
            })
            .collect();
        FileTreeView::build(&inputs)
    }

    pub fn selected_tree_row(&self, tree: &FileTreeView) -> Option<usize> {
        tree.selected_row_for_file(self.selected)
    }

    pub fn toggle_viewed(&mut self) {
        if let Some(file) = self.selected_file_mut() {
            file.viewed = !file.viewed;
        }
    }

    pub fn mark_selected_viewed(&mut self) {
        if let Some(file) = self.selected_file_mut() {
            file.viewed = true;
        }
    }

    pub fn mark_all_viewed(&mut self) {
        for file in &mut self.files {
            file.viewed = true;
        }
    }

    pub fn scroll_diff(&mut self, delta: i16) {
        self.diff_scroll = if delta.is_negative() {
            self.diff_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.diff_scroll.saturating_add(delta as u16)
        };
    }

    pub fn add_comment(&mut self, body: String) {
        let Some(path) = self.selected_file().map(|file| file.path.clone()) else {
            return;
        };
        if body.trim().is_empty() {
            return;
        }
        let created_at = Utc::now();
        self.comments.push(Comment {
            id: format!(
                "{}-{}",
                created_at.timestamp_millis(),
                self.comments.len() + 1
            ),
            path,
            line: None,
            body,
            created_at,
        });
    }

    pub fn into_state(self) -> ReviewState {
        ReviewState {
            files: self
                .files
                .into_iter()
                .map(|file| {
                    (
                        file.path,
                        FileState {
                            fingerprint: file.fingerprint,
                            viewed: file.viewed,
                        },
                    )
                })
                .collect(),
            comments: self.comments,
        }
    }

    pub fn summary_line(&self) -> String {
        let viewed = self.files.iter().filter(|file| file.viewed).count();
        let additions: usize = self.files.iter().map(|file| file.additions).sum();
        let deletions: usize = self.files.iter().map(|file| file.deletions).sum();
        format!(
            "{} files ({viewed}/{} viewed), +{additions}/-{deletions}, {} comments",
            self.files.len(),
            self.files.len(),
            self.comments.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{diff::DiffSet, state::ReviewState};

    fn session() -> ReviewSession {
        let diff = DiffSet::parse(
            r#"diff --git a/src/tui.rs b/src/tui.rs
--- a/src/tui.rs
+++ b/src/tui.rs
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
        .unwrap();
        ReviewSession::new(".".into(), "@".into(), diff, ReviewState::default())
    }

    #[test]
    fn move_selection_uses_tree_file_order() {
        let mut session = session();

        session.move_selection(1);

        assert_eq!(session.selected_file().unwrap().path, "README.md");
    }

    #[test]
    fn viewed_toggle_updates_tree_counts() {
        let mut session = session();
        session.toggle_viewed();

        let tree = session.file_tree();

        assert_eq!(tree.rows[0].label, "src");
        assert_eq!(tree.rows[0].stats.viewed, 1);
    }
}
