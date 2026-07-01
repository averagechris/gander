use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    anchor::{CommentAnchor, DiffSide, RangeLineAnchor, fingerprint_line, fingerprint_range},
    config::Config,
    diff::{DiffLineKind, DiffSet, FileDiff, FileStatus},
    file_tree::{FileTreeInput, FileTreeView, FlatTreeRowKind, TreeRowId},
    jj::ReviewTarget,
    state::{Comment, FileState, ReviewState, ReviewStateMeta},
    syntax::{HighlightOutcome, SyntaxConfig, SyntaxSpan, SyntaxSummary},
};

#[derive(Debug, Clone)]
pub struct ReviewSession {
    pub repo: PathBuf,
    pub target: ReviewTarget,
    pub files: Vec<ReviewFile>,
    pub comments: Vec<Comment>,
    pub selected: usize,
    pub diff_scroll: u16,
    pub diff_cursor: usize,
    pub focus: Focus,
    pub syntax: SyntaxConfig,
    pub hide_generated: bool,
    pub collapsed_dirs: BTreeSet<String>,
    pub diff_range_selection: Option<DiffRangeSelection>,
    syntax_cache: RefCell<BTreeMap<SyntaxCacheKey, SyntaxFileCache>>,
    viewport_by_path: BTreeMap<String, FileViewport>,
    tree_cursor: Option<TreeRowId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SyntaxCacheKey {
    path: String,
    fingerprint: String,
    config_key: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SyntaxFileCache {
    summary: Option<SyntaxSummary>,
    new_lines: BTreeMap<(usize, usize), Vec<SyntaxSpan>>,
    old_lines: BTreeMap<(usize, usize), Vec<SyntaxSpan>>,
    new_status: SyntaxCacheStatus,
    old_status: SyntaxCacheStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SyntaxCacheStatus {
    #[default]
    Disabled,
    Unsupported,
    Highlighted,
    Failed,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRangeSelection {
    pub file_path: String,
    pub file_fingerprint: String,
    pub start_cursor: usize,
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
    pub syntax: Vec<SyntaxSpan>,
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
    #[cfg(test)]
    pub fn new(repo: PathBuf, target: ReviewTarget, diff: DiffSet, state: ReviewState) -> Self {
        Self::new_with_syntax(repo, target, diff, state, SyntaxConfig::default())
    }

    pub fn new_with_config(
        repo: PathBuf,
        target: ReviewTarget,
        diff: DiffSet,
        state: ReviewState,
        config: &Config,
    ) -> Self {
        Self::new_with_syntax(repo, target, diff, state, config.syntax.clone())
    }

    fn new_with_syntax(
        repo: PathBuf,
        target: ReviewTarget,
        diff: DiffSet,
        state: ReviewState,
        syntax: SyntaxConfig,
    ) -> Self {
        let ReviewState {
            files, comments, ..
        } = state;
        let mut session = Self {
            repo,
            target,
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
            syntax,
            hide_generated: false,
            collapsed_dirs: BTreeSet::new(),
            diff_range_selection: None,
            syntax_cache: RefCell::new(BTreeMap::new()),
            viewport_by_path: BTreeMap::new(),
            tree_cursor: None,
        };
        session.apply_state_files(&files);
        session
    }

    pub fn replace_diff(&mut self, target: ReviewTarget, diff: DiffSet) {
        let state = ReviewState {
            meta: ReviewStateMeta::default(),
            files: self
                .files
                .iter()
                .map(|file| {
                    (
                        file.path.clone(),
                        FileState {
                            fingerprint: file.fingerprint.clone(),
                            viewed: file.viewed,
                        },
                    )
                })
                .collect(),
            comments: self.comments.clone(),
        };
        *self = Self::new_with_syntax(self.repo.clone(), target, diff, state, self.syntax.clone());
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

    pub fn selected_visible_file(&self) -> Option<&ReviewFile> {
        self.selected_file().filter(|file| self.file_visible(file))
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

    pub fn toggle_generated_visibility(&mut self) {
        self.hide_generated = !self.hide_generated;
        self.ensure_selected_file_visible();
    }

    pub fn move_to_unviewed(&mut self, delta: isize) {
        if let Some(candidate) = self.next_unviewed_index(delta, None) {
            self.select_file_index(candidate);
        }
    }

    fn next_unviewed_index(&self, delta: isize, exclude: Option<usize>) -> Option<usize> {
        if self.files.is_empty() {
            return None;
        }

        let file_indices: Vec<_> = self
            .full_file_tree()
            .rows
            .iter()
            .filter_map(|row| match row.kind {
                FlatTreeRowKind::Directory { .. } => None,
                FlatTreeRowKind::File { file_index } => Some(file_index),
            })
            .collect();
        if file_indices.is_empty() {
            return None;
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
            if Some(candidate) != exclude && !self.files[candidate].viewed {
                return Some(candidate);
            }
        }
        None
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
        self.clear_diff_range_selection();
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

    pub fn select_visible_tree_row(&mut self, row_index: usize) {
        let tree = self.file_tree();
        self.select_tree_row(&tree, row_index);
    }

    pub fn select_diff_row(&mut self, row_index: usize) {
        let rows = self.diff_rows_for_selected_file();
        if rows.is_empty() {
            self.diff_cursor = 0;
            return;
        }
        self.focus = Focus::Diff;
        let row_index = row_index.min(rows.len() - 1);
        if rows[row_index].anchor.is_some() {
            self.diff_cursor = row_index;
        } else if let Some(nearest) = nearest_commentable_row(&rows, row_index) {
            self.diff_cursor = nearest;
        } else {
            self.diff_cursor = row_index;
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
            .filter(|(_, file)| self.file_visible(file))
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
            .filter(|(_, file)| self.file_visible(file))
            .map(|(index, file)| FileTreeInput {
                index,
                path: &file.path,
                viewed: file.viewed,
            })
            .collect();
        FileTreeView::build(&inputs, &BTreeSet::new())
    }

    pub fn selected_tree_row(&self, tree: &FileTreeView) -> Option<usize> {
        if self
            .selected_file()
            .is_none_or(|file| !self.file_visible(file))
        {
            return self
                .tree_cursor
                .as_ref()
                .and_then(|cursor| tree.row_for_id(cursor));
        }
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
        let selected = self.selected;
        let next = self.next_unviewed_index(1, Some(selected));
        if let Some(file) = self.selected_file_mut() {
            file.viewed = true;
        }
        if let Some(next) = next {
            self.select_file_index(next);
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
        self.ensure_selected_file_visible();
    }

    fn file_visible(&self, file: &ReviewFile) -> bool {
        !self.hide_generated || !file.generated
    }

    fn ensure_selected_file_visible(&mut self) {
        if self
            .selected_file()
            .is_some_and(|file| self.file_visible(file))
        {
            return;
        }
        if let Some(index) = self.files.iter().position(|file| self.file_visible(file)) {
            self.select_file_index(index);
        } else {
            self.tree_cursor = None;
            self.diff_cursor = 0;
            self.diff_scroll = 0;
            self.clear_diff_range_selection();
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

    pub fn toggle_diff_range_selection(&mut self) {
        if self.focus != Focus::Diff || self.selected_line_anchor().is_none() {
            return;
        }
        if self.diff_range_selection.is_some() {
            self.diff_range_selection = None;
            return;
        }
        let Some(file) = self.selected_file() else {
            return;
        };
        self.diff_range_selection = Some(DiffRangeSelection {
            file_path: file.path.clone(),
            file_fingerprint: file.fingerprint.clone(),
            start_cursor: self.diff_cursor,
        });
    }

    pub fn clear_diff_range_selection(&mut self) {
        self.diff_range_selection = None;
    }

    pub fn set_diff_range_selection(&mut self, start_row: usize, current_row: usize) {
        let Some(file) = self.selected_file() else {
            return;
        };
        self.diff_range_selection = Some(DiffRangeSelection {
            file_path: file.path.clone(),
            file_fingerprint: file.fingerprint.clone(),
            start_cursor: start_row,
        });
        self.select_diff_row(current_row);
    }

    pub fn has_active_diff_range(&self) -> bool {
        self.diff_range_bounds().is_some()
    }

    pub fn diff_range_bounds(&self) -> Option<(usize, usize)> {
        let selection = self.diff_range_selection.as_ref()?;
        let file = self.selected_file()?;
        if selection.file_path != file.path || selection.file_fingerprint != file.fingerprint {
            return None;
        }
        Some((
            selection.start_cursor.min(self.diff_cursor),
            selection.start_cursor.max(self.diff_cursor),
        ))
    }

    pub fn diff_row_in_active_range(&self, row_index: usize) -> bool {
        self.diff_range_bounds()
            .map(|(start, end)| row_index >= start && row_index <= end)
            .unwrap_or(false)
    }

    pub fn diff_rows_for_selected_file(&self) -> Vec<DiffRow> {
        let Some(file) = self.selected_visible_file() else {
            return Vec::new();
        };

        let mut rows = vec![DiffRow {
            old_lineno: None,
            new_lineno: None,
            prefix: " ",
            text: format!("{}  +{} -{}", file.path, file.additions, file.deletions),
            syntax: Vec::new(),
            kind: DiffRowKind::FileHeader,
            anchor: None,
        }];

        let (new_source, new_line_indices) = syntax_source(file, SyntaxSide::New);
        let (old_source, old_line_indices) = syntax_source(file, SyntaxSide::Old);
        let syntax_cache = self.syntax_cache_for_file(
            file,
            &new_source,
            &new_line_indices,
            &old_source,
            &old_line_indices,
        );
        if let Some(summary) = syntax_cache.summary.clone() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: format!(
                    "tree-sitter: {} root={} errors={}",
                    summary.language, summary.root_kind, summary.has_error
                ),
                syntax: Vec::new(),
                kind: DiffRowKind::SyntaxSummary,
                anchor: None,
            });
        }
        if syntax_cache.new_status == SyntaxCacheStatus::Failed
            || syntax_cache.old_status == SyntaxCacheStatus::Failed
        {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: "tree-sitter: highlighting unavailable".to_owned(),
                syntax: Vec::new(),
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
                syntax: Vec::new(),
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
                let syntax = match line.kind {
                    DiffLineKind::Added | DiffLineKind::Context => syntax_cache
                        .new_lines
                        .get(&(hunk_index, line_index))
                        .cloned()
                        .unwrap_or_default(),
                    DiffLineKind::Removed => syntax_cache
                        .old_lines
                        .get(&(hunk_index, line_index))
                        .cloned()
                        .unwrap_or_default(),
                    DiffLineKind::Meta => Vec::new(),
                };
                rows.push(DiffRow {
                    old_lineno: line.old_lineno,
                    new_lineno: line.new_lineno,
                    prefix,
                    text: line.text.clone(),
                    syntax,
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
                syntax: Vec::new(),
                kind: DiffRowKind::Raw,
                anchor: None,
            });
        }

        rows
    }

    fn syntax_cache_for_file(
        &self,
        file: &ReviewFile,
        new_source: &str,
        new_line_indices: &[(usize, usize)],
        old_source: &str,
        old_line_indices: &[(usize, usize)],
    ) -> SyntaxFileCache {
        let key = SyntaxCacheKey {
            path: file.path.clone(),
            fingerprint: file.fingerprint.clone(),
            config_key: self.syntax.cache_key(),
        };
        if let Some(cached) = self.syntax_cache.borrow().get(&key).cloned() {
            return cached;
        }

        let (new_lines, new_status) =
            syntax_highlights_by_diff_line(&file.path, new_source, new_line_indices, &self.syntax);
        let (old_lines, old_status) =
            syntax_highlights_by_diff_line(&file.path, old_source, old_line_indices, &self.syntax);

        let computed = SyntaxFileCache {
            summary: crate::syntax::summarize_with_config(&file.path, new_source, &self.syntax),
            new_lines,
            old_lines,
            new_status,
            old_status,
        };
        self.syntax_cache.borrow_mut().insert(key, computed.clone());
        computed
    }

    pub fn selected_line_anchor(&self) -> Option<CommentAnchor> {
        self.diff_rows_for_selected_file()
            .get(self.diff_cursor)
            .and_then(|row| row.anchor.clone())
    }

    pub fn selected_comment_anchor(&self) -> Option<CommentAnchor> {
        self.selected_range_anchor()
            .or_else(|| self.selected_line_anchor())
    }

    pub fn selected_range_anchor(&self) -> Option<CommentAnchor> {
        let (start, end) = self.diff_range_bounds()?;
        let file = self.selected_file()?;
        let rows = self.diff_rows_for_selected_file();
        let mut range_lines = Vec::new();
        for (row_index, row) in rows.iter().enumerate().take(end + 1).skip(start) {
            if let Some(CommentAnchor::Line {
                side,
                line,
                old_line,
                new_line,
                hunk_header,
                hunk_index,
                line_index,
                line_kind,
                line_text,
                line_fingerprint,
                ..
            }) = row.anchor.clone()
            {
                range_lines.push(RangeLineAnchor {
                    side,
                    line,
                    old_line,
                    new_line,
                    hunk_header,
                    hunk_index,
                    line_index,
                    row_index,
                    line_kind,
                    line_text,
                    line_fingerprint,
                });
            }
        }
        match range_lines.as_slice() {
            [] => None,
            [single] => rows
                .get(single.row_index)
                .and_then(|row| row.anchor.clone()),
            _ => {
                let start_line = range_lines.first()?.line;
                let end_line = range_lines.last()?.line;
                let line_fingerprints: Vec<_> = range_lines
                    .iter()
                    .map(|line| line.line_fingerprint.clone())
                    .collect();
                Some(CommentAnchor::Range {
                    path: file.path.clone(),
                    old_path: file.old_path.clone(),
                    start_line,
                    end_line,
                    start_row_index: range_lines.first()?.row_index,
                    end_row_index: range_lines.last()?.row_index,
                    lines: range_lines,
                    diff_fingerprint: file.fingerprint.clone(),
                    range_fingerprint: fingerprint_range(
                        &file.path,
                        &file.fingerprint,
                        &line_fingerprints,
                    ),
                })
            }
        }
    }

    pub fn comments_for_anchor(&self, anchor: &CommentAnchor) -> usize {
        self.comments
            .iter()
            .filter(|comment| comment.anchor.as_ref() == Some(anchor))
            .count()
    }

    pub fn comments_for_diff_row_anchor(&self, anchor: &CommentAnchor) -> usize {
        let row_fingerprint = match anchor {
            CommentAnchor::Line {
                line_fingerprint, ..
            } => line_fingerprint,
            _ => return self.comments_for_anchor(anchor),
        };
        self.comments
            .iter()
            .filter(|comment| match comment.anchor.as_ref() {
                Some(existing) if existing == anchor => true,
                Some(CommentAnchor::Range { lines, .. }) => lines
                    .iter()
                    .any(|line| &line.line_fingerprint == row_fingerprint),
                _ => false,
            })
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

    pub fn selected_comment(&self) -> Option<&Comment> {
        self.selected_comment_index()
            .and_then(|index| self.comments.get(index))
    }

    pub fn selected_comment_index(&self) -> Option<usize> {
        match self.focus {
            Focus::Diff => self.selected_line_anchor().and_then(|anchor| {
                let row_fingerprint = match &anchor {
                    CommentAnchor::Line {
                        line_fingerprint, ..
                    } => Some(line_fingerprint),
                    _ => None,
                };
                self.comments.iter().position(|comment| {
                    comment.anchor.as_ref() == Some(&anchor)
                        || matches!(
                            (comment.anchor.as_ref(), row_fingerprint),
                            (Some(CommentAnchor::Range { lines, .. }), Some(fingerprint))
                                if lines.iter().any(|line| &line.line_fingerprint == fingerprint)
                        )
                })
            }),
            Focus::Files => self.selected_file().and_then(|file| {
                self.comments.iter().position(|comment| {
                    comment.path == file.path
                        && matches!(comment.anchor, Some(CommentAnchor::File { .. }) | None)
                })
            }),
        }
    }

    pub fn update_comment_body(&mut self, id: &str, body: String) -> bool {
        if body.trim().is_empty() {
            return false;
        }
        let Some(comment) = self.comments.iter_mut().find(|comment| comment.id == id) else {
            return false;
        };
        comment.body = body;
        true
    }

    pub fn delete_comment(&mut self, id: &str) -> bool {
        let Some(index) = self.comments.iter().position(|comment| comment.id == id) else {
            return false;
        };
        self.comments.remove(index);
        true
    }

    pub fn add_comment(&mut self, body: String) {
        match self.focus {
            Focus::Files => self.add_file_comment(body),
            Focus::Diff => {
                if let Some(anchor) = self.selected_comment_anchor() {
                    self.add_comment_with_anchor(body, anchor);
                    self.clear_diff_range_selection();
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
            end_line: anchor
                .end_line()
                .filter(|end_line| Some(*end_line) != anchor.line()),
            anchor: Some(anchor),
            body,
            created_at,
        });
    }

    fn current_comment_index(&self) -> Option<usize> {
        self.selected_comment_index()
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
            meta: ReviewStateMeta {
                version: 1,
                base: Some(self.target.base.clone()),
                revision: Some(self.target.rev.clone()),
                repo: Some(self.repo.display().to_string()),
                saved_at: Some(Utc::now()),
            },
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
        let generated = self.files.iter().filter(|file| file.generated).count();
        let additions: usize = self.files.iter().map(|file| file.additions).sum();
        let deletions: usize = self.files.iter().map(|file| file.deletions).sum();
        format!(
            "{} files ({viewed}/{} viewed, {generated} generated/noisy), +{additions}/-{deletions}, {} comments",
            self.files.len(),
            self.files.len(),
            self.comments.len()
        )
    }
}

#[derive(Debug, Clone, Copy)]
enum SyntaxSide {
    Old,
    New,
}

fn syntax_source(file: &ReviewFile, side: SyntaxSide) -> (String, Vec<(usize, usize)>) {
    let mut lines = Vec::new();
    let mut indices = Vec::new();
    for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
        for (line_index, line) in hunk.lines.iter().enumerate() {
            let included = match (side, line.kind) {
                (SyntaxSide::New, DiffLineKind::Added | DiffLineKind::Context) => true,
                (SyntaxSide::Old, DiffLineKind::Removed | DiffLineKind::Context) => true,
                (_, DiffLineKind::Meta) => false,
                _ => false,
            };
            if included {
                lines.push(line.text.as_str());
                indices.push((hunk_index, line_index));
            }
        }
    }
    (lines.join("\n"), indices)
}

fn syntax_highlights_by_diff_line(
    path: &str,
    source: &str,
    line_indices: &[(usize, usize)],
    config: &SyntaxConfig,
) -> (BTreeMap<(usize, usize), Vec<SyntaxSpan>>, SyntaxCacheStatus) {
    match crate::syntax::highlight_outcome(path, source, config) {
        HighlightOutcome::Highlighted { lines, .. } => (
            lines
                .into_iter()
                .zip(line_indices.iter())
                .map(|(line, index)| (*index, line.spans))
                .collect(),
            SyntaxCacheStatus::Highlighted,
        ),
        HighlightOutcome::Disabled => (BTreeMap::new(), SyntaxCacheStatus::Disabled),
        HighlightOutcome::Unsupported => (BTreeMap::new(), SyntaxCacheStatus::Unsupported),
        HighlightOutcome::Failed { .. } => (BTreeMap::new(), SyntaxCacheStatus::Failed),
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

fn nearest_commentable_row(rows: &[DiffRow], target: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.anchor.is_some())
        .min_by_key(|(index, _)| index.abs_diff(target))
        .map(|(index, _)| index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        diff::DiffSet,
        jj::ReviewTarget,
        state::ReviewState,
        syntax::{HighlightKind, SyntaxSpan},
    };

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
        ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        )
    }

    fn three_file_session() -> ReviewSession {
        let diff = DiffSet::parse(
            r#"diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1 +1 @@
-old
+new
diff --git a/src/c.rs b/src/c.rs
--- a/src/c.rs
+++ b/src/c.rs
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        )
    }

    fn multi_line_session() -> ReviewSession {
        let diff = DiffSet::parse(
            r#"diff --git a/src/app.rs b/src/app.rs
--- a/src/app.rs
+++ b/src/app.rs
@@ -1,3 +1,4 @@
 fn main() {
-    old();
+    new();
+    extra();
 }
"#,
        )
        .unwrap();
        ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        )
    }

    fn rust_syntax_session() -> ReviewSession {
        let diff = DiffSet::parse(
            r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
-fn old() {
+fn new() {
     println!("hi");
 }
"#,
        )
        .unwrap();
        ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        )
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
    fn toggle_viewed_can_unmark_viewed_file() {
        let mut session = session();

        session.toggle_viewed();
        assert!(session.selected_file().unwrap().viewed);

        session.toggle_viewed();
        assert!(!session.selected_file().unwrap().viewed);
    }

    #[test]
    fn marking_viewed_advances_to_next_unviewed_file() {
        let mut session = three_file_session();
        session.move_selection(1);
        assert_eq!(session.selected_file().unwrap().path, "src/b.rs");

        session.mark_selected_viewed();

        assert_eq!(session.selected_file().unwrap().path, "src/c.rs");
        assert!(session.files[1].viewed);
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
    fn rust_diff_rows_include_syntax_spans() {
        let session = rust_syntax_session();

        let rows = session.diff_rows_for_selected_file();
        let added_fn = rows.iter().find(|row| row.text == "fn new() {").unwrap();

        assert!(added_fn.syntax.iter().any(|span| span
            == &SyntaxSpan {
                text: "fn".to_owned(),
                kind: Some(HighlightKind::Keyword),
            }));
        assert!(added_fn.syntax.iter().any(|span| span
            == &SyntaxSpan {
                text: "new".to_owned(),
                kind: Some(HighlightKind::Function),
            }));
    }

    #[test]
    fn disabled_syntax_config_skips_diff_row_spans() {
        let mut session = rust_syntax_session();
        session.syntax = SyntaxConfig {
            enabled: false,
            ..SyntaxConfig::default()
        };

        let rows = session.diff_rows_for_selected_file();
        let added_fn = rows.iter().find(|row| row.text == "fn new() {").unwrap();

        assert!(added_fn.syntax.is_empty());
        assert!(
            !rows
                .iter()
                .any(|row| matches!(row.kind, DiffRowKind::SyntaxSummary))
        );
    }

    #[test]
    fn syntax_highlights_are_cached_per_file() {
        let session = rust_syntax_session();

        assert_eq!(session.syntax_cache.borrow().len(), 0);
        session.diff_rows_for_selected_file();
        assert_eq!(session.syntax_cache.borrow().len(), 1);
        session.diff_rows_for_selected_file();
        assert_eq!(session.syntax_cache.borrow().len(), 1);
    }

    #[test]
    fn syntax_cache_key_tracks_config_changes() {
        let mut session = rust_syntax_session();

        session.diff_rows_for_selected_file();
        session.syntax.languages = vec!["python".to_owned()];
        session.diff_rows_for_selected_file();

        assert_eq!(session.syntax_cache.borrow().len(), 2);
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
    fn range_selection_builds_range_anchor() {
        let mut session = multi_line_session();
        session.toggle_focus();
        session.toggle_diff_range_selection();
        session.move_diff_cursor(2);

        let anchor = session.selected_range_anchor().unwrap();

        assert!(matches!(anchor, CommentAnchor::Range { .. }));
        if let CommentAnchor::Range { lines, .. } = anchor {
            assert!(lines.len() > 1);
        }
    }

    #[test]
    fn range_comment_records_end_line_and_clears_selection() {
        let mut session = multi_line_session();
        session.toggle_focus();
        session.toggle_diff_range_selection();
        session.move_diff_cursor(2);

        session.add_comment("Range note".into());

        assert!(matches!(
            session.comments[0].anchor,
            Some(CommentAnchor::Range { .. })
        ));
        assert!(session.comments[0].end_line.is_some());
        assert!(session.diff_range_selection.is_none());
    }

    #[test]
    fn selected_comment_matches_range_containing_cursor() {
        let mut session = multi_line_session();
        session.toggle_focus();
        session.toggle_diff_range_selection();
        session.move_diff_cursor(2);
        session.add_comment("Range note".into());
        session.move_diff_cursor(-1);

        let selected = session.selected_comment().unwrap();

        assert_eq!(selected.body, "Range note");
    }

    #[test]
    fn can_update_and_delete_comment_by_id() {
        let mut session = session();
        session.add_comment("Old".into());
        let id = session.comments[0].id.clone();

        assert!(session.update_comment_body(&id, "New".into()));
        assert_eq!(session.comments[0].body, "New");
        assert!(session.delete_comment(&id));
        assert!(session.comments.is_empty());
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

    #[test]
    fn hiding_generated_filters_file_tree_and_moves_selection() {
        let mut session = three_file_session();
        session.files[0].generated = true;

        session.toggle_generated_visibility();

        assert!(session.hide_generated);
        assert_eq!(session.selected_file().unwrap().path, "src/b.rs");
        assert!(session.file_tree().rows.iter().all(|row| {
            !matches!(row.kind, FlatTreeRowKind::File { file_index } if file_index == 0)
        }));
    }

    #[test]
    fn hiding_all_generated_files_clears_visible_diff_rows() {
        let mut session = session();
        for file in &mut session.files {
            file.generated = true;
        }

        session.toggle_generated_visibility();

        assert!(session.selected_visible_file().is_none());
        assert!(session.diff_rows_for_selected_file().is_empty());
        assert!(session.file_tree().rows.is_empty());
    }

    #[test]
    fn summary_counts_generated_files() {
        let mut session = session();
        session.annotate_generated_where(|file| file.path == "README.md");

        assert!(session.summary_line().contains("1 generated/noisy"));
    }

    #[test]
    fn into_state_records_session_metadata() {
        let session = session();

        let state = session.into_state();

        assert_eq!(state.meta.version, 1);
        assert_eq!(state.meta.base.as_deref(), Some("trunk()"));
        assert_eq!(state.meta.revision.as_deref(), Some("@"));
        assert!(state.meta.repo.is_some());
        assert!(state.meta.saved_at.is_some());
    }
}
