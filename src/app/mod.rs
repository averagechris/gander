//! Review session domain state: file selection, tree folding, viewed marks,
//! comments, and viewport bookkeeping.
//!
//! Submodules:
//! - [`context_expansion`]: per-gap hunk context expansion state and math
//! - [`diff_rows`]: flattened diff-row construction for the diff pane
//! - [`syntax_cache`]: per-file tree-sitter highlight caching

mod context_expansion;
mod diff_rows;
mod split_rows;
mod syntax_cache;
mod word_diff;

pub use context_expansion::Expansion;
pub use diff_rows::{DiffRow, DiffRowKind};
pub use split_rows::{SplitRow, split_index_of, split_rows};

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    rc::Rc,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    agent::{AgentDraft, AgentFlag, AgentOverlay, ChangeBrief, ChunkPart, DraftState, ReviewChunk},
    anchor::{CommentAnchor, RangeLineAnchor, comment_anchor_for_file_lines, fingerprint_range},
    config::{Config, DiffConfig, DiffViewModeConfig, LimitsConfig},
    diff::{DiffSet, FileDiff, FileStatus},
    file_tree::{FileTreeInput, FileTreeView, FlatTreeRowKind, TreeRowId},
    jj::ReviewTarget,
    review,
    state::{
        Comment, CommentState, FileState, REVIEW_STATE_SCHEMA_VERSION, ReviewSessionStatus,
        ReviewState, ReviewStateMeta, ReviewTarget as StateReviewTarget, StepArtifact,
        StepArtifactKind, StepImportance, StepKind, Walkthrough, WalkthroughStep,
    },
    syntax::SyntaxConfig,
};

use diff_rows::nearest_commentable_row;
use syntax_cache::{SyntaxCacheKey, SyntaxFileCache, SyntaxSide, syntax_source};

/// Memoized diff rows keyed by file/syntax-config cache key plus the
/// context-fold, force-render-large, and word-highlight flags and the
/// context-expansion epoch (bumped whenever expansion state or fetched file
/// contents change).
type DiffRowsCache = BTreeMap<(SyntaxCacheKey, bool, bool, bool, u64), Rc<Vec<DiffRow>>>;

const GENERATED_TREE_GROUP: &str = "generated/noisy";

/// Rough number of diff rows kept visible below the cursor when auto-scrolling.
const DIFF_CURSOR_SCROLL_MARGIN: usize = 15;

fn chunk_to_step(chunk: &ReviewChunk) -> WalkthroughStep {
    let mut targets = chunk.parts.iter().map(chunk_part_to_target);
    WalkthroughStep {
        id: chunk.id.clone(),
        title: Some(chunk.title.clone()),
        importance: match chunk.importance {
            crate::agent::ChunkImportance::Spotlight => StepImportance::Spotlight,
            crate::agent::ChunkImportance::Glance => StepImportance::Glance,
        },
        kind: StepKind::Step,
        change_id: chunk.change_id.clone(),
        why: chunk.rationale.clone(),
        body: chunk.explanation.clone(),
        artifacts: chunk.artifacts.iter().map(agent_artifact_to_step).collect(),
        target: targets.next().unwrap_or_default(),
        extra_targets: targets.collect(),
        ..Default::default()
    }
}

fn brief_to_step(brief: &ChangeBrief) -> WalkthroughStep {
    WalkthroughStep {
        id: format!("chapter-{}", brief.change_id),
        title: Some(brief.change_id.clone()),
        kind: StepKind::Chapter,
        change_id: Some(brief.change_id.clone()),
        body: Some(brief.summary.clone()),
        artifacts: brief.artifacts.iter().map(agent_artifact_to_step).collect(),
        ..Default::default()
    }
}

fn chunk_part_to_target(part: &ChunkPart) -> StateReviewTarget {
    StateReviewTarget {
        file: Some(part.path.clone()),
        line: part.start_line,
        end_line: part.end_line,
        ..Default::default()
    }
}

fn agent_artifact_to_step(artifact: &crate::agent::Artifact) -> StepArtifact {
    StepArtifact {
        title: artifact.title.clone(),
        kind: match artifact.kind {
            crate::agent::ArtifactKind::Example => StepArtifactKind::Example,
            crate::agent::ArtifactKind::Output => StepArtifactKind::Output,
            crate::agent::ArtifactKind::Diagram => StepArtifactKind::Diagram,
            crate::agent::ArtifactKind::Note => StepArtifactKind::Note,
        },
        body: artifact.body.clone(),
    }
}

#[derive(Debug, Clone)]
pub struct ReviewSession {
    pub repo: PathBuf,
    pub target: ReviewTarget,
    pub files: Vec<ReviewFile>,
    persisted_files: BTreeMap<String, FileState>,
    pub comments: Vec<Comment>,
    pub sessions: Vec<crate::state::ReviewSession>,
    /// Configured lifecycle state for newly accepted durable comments.
    pub comment_initial_state: CommentState,
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
    /// Diff visual cues (word-level highlights, line backgrounds, gutter
    /// bar) plus their style specs. Runtime-toggleable, session-only.
    pub diff_cues: DiffConfig,
    /// Whether the files pane is shown. Session-only; hiding it gives the
    /// diff the full width for focused reading (docs/focused-diff-ux.md §2).
    pub file_pane_visible: bool,
    /// The zen-mode focus frame: the current stop's file and optional line
    /// range. Diff rows outside it render dimmed so the stop visually pops.
    /// Session-only view state owned by the TUI zen layer
    /// (docs/focused-diff-ux.md §6); reset on retarget like all view state.
    pub zen_focus: Option<ZenFocus>,
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
    /// Agent-written per-change briefings for stacked walkthroughs.
    pub change_briefs: Vec<ChangeBrief>,
    /// Lazily populated jj diffs for each stack change, keyed by change id.
    /// Zen fallback uses these so per-change chapter facts come from that
    /// change's own parent diff instead of the whole reviewed range.
    pub change_diffs: Vec<(String, DiffSet)>,
    /// Agent-drafted comments with their dispositions.
    pub agent_drafts: Vec<AgentDraft>,
    selected_comment_id: Option<String>,
    /// Per-gap context expansion state, keyed by `(path, gap id)`.
    /// Session-only; joins the rows-cache key via [`Self::expansion_epoch`]
    /// (docs/focused-diff-ux.md §5).
    context_expansion: BTreeMap<(String, usize), Expansion>,
    /// Lazily fetched full file contents (new side) per path, split into
    /// lines. `None` records a failed fetch (deleted/binary file) so gap
    /// rows stop offering expansion.
    file_contents: BTreeMap<String, Option<Rc<Vec<String>>>>,
    /// Bumped whenever expansion state or fetched contents change so cached
    /// diff rows rebuild.
    expansion_epoch: u64,
    syntax_cache: RefCell<BTreeMap<SyntaxCacheKey, SyntaxFileCache>>,
    /// Memoized diff rows per file (same key as the syntax cache plus the
    /// context-fold flag), rebuilt only when the diff fingerprint, syntax
    /// config, or fold mode changes.
    rows_cache: RefCell<DiffRowsCache>,
    viewport_by_path: BTreeMap<String, FileViewport>,
    tree_cursor: Option<TreeRowId>,
    /// Files whose content changed in the most recent in-place refresh —
    /// this refresh only, unlike the accumulated `changed_since_look`
    /// marks. Session-only; feeds the watch activity events.
    pub last_refresh_changes: Vec<RefreshedFileChange>,
}

/// One file's content change detected by the most recent in-place refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshedFileChange {
    pub path: String,
    /// The path was not present before this refresh.
    pub is_new: bool,
    pub additions_delta: i64,
    pub deletions_delta: i64,
    /// This file had already been reviewed (viewed or caught up) before the
    /// refresh changed its fingerprint, so the new diff needs re-review.
    pub was_reviewed: bool,
    /// The refreshed fingerprint matches a previously viewed/caught-up
    /// fingerprint, so the apparent change is a revert to known content.
    pub reverted_to_seen: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FileViewport {
    diff_scroll: u16,
    diff_cursor: usize,
}

struct ReviewSessionOptions {
    syntax: SyntaxConfig,
    limits: LimitsConfig,
    diff_cues: DiffConfig,
    comment_initial_state: CommentState,
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
    fn admits(self, done: bool) -> bool {
        match self {
            Self::All => true,
            Self::Unviewed => !done,
            Self::Viewed => done,
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

/// What zen mode is currently framing: a file, optionally narrowed to a
/// line range (new-side line numbers, matching [`crate::agent::ChunkPart`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZenFocus {
    pub path: String,
    pub lines: Option<(usize, usize)>,
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
    #[serde(default)]
    pub caught_up: bool,
    #[serde(default)]
    pub changed_since_look: bool,
    /// Fingerprint the file had when `changed_since_look` was first raised.
    /// If a later refresh returns to this baseline, the freshness badge clears
    /// even when the file was never explicitly viewed.
    #[serde(default)]
    pub changed_since_look_baseline: Option<String>,
    #[serde(default)]
    pub viewed_stale: bool,
    #[serde(default)]
    pub changed_hunks: BTreeSet<usize>,
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
        Self::new_with_options(
            repo,
            target,
            diff,
            state,
            ReviewSessionOptions {
                syntax: config.syntax.clone(),
                limits: config.limits.clone(),
                diff_cues: config.diff.clone(),
                comment_initial_state: config.comments.initial_state.into(),
            },
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
        Self::new_with_options(
            repo,
            target,
            diff,
            state,
            ReviewSessionOptions {
                syntax,
                limits: LimitsConfig::default(),
                diff_cues: DiffConfig::default(),
                comment_initial_state: CommentState::Draft,
            },
        )
    }

    fn new_with_options(
        repo: PathBuf,
        target: ReviewTarget,
        diff: DiffSet,
        state: ReviewState,
        options: ReviewSessionOptions,
    ) -> Self {
        let ReviewSessionOptions {
            syntax,
            limits,
            diff_cues,
            comment_initial_state,
        } = options;
        let mut state = state;
        state.normalize_legacy_file_state();
        let ReviewState {
            files,
            comments,
            sessions,
            ..
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
                    caught_up: false,
                    changed_since_look: false,
                    changed_since_look_baseline: None,
                    viewed_stale: false,
                    changed_hunks: BTreeSet::new(),
                    fingerprint: file.fingerprint.clone(),
                    diff: file,
                })
                .collect(),
            persisted_files: files,
            comments,
            sessions,
            comment_initial_state,
            selected: 0,
            diff_scroll: 0,
            diff_cursor: 0,
            focus: Focus::Files,
            syntax,
            hide_generated: false,
            viewed_filter: ViewedFilter::default(),
            fold_context: false,
            diff_cues,
            file_pane_visible: true,
            zen_focus: None,
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
            change_briefs: Vec::new(),
            change_diffs: Vec::new(),
            agent_drafts: Vec::new(),
            selected_comment_id: None,
            context_expansion: BTreeMap::new(),
            file_contents: BTreeMap::new(),
            expansion_epoch: 0,
            syntax_cache: RefCell::new(BTreeMap::new()),
            rows_cache: RefCell::new(BTreeMap::new()),
            viewport_by_path: BTreeMap::new(),
            tree_cursor: None,
            last_refresh_changes: Vec::new(),
        };
        session.apply_state_files();
        session
    }

    pub fn replace_diff(&mut self, target: ReviewTarget, diff: DiffSet) {
        let mut state = self.to_state();
        state.meta = ReviewStateMeta::default();
        *self = Self::new_with_options(
            self.repo.clone(),
            target,
            diff,
            state,
            ReviewSessionOptions {
                syntax: self.syntax.clone(),
                limits: LimitsConfig {
                    max_diff_lines: self.max_diff_lines,
                    nudge_diff_lines: self.nudge_diff_lines,
                    nudge_files: self.nudge_files,
                },
                diff_cues: self.diff_cues.clone(),
                comment_initial_state: self.comment_initial_state,
            },
        );
        self.refresh_comment_anchors_for_current_diff();
    }

    /// Like [`Self::replace_diff`], but for background refreshes of the
    /// *same* review: view state (pane visibility, focus, filters, folds,
    /// per-file viewports, the selected file, and the zen focus frame)
    /// carries over so the reload does not yank the reviewer around.
    pub fn replace_diff_preserving_view(&mut self, target: ReviewTarget, diff: DiffSet) {
        let file_pane_visible = self.file_pane_visible;
        let focus = self.focus;
        let hide_generated = self.hide_generated;
        let viewed_filter = self.viewed_filter;
        let fold_context = self.fold_context;
        let use_agent_order = self.use_agent_order;
        let collapsed_dirs = std::mem::take(&mut self.collapsed_dirs);
        let force_rendered = std::mem::take(&mut self.force_rendered);
        let viewports = std::mem::take(&mut self.viewport_by_path);
        let zen_focus = self.zen_focus.take();
        let selected_path = self.selected_file().map(|file| file.path.clone());
        struct PreviousFile {
            fingerprint: String,
            changed_since_look: bool,
            changed_since_look_baseline: Option<String>,
            changed_hunks: BTreeSet<usize>,
            hunk_fingerprints: Vec<String>,
            additions: usize,
            deletions: usize,
            viewed: bool,
            caught_up: bool,
        }
        let old_files: BTreeMap<String, PreviousFile> = self
            .files
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    PreviousFile {
                        fingerprint: file.fingerprint.clone(),
                        changed_since_look: file.changed_since_look,
                        changed_since_look_baseline: file.changed_since_look_baseline.clone(),
                        changed_hunks: file.changed_hunks.clone(),
                        hunk_fingerprints: file
                            .diff
                            .hunks
                            .iter()
                            .map(|hunk| hunk.content_fingerprint())
                            .collect(),
                        additions: file.diff.additions,
                        deletions: file.diff.deletions,
                        viewed: file.viewed,
                        caught_up: file.caught_up,
                    },
                )
            })
            .collect();

        self.replace_diff(target, diff);

        let mut refresh_changes = Vec::new();
        for file in &mut self.files {
            let new_hunks: Vec<String> = file
                .diff
                .hunks
                .iter()
                .map(|hunk| hunk.content_fingerprint())
                .collect();
            let previous = old_files.get(&file.path);
            let file_changed = previous.is_none_or(|old| old.fingerprint != file.fingerprint);
            let carried_file = previous.is_some_and(|old| old.changed_since_look);
            let baseline = previous.and_then(|old| {
                old.changed_since_look_baseline
                    .clone()
                    .or_else(|| old.changed_since_look.then(|| old.fingerprint.clone()))
            });
            let carried_hunks = previous
                .map(|old| old.changed_hunks.clone())
                .unwrap_or_default();
            let old_hunks = previous
                .map(|old| old.hunk_fingerprints.clone())
                .unwrap_or_default();
            if file_changed {
                let reverted_to_baseline = baseline.as_ref() == Some(&file.fingerprint);
                let reverted_to_seen = previous.is_some()
                    && (file.viewed
                        || file.caught_up
                        || reverted_to_baseline
                        || self.persisted_files.get(&file.path).is_some_and(|saved| {
                            saved.is_viewed_fingerprint(&file.fingerprint)
                                || saved.is_caught_up_fingerprint(&file.fingerprint)
                        }));
                refresh_changes.push(RefreshedFileChange {
                    path: file.path.clone(),
                    is_new: previous.is_none(),
                    additions_delta: file.diff.additions as i64
                        - previous.map_or(0, |old| old.additions as i64),
                    deletions_delta: file.diff.deletions as i64
                        - previous.map_or(0, |old| old.deletions as i64),
                    was_reviewed: previous.is_some_and(|old| old.viewed || old.caught_up)
                        && !reverted_to_seen,
                    reverted_to_seen,
                });
            }
            let reverted_to_baseline = baseline.as_ref() == Some(&file.fingerprint);
            let reverted_to_seen = file_changed
                && previous.is_some()
                && (file.viewed
                    || file.caught_up
                    || reverted_to_baseline
                    || self.persisted_files.get(&file.path).is_some_and(|saved| {
                        saved.is_viewed_fingerprint(&file.fingerprint)
                            || saved.is_caught_up_fingerprint(&file.fingerprint)
                    }));
            file.changed_since_look = (carried_file || file_changed) && !reverted_to_seen;
            file.changed_since_look_baseline = if file.changed_since_look {
                baseline.or_else(|| previous.map(|old| old.fingerprint.clone()))
            } else {
                None
            };
            file.changed_hunks = carried_hunks;
            for (index, fingerprint) in new_hunks.iter().enumerate() {
                if !old_hunks.iter().any(|old| old == fingerprint) {
                    file.changed_hunks.insert(index);
                }
            }
        }
        self.last_refresh_changes = refresh_changes;

        self.file_pane_visible = file_pane_visible;
        self.hide_generated = hide_generated;
        self.viewed_filter = viewed_filter;
        self.fold_context = fold_context;
        self.use_agent_order = use_agent_order;
        self.collapsed_dirs = collapsed_dirs;
        self.force_rendered = force_rendered;
        self.viewport_by_path = viewports;
        if let Some(index) = selected_path
            .as_deref()
            .and_then(|path| self.files.iter().position(|file| file.path == path))
        {
            self.reveal_file_in_tree(index);
            self.tree_cursor = Some(TreeRowId::File { file_index: index });
            self.selected = index;
            self.restore_current_viewport();
        }
        self.focus = focus;
        if self.focus == Focus::Diff {
            // Row counts may have shifted; keep the cursor on a real row.
            let rows = self.diff_rows_for_selected_file().len();
            if self.diff_cursor >= rows {
                self.diff_cursor = rows.saturating_sub(1);
            }
            self.ensure_diff_cursor_commentable();
        }
        // The zen frame only survives when its file is still in the diff.
        self.zen_focus =
            zen_focus.filter(|focus| self.files.iter().any(|file| file.path == focus.path));
    }

    fn refresh_comment_anchors_for_current_diff(&mut self) {
        for comment in &mut self.comments {
            let Some(path) = comment.path.clone() else {
                continue;
            };
            let file = self
                .files
                .iter()
                .find(|file| file.path == path)
                .or_else(|| {
                    self.files.iter().find(|file| {
                        file.status == FileStatus::Renamed
                            && file.old_path.as_deref() == Some(path.as_str())
                    })
                });
            let Some(file) = file else {
                continue;
            };
            let Some(anchor) = comment_anchor_for_file_lines(file, comment.line, comment.end_line)
            else {
                continue;
            };
            comment.line = anchor.line();
            comment.end_line = anchor
                .end_line()
                .filter(|end_line| Some(*end_line) != anchor.line());
            comment.path = Some(file.path.clone());
            comment.anchor = Some(anchor);
        }
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

    fn apply_state_files(&mut self) {
        for file in &mut self.files {
            if let Some(saved) = self.persisted_files.get(&file.path) {
                file.viewed = saved.is_viewed_fingerprint(&file.fingerprint);
                file.caught_up = !file.viewed && saved.is_caught_up_fingerprint(&file.fingerprint);
                if file.caught_up {
                    file.changed_since_look = false;
                    file.changed_since_look_baseline = None;
                }
                file.viewed_stale =
                    !file.viewed && !file.caught_up && saved.has_any_viewed_fingerprint();
            }
        }
    }

    pub fn apply_review_state(&mut self, mut state: ReviewState) {
        state.normalize_legacy_file_state();
        self.persisted_files = state.files;
        self.comments = state.comments;
        self.sessions = state.sessions;
        self.apply_state_files();
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

    /// Toggle word-level emphasis of changed tokens within modified line
    /// pairs. Session-only; config sets the default.
    pub fn toggle_word_highlight(&mut self) {
        self.diff_cues.word_highlight = !self.diff_cues.word_highlight;
    }

    /// Toggle added/removed line background tints. Session-only.
    pub fn toggle_line_background(&mut self) {
        self.diff_cues.line_background = !self.diff_cues.line_background;
    }

    /// Toggle the colored gutter change bar. Session-only.
    pub fn toggle_gutter_bar(&mut self) {
        self.diff_cues.gutter_bar = !self.diff_cues.gutter_bar;
    }

    /// Toggle between the unified and side-by-side diff layouts.
    /// Session-only; config sets the default ([diff] view).
    pub fn toggle_diff_view(&mut self) {
        self.diff_cues.view = match self.diff_cues.view {
            DiffViewModeConfig::Unified => DiffViewModeConfig::SideBySide,
            DiffViewModeConfig::SideBySide => DiffViewModeConfig::Unified,
        };
    }

    /// Toggle the files pane. Hiding it moves focus to the diff so the
    /// keyboard keeps working on what is visible; focusing the files pane
    /// while hidden re-shows it (never trap the user).
    pub fn toggle_file_pane(&mut self) {
        self.file_pane_visible = !self.file_pane_visible;
        if !self.file_pane_visible && self.focus == Focus::Files {
            self.focus = Focus::Diff;
            self.clear_selected_freshness_marks();
            self.ensure_diff_cursor_commentable();
        }
    }

    pub fn move_to_unviewed(&mut self, delta: isize) {
        if let Some(candidate) = self.next_unviewed_index(delta, None) {
            self.select_file_index(candidate);
        }
    }

    /// Visible file paths in display order (agent order when active,
    /// tree order otherwise), ignoring directory fold state. Used by the
    /// zen walkthrough's chunkless fallback (docs/focused-diff-ux.md §6).
    pub fn ordered_visible_file_paths(&self) -> Vec<String> {
        self.full_file_tree()
            .rows
            .iter()
            .filter_map(|row| match row.kind {
                FlatTreeRowKind::Directory { .. } => None,
                FlatTreeRowKind::File { file_index } => {
                    self.files.get(file_index).map(|file| file.path.clone())
                }
            })
            .collect()
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

    pub(crate) fn select_file_index(&mut self, index: usize) {
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
        self.clear_selected_freshness_marks();
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
        self.change_briefs = overlay.briefs.clone();
        self.agent_drafts = overlay.drafts.clone();
        self.apply_overlay_as_walkthrough(overlay);
    }

    fn apply_overlay_as_walkthrough(&mut self, overlay: &AgentOverlay) {
        if overlay.chunks.is_empty() && overlay.briefs.is_empty() {
            return;
        }
        let target = StateReviewTarget {
            repo: Some(review::canonical_repo_identity(&self.repo)),
            base: Some(self.target.base.clone()),
            revision: Some(self.target.rev.clone()),
            ..Default::default()
        };
        let session = if let Some(index) = self.sessions.iter().position(|session| {
            session.status == ReviewSessionStatus::Open && session.target == target
        }) {
            &mut self.sessions[index]
        } else {
            self.sessions.push(crate::state::ReviewSession {
                id: uuid::Uuid::new_v4().to_string(),
                target,
                status: ReviewSessionStatus::Open,
                ..Default::default()
            });
            self.sessions.last_mut().unwrap()
        };
        let mut steps: Vec<WalkthroughStep> = overlay
            .briefs
            .iter()
            .map(brief_to_step)
            .chain(overlay.chunks.iter().map(chunk_to_step))
            .collect();
        if steps.is_empty() {
            return;
        }
        if session.walkthroughs.is_empty() {
            session.walkthroughs.push(Walkthrough {
                id: uuid::Uuid::new_v4().to_string(),
                title: Some("Walkthrough".to_owned()),
                ..Default::default()
            });
        }
        session.walkthroughs[0].steps.clear();
        session.walkthroughs[0].steps.append(&mut steps);
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
            "large change ({files} files, {lines} changed lines) — @ summons an agent to organize it, T starts a zen walkthrough"
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
                            viewed: usize::from(file.viewed || file.caught_up),
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
                viewed: file.viewed || file.caught_up,
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
            file.caught_up = false;
            file.changed_since_look = false;
            file.changed_since_look_baseline = None;
            file.changed_since_look_baseline = None;
            file.changed_hunks.clear();
            file.viewed_stale = false;
        }
        self.ensure_selected_file_visible();
    }

    pub fn mark_selected_viewed(&mut self) {
        let selected = self.selected;
        let next = self.next_unviewed_index(1, Some(selected));
        if let Some(file) = self.selected_file_mut() {
            file.viewed = true;
            file.caught_up = false;
            file.changed_since_look = false;
            file.changed_since_look_baseline = None;
            file.changed_hunks.clear();
            file.viewed_stale = false;
        }
        if let Some(next) = next {
            self.select_file_index(next);
        }
        self.ensure_selected_file_visible();
    }

    pub fn mark_all_viewed(&mut self) {
        for file in &mut self.files {
            file.viewed = true;
            file.caught_up = false;
            file.changed_since_look = false;
            file.changed_since_look_baseline = None;
            file.changed_hunks.clear();
            file.viewed_stale = false;
        }
        self.ensure_selected_file_visible();
    }

    pub fn mark_files_viewed_where(&mut self, mut predicate: impl FnMut(&ReviewFile) -> bool) {
        for file in &mut self.files {
            if predicate(file) {
                file.viewed = true;
                file.caught_up = false;
                file.changed_since_look = false;
                file.changed_since_look_baseline = None;
                file.changed_hunks.clear();
                file.viewed_stale = false;
            }
        }
        self.ensure_selected_file_visible();
    }

    /// Incremental re-review against a prior snapshot of the same target:
    /// files whose diff fingerprint is unchanged are marked caught-up unless
    /// already explicitly viewed; files
    /// that changed (or are new) since the snapshot are marked unviewed.
    ///
    /// `prior_fingerprints` maps file path to the diff fingerprint the file
    /// had at the prior snapshot. Returns `(caught_up, already_viewed, changed)` counts.
    pub fn apply_incremental_review(
        &mut self,
        prior_fingerprints: &BTreeMap<String, String>,
    ) -> (usize, usize, usize) {
        let mut caught_up = 0;
        let mut already_viewed = 0;
        let mut changed = 0;
        for file in &mut self.files {
            if prior_fingerprints.get(&file.path) == Some(&file.fingerprint) {
                if file.viewed {
                    file.caught_up = false;
                    file.changed_since_look = false;
                    file.changed_since_look_baseline = None;
                    already_viewed += 1;
                } else {
                    file.caught_up = true;
                    file.changed_since_look = false;
                    file.changed_since_look_baseline = None;
                    caught_up += 1;
                }
            } else {
                file.viewed = false;
                file.caught_up = false;
                changed += 1;
            }
        }
        self.ensure_selected_file_visible();
        (caught_up, already_viewed, changed)
    }

    pub fn annotate_generated_where(&mut self, mut predicate: impl FnMut(&ReviewFile) -> bool) {
        for file in &mut self.files {
            file.generated = predicate(file);
        }
        self.ensure_selected_file_visible();
    }

    fn file_visible(&self, file: &ReviewFile) -> bool {
        (!self.hide_generated || !file.generated)
            && self.viewed_filter.admits(file.viewed || file.caught_up)
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
        self.clamp_diff_scroll();
    }

    fn clamp_diff_scroll(&mut self) {
        let rows = self.diff_rows_for_selected_file();
        let max_scroll = rows.len().saturating_sub(1).min(u16::MAX as usize) as u16;
        self.diff_scroll = self.diff_scroll.min(max_scroll);
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
            .min(rows.len().saturating_sub(1))
            .min(u16::MAX as usize) as u16;
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Files => Focus::Diff,
            Focus::Diff => Focus::Files,
        };
        if self.focus == Focus::Diff {
            self.clear_selected_freshness_marks();
        }
        // Never trap: focusing the files pane while it is hidden re-shows it.
        if self.focus == Focus::Files {
            self.file_pane_visible = true;
        }
        self.ensure_diff_cursor_commentable();
    }

    fn clear_selected_freshness_marks(&mut self) {
        if let Some(file) = self.selected_file_mut() {
            file.changed_since_look = false;
            file.changed_hunks.clear();
        }
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

    pub fn jump_to_changed_hunk(&mut self, delta: isize) {
        if self.files.is_empty() {
            return;
        }
        let direction = if delta.is_negative() { -1 } else { 1 };
        let start_file = self.selected;
        for file_step in 0..self.files.len() {
            let file_index = if direction > 0 {
                (start_file + file_step) % self.files.len()
            } else {
                (start_file + self.files.len() - (file_step % self.files.len())) % self.files.len()
            };
            if self.files[file_index].changed_hunks.is_empty()
                || !self.file_visible(&self.files[file_index])
            {
                continue;
            }
            if file_index != self.selected {
                self.reveal_file_in_tree(file_index);
                self.tree_cursor = Some(TreeRowId::File { file_index });
                self.save_current_viewport();
                self.clear_diff_range_selection();
                self.selected = file_index;
                self.restore_current_viewport();
            }
            let rows = self.diff_rows_for_selected_file();
            let mut targets: Vec<usize> = rows
                .iter()
                .enumerate()
                .filter_map(|(row_index, row)| match row.kind {
                    DiffRowKind::HunkHeader
                        if row
                            .hunk_index
                            .is_some_and(|h| self.files[file_index].changed_hunks.contains(&h)) =>
                    {
                        Some(row_index)
                    }
                    _ => None,
                })
                .collect();
            if targets.is_empty() {
                continue;
            }
            targets.sort_unstable();
            let target = if file_index == start_file && self.focus == Focus::Diff {
                if direction > 0 {
                    targets
                        .iter()
                        .copied()
                        .find(|row| *row > self.diff_cursor)
                        .or_else(|| {
                            if self.files.len() == 1 {
                                Some(targets[0])
                            } else {
                                None
                            }
                        })
                } else {
                    targets
                        .iter()
                        .rev()
                        .copied()
                        .find(|row| *row < self.diff_cursor)
                        .or_else(|| {
                            if self.files.len() == 1 {
                                Some(*targets.last().unwrap())
                            } else {
                                None
                            }
                        })
                }
            } else if direction > 0 {
                Some(targets[0])
            } else {
                Some(*targets.last().unwrap())
            };
            if let Some(target) = target {
                self.focus = Focus::Diff;
                self.diff_cursor = target;
                self.diff_scroll = self.diff_cursor.saturating_sub(5) as u16;
                return;
            }
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
                    comment.path.as_deref() == Some(file.path.as_str())
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
                comment.path.as_deref() == Some(file.path.as_str())
                    && matches!(comment.anchor, Some(CommentAnchor::File { .. }) | None)
            }),
        }
    }

    pub fn update_comment_body(&mut self, id: &str, body: String) -> bool {
        let index = self.ensure_active_review_session_index();
        review::edit_comment(
            &mut self.sessions[index],
            &mut self.comments,
            id,
            review::CommentEdits {
                body: Some(body),
                ..Default::default()
            },
        )
        .is_ok()
    }

    /// Advance a comment's state (draft -> todo -> resolved -> draft).
    pub fn cycle_comment_state(&mut self, id: &str) -> Option<CommentState> {
        let comment = self.comments.iter_mut().find(|comment| comment.id == id)?;
        let next = comment.state.next();
        let index = self.ensure_active_review_session_index();
        review::set_comment_state(&mut self.sessions[index], &mut self.comments, id, next)
            .ok()
            .map(|comment| comment.state)
    }

    /// Advance a comment's action intent (none -> fix -> explain -> test -> follow-up -> none).
    pub fn cycle_comment_action(&mut self, id: &str) -> Option<Option<crate::state::ActionIntent>> {
        let next = match self
            .comments
            .iter()
            .find(|comment| comment.id == id)?
            .action
            .unwrap_or(crate::state::ActionIntent::None)
        {
            crate::state::ActionIntent::None => Some(crate::state::ActionIntent::Fix),
            crate::state::ActionIntent::Fix => Some(crate::state::ActionIntent::Explain),
            crate::state::ActionIntent::Explain => Some(crate::state::ActionIntent::Test),
            crate::state::ActionIntent::Test => Some(crate::state::ActionIntent::FollowUp),
            crate::state::ActionIntent::FollowUp => None,
        };
        let index = self.ensure_active_review_session_index();
        review::edit_comment(
            &mut self.sessions[index],
            &mut self.comments,
            id,
            review::CommentEdits {
                action: Some(next),
                ..Default::default()
            },
        )
        .ok()
        .map(|comment| comment.action)
    }

    /// Advance a comment's kind (none -> note -> issue -> question -> praise -> none).
    pub fn cycle_comment_kind(&mut self, id: &str) -> Option<Option<crate::state::CommentKind>> {
        let next = match self.comments.iter().find(|comment| comment.id == id)?.kind {
            None => Some(crate::state::CommentKind::Note),
            Some(crate::state::CommentKind::Note) => Some(crate::state::CommentKind::Issue),
            Some(crate::state::CommentKind::Issue) => Some(crate::state::CommentKind::Question),
            Some(crate::state::CommentKind::Question) => Some(crate::state::CommentKind::Praise),
            Some(crate::state::CommentKind::Praise) => None,
        };
        let index = self.ensure_active_review_session_index();
        review::edit_comment(
            &mut self.sessions[index],
            &mut self.comments,
            id,
            review::CommentEdits {
                kind: Some(next),
                ..Default::default()
            },
        )
        .ok()
        .map(|comment| comment.kind)
    }

    /// Select a comment by id, moving the file/diff cursors to its anchor.
    pub fn select_comment_by_id(&mut self, id: &str) {
        if let Some(index) = self.comments.iter().position(|comment| comment.id == id) {
            self.select_comment(index);
        }
    }

    pub fn delete_comment(&mut self, id: &str) -> bool {
        let session_index = self.ensure_active_review_session_index();
        if review::delete_comment(&mut self.sessions[session_index], &mut self.comments, id)
            .is_err()
        {
            return false;
        }
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
        let index = self.ensure_active_review_session_index();
        let session_id = self.sessions[index].id.clone();
        let observation = crate::provenance::CommentObservation::new(
            self.provenance_snapshot(index),
            Some(anchor.clone()),
        );
        let _ = review::add_comment(
            &mut self.sessions[index],
            &mut self.comments,
            review::NewComment {
                session_id,
                path: Some(anchor.path().to_owned()),
                line: anchor.line(),
                end_line: anchor
                    .end_line()
                    .filter(|end_line| Some(*end_line) != anchor.line()),
                anchor: Some(anchor),
                observation: Some(observation),
                body,
                kind: None,
                action: None,
                state: self.comment_initial_state,
            },
        );
    }

    pub fn add_general_comment(&mut self, body: String) {
        let index = self.ensure_active_review_session_index();
        let session_id = self.sessions[index].id.clone();
        let observation =
            crate::provenance::CommentObservation::new(self.provenance_snapshot(index), None);
        let _ = review::add_comment(
            &mut self.sessions[index],
            &mut self.comments,
            review::NewComment {
                session_id,
                path: None,
                line: None,
                end_line: None,
                anchor: None,
                observation: Some(observation),
                body,
                kind: None,
                action: None,
                state: self.comment_initial_state,
            },
        );
    }

    fn provenance_snapshot(&self, session_index: usize) -> crate::provenance::SnapshotEvidence {
        let durable = &self.sessions[session_index];
        crate::provenance::SnapshotEvidence::capture(
            chrono::Utc::now(),
            durable.id.clone(),
            durable.target.clone(),
            self.files.iter().map(|file| &file.diff),
        )
    }

    pub fn ready_all_draft_comments(&mut self) -> review::ReadyCommentsResult {
        let index = self.ensure_active_review_session_index();
        review::ready_all_drafts(&mut self.sessions[index], &mut self.comments).unwrap_or_default()
    }

    fn ensure_active_review_session_index(&mut self) -> usize {
        if let Some(id) = review::active_session_for_loaded_review(
            &self.sessions,
            &self.repo,
            &self.target.base,
            &self.target.rev,
        )
        .map(|session| session.id.clone())
            && let Some(index) = self.sessions.iter().position(|session| session.id == id)
        {
            return index;
        }
        let mut state = ReviewState {
            sessions: std::mem::take(&mut self.sessions),
            ..ReviewState::default()
        };
        let spec = review::SessionTargetSpec {
            repo: Some(review::canonical_repo_identity(&self.repo)),
            base: Some(self.target.base.clone()),
            revision: Some(self.target.rev.clone()),
            revset: None,
        };
        let id = review::ensure_session(&mut state, &spec, None).id.clone();
        self.sessions = state.sessions;
        self.sessions
            .iter()
            .position(|session| session.id == id)
            .expect("ensured session must exist")
    }

    fn current_comment_index(&self) -> Option<usize> {
        self.selected_comment_index()
    }

    fn select_comment(&mut self, index: usize) {
        let Some(comment) = self.comments.get(index).cloned() else {
            return;
        };
        self.selected_comment_id = Some(comment.id.clone());
        let Some(target_path) = comment
            .anchor
            .as_ref()
            .map(CommentAnchor::path)
            .or(comment.path.as_deref())
        else {
            return;
        };
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
                version: REVIEW_STATE_SCHEMA_VERSION,
                base: Some(self.target.base.clone()),
                revision: Some(self.target.rev.clone()),
                repo: Some(review::canonical_repo_identity(&self.repo)),
                saved_at: Some(Utc::now()),
            },
            files: {
                let mut files = self.persisted_files.clone();
                for file in &self.files {
                    let mut state = files.remove(&file.path).unwrap_or_default();
                    state.normalize_legacy();
                    if file.viewed {
                        state.viewed_fingerprints.insert(file.fingerprint.clone());
                        state.caught_up_fingerprints.remove(&file.fingerprint);
                    } else if file.caught_up {
                        state
                            .caught_up_fingerprints
                            .insert(file.fingerprint.clone());
                        state.viewed_fingerprints.remove(&file.fingerprint);
                    } else {
                        // Unmarking a file only removes the current content version;
                        // marks for other fingerprints remain valid when switching targets.
                        state.viewed_fingerprints.remove(&file.fingerprint);
                        state.caught_up_fingerprints.remove(&file.fingerprint);
                    }
                    state.fingerprint = file.fingerprint.clone();
                    state.viewed = state.is_viewed_fingerprint(&file.fingerprint);
                    files.insert(file.path.clone(), state);
                }
                files
            },
            comments: self.comments.clone(),
            sessions: self.sessions.clone(),
        }
    }

    pub fn summary_line(&self) -> String {
        let viewed = self
            .files
            .iter()
            .filter(|file| file.viewed || file.caught_up)
            .count();
        let generated = self.files.iter().filter(|file| file.generated).count();
        let additions: usize = self.files.iter().map(|file| file.additions).sum();
        let deletions: usize = self.files.iter().map(|file| file.deletions).sum();
        format!(
            "{} files ({viewed}/{} viewed, {generated} generated/noisy), +{additions}/-{deletions}, {}",
            self.files.len(),
            self.files.len(),
            pluralize(self.comments.len(), "comment")
        )
    }
}

fn pluralize(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
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

    #[test]
    fn hiding_the_file_pane_moves_focus_to_the_diff() {
        let mut session = session();
        assert!(session.file_pane_visible);
        assert_eq!(session.focus, Focus::Files);

        session.toggle_file_pane();

        assert!(!session.file_pane_visible);
        assert_eq!(session.focus, Focus::Diff);
    }

    #[test]
    fn focusing_the_files_pane_reshows_it() {
        let mut session = session();
        session.toggle_file_pane();
        assert!(!session.file_pane_visible);

        // Never trap: tab back to the files pane brings it back.
        session.toggle_focus();

        assert_eq!(session.focus, Focus::Files);
        assert!(session.file_pane_visible);
    }

    #[test]
    fn showing_the_file_pane_again_keeps_diff_focus() {
        let mut session = session();
        session.toggle_file_pane();
        session.toggle_file_pane();

        assert!(session.file_pane_visible);
        assert_eq!(session.focus, Focus::Diff);
    }

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

    #[test]
    fn tui_comments_capture_loaded_snapshot_and_refresh_preserves_observation() {
        let mut session = session();
        session.add_file_comment("file concern".into());
        session.add_general_comment("overall concern".into());

        let located = session.comments[0].clone();
        let observation = located.observation.clone().expect("captured observation");
        assert_eq!(observation.snapshot.files.len(), 2);
        assert_eq!(observation.anchor, located.anchor);
        assert!(session.comments[1].is_general());
        assert!(
            session.comments[1]
                .observation
                .as_ref()
                .unwrap()
                .anchor
                .is_none()
        );
        assert!(session.update_comment_body(&located.id, "edited concern".into()));
        assert_eq!(session.comments[0].observation.as_ref(), Some(&observation));

        let refreshed = DiffSet::parse(
            "diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -2 +2 @@\n-old\n+newer",
        )
        .unwrap();
        session.replace_diff(ReviewTarget::trunk_to_current(), refreshed);

        assert_eq!(session.comments[0].observation.as_ref(), Some(&observation));
        assert_ne!(session.comments[0].anchor, observation.anchor);
    }

    #[test]
    fn comment_capture_does_not_reuse_same_target_session_from_another_repo() {
        let diff = DiffSet::parse(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new",
        )
        .unwrap();
        let state = ReviewState {
            sessions: vec![crate::state::ReviewSession {
                id: "wrong-repo".into(),
                target: crate::state::ReviewTarget {
                    repo: Some("/wrong".into()),
                    base: Some("trunk()".into()),
                    revision: Some("@".into()),
                    ..Default::default()
                },
                status: ReviewSessionStatus::Open,
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut session = ReviewSession::new(
            "/right".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            state,
        );

        session.add_file_comment("right repo".into());

        assert_eq!(session.sessions.len(), 2);
        assert_ne!(
            session.comments[0].session_id.as_deref(),
            Some("wrong-repo")
        );
        let owner = session
            .sessions
            .iter()
            .find(|durable| durable.id == session.comments[0].session_id.as_deref().unwrap())
            .unwrap();
        assert_eq!(owner.target.repo.as_deref(), Some("/right"));
    }

    #[test]
    fn refresh_reanchor_follows_rename_but_not_copy_and_preserves_observation() {
        let original = DiffSet::parse(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-old\n+new",
        )
        .unwrap();
        let mut renamed = ReviewSession::new(
            "/repo".into(),
            ReviewTarget::trunk_to_current(),
            original.clone(),
            ReviewState::default(),
        );
        renamed.add_file_comment("follow rename".into());
        let observation = renamed.comments[0].observation.clone();
        renamed.replace_diff(
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs",
            )
            .unwrap(),
        );
        assert_eq!(renamed.comments[0].path.as_deref(), Some("new.rs"));
        assert_eq!(renamed.comments[0].observation, observation);

        let mut copied = ReviewSession::new(
            "/repo".into(),
            ReviewTarget::trunk_to_current(),
            original,
            ReviewState::default(),
        );
        copied.add_file_comment("do not follow copy".into());
        let observation = copied.comments[0].observation.clone();
        copied.replace_diff(
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/old.rs b/copy.rs\nsimilarity index 100%\ncopy from old.rs\ncopy to copy.rs",
            )
            .unwrap(),
        );
        assert_eq!(copied.comments[0].path.as_deref(), Some("old.rs"));
        assert_eq!(copied.comments[0].observation, observation);
    }

    #[test]
    fn replace_diff_preserving_view_keeps_the_reviewers_place() {
        let mut session = session();
        session.toggle_file_pane(); // hidden pane, focus moves to the diff
        session.hide_generated = true;
        session.viewed_filter = ViewedFilter::Unviewed;
        // Select the second file and mark the first viewed.
        let index = session
            .files
            .iter()
            .position(|file| file.path == "README.md")
            .unwrap();
        session.select_file_index(index);
        session.zen_focus = Some(ZenFocus {
            path: "README.md".to_owned(),
            lines: Some((1, 1)),
        });
        session.files[0].viewed = true;

        // The same review grew a third file (new work landed).
        let refreshed = DiffSet::parse(
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
diff --git a/src/new.rs b/src/new.rs
--- a/src/new.rs
+++ b/src/new.rs
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);

        assert!(!session.file_pane_visible);
        assert_eq!(session.focus, Focus::Diff);
        assert!(session.hide_generated);
        assert_eq!(session.viewed_filter, ViewedFilter::Unviewed);
        assert_eq!(session.selected_file().unwrap().path, "README.md");
        assert_eq!(session.zen_focus.as_ref().unwrap().path, "README.md");
        // Viewed marks still carry over by fingerprint, and the new file
        // arrived unviewed.
        assert!(
            session
                .files
                .iter()
                .find(|file| file.path == "src/tui.rs")
                .unwrap()
                .viewed
        );
        assert!(
            !session
                .files
                .iter()
                .find(|file| file.path == "src/new.rs")
                .unwrap()
                .viewed
        );
    }

    #[test]
    fn replace_diff_preserving_view_reanchors_line_comments_for_rendering() {
        let mut session = session();
        session.toggle_focus();
        let original_anchor = session.selected_line_anchor().unwrap();
        session.add_comment("Line note".into());
        assert_eq!(
            session
                .comments_for_diff_row_anchor_details(&original_anchor)
                .len(),
            1
        );

        let refreshed = DiffSet::parse(
            r#"diff --git a/src/tui.rs b/src/tui.rs
--- a/src/tui.rs
+++ b/src/tui.rs
@@ -1 +1 @@
-old
+newer
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);

        let refreshed_anchor = session.comments[0].anchor.clone().unwrap();
        assert_ne!(original_anchor, refreshed_anchor);
        assert_eq!(
            session
                .comments_for_diff_row_anchor_details(&refreshed_anchor)
                .len(),
            1,
            "line comments must be attached to the refreshed diff row so the gutter marker and inline body render after watch refresh"
        );
    }

    #[test]
    fn refresh_marks_changed_new_and_stale_files_and_clears_on_look() {
        let mut session = session();
        session.files[0].viewed = true;
        let old_readme = session.files[1].fingerprint.clone();
        let refreshed = DiffSet::parse(
            r#"diff --git a/src/tui.rs b/src/tui.rs
--- a/src/tui.rs
+++ b/src/tui.rs
@@ -1 +1 @@
-old
+newer
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
diff --git a/new.rs b/new.rs
--- /dev/null
+++ b/new.rs
@@ -0,0 +1 @@
+hi
"#,
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        assert!(session.files[0].changed_since_look);
        assert!(!session.files[1].changed_since_look);
        assert_eq!(session.files[1].fingerprint, old_readme);
        assert!(session.files[2].changed_since_look);

        session.select_file_index(2);
        assert!(session.files[2].changed_since_look);
        session.toggle_focus();
        assert!(!session.files[2].changed_since_look);
        session.select_file_index(0);
        session.mark_selected_viewed();
        assert!(!session.files[0].changed_since_look);
    }

    #[test]
    fn refresh_revert_to_seen_content_clears_freshness_badge() {
        let mut session = session();
        let original_fingerprint = session.files[0].fingerprint.clone();
        session.files[0].viewed = true;
        session.persisted_files.insert(
            "src/tui.rs".to_owned(),
            FileState {
                fingerprint: original_fingerprint.clone(),
                viewed: true,
                viewed_fingerprints: [original_fingerprint].into_iter().collect(),
                ..Default::default()
            },
        );

        let changed = DiffSet::parse(
            r#"diff --git a/src/tui.rs b/src/tui.rs
--- a/src/tui.rs
+++ b/src/tui.rs
@@ -1 +1 @@
-old
+newer
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), changed);
        assert!(session.files[0].changed_since_look);
        assert!(session.last_refresh_changes[0].was_reviewed);

        let reverted = DiffSet::parse(
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
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), reverted);

        assert!(session.files[0].viewed);
        assert!(!session.files[0].changed_since_look);
        assert!(session.last_refresh_changes[0].reverted_to_seen);
        assert!(!session.last_refresh_changes[0].was_reviewed);
    }

    #[test]
    fn refresh_revert_to_unviewed_baseline_clears_freshness_badge() {
        let mut session = session();
        assert!(!session.files[0].viewed);

        let changed = DiffSet::parse(
            "diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+newer\ndiff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), changed);
        assert!(session.files[0].changed_since_look);
        assert!(session.files[0].changed_since_look_baseline.is_some());

        let reverted = DiffSet::parse(
            "diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), reverted);

        assert!(!session.files[0].changed_since_look);
        assert!(session.files[0].changed_since_look_baseline.is_none());
        assert!(session.last_refresh_changes[0].reverted_to_seen);
    }

    #[test]
    fn apply_state_files_marks_viewed_mismatch_stale() {
        let mut state = ReviewState::default();
        state.files.insert(
            "src/tui.rs".to_owned(),
            FileState {
                fingerprint: "old".to_owned(),
                viewed: true,
                ..Default::default()
            },
        );
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse("diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+new\n").unwrap(),
            state,
        );
        assert!(!session.files[0].viewed);
        assert!(session.files[0].viewed_stale);
    }

    #[test]
    fn viewed_marks_survive_saving_other_fingerprints_for_same_path() {
        let wide = DiffSet::parse("diff --git a/src/config.rs b/src/config.rs\n--- a/src/config.rs\n+++ b/src/config.rs\n@@ -1 +1 @@\n-old\n+wide\n").unwrap();
        let narrow = DiffSet::parse("diff --git a/src/config.rs b/src/config.rs\n--- a/src/config.rs\n+++ b/src/config.rs\n@@ -1 +1 @@\n-old\n+narrow\n").unwrap();
        let mut wide_session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            wide.clone(),
            ReviewState::default(),
        );
        wide_session.mark_selected_viewed();
        let saved_wide = wide_session.to_state();
        let wide_fingerprint = wide_session.files[0].fingerprint.clone();

        let narrow_session = ReviewSession::new(
            ".".into(),
            ReviewTarget::new("change-", "change"),
            narrow,
            saved_wide,
        );
        assert_ne!(wide_fingerprint, narrow_session.files[0].fingerprint);
        assert!(!narrow_session.files[0].viewed);
        assert!(narrow_session.files[0].viewed_stale);
        let saved_narrow = narrow_session.to_state();

        let wide_again = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            wide,
            saved_narrow,
        );
        assert!(wide_again.files[0].viewed);
        assert!(!wide_again.files[0].viewed_stale);
    }

    #[test]
    fn viewed_stale_survives_save_and_refresh_until_current_fingerprint_is_viewed() {
        let original = DiffSet::parse("diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
        let refreshed = DiffSet::parse("diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+newer\n").unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            original,
            ReviewState::default(),
        );
        session.mark_selected_viewed();
        let saved = session.to_state();

        let stale = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            refreshed.clone(),
            saved,
        );
        assert!(!stale.files[0].viewed);
        assert!(stale.files[0].viewed_stale);

        let saved_stale = stale.to_state();
        let mut stale_again = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            refreshed,
            saved_stale,
        );
        assert!(!stale_again.files[0].viewed);
        assert!(stale_again.files[0].viewed_stale);

        stale_again.mark_selected_viewed();
        assert!(stale_again.files[0].viewed);
        assert!(!stale_again.files[0].viewed_stale);
    }

    #[test]
    fn toggle_unmark_removes_only_current_fingerprint() {
        let mut session = session();
        let current = session.files[0].fingerprint.clone();
        let other = "other-version".to_owned();
        session.persisted_files.insert(
            "src/tui.rs".to_owned(),
            FileState {
                fingerprint: current.clone(),
                viewed: true,
                viewed_fingerprints: BTreeSet::from([current.clone(), other.clone()]),
                ..Default::default()
            },
        );
        session.apply_state_files();

        session.toggle_viewed();
        let saved = session.to_state();
        let file = &saved.files["src/tui.rs"];
        assert!(!file.viewed_fingerprints.contains(&current));
        assert!(file.viewed_fingerprints.contains(&other));
    }

    #[test]
    fn refresh_marks_only_new_hunk_fingerprints() {
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-x\n+y\n").unwrap(),
            ReviewState::default(),
        );
        let refreshed = DiffSet::parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-x\n+z\n").unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        assert!(!session.files[0].changed_hunks.contains(&0));
        assert!(session.files[0].changed_hunks.contains(&1));
    }

    #[test]
    fn replace_diff_preserving_view_drops_a_zen_frame_for_a_vanished_file() {
        let mut session = session();
        session.zen_focus = Some(ZenFocus {
            path: "src/tui.rs".to_owned(),
            lines: None,
        });

        let refreshed = DiffSet::parse(
            r#"diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);

        assert!(session.zen_focus.is_none());
    }

    fn three_file_diff() -> DiffSet {
        DiffSet::parse(
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
        .unwrap()
    }

    fn three_file_session() -> ReviewSession {
        let diff = three_file_diff();
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

        let (caught_up, already_viewed, changed) = session.apply_incremental_review(&prior);

        assert_eq!((caught_up, already_viewed, changed), (0, 1, 2));
        assert!(session.files[0].viewed);
        assert!(!session.files[0].caught_up);
        assert!(!session.files[1].viewed);
        assert!(!session.files[1].caught_up);
        assert!(!session.files[2].viewed);
        assert!(!session.files[2].caught_up);
    }

    #[test]
    fn incremental_review_marks_never_viewed_unchanged_caught_up() {
        let mut session = three_file_session();
        session.files[0].changed_since_look = true;
        session.files[0].changed_since_look_baseline = Some("baseline".to_owned());
        let mut prior = BTreeMap::new();
        prior.insert("src/a.rs".to_owned(), session.files[0].fingerprint.clone());

        let counts = session.apply_incremental_review(&prior);

        assert_eq!(counts, (1, 0, 2));
        assert!(!session.files[0].viewed);
        assert!(session.files[0].caught_up);
        assert!(!session.files[0].changed_since_look);
        assert!(session.files[0].changed_since_look_baseline.is_none());
    }

    #[test]
    fn caught_up_persists_promotes_and_decays_to_stale_on_change() {
        let mut session = three_file_session();
        session.files[0].caught_up = true;
        let state = session.to_state();
        assert!(!state.files["src/a.rs"].viewed);
        assert!(
            state.files["src/a.rs"]
                .caught_up_fingerprints
                .contains(&session.files[0].fingerprint)
        );

        let loaded = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            three_file_diff(),
            state.clone(),
        );
        assert!(loaded.files[0].caught_up);
        assert!(!loaded.files[0].viewed);

        let mut promoted = loaded;
        promoted.mark_selected_viewed();
        let promoted_state = promoted.to_state();
        assert!(
            promoted_state.files["src/a.rs"]
                .viewed_fingerprints
                .contains(&promoted.files[0].fingerprint)
        );
        assert!(
            !promoted_state.files["src/a.rs"]
                .caught_up_fingerprints
                .contains(&promoted.files[0].fingerprint)
        );

        let changed = DiffSet::parse(
            r#"diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1 +1 @@
-old
+changed again
"#,
        )
        .unwrap();
        let stale =
            ReviewSession::new(".".into(), ReviewTarget::trunk_to_current(), changed, state);
        assert!(!stale.files[0].viewed);
        assert!(!stale.files[0].caught_up);
        assert!(stale.files[0].viewed_stale);
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

    fn comment(id: &str, path: &str) -> Comment {
        Comment {
            id: id.to_owned(),
            path: Some(path.to_owned()),
            line: Some(1),
            end_line: None,
            anchor: None,
            body: "review note".to_owned(),
            kind: None,
            action: None,
            state: CommentState::Draft,
            created_at: Utc::now(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_diff_preserves_persisted_comments_and_viewed_files() {
        let state = ReviewState {
            files: BTreeMap::from([(
                "src/app.rs".to_owned(),
                FileState {
                    fingerprint: "abc".to_owned(),
                    viewed: true,
                    ..Default::default()
                },
            )]),
            comments: vec![comment("old", "src/app.rs")],
            ..Default::default()
        };

        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse("").unwrap(),
            state,
        );

        assert!(session.files.is_empty());
        assert_eq!(session.comments.len(), 1);
        assert_eq!(session.to_state().files["src/app.rs"].fingerprint, "abc");
    }

    #[test]
    fn retarget_to_empty_diff_does_not_erase_unseen_state() {
        let mut session = session();
        session.files[0].viewed = true;
        session.comments.push(comment("a", "src/tui.rs"));

        session.replace_diff(
            ReviewTarget::new("change-1-", "change-1"),
            DiffSet::parse("").unwrap(),
        );
        let saved = session.to_state();

        assert_eq!(
            saved
                .comments
                .iter()
                .map(|c| c.id.as_str())
                .collect::<Vec<_>>(),
            ["a"]
        );
        assert!(saved.files["src/tui.rs"].viewed);

        session.replace_diff(
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                r#"diff --git a/src/tui.rs b/src/tui.rs
--- a/src/tui.rs
+++ b/src/tui.rs
@@ -1 +1 @@
-old
+new
"#,
            )
            .unwrap(),
        );

        assert!(session.files[0].viewed);
        assert_eq!(session.comments.len(), 1);
    }

    #[test]
    fn comments_added_on_different_targets_survive_each_other_saves() {
        let diff_a = DiffSet::parse(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let diff_b = DiffSet::parse(
            "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff_a.clone(),
            ReviewState::default(),
        );
        session.comments.push(comment("a", "a.rs"));
        session.replace_diff(ReviewTarget::new("b-", "b"), diff_b);
        session.comments.push(comment("b", "b.rs"));
        let saved_from_b = session.to_state();

        assert_eq!(saved_from_b.comments.len(), 2);

        session.replace_diff(ReviewTarget::trunk_to_current(), diff_a);
        let saved_from_a = session.to_state();

        assert_eq!(saved_from_a.comments.len(), 2);
        assert!(
            saved_from_a
                .comments
                .iter()
                .any(|comment| comment.id == "a")
        );
        assert!(
            saved_from_a
                .comments
                .iter()
                .any(|comment| comment.id == "b")
        );
    }

    #[test]
    fn tui_state_snapshots_preserve_durable_sessions() {
        let durable = crate::state::ReviewSession {
            id: "review-1".to_owned(),
            title: Some("CLI session".to_owned()),
            ..Default::default()
        };
        let state = ReviewState {
            sessions: vec![durable.clone()],
            ..Default::default()
        };
        let diff = DiffSet::parse(
            "diff --git a/src/app.rs b/src/app.rs\n--- a/src/app.rs\n+++ b/src/app.rs\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();

        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff.clone(),
            state,
        );

        assert_eq!(session.to_state().sessions, vec![durable.clone()]);
        session.replace_diff(ReviewTarget::trunk_to_current(), diff);
        assert_eq!(session.into_state().sessions, vec![durable]);
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
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: None,
                explanation: None,
                rationale: None,
                artifacts: Vec::new(),
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
                importance: crate::agent::ChunkImportance::Spotlight,
                change_id: None,
                explanation: None,
                rationale: None,
                artifacts: Vec::new(),
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
        assert_eq!(comment.path.as_deref(), Some("src/tui.rs"));
        assert_eq!(comment.line, None);
        assert!(matches!(comment.anchor, Some(CommentAnchor::File { .. })));
    }

    #[test]
    fn add_line_comment_uses_diff_cursor_anchor() {
        let mut session = session();
        session.toggle_focus();

        session.add_comment("Line note".into());

        let comment = &session.comments[0];
        assert_eq!(comment.path.as_deref(), Some("src/tui.rs"));
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
    fn scroll_diff_clamps_to_last_content_line() {
        let mut session = multi_line_session();

        session.scroll_diff(i16::MAX);

        let rows = session.diff_rows_for_selected_file();
        assert_eq!(session.diff_scroll as usize, rows.len() - 1);
    }

    #[test]
    fn summary_line_pluralizes_comment_count() {
        let mut session = session();

        assert!(session.summary_line().contains("0 comments"));
        session.add_comment("one".into());
        assert!(session.summary_line().contains("1 comment"));
        assert!(!session.summary_line().contains("1 comments"));
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
    fn jump_to_changed_hunk_wraps_within_file() {
        let mut session = ReviewSession::new(
            ".".into(), ReviewTarget::trunk_to_current(),
            DiffSet::parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-x\n+y\n").unwrap(),
            ReviewState::default());
        session.files[0].changed_hunks = [0, 1].into_iter().collect();
        session.jump_to_changed_hunk(1);
        let first = session.diff_cursor;
        session.jump_to_changed_hunk(1);
        assert!(session.diff_cursor > first);
        session.jump_to_changed_hunk(1);
        assert_eq!(session.diff_cursor, first);
    }

    #[test]
    fn jump_to_changed_hunk_continues_across_files() {
        let mut session = ReviewSession::new(
            ".".into(), ReviewTarget::trunk_to_current(),
            DiffSet::parse("diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-x\n+y\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-c\n+d\n").unwrap(),
            ReviewState::default());
        session.files[0].changed_hunks = [0, 1].into_iter().collect();
        session.files[1].changed_hunks.insert(0);

        session.jump_to_changed_hunk(1);
        assert_eq!(session.selected, 0);
        let first = session.diff_cursor;
        session.jump_to_changed_hunk(1);
        assert_eq!(session.selected, 0);
        assert!(session.diff_cursor > first);
        session.jump_to_changed_hunk(1);
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        session.jump_to_changed_hunk(1);
        assert_eq!(session.selected_file().unwrap().path.as_str(), "a.rs");
        assert_eq!(session.diff_cursor, first);
    }

    #[test]
    fn selecting_file_does_not_clear_freshness_until_diff_is_focused() {
        let mut session = session();
        session.files[1].changed_since_look = true;
        session.files[1].changed_hunks.insert(0);

        session.select_file_index(1);
        assert!(session.files[1].changed_since_look);
        assert!(session.files[1].changed_hunks.contains(&0));

        session.toggle_focus();
        assert_eq!(session.focus, Focus::Diff);
        assert!(!session.files[1].changed_since_look);
        assert!(session.files[1].changed_hunks.is_empty());
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

        assert_eq!(state.meta.version, REVIEW_STATE_SCHEMA_VERSION);
        assert_eq!(state.meta.base.as_deref(), Some("trunk()"));
        assert_eq!(state.meta.revision.as_deref(), Some("@"));
        assert!(state.meta.repo.is_some());
        assert!(state.meta.saved_at.is_some());
    }
}
