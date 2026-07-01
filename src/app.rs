use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    anchor::{CommentAnchor, DiffSide, fingerprint_line},
    diff::{DiffLineKind, DiffSet, FileDiff, FileStatus},
    file_tree::{FileTreeInput, FileTreeView, FlatTreeRowKind, TreeRowId},
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
    pub diff_cursor: usize,
    pub focus: Focus,
    pub collapsed_dirs: BTreeSet<String>,
    viewport_by_path: BTreeMap<String, FileViewport>,
    tree_cursor: Option<TreeRowId>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FileViewport {
    diff_scroll: u16,
    diff_cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files,
    Diff,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: usize,
    pub deletions: usize,
    #[serde(default)]
    pub generated: bool,
    pub viewed: bool,
    pub fingerprint: String,
    pub diff: FileDiff,
}

#[derive(Debug, Clone)]
pub struct DiffRow {
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub prefix: &'static str,
    pub text: String,
    pub kind: DiffRowKind,
    pub anchor: Option<CommentAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    FileHeader,
    SyntaxSummary,
    HunkHeader,
    DiffLine(DiffLineKind),
    Raw,
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
                    generated: false,
                    viewed: false,
                    fingerprint: file.fingerprint.clone(),
                    diff: file,
                })
                .collect(),
            comments,
            selected: 0,
            diff_scroll: 0,
            diff_cursor: 0,
            focus: Focus::Files,
            collapsed_dirs: BTreeSet::new(),
            viewport_by_path: BTreeMap::new(),
            tree_cursor: None,
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
        let tree = self.file_tree();
        let current = self.selected_tree_row(&tree);
        if let Some(row_index) = tree.next_row_index(current, delta) {
            self.select_tree_row(&tree, row_index);
        }
    }

    pub fn move_to_unviewed(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }

        let tree = self.full_file_tree();
        let file_indices: Vec<_> = tree
            .rows
            .iter()
            .filter_map(|row| match row.kind {
                FlatTreeRowKind::Directory { .. } => None,
                FlatTreeRowKind::File { file_index } => Some(file_index),
            })
            .collect();
        if file_indices.is_empty() {
            return;
        }

        let current_position = file_indices
            .iter()
            .position(|index| *index == self.selected)
            .unwrap_or(0);
        let direction = if delta.is_negative() { -1 } else { 1 };
        for offset in 1..=file_indices.len() {
            let position = if direction > 0 {
                (current_position + offset) % file_indices.len()
            } else {
                (current_position + file_indices.len() - offset) % file_indices.len()
            };
            let candidate = file_indices[position];
            if !self.files[candidate].viewed {
                self.select_file_index(candidate);
                return;
            }
        }
    }

    fn select_file_index(&mut self, index: usize) {
        if index >= self.files.len() {
            return;
        }
        self.reveal_file_in_tree(index);
        self.tree_cursor = Some(TreeRowId::File { file_index: index });
        if index == self.selected {
            return;
        }
        self.save_current_viewport();
        self.selected = index;
        self.restore_current_viewport();
    }

    fn select_tree_row(&mut self, tree: &FileTreeView, row_index: usize) {
        let Some(row) = tree.rows.get(row_index) else {
            return;
        };
        self.tree_cursor = Some(row.id());
        if let FlatTreeRowKind::File { file_index } = row.kind {
            self.select_file_index(file_index);
        }
    }

    fn save_current_viewport(&mut self) {
        let Some(path) = self.selected_file().map(|file| file.path.clone()) else {
            return;
        };
        self.viewport_by_path.insert(
            path,
            FileViewport {
                diff_scroll: self.diff_scroll,
                diff_cursor: self.diff_cursor,
            },
        );
    }

    fn restore_current_viewport(&mut self) {
        let viewport = self
            .selected_file()
            .and_then(|file| self.viewport_by_path.get(&file.path).copied())
            .unwrap_or_default();
        self.diff_scroll = viewport.diff_scroll;
        self.diff_cursor = viewport.diff_cursor;
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
        FileTreeView::build(&inputs, &self.collapsed_dirs)
    }

    fn full_file_tree(&self) -> FileTreeView {
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
        FileTreeView::build(&inputs, &BTreeSet::new())
    }

    pub fn selected_tree_row(&self, tree: &FileTreeView) -> Option<usize> {
        self.tree_cursor
            .as_ref()
            .and_then(|cursor| tree.row_for_id(cursor))
            .or_else(|| tree.selected_row_for_file(self.selected))
            .or_else(|| {
                self.selected_file()
                    .and_then(|file| visible_ancestor_row(tree, &file.path))
            })
    }

    pub fn toggle_tree_fold(&mut self) {
        let Some(directory) = self.fold_target_directory() else {
            return;
        };
        if !self.collapsed_dirs.remove(&directory) {
            self.collapsed_dirs.insert(directory.clone());
        }
        self.tree_cursor = Some(TreeRowId::Directory(directory));
    }

    pub fn collapse_tree_node(&mut self) {
        let Some(directory) = self.fold_target_directory() else {
            return;
        };
        self.collapsed_dirs.insert(directory.clone());
        self.tree_cursor = Some(TreeRowId::Directory(directory));
    }

    pub fn expand_tree_node(&mut self) {
        let Some(directory) = self.fold_target_directory() else {
            return;
        };
        self.collapsed_dirs.remove(&directory);
        self.tree_cursor = Some(TreeRowId::Directory(directory));
    }

    fn fold_target_directory(&self) -> Option<String> {
        if let Some(TreeRowId::Directory(directory)) = &self.tree_cursor {
            return Some(directory.clone());
        }
        self.selected_file()
            .and_then(|file| parent_dir_for_path(&file.path))
    }

    fn reveal_file_in_tree(&mut self, file_index: usize) {
        let Some(path) = self.files.get(file_index).map(|file| file.path.clone()) else {
            return;
        };
        for ancestor in ancestors_for_path(&path) {
            self.collapsed_dirs.remove(&ancestor);
        }
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

    pub fn mark_files_viewed_where(&mut self, mut predicate: impl FnMut(&ReviewFile) -> bool) {
        for file in &mut self.files {
            if predicate(file) {
                file.viewed = true;
            }
        }
    }

    pub fn annotate_generated_where(&mut self, mut predicate: impl FnMut(&ReviewFile) -> bool) {
        for file in &mut self.files {
            file.generated = predicate(file);
        }
    }

    pub fn scroll_diff(&mut self, delta: i16) {
        self.diff_scroll = if delta.is_negative() {
            self.diff_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.diff_scroll.saturating_add(delta as u16)
        };
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Files => Focus::Diff,
            Focus::Diff => Focus::Files,
        };
        self.ensure_diff_cursor_commentable();
    }

    pub fn move_diff_cursor(&mut self, delta: isize) {
        let rows = self.diff_rows_for_selected_file();
        if rows.is_empty() {
            self.diff_cursor = 0;
            return;
        }

        let max = rows.len() as isize - 1;
        let mut cursor = (self.diff_cursor as isize + delta).clamp(0, max) as usize;
        while cursor < rows.len() && rows[cursor].anchor.is_none() {
            let next = (cursor as isize + delta.signum()).clamp(0, max) as usize;
            if next == cursor {
                break;
            }
            cursor = next;
        }

        self.diff_cursor = cursor;
        if self.diff_cursor < self.diff_scroll as usize {
            self.diff_scroll = self.diff_cursor as u16;
        } else if self.diff_cursor > self.diff_scroll as usize + 15 {
            self.diff_scroll = self.diff_cursor.saturating_sub(15) as u16;
        }
    }

    pub fn diff_rows_for_selected_file(&self) -> Vec<DiffRow> {
        let Some(file) = self.selected_file() else {
            return Vec::new();
        };

        let mut rows = vec![DiffRow {
            old_lineno: None,
            new_lineno: None,
            prefix: " ",
            text: format!("{}  +{} -{}", file.path, file.additions, file.deletions),
            kind: DiffRowKind::FileHeader,
            anchor: None,
        }];

        let added_source = file
            .diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .filter(|line| matches!(line.kind, DiffLineKind::Added | DiffLineKind::Context))
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if let Some(summary) = crate::syntax::summarize(&file.path, &added_source) {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: format!(
                    "tree-sitter: {} root={} errors={}",
                    summary.language, summary.root_kind, summary.has_error
                ),
                kind: DiffRowKind::SyntaxSummary,
                anchor: None,
            });
        }

        for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: hunk.header.clone(),
                kind: DiffRowKind::HunkHeader,
                anchor: None,
            });
            for (line_index, line) in hunk.lines.iter().enumerate() {
                let prefix = match line.kind {
                    DiffLineKind::Context => " ",
                    DiffLineKind::Added => "+",
                    DiffLineKind::Removed => "-",
                    DiffLineKind::Meta => "\\",
                };
                rows.push(DiffRow {
                    old_lineno: line.old_lineno,
                    new_lineno: line.new_lineno,
                    prefix,
                    text: line.text.clone(),
                    kind: DiffRowKind::DiffLine(line.kind),
                    anchor: self.line_anchor(file, hunk_index, line_index),
                });
            }
        }

        if file.diff.hunks.is_empty() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: file.diff.raw.clone(),
                kind: DiffRowKind::Raw,
                anchor: None,
            });
        }

        rows
    }

    pub fn selected_line_anchor(&self) -> Option<CommentAnchor> {
        self.diff_rows_for_selected_file()
            .get(self.diff_cursor)
            .and_then(|row| row.anchor.clone())
    }

    pub fn comments_for_anchor(&self, anchor: &CommentAnchor) -> usize {
        self.comments
            .iter()
            .filter(|comment| comment.anchor.as_ref() == Some(anchor))
            .count()
    }

    pub fn move_to_comment(&mut self, delta: isize) {
        if self.comments.is_empty() {
            return;
        }

        let current = self.current_comment_index();
        let next = match (current, delta.is_negative()) {
            (Some(index), false) => (index + 1) % self.comments.len(),
            (Some(index), true) => (index + self.comments.len() - 1) % self.comments.len(),
            (None, false) => 0,
            (None, true) => self.comments.len() - 1,
        };
        self.select_comment(next);
    }

    pub fn add_comment(&mut self, body: String) {
        match self.focus {
            Focus::Files => self.add_file_comment(body),
            Focus::Diff => {
                if let Some(anchor) = self.selected_line_anchor() {
                    self.add_comment_with_anchor(body, anchor);
                }
            }
        }
    }

    pub fn add_file_comment(&mut self, body: String) {
        let Some(file) = self.selected_file() else {
            return;
        };
        let anchor = CommentAnchor::File {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            diff_fingerprint: file.fingerprint.clone(),
        };
        self.add_comment_with_anchor(body, anchor);
    }

    pub fn add_comment_with_anchor(&mut self, body: String, anchor: CommentAnchor) {
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
            path: anchor.path().to_owned(),
            line: anchor.line(),
            anchor: Some(anchor),
            body,
            created_at,
        });
    }

    fn current_comment_index(&self) -> Option<usize> {
        match self.focus {
            Focus::Diff => self.selected_line_anchor().and_then(|anchor| {
                self.comments
                    .iter()
                    .position(|comment| comment.anchor.as_ref() == Some(&anchor))
            }),
            Focus::Files => self.selected_file().and_then(|file| {
                self.comments.iter().position(|comment| {
                    comment.path == file.path
                        && matches!(comment.anchor, Some(CommentAnchor::File { .. }) | None)
                })
            }),
        }
    }

    fn select_comment(&mut self, index: usize) {
        let Some(comment) = self.comments.get(index).cloned() else {
            return;
        };
        let target_path = comment
            .anchor
            .as_ref()
            .map(CommentAnchor::path)
            .unwrap_or(&comment.path);
        let Some(file_index) = self.files.iter().position(|file| file.path == target_path) else {
            return;
        };

        self.select_file_index(file_index);
        match comment.anchor {
            Some(anchor @ CommentAnchor::Line { .. }) => {
                self.focus = Focus::Diff;
                if let Some(row_index) = self
                    .diff_rows_for_selected_file()
                    .iter()
                    .position(|row| row.anchor.as_ref() == Some(&anchor))
                {
                    self.diff_cursor = row_index;
                    self.diff_scroll = self.diff_cursor.saturating_sub(5) as u16;
                }
            }
            _ => self.focus = Focus::Files,
        }
    }

    fn ensure_diff_cursor_commentable(&mut self) {
        let rows = self.diff_rows_for_selected_file();
        if rows
            .get(self.diff_cursor)
            .and_then(|row| row.anchor.as_ref())
            .is_none()
            && let Some(index) = rows.iter().position(|row| row.anchor.is_some())
        {
            self.diff_cursor = index;
        }
    }

    fn line_anchor(
        &self,
        file: &ReviewFile,
        hunk_index: usize,
        line_index: usize,
    ) -> Option<CommentAnchor> {
        let hunk = file.diff.hunks.get(hunk_index)?;
        let line = hunk.lines.get(line_index)?;
        let (side, line_number) = match line.kind {
            DiffLineKind::Added => (DiffSide::New, line.new_lineno?),
            DiffLineKind::Removed => (DiffSide::Old, line.old_lineno?),
            DiffLineKind::Context => (DiffSide::New, line.new_lineno?),
            DiffLineKind::Meta => return None,
        };
        Some(CommentAnchor::Line {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            side,
            line: line_number,
            old_line: line.old_lineno,
            new_line: line.new_lineno,
            hunk_header: hunk.header.clone(),
            hunk_old_start: hunk.old_start,
            hunk_old_len: hunk.old_len,
            hunk_new_start: hunk.new_start,
            hunk_new_len: hunk.new_len,
            hunk_index,
            line_index,
            line_kind: match line.kind {
                DiffLineKind::Context => "context",
                DiffLineKind::Added => "added",
                DiffLineKind::Removed => "removed",
                DiffLineKind::Meta => "meta",
            }
            .to_owned(),
            line_text: line.text.clone(),
            line_fingerprint: fingerprint_line(
                &file.path,
                side,
                line_number,
                &line.text,
                &file.fingerprint,
            ),
            diff_fingerprint: file.fingerprint.clone(),
        })
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

fn parent_dir_for_path(path: &str) -> Option<String> {
    path.rsplit_once('/')
        .map(|(directory, _)| directory.to_owned())
}

fn ancestors_for_path(path: &str) -> Vec<String> {
    let Some(directory) = parent_dir_for_path(path) else {
        return Vec::new();
    };
    let mut ancestors = Vec::new();
    let mut current = String::new();
    for part in directory.split('/') {
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        ancestors.push(current.clone());
    }
    ancestors
}

fn visible_ancestor_row(tree: &FileTreeView, path: &str) -> Option<usize> {
    ancestors_for_path(path)
        .into_iter()
        .rev()
        .find_map(|directory| tree.row_for_id(&TreeRowId::Directory(directory)))
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
    fn file_tree_cursor_can_select_directory_without_changing_diff_file() {
        let mut session = session();

        session.move_selection(-1);

        assert_eq!(session.selected_file().unwrap().path, "src/tui.rs");
        assert!(matches!(
            session.tree_cursor,
            Some(TreeRowId::Directory(ref directory)) if directory == "src"
        ));
    }

    #[test]
    fn toggle_fold_collapses_selected_directory() {
        let mut session = session();
        session.move_selection(-1);

        session.toggle_tree_fold();

        assert!(session.collapsed_dirs.contains("src"));
        let tree = session.file_tree();
        assert_eq!(tree.rows.len(), 2);
        assert!(matches!(
            tree.rows[0].kind,
            FlatTreeRowKind::Directory { collapsed: true }
        ));
    }

    #[test]
    fn move_to_unviewed_reveals_folded_target() {
        let mut session = session();
        session.files[0].viewed = true;
        session.collapsed_dirs.insert("src".to_owned());

        session.move_to_unviewed(1);
        session.files[0].viewed = false;
        session.move_to_unviewed(1);

        assert_eq!(session.selected_file().unwrap().path, "src/tui.rs");
        assert!(!session.collapsed_dirs.contains("src"));
        assert!(session.selected_tree_row(&session.file_tree()).is_some());
    }

    #[test]
    fn viewed_toggle_updates_tree_counts() {
        let mut session = session();
        session.toggle_viewed();

        let tree = session.file_tree();

        assert_eq!(tree.rows[0].label, "src");
        assert_eq!(tree.rows[0].stats.viewed, 1);
    }

    #[test]
    fn add_file_comment_preserves_file_anchor() {
        let mut session = session();

        session.add_comment("File note".into());

        let comment = &session.comments[0];
        assert_eq!(comment.path, "src/tui.rs");
        assert_eq!(comment.line, None);
        assert!(matches!(comment.anchor, Some(CommentAnchor::File { .. })));
    }

    #[test]
    fn add_line_comment_uses_diff_cursor_anchor() {
        let mut session = session();
        session.toggle_focus();

        session.add_comment("Line note".into());

        let comment = &session.comments[0];
        assert_eq!(comment.path, "src/tui.rs");
        assert_eq!(comment.line, Some(1));
        assert!(matches!(comment.anchor, Some(CommentAnchor::Line { .. })));
    }

    #[test]
    fn comments_for_anchor_matches_existing_line_comment() {
        let mut session = session();
        session.toggle_focus();
        let anchor = session.selected_line_anchor().unwrap();

        session.add_comment("Line note".into());

        assert_eq!(session.comments_for_anchor(&anchor), 1);
    }

    #[test]
    fn move_to_unviewed_uses_tree_order_and_wraps() {
        let mut session = session();
        session.files[0].viewed = true;

        session.move_to_unviewed(1);

        assert_eq!(session.selected_file().unwrap().path, "README.md");

        session.files[0].viewed = false;
        session.move_to_unviewed(1);

        assert_eq!(session.selected_file().unwrap().path, "src/tui.rs");
    }

    #[test]
    fn preserves_diff_viewport_per_file() {
        let mut session = session();
        session.diff_scroll = 7;
        session.diff_cursor = 3;

        session.move_selection(1);
        session.diff_scroll = 2;
        session.diff_cursor = 1;
        session.move_selection(-1);

        assert_eq!(session.selected_file().unwrap().path, "src/tui.rs");
        assert_eq!(session.diff_scroll, 7);
        assert_eq!(session.diff_cursor, 3);

        session.move_selection(1);

        assert_eq!(session.selected_file().unwrap().path, "README.md");
        assert_eq!(session.diff_scroll, 2);
        assert_eq!(session.diff_cursor, 1);
    }

    #[test]
    fn move_to_comment_selects_line_comment_target() {
        let mut session = session();
        session.add_comment("File note".into());
        session.toggle_focus();
        let line_anchor = session.selected_line_anchor().unwrap();
        session.add_comment("Line note".into());

        session.focus = Focus::Files;
        session.move_to_comment(1);

        assert_eq!(session.focus, Focus::Diff);
        assert_eq!(session.selected_line_anchor(), Some(line_anchor));
    }

    #[test]
    fn marks_files_viewed_by_predicate() {
        let mut session = session();

        session.mark_files_viewed_where(|file| file.path == "README.md");

        assert!(!session.files[0].viewed);
        assert!(session.files[1].viewed);
    }

    #[test]
    fn annotates_generated_files_by_predicate() {
        let mut session = session();

        session.annotate_generated_where(|file| file.path == "README.md");

        assert!(!session.files[0].generated);
        assert!(session.files[1].generated);
    }
}
