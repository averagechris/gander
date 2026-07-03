//! Review session domain state: file selection, tree folding, viewed marks,
//! comments, and viewport bookkeeping.
//!
//! Submodules:
//! - [`diff_rows`]: flattened diff-row construction for the diff pane
//! - [`syntax_cache`]: per-file tree-sitter highlight caching

mod diff_rows;
mod syntax_cache;

pub use diff_rows::{DiffRow, DiffRowKind};

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    rc::Rc,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    agent::{AgentDraft, AgentFlag, AgentOverlay, ChunkPart, DraftState, ReviewChunk},
    anchor::{CommentAnchor, RangeLineAnchor, fingerprint_range},
    config::{Config, LimitsConfig},
    diff::{DiffSet, FileDiff, FileStatus},
    file_tree::{FileTreeInput, FileTreeView, FlatTreeRowKind, TreeRowId},
    jj::ReviewTarget,
    state::{Comment, CommentState, FileState, ReviewState, ReviewStateMeta},
    syntax::SyntaxConfig,
};

use diff_rows::nearest_commentable_row;
use syntax_cache::{SyntaxCacheKey, SyntaxFileCache, SyntaxSide, syntax_source};

/// Memoized diff rows keyed by file/syntax-config cache key plus the
/// context-fold and force-render-large flags.
type DiffRowsCache = BTreeMap<(SyntaxCacheKey, bool, bool), Rc<Vec<DiffRow>>>;

const GENERATED_TREE_GROUP: &str = "generated/noisy";

/// Rough number of diff rows kept visible below the cursor when auto-scrolling.
const DIFF_CURSOR_SCROLL_MARGIN: usize = 15;

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
    pub viewed_filter: ViewedFilter,
    /// When set, long runs of unchanged context lines collapse into
    /// symbol-labelled fold rows in the diff pane.
    pub fold_context: bool,
    pub collapsed_dirs: BTreeSet<String>,
    pub diff_range_selection: Option<DiffRangeSelection>,
    /// Diffs with more lines than this render as a placeholder until the
    /// file is explicitly expanded.
    pub max_diff_lines: usize,
    /// Large-change nudge thresholds (changed lines / changed files); 0
    /// disables a criterion. See [`Self::large_change_nudge`].
    pub nudge_diff_lines: usize,
    pub nudge_files: usize,
    /// Files the user expanded past the large-diff threshold.
    pub force_rendered: BTreeSet<String>,
    /// Agent-suggested review order (highest priority first), from the
    /// agent overlay. Empty when no agent has made a suggestion.
    pub agent_ordering: Vec<String>,
    /// Whether the agent-suggested order is applied to the file list.
    pub use_agent_order: bool,
    /// Sections agents flagged as critical, surfaced in the diff gutter and
    /// the flag list popup.
    pub agent_flags: Vec<AgentFlag>,
    /// Agent-defined reviewable units that can span or subdivide files.
    pub review_chunks: Vec<ReviewChunk>,
    /// Agent-drafted comments with their dispositions.
    pub agent_drafts: Vec<AgentDraft>,
    selected_comment_id: Option<String>,
    syntax_cache: RefCell<BTreeMap<SyntaxCacheKey, SyntaxFileCache>>,
    /// Memoized diff rows per file (same key as the syntax cache plus the
    /// context-fold flag), rebuilt only when the diff fingerprint, syntax
    /// config, or fold mode changes.
    rows_cache: RefCell<DiffRowsCache>,
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

/// Which files remain visible in the tree based on their viewed mark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ViewedFilter {
    #[default]
    All,
    Unviewed,
    Viewed,
}

impl ViewedFilter {
    fn admits(self, viewed: bool) -> bool {
        match self {
            Self::All => true,
            Self::Unviewed => !viewed,
            Self::Viewed => viewed,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Unviewed,
            Self::Unviewed => Self::Viewed,
            Self::Viewed => Self::All,
        }
    }

    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Unviewed => Some("unviewed only"),
            Self::Viewed => Some("viewed only"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRangeSelection {
    pub file_path: String,
    pub file_fingerprint: String,
    pub start_cursor: usize,
}

/// A changed symbol in the selected file, resolved to its first changed
/// diff row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedSymbolTarget {
    pub label: String,
    pub row_index: usize,
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
        Self::new_with_syntax_and_limits(
            repo,
            target,
            diff,
            state,
            config.syntax.clone(),
            config.limits.clone(),
        )
    }

    #[cfg(test)]
    fn new_with_syntax(
        repo: PathBuf,
        target: ReviewTarget,
        diff: DiffSet,
        state: ReviewState,
        syntax: SyntaxConfig,
    ) -> Self {
        Self::new_with_syntax_and_limits(repo, target, diff, state, syntax, LimitsConfig::default())
    }

    fn new_with_syntax_and_limits(
        repo: PathBuf,
        target: ReviewTarget,
        diff: DiffSet,
        state: ReviewState,
        syntax: SyntaxConfig,
        limits: LimitsConfig,
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
            viewed_filter: ViewedFilter::default(),
            fold_context: false,
            collapsed_dirs: BTreeSet::new(),
            diff_range_selection: None,
            max_diff_lines: limits.max_diff_lines,
            nudge_diff_lines: limits.nudge_diff_lines,
            nudge_files: limits.nudge_files,
            force_rendered: BTreeSet::new(),
            agent_ordering: Vec::new(),
            use_agent_order: true,
            agent_flags: Vec::new(),
            review_chunks: Vec::new(),
            agent_drafts: Vec::new(),
            selected_comment_id: None,
            syntax_cache: RefCell::new(BTreeMap::new()),
            rows_cache: RefCell::new(BTreeMap::new()),
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
        *self = Self::new_with_syntax_and_limits(
            self.repo.clone(),
            target,
            diff,
            state,
            self.syntax.clone(),
            LimitsConfig {
                max_diff_lines: self.max_diff_lines,
                nudge_diff_lines: self.nudge_diff_lines,
                nudge_files: self.nudge_files,
            },
        );
    }

    /// Toggle rendering the selected file even though its diff exceeds the
    /// large-diff threshold.
    pub fn toggle_large_diff_render(&mut self) {
        let Some(path) = self.selected_file().map(|file| file.path.clone()) else {
            return;
        };
        if !self.force_rendered.remove(&path) {
            self.force_rendered.insert(path);
        }
        self.diff_cursor = 0;
        self.diff_scroll = 0;
        self.ensure_diff_cursor_commentable();
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

    /// Cycle the viewed filter: all files -> unviewed only -> viewed only.
    pub fn cycle_viewed_filter(&mut self) {
        self.viewed_filter = self.viewed_filter.next();
        self.ensure_selected_file_visible();
    }

    /// Toggle collapsing long unchanged-context runs in the diff pane.
    pub fn toggle_context_fold(&mut self) {
        self.fold_context = !self.fold_context;
        // Row indices shift when folds appear/disappear; snap the cursor back
        // to a commentable row.
        self.ensure_diff_cursor_commentable();
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

    /// Indices of files currently visible (respecting the generated-file
    /// toggle), in session order. Used by the fuzzy file search overlay.
    pub fn visible_file_indices(&self) -> Vec<usize> {
        self.files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| self.file_visible(file).then_some(index))
            .collect()
    }

    /// Jump straight to a file by index, revealing it in the tree and moving
    /// focus to the files pane.
    pub fn jump_to_file(&mut self, file_index: usize) {
        self.select_file_index(file_index);
        self.focus = Focus::Files;
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
        if self.agent_order_active() {
            return self.agent_ordered_tree();
        }
        self.build_file_tree(&self.collapsed_dirs)
    }

    /// Tree with every directory expanded, used for stable file ordering
    /// regardless of the user's current fold state.
    fn full_file_tree(&self) -> FileTreeView {
        if self.agent_order_active() {
            return self.agent_ordered_tree();
        }
        self.build_file_tree(&BTreeSet::new())
    }

    /// Apply agent overlay suggestions to the session.
    pub fn apply_agent_overlay(&mut self, overlay: &AgentOverlay) {
        self.agent_ordering = overlay.ordering.clone();
        self.agent_flags = overlay.flags.clone();
        self.review_chunks = overlay.chunks.clone();
        self.agent_drafts = overlay.drafts.clone();
    }

    /// Agent drafts still awaiting a human decision.
    pub fn pending_agent_drafts(&self) -> Vec<AgentDraft> {
        self.agent_drafts
            .iter()
            .filter(|draft| draft.state == DraftState::Pending)
            .cloned()
            .collect()
    }

    /// Accept an agent draft as a real comment (optionally with an edited
    /// body). Anchors to the drafted line when it exists in the current
    /// diff, otherwise to the file. Returns the new comment id, or `None`
    /// when the file is not part of the current diff or the body is empty.
    pub fn accept_agent_draft(&mut self, draft: &AgentDraft, body: String) -> Option<String> {
        if body.trim().is_empty() {
            return None;
        }
        let file_index = self.files.iter().position(|file| file.path == draft.path)?;
        self.select_file_index(file_index);
        let anchor = draft
            .line
            .and_then(|line| {
                self.diff_rows_for_selected_file()
                    .iter()
                    .find(|row| row.new_lineno == Some(line) && row.anchor.is_some())
                    .and_then(|row| row.anchor.clone())
            })
            .unwrap_or_else(|| {
                let file = &self.files[file_index];
                CommentAnchor::File {
                    path: file.path.clone(),
                    old_path: file.old_path.clone(),
                    diff_fingerprint: file.fingerprint.clone(),
                }
            });
        self.add_comment_with_anchor(body, anchor);
        let comment_id = self.comments.last()?.id.clone();
        self.set_agent_draft_state(&draft.id, DraftState::Accepted, Some(comment_id.clone()));
        Some(comment_id)
    }

    /// Discard an agent draft without creating a comment.
    pub fn discard_agent_draft(&mut self, draft_id: &str) {
        self.set_agent_draft_state(draft_id, DraftState::Discarded, None);
    }

    fn set_agent_draft_state(
        &mut self,
        draft_id: &str,
        state: DraftState,
        accepted_comment_id: Option<String>,
    ) {
        if let Some(draft) = self
            .agent_drafts
            .iter_mut()
            .find(|draft| draft.id == draft_id)
        {
            draft.state = state;
            draft.accepted_comment_id = accepted_comment_id;
        }
    }

    /// Jump to a chunk part: select its file and move the diff cursor to
    /// the part's first line (or the top of the file without line info).
    pub fn jump_to_chunk_part(&mut self, part: &ChunkPart) {
        let Some(file_index) = self.files.iter().position(|file| file.path == part.path) else {
            return;
        };
        self.select_file_index(file_index);
        let Some(start_line) = part.start_line else {
            self.focus = Focus::Files;
            self.diff_scroll = 0;
            return;
        };
        if let Some(row_index) = self.diff_rows_for_selected_file().iter().position(|row| {
            row.anchor.is_some() && row.new_lineno.is_some_and(|line| line >= start_line)
        }) {
            self.jump_to_diff_row(row_index);
        }
    }

    /// A nudge for large changes: when the diff exceeds the size thresholds
    /// and no agent has organized the review yet (no chunks or ordering in
    /// the overlay), suggest summoning one. `None` when the change is small,
    /// nudging is disabled, or an agent already structured the review.
    pub fn large_change_nudge(&self) -> Option<String> {
        if !self.review_chunks.is_empty() || !self.agent_ordering.is_empty() {
            return None;
        }
        let files = self.files.len();
        let lines: usize = self
            .files
            .iter()
            .map(|file| file.additions + file.deletions)
            .sum();
        let many_lines = self.nudge_diff_lines > 0 && lines >= self.nudge_diff_lines;
        let many_files = self.nudge_files > 0 && files >= self.nudge_files;
        if !many_lines && !many_files {
            return None;
        }
        Some(format!(
            "large change ({files} files, {lines} changed lines) — @ summons an agent to organize it, T tours the chunks"
        ))
    }

    /// Flags sorted for display: critical first, then by path and line.
    pub fn flags_sorted(&self) -> Vec<AgentFlag> {
        let mut flags = self.agent_flags.clone();
        flags.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.path.cmp(&right.path))
                .then_with(|| left.line.cmp(&right.line))
        });
        flags
    }

    pub fn file_has_flags(&self, path: &str) -> bool {
        self.agent_flags.iter().any(|flag| flag.path == path)
    }

    /// Whether a diff row of the selected file is covered by an agent flag:
    /// line flags match the new-side line number, file-level flags mark the
    /// file header row.
    pub fn diff_row_flagged(&self, row: &DiffRow) -> bool {
        let Some(file) = self.selected_visible_file() else {
            return false;
        };
        self.agent_flags.iter().any(|flag| {
            flag.path == file.path
                && match flag.line {
                    Some(line) => row.new_lineno == Some(line),
                    None => matches!(row.kind, DiffRowKind::FileHeader),
                }
        })
    }

    /// Jump to a flagged section: select the file and move the diff cursor
    /// to the flagged line (or the top of the file for file-level flags).
    pub fn jump_to_flag(&mut self, flag: &AgentFlag) {
        let Some(file_index) = self.files.iter().position(|file| file.path == flag.path) else {
            return;
        };
        self.select_file_index(file_index);
        let Some(line) = flag.line else {
            self.focus = Focus::Files;
            self.diff_scroll = 0;
            return;
        };
        if let Some(row_index) = self
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.new_lineno == Some(line) && row.anchor.is_some())
        {
            self.jump_to_diff_row(row_index);
        }
    }

    pub fn agent_order_active(&self) -> bool {
        self.use_agent_order && !self.agent_ordering.is_empty()
    }

    /// Toggle applying the agent-suggested review order.
    pub fn toggle_agent_order(&mut self) {
        self.use_agent_order = !self.use_agent_order;
    }

    /// Flat file list in agent-priority order: listed files first (in the
    /// order the agent gave), everything else after in natural order.
    fn agent_ordered_tree(&self) -> FileTreeView {
        let rank_for = |path: &str| {
            self.agent_ordering
                .iter()
                .position(|candidate| candidate == path)
                .unwrap_or(self.agent_ordering.len())
        };
        let mut visible: Vec<usize> = self.visible_file_indices();
        visible.sort_by_key(|index| (rank_for(&self.files[*index].path), *index));
        FileTreeView {
            rows: visible
                .into_iter()
                .map(|file_index| {
                    let file = &self.files[file_index];
                    crate::file_tree::FlatTreeRow {
                        depth: 0,
                        path: file.path.clone(),
                        label: file.path.clone(),
                        kind: FlatTreeRowKind::File { file_index },
                        stats: crate::file_tree::ViewedStats {
                            viewed: usize::from(file.viewed),
                            total: 1,
                        },
                    }
                })
                .collect(),
        }
    }

    fn build_file_tree(&self, collapsed_dirs: &BTreeSet<String>) -> FileTreeView {
        let inputs: Vec<_> = self
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| self.file_visible(file))
            .map(|(index, file)| FileTreeInput {
                index,
                path: &file.path,
                viewed: file.viewed,
                group: self.tree_group_for_file(file),
            })
            .collect();
        FileTreeView::build(&inputs, collapsed_dirs)
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
                    .and_then(|file| visible_ancestor_row(tree, &self.tree_path_for_file(file)))
            })
    }

    fn tree_group_for_file(&self, file: &ReviewFile) -> Option<&'static str> {
        file.generated.then_some(GENERATED_TREE_GROUP)
    }

    fn tree_path_for_file(&self, file: &ReviewFile) -> String {
        self.tree_group_for_file(file)
            .map(|group| format!("{group}/{}", file.path))
            .unwrap_or_else(|| file.path.clone())
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
            .and_then(|file| parent_dir_for_path(&self.tree_path_for_file(file)))
    }

    fn reveal_file_in_tree(&mut self, file_index: usize) {
        let Some(file) = self.files.get(file_index) else {
            return;
        };
        for ancestor in ancestors_for_path(&self.tree_path_for_file(file)) {
            self.collapsed_dirs.remove(&ancestor);
        }
    }

    pub fn toggle_viewed(&mut self) {
        if let Some(file) = self.selected_file_mut() {
            file.viewed = !file.viewed;
        }
        self.ensure_selected_file_visible();
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
        self.ensure_selected_file_visible();
    }

    pub fn mark_all_viewed(&mut self) {
        for file in &mut self.files {
            file.viewed = true;
        }
        self.ensure_selected_file_visible();
    }

    pub fn mark_files_viewed_where(&mut self, mut predicate: impl FnMut(&ReviewFile) -> bool) {
        for file in &mut self.files {
            if predicate(file) {
                file.viewed = true;
            }
        }
    }

    /// Incremental re-review against a prior snapshot of the same target:
    /// files whose diff fingerprint is unchanged are marked viewed, files
    /// that changed (or are new) since the snapshot are marked unviewed.
    ///
    /// `prior_fingerprints` maps file path to the diff fingerprint the file
    /// had at the prior snapshot. Returns `(unchanged, changed)` counts.
    pub fn apply_incremental_review(
        &mut self,
        prior_fingerprints: &BTreeMap<String, String>,
    ) -> (usize, usize) {
        let mut unchanged = 0;
        let mut changed = 0;
        for file in &mut self.files {
            if prior_fingerprints.get(&file.path) == Some(&file.fingerprint) {
                file.viewed = true;
                unchanged += 1;
            } else {
                file.viewed = false;
                changed += 1;
            }
        }
        self.ensure_selected_file_visible();
        (unchanged, changed)
    }

    pub fn annotate_generated_where(&mut self, mut predicate: impl FnMut(&ReviewFile) -> bool) {
        for file in &mut self.files {
            file.generated = predicate(file);
        }
        self.ensure_selected_file_visible();
    }

    fn file_visible(&self, file: &ReviewFile) -> bool {
        (!self.hide_generated || !file.generated) && self.viewed_filter.admits(file.viewed)
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

    /// Scroll near the end of the diff instead of past it, and keep the cursor
    /// on the last commentable row so diff-focus actions still make sense.
    pub fn scroll_diff_to_bottom(&mut self) {
        let rows = self.diff_rows_for_selected_file();
        if rows.is_empty() {
            self.diff_scroll = 0;
            return;
        }
        if self.focus == Focus::Diff
            && let Some(last_anchor) = rows.iter().rposition(|row| row.anchor.is_some())
        {
            self.diff_cursor = last_anchor;
        }
        self.diff_scroll = rows
            .len()
            .saturating_sub(DIFF_CURSOR_SCROLL_MARGIN)
            .min(u16::MAX as usize) as u16;
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
        } else if self.diff_cursor > self.diff_scroll as usize + DIFF_CURSOR_SCROLL_MARGIN {
            self.diff_scroll = self.diff_cursor.saturating_sub(DIFF_CURSOR_SCROLL_MARGIN) as u16;
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

    pub fn selected_line_anchor(&self) -> Option<CommentAnchor> {
        self.diff_rows_for_selected_file()
            .get(self.diff_cursor)
            .and_then(|row| row.anchor.clone())
    }

    /// Changed symbols in the selected file, resolved to diff row indices.
    pub fn changed_symbol_targets(&self) -> Vec<ChangedSymbolTarget> {
        let Some(file) = self.selected_visible_file() else {
            return Vec::new();
        };
        let (new_source, new_line_indices) = syntax_source(file, SyntaxSide::New);
        let (old_source, old_line_indices) = syntax_source(file, SyntaxSide::Old);
        let cache = self.syntax_cache_for_file(
            file,
            &new_source,
            &new_line_indices,
            &old_source,
            &old_line_indices,
        );
        let rows = self.diff_rows_for_selected_file();
        cache
            .changed_symbols
            .iter()
            .filter_map(|symbol| {
                let row_index = rows.iter().position(|row| {
                    matches!(
                        &row.anchor,
                        Some(CommentAnchor::Line { hunk_index, line_index, .. })
                            if *hunk_index == symbol.hunk_index
                                && *line_index == symbol.line_index
                    )
                })?;
                Some(ChangedSymbolTarget {
                    label: format!("{} {}", symbol.kind, symbol.name),
                    row_index,
                })
            })
            .collect()
    }

    /// Cycle the diff cursor between changed symbols (wrapping).
    pub fn jump_to_changed_symbol(&mut self, delta: isize) {
        let targets = self.changed_symbol_targets();
        if targets.is_empty() {
            return;
        }
        let at_diff_cursor = self.focus == Focus::Diff;
        let current = self.diff_cursor;
        let next = if delta.is_negative() {
            targets
                .iter()
                .rev()
                .find(|target| !at_diff_cursor || target.row_index < current)
                .or_else(|| targets.last())
        } else {
            targets
                .iter()
                .find(|target| !at_diff_cursor || target.row_index > current)
                .or_else(|| targets.first())
        };
        if let Some(target) = next {
            self.jump_to_diff_row(target.row_index);
        }
    }

    /// Move to a diff row (from outline/symbol navigation), focusing the diff
    /// pane and scrolling the row into view.
    pub fn jump_to_diff_row(&mut self, row_index: usize) {
        self.select_diff_row(row_index);
        self.diff_scroll = self.diff_cursor.saturating_sub(5) as u16;
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

    pub fn comments_for_diff_row_anchor_details(&self, anchor: &CommentAnchor) -> Vec<&Comment> {
        self.comments
            .iter()
            .filter(|comment| self.comment_matches_diff_row_anchor(comment, anchor))
            .collect()
    }

    fn comment_matches_diff_row_anchor(&self, comment: &Comment, anchor: &CommentAnchor) -> bool {
        let row_fingerprint = match anchor {
            CommentAnchor::Line {
                line_fingerprint, ..
            } => Some(line_fingerprint),
            _ => None,
        };
        match comment.anchor.as_ref() {
            Some(existing) if existing == anchor => true,
            Some(CommentAnchor::Range { lines, .. }) => {
                row_fingerprint.is_some_and(|fingerprint| {
                    lines
                        .iter()
                        .any(|line| &line.line_fingerprint == fingerprint)
                })
            }
            _ => false,
        }
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
        if let Some(selected_id) = &self.selected_comment_id
            && let Some(index) = self.comments.iter().position(|comment| {
                comment.id == *selected_id && self.comment_matches_current_selection(comment)
            })
        {
            return Some(index);
        }

        match self.focus {
            Focus::Diff => self.selected_line_anchor().and_then(|anchor| {
                self.comments
                    .iter()
                    .position(|comment| self.comment_matches_diff_row_anchor(comment, &anchor))
            }),
            Focus::Files => self.selected_file().and_then(|file| {
                self.comments.iter().position(|comment| {
                    comment.path == file.path
                        && matches!(comment.anchor, Some(CommentAnchor::File { .. }) | None)
                })
            }),
        }
    }

    fn comment_matches_current_selection(&self, comment: &Comment) -> bool {
        match self.focus {
            Focus::Diff => self
                .selected_line_anchor()
                .is_some_and(|anchor| self.comment_matches_diff_row_anchor(comment, &anchor)),
            Focus::Files => self.selected_file().is_some_and(|file| {
                comment.path == file.path
                    && matches!(comment.anchor, Some(CommentAnchor::File { .. }) | None)
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

    /// Advance a comment's state (draft -> todo -> resolved -> draft).
    pub fn cycle_comment_state(&mut self, id: &str) -> Option<CommentState> {
        let comment = self.comments.iter_mut().find(|comment| comment.id == id)?;
        comment.state = comment.state.next();
        Some(comment.state)
    }

    /// Select a comment by id, moving the file/diff cursors to its anchor.
    pub fn select_comment_by_id(&mut self, id: &str) {
        if let Some(index) = self.comments.iter().position(|comment| comment.id == id) {
            self.select_comment(index);
        }
    }

    pub fn delete_comment(&mut self, id: &str) -> bool {
        let Some(index) = self.comments.iter().position(|comment| comment.id == id) else {
            return false;
        };
        self.comments.remove(index);
        if self.selected_comment_id.as_deref() == Some(id) {
            self.selected_comment_id = None;
        }
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
            // UUIDs keep comment ids collision-free across delete/re-add cycles
            // and across sessions/people, which matters because artifact import
            // dedupes comments by id.
            id: uuid::Uuid::new_v4().to_string(),
            path: anchor.path().to_owned(),
            line: anchor.line(),
            end_line: anchor
                .end_line()
                .filter(|end_line| Some(*end_line) != anchor.line()),
            anchor: Some(anchor),
            body,
            state: CommentState::default(),
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
        self.selected_comment_id = Some(comment.id.clone());
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
            Some(CommentAnchor::Range { lines, .. }) => {
                self.focus = Focus::Diff;
                if let Some(row_index) = self.diff_rows_for_selected_file().iter().position(|row| {
                    row.anchor.as_ref().is_some_and(|anchor| {
                        matches!(
                            anchor,
                            CommentAnchor::Line { line_fingerprint, .. }
                                if lines.iter().any(|line| &line.line_fingerprint == line_fingerprint)
                        )
                    })
                }) {
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

    pub fn into_state(self) -> ReviewState {
        self.to_state()
    }

    /// Snapshot the persistable review state (viewed marks and comments)
    /// without consuming the session, so the TUI can autosave mid-session.
    pub fn to_state(&self) -> ReviewState {
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
    fn incremental_review_marks_unchanged_viewed_and_changed_unviewed() {
        let mut session = three_file_session();
        session.files[0].viewed = true;
        session.files[2].viewed = true;

        let mut prior = BTreeMap::new();
        // a.rs unchanged since the prior snapshot, b.rs changed, c.rs is new.
        prior.insert("src/a.rs".to_owned(), session.files[0].fingerprint.clone());
        prior.insert("src/b.rs".to_owned(), "different".to_owned());

        let (unchanged, changed) = session.apply_incremental_review(&prior);

        assert_eq!((unchanged, changed), (1, 2));
        assert!(session.files[0].viewed);
        assert!(!session.files[1].viewed);
        assert!(!session.files[2].viewed);
    }

    #[test]
    fn large_diffs_render_placeholder_until_expanded() {
        let mut body = String::from(
            "diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1,4 +1,4 @@\n",
        );
        for index in 1..=4 {
            body.push_str(&format!(" line {index}\n"));
        }
        let diff = DiffSet::parse(&body).unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.max_diff_lines = 3;

        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[1].kind, DiffRowKind::Placeholder));
        assert!(rows[1].text.contains("4 lines exceed the 3 line threshold"));

        session.toggle_large_diff_render();
        let rows = session.diff_rows_for_selected_file();
        assert!(rows.len() > 2);
        assert!(!rows.iter().any(|row| row.kind == DiffRowKind::Placeholder));

        session.toggle_large_diff_render();
        let rows = session.diff_rows_for_selected_file();
        assert!(matches!(rows[1].kind, DiffRowKind::Placeholder));
    }

    #[test]
    fn binary_files_render_placeholder_and_stay_viewable() {
        let diff = DiffSet::parse(
            "diff --git a/logo.png b/logo.png\nBinary files a/logo.png and b/logo.png differ\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );

        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[1].kind, DiffRowKind::Placeholder));
        assert!(rows[1].text.contains("binary file"));

        session.toggle_viewed();
        assert!(session.selected_file().unwrap().viewed);
    }

    #[test]
    fn agent_ordering_flattens_tree_in_priority_order() {
        let mut session = three_file_session();
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            ordering: vec!["src/c.rs".to_owned(), "src/a.rs".to_owned()],
            ..Default::default()
        });

        let tree = session.file_tree();
        let labels: Vec<&str> = tree.rows.iter().map(|row| row.label.as_str()).collect();
        // Listed files first in agent order; unlisted files after, flat list.
        assert_eq!(labels, ["src/c.rs", "src/a.rs", "src/b.rs"]);
        assert!(
            tree.rows
                .iter()
                .all(|row| matches!(row.kind, FlatTreeRowKind::File { .. }))
        );

        // Navigation follows the agent order too: after the currently
        // selected src/a.rs, the next unviewed file in agent order is b.
        session.move_to_unviewed(1);
        assert_eq!(session.selected_file().unwrap().path, "src/b.rs");

        // Toggling off restores the nested directory tree.
        session.toggle_agent_order();
        assert!(!session.agent_order_active());
        let tree = session.file_tree();
        assert_eq!(tree.rows[0].label, "src");
    }

    #[test]
    fn agent_ordering_respects_visibility_filters() {
        let mut session = three_file_session();
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            ordering: vec!["src/b.rs".to_owned()],
            ..Default::default()
        });
        session.files[1].viewed = true;
        session.viewed_filter = ViewedFilter::Unviewed;

        let tree = session.file_tree();
        let labels: Vec<&str> = tree.rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["src/a.rs", "src/c.rs"]);
    }

    #[test]
    fn agent_flags_mark_rows_and_jump_to_lines() {
        let mut session = multi_line_session();
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            flags: vec![
                crate::agent::AgentFlag {
                    id: "line-flag".to_owned(),
                    path: "src/app.rs".to_owned(),
                    line: Some(2),
                    reason: "risky call".to_owned(),
                    priority: crate::agent::FlagPriority::Critical,
                },
                crate::agent::AgentFlag {
                    id: "file-flag".to_owned(),
                    path: "src/app.rs".to_owned(),
                    line: None,
                    reason: "whole file".to_owned(),
                    priority: crate::agent::FlagPriority::Low,
                },
            ],
            ..Default::default()
        });

        assert!(session.file_has_flags("src/app.rs"));
        assert!(!session.file_has_flags("other.rs"));

        let rows = session.diff_rows_for_selected_file();
        let flagged: Vec<usize> = (0..rows.len())
            .filter(|index| session.diff_row_flagged(&rows[*index]))
            .collect();
        // File header (file-level flag) plus the row for new line 2.
        assert!(flagged.contains(&0));
        assert!(
            flagged
                .iter()
                .any(|index| rows[*index].new_lineno == Some(2))
        );

        let flag = session.flags_sorted()[0].clone();
        assert_eq!(flag.id, "line-flag");
        session.jump_to_flag(&flag);
        assert_eq!(session.focus, Focus::Diff);
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_cursor].new_lineno, Some(2));
    }

    #[test]
    fn jump_to_chunk_part_moves_cursor_to_start_line() {
        let mut session = multi_line_session();
        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "core".to_owned(),
                rationale: None,
                parts: vec![crate::agent::ChunkPart {
                    path: "src/app.rs".to_owned(),
                    start_line: Some(3),
                    end_line: Some(4),
                }],
            }],
            ..Default::default()
        });

        let part = session.review_chunks[0].parts[0].clone();
        session.jump_to_chunk_part(&part);

        assert_eq!(session.focus, Focus::Diff);
        let rows = session.diff_rows_for_selected_file();
        assert!(rows[session.diff_cursor].new_lineno.unwrap() >= 3);
    }

    #[test]
    fn large_change_nudge_triggers_on_thresholds_only() {
        let mut session = session();

        // Small change under both thresholds: no nudge.
        assert!(session.large_change_nudge().is_none());

        // Lower the line threshold under the session's changed-line count.
        session.nudge_diff_lines = 1;
        let nudge = session.large_change_nudge().unwrap();
        assert!(nudge.contains("large change"));
        assert!(nudge.contains('@'));

        // 0 disables the line criterion; the file criterion still applies.
        session.nudge_diff_lines = 0;
        assert!(session.large_change_nudge().is_none());
        session.nudge_files = 1;
        assert!(session.large_change_nudge().is_some());
    }

    #[test]
    fn large_change_nudge_suppressed_once_an_agent_organized_the_review() {
        let mut session = session();
        session.nudge_files = 1;
        assert!(session.large_change_nudge().is_some());

        session.apply_agent_overlay(&crate::agent::AgentOverlay {
            chunks: vec![crate::agent::ReviewChunk {
                id: "c1".to_owned(),
                title: "core".to_owned(),
                rationale: None,
                parts: Vec::new(),
            }],
            ..Default::default()
        });

        assert!(session.large_change_nudge().is_none());
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
    fn diff_rows_are_memoized_per_file_fingerprint() {
        let session = rust_syntax_session();

        let first = session.diff_rows_for_selected_file();
        let second = session.diff_rows_for_selected_file();

        assert!(std::rc::Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn diff_rows_cache_tracks_syntax_config_changes() {
        let mut session = rust_syntax_session();

        let before = session.diff_rows_for_selected_file();
        session.syntax = SyntaxConfig {
            enabled: false,
            ..SyntaxConfig::default()
        };
        let after = session.diff_rows_for_selected_file();

        assert!(!std::rc::Rc::ptr_eq(&before, &after));
        assert!(
            !after
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
    fn comments_for_diff_row_anchor_matches_existing_line_comment() {
        let mut session = session();
        session.toggle_focus();
        let anchor = session.selected_line_anchor().unwrap();

        session.add_comment("Line note".into());

        assert_eq!(
            session.comments_for_diff_row_anchor_details(&anchor).len(),
            1
        );
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
    fn comment_ids_stay_unique_across_delete_and_readd() {
        let mut session = session();
        session.add_comment("first".into());
        let first_id = session.comments[0].id.clone();
        assert!(session.delete_comment(&first_id));

        session.add_comment("second".into());

        assert_ne!(session.comments[0].id, first_id);
        assert!(!session.comments[0].id.is_empty());
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
    fn scroll_diff_to_bottom_stays_within_content() {
        let mut session = multi_line_session();
        session.toggle_focus();

        session.scroll_diff_to_bottom();

        let rows = session.diff_rows_for_selected_file();
        assert!((session.diff_scroll as usize) < rows.len());
        assert!(rows[session.diff_cursor].anchor.is_some());
        assert_eq!(
            session.diff_cursor,
            rows.iter().rposition(|row| row.anchor.is_some()).unwrap()
        );
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
    fn move_to_comment_cycles_comments_on_same_anchor() {
        let mut session = session();
        session.toggle_focus();
        session.add_comment("First".into());
        session.add_comment("Second".into());

        session.move_to_comment(1);
        assert_eq!(session.selected_comment().unwrap().body, "Second");

        session.move_to_comment(1);
        assert_eq!(session.selected_comment().unwrap().body, "First");
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
    fn generated_files_are_grouped_when_visible() {
        let mut session = session();
        session.files[1].generated = true;

        let labels: Vec<_> = session
            .file_tree()
            .rows
            .into_iter()
            .map(|row| (row.depth, row.label))
            .collect();

        assert_eq!(labels[0], (0, "generated/noisy".to_owned()));
        assert!(labels.contains(&(1, "README.md".to_owned())));
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
    fn unviewed_filter_hides_viewed_files_and_moves_selection() {
        let mut session = three_file_session();
        session.files[0].viewed = true;

        session.cycle_viewed_filter();

        assert_eq!(session.viewed_filter, ViewedFilter::Unviewed);
        assert_eq!(session.selected_file().unwrap().path, "src/b.rs");
        assert!(session.file_tree().rows.iter().all(|row| {
            !matches!(row.kind, FlatTreeRowKind::File { file_index } if file_index == 0)
        }));
    }

    #[test]
    fn viewed_filter_shows_only_viewed_files() {
        let mut session = three_file_session();
        session.files[2].viewed = true;

        session.cycle_viewed_filter();
        session.cycle_viewed_filter();

        assert_eq!(session.viewed_filter, ViewedFilter::Viewed);
        assert_eq!(session.selected_file().unwrap().path, "src/c.rs");
        let file_rows = session
            .file_tree()
            .rows
            .iter()
            .filter(|row| matches!(row.kind, FlatTreeRowKind::File { .. }))
            .count();
        assert_eq!(file_rows, 1);
    }

    #[test]
    fn viewed_filter_cycles_back_to_all() {
        let mut session = session();

        session.cycle_viewed_filter();
        session.cycle_viewed_filter();
        session.cycle_viewed_filter();

        assert_eq!(session.viewed_filter, ViewedFilter::All);
    }

    #[test]
    fn marking_viewed_under_unviewed_filter_keeps_selection_visible() {
        let mut session = three_file_session();
        session.cycle_viewed_filter();

        session.mark_selected_viewed();

        assert_eq!(session.viewed_filter, ViewedFilter::Unviewed);
        let selected = session.selected_visible_file().unwrap();
        assert!(!selected.viewed);
    }

    #[test]
    fn unviewed_filter_with_everything_viewed_clears_selection() {
        let mut session = session();
        session.cycle_viewed_filter();

        session.mark_all_viewed();

        assert!(session.selected_visible_file().is_none());
        assert!(session.file_tree().rows.is_empty());
    }

    #[test]
    fn changed_symbol_targets_resolve_added_lines_to_rows() {
        let session = rust_syntax_session();

        let targets = session.changed_symbol_targets();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].label, "fn new");
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[targets[0].row_index].text, "fn new() {");
    }

    #[test]
    fn jump_to_changed_symbol_cycles_between_symbols() {
        let diff = DiffSet::parse(
            r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,8 +1,8 @@
 fn alpha() {
-    old_alpha();
+    new_alpha();
 }
 
 fn beta() {
-    old_beta();
+    new_beta();
 }
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );

        session.jump_to_changed_symbol(1);
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(session.focus, Focus::Diff);
        assert_eq!(rows[session.diff_cursor].text.trim(), "new_alpha();");

        session.jump_to_changed_symbol(1);
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_cursor].text.trim(), "new_beta();");

        // Wraps back to the first symbol.
        session.jump_to_changed_symbol(1);
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_cursor].text.trim(), "new_alpha();");

        session.jump_to_changed_symbol(-1);
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_cursor].text.trim(), "new_beta();");
    }

    #[test]
    fn changed_symbol_targets_empty_for_unsupported_files() {
        let session = session();
        let mut session = session;
        session.move_selection(1);
        assert_eq!(session.selected_file().unwrap().path, "README.md");

        // Markdown has no symbol declarations in these hunks.
        assert!(session.changed_symbol_targets().is_empty());
    }

    #[test]
    fn context_folding_collapses_long_context_runs_with_symbol_labels() {
        let diff = DiffSet::parse(
            r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,12 +1,12 @@
-fn changed() {
+fn renamed() {
     one();
     two();
     three();
     four();
     five();
     six();
     seven();
     eight();
     nine();
-    old_tail();
+    new_tail();
 }
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );

        let unfolded_len = session.diff_rows_for_selected_file().len();
        session.toggle_context_fold();
        let rows = session.diff_rows_for_selected_file();

        assert!(rows.len() < unfolded_len);
        let fold = rows
            .iter()
            .find(|row| matches!(row.kind, DiffRowKind::ContextFold))
            .expect("expected a context fold row");
        assert_eq!(fold.text, "⋯ 5 unchanged lines (in fn renamed)");
        assert!(fold.anchor.is_none());
        // Kept context around the fold survives.
        assert!(rows.iter().any(|row| row.text.trim() == "two();"));
        assert!(rows.iter().any(|row| row.text.trim() == "eight();"));
        assert!(!rows.iter().any(|row| row.text.trim() == "five();"));

        session.toggle_context_fold();
        assert_eq!(session.diff_rows_for_selected_file().len(), unfolded_len);
    }

    #[test]
    fn short_context_runs_do_not_fold() {
        let mut session = multi_line_session();
        let unfolded_len = session.diff_rows_for_selected_file().len();

        session.toggle_context_fold();

        assert_eq!(session.diff_rows_for_selected_file().len(), unfolded_len);
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
