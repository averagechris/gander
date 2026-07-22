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
mod stream;
mod syntax_cache;
mod word_diff;

pub use context_expansion::Expansion;
pub use diff_rows::{DiffRow, DiffRowKind};
pub use split_rows::{SplitRow, split_rows};
#[allow(unused_imports)]
pub use stream::{
    ChapterHeader, Coverage, ReviewStream, SkimAcknowledgeResult, SkimFold, SpotlightTarget,
    StreamRow, StreamRowKind,
};

use crate::state::ReviewSessionStatus;
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    rc::Rc,
};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::{
    agent::{AgentFlag, AgentOverlay, LegacyDraftState},
    anchor::{
        CommentAnchor, DiffSide, RangeLineAnchor, comment_anchor_for_file_lines,
        comment_anchor_for_sided_lines, fingerprint_range, line_anchor_for_side_line,
    },
    config::{Config, DiffConfig, DiffViewModeConfig, LimitsConfig},
    diff::{DiffSet, FileDiff, FileStatus},
    file_tree::{FileTreeInput, FileTreeView, FlatTreeRowKind, TreeRowId},
    jj::{JjChangeSummary, ReviewTarget},
    review,
    state::{
        AuthorKind, Channel, Comment, CommentState, FileState, Identity,
        REVIEW_STATE_SCHEMA_VERSION, ReviewState, ReviewStateMeta,
        ReviewTarget as StateReviewTarget,
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
/// file index → (diff fingerprint, memoized cheap structural stream rows).
type StructuralRowsCache = BTreeMap<usize, (String, Rc<Vec<DiffRow>>)>;

const GENERATED_TREE_GROUP: &str = "generated/noisy";

/// Rough number of diff rows kept visible below the cursor when auto-scrolling.
const DIFF_CURSOR_SCROLL_MARGIN: usize = 15;

#[derive(Debug, Clone)]
pub struct ReviewSession {
    pub repo: PathBuf,
    pub target: ReviewTarget,
    pub files: Vec<ReviewFile>,
    persisted_files: BTreeMap<String, FileState>,
    pub comments: Vec<Comment>,
    /// Durable review sessions loaded from the state file. Private on
    /// purpose: every mutation must pass through
    /// [`Self::durable_sessions_mut`] (or an internal seam that calls
    /// [`Self::touch_durable_review`]) so the stream cache and autosave
    /// generations observe the change without hashing session content.
    durable_sessions: Vec<crate::state::ReviewSession>,
    /// Configured lifecycle state for newly accepted durable comments.
    pub comment_initial_state: CommentState,
    pub comment_default_channel: Option<Channel>,
    pub human_identity: Identity,
    /// Explicit configured identity name, distinct from the migration-safe
    /// `human:local` fallback used for authorship stamping.
    pub configured_human_name: Option<String>,
    /// Explicit configured identity email, used only for ownership matching.
    pub configured_human_email: Option<String>,
    pub agent_identity: Identity,
    /// Author name of exactly one target revision, populated by a read-only jj
    /// query. `None` means unavailable or ambiguous and inference stays private.
    pub target_author_name: Option<String>,
    /// Author email of the same consistent target range, independent of name.
    pub target_author_email: Option<String>,
    pub selected: usize,
    pub diff_scroll: u16,
    pub diff_cursor: usize,
    /// Cursor and logical top in the continuous cross-file review stream.
    /// `diff_cursor` remains the selected file's local coordinate for stable
    /// comments and range anchors.
    pub stream_cursor: usize,
    pub stream_scroll: u16,
    /// Enabled by the TUI runtime after construction. Non-TUI domain consumers
    /// retain the selected-file coordinate system.
    pub stream_mode: bool,
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
    /// Lazily populated jj diffs for each stack change, keyed by change id.
    /// Stream chapter headers use these for per-change diff statistics.
    pub change_diffs: Vec<(String, DiffSet)>,
    /// Read-only jj metadata used by change-scoped stream chapter headers.
    pub stack_changes: Vec<JjChangeSummary>,
    /// Ephemeral in-place skim peeks. Durable acknowledgement lives in the
    /// active review session's attention progress.
    pub expanded_skim_folds: BTreeSet<String>,
    /// Files whose full syntax/folding diff rows have been requested by the
    /// current stream viewport. All other files stay on cheap parsed-diff
    /// structural rows.
    stream_materialized_files: RefCell<BTreeSet<usize>>,
    stream_cache: RefCell<Option<(stream::StreamCacheKey, Rc<ReviewStream>)>>,
    /// Monotonic generation covering every stream-projection input that the
    /// cheap scalar cache key cannot observe directly: the file set and
    /// fingerprints, force-render marks, skim-fold peeks, durable sessions
    /// (attention regions/progress, walkthroughs), stack changes, and
    /// per-change diffs. Bumped explicitly at each mutation seam via
    /// [`Self::touch_stream_inputs`]; cache validation is a u64 compare
    /// instead of hashing session-scale content per access.
    stream_generation: Cell<u64>,
    /// Monotonic generation covering durable review state (viewed marks,
    /// comments, durable sessions). Autosave gates on it before serializing
    /// or fingerprinting anything, so an unchanged session costs nothing per
    /// keystroke. Bumped via [`Self::touch_durable_state`].
    durable_generation: Cell<u64>,
    /// Canonical (filesystem-resolved) repo identity, computed once at
    /// construction. Per-frame session lookups must use this instead of
    /// re-canonicalizing (`getattrlist`) on every access.
    canonical_repo: String,
    /// Memoized cheap structural stream rows per file, keyed by the file's
    /// diff fingerprint. Structural rows depend only on parsed diff content,
    /// so entries self-invalidate when a refresh changes the fingerprint.
    structural_rows_cache: RefCell<StructuralRowsCache>,
    #[cfg(test)]
    stream_projection_builds: Cell<usize>,
    /// Test-only count of [`Self::to_state`] snapshots (session-scale clone +
    /// serialize precursor). Regression tests assert deltas to prove key
    /// events over an unchanged session never snapshot durable state.
    #[cfg(test)]
    state_snapshots: Cell<usize>,
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
    syntax_cache: RefCell<BTreeMap<SyntaxCacheKey, Rc<SyntaxFileCache>>>,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FileViewport {
    diff_scroll: u16,
    diff_cursor: usize,
    top_identity: Option<DiffRowIdentity>,
    cursor_identity: Option<DiffRowIdentity>,
}

/// Exact ephemeral fold state captured by the TUI Focus preset. Kept opaque
/// outside the app core so every fold-producing cache is invalidated together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FocusFoldingSnapshot {
    fold_context: bool,
    expanded_skim_folds: BTreeSet<String>,
    context_expansion: BTreeMap<(String, usize), Expansion>,
}

#[derive(Debug, Clone)]
pub(crate) struct FocusViewSnapshot {
    selected: usize,
    diff_scroll: u16,
    diff_cursor: usize,
    stream_scroll: u16,
    stream_cursor: usize,
    focus: Focus,
    folding: FocusFoldingSnapshot,
    viewport_by_path: BTreeMap<String, FileViewport>,
    tree_cursor: Option<TreeRowId>,
    diff_range_selection: Option<DiffRangeSelection>,
    selected_comment_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct DiffRowIdentity {
    old_lineno: Option<usize>,
    new_lineno: Option<usize>,
    text: String,
    kind: DiffRowKind,
    hunk_index: Option<usize>,
    semantic_key: Option<String>,
    semantic_parent_key: Option<String>,
    semantic_occurrence: usize,
    semantic_total: usize,
    logical_range: Option<std::ops::Range<usize>>,
    old_logical_range: Option<std::ops::Range<usize>>,
}

impl PartialEq for DiffRowIdentity {
    fn eq(&self, other: &Self) -> bool {
        if self.semantic_key.is_some() || other.semantic_key.is_some() {
            let same_source_location = self.logical_range.is_some()
                && self.logical_range == other.logical_range
                || self.old_logical_range.is_some()
                    && self.old_logical_range == other.old_logical_range
                || self.new_lineno.is_some()
                    && self.new_lineno == other.new_lineno
                    && self.old_lineno == other.old_lineno;
            return self.semantic_key == other.semantic_key
                && (self.semantic_parent_key.is_none()
                    || other.semantic_parent_key.is_none()
                    || self.semantic_parent_key == other.semantic_parent_key)
                && (same_source_location
                    || self.semantic_total == other.semantic_total
                        && self.semantic_occurrence == other.semantic_occurrence);
        }
        match (self.kind, other.kind) {
            (DiffRowKind::FileHeader, DiffRowKind::FileHeader)
            | (DiffRowKind::Placeholder, DiffRowKind::Placeholder) => true,
            (DiffRowKind::HunkHeader, DiffRowKind::HunkHeader)
            | (DiffRowKind::ContextFold, DiffRowKind::ContextFold) => {
                self.hunk_index == other.hunk_index
            }
            (
                DiffRowKind::ExpandGap { gap_id, .. },
                DiffRowKind::ExpandGap {
                    gap_id: other_gap, ..
                },
            ) => gap_id == other_gap,
            _ => {
                self.kind == other.kind
                    && self.old_lineno == other.old_lineno
                    && self.new_lineno == other.new_lineno
                    && self.text == other.text
            }
        }
    }
}

impl Eq for DiffRowIdentity {}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefreshFileLineage {
    path: String,
    status: FileStatus,
    old_path: Option<String>,
}

fn refresh_path_mapping(
    previous: &[RefreshFileLineage],
    current: &[RefreshFileLineage],
) -> BTreeMap<String, String> {
    let mut mapping = BTreeMap::new();
    let mut used = BTreeSet::new();
    let mut assign = |matches: &dyn Fn(&RefreshFileLineage, &RefreshFileLineage) -> bool| {
        for file in current {
            if mapping.contains_key(&file.path) {
                continue;
            }
            if let Some(prior) = previous
                .iter()
                .find(|prior| !used.contains(&prior.path) && matches(prior, file))
            {
                used.insert(prior.path.clone());
                mapping.insert(file.path.clone(), prior.path.clone());
            }
        }
    };
    assign(&|prior, file| {
        prior.status == FileStatus::Renamed
            && file.status == FileStatus::Renamed
            && prior.old_path.is_some()
            && prior.old_path == file.old_path
    });
    assign(&|prior, file| {
        prior.status == FileStatus::Renamed
            && !matches!(file.status, FileStatus::Renamed | FileStatus::Copied)
            && prior.old_path.as_ref() == Some(&file.path)
    });
    assign(&|prior, file| {
        file.status == FileStatus::Renamed && file.old_path.as_ref() == Some(&prior.path)
    });
    assign(&|prior, file| prior.path == file.path);
    mapping
}

impl From<&DiffRow> for DiffRowIdentity {
    fn from(row: &DiffRow) -> Self {
        Self {
            old_lineno: row.old_lineno,
            new_lineno: row.new_lineno,
            text: if row.semantic_key.is_some() {
                String::new()
            } else {
                row.text.clone()
            },
            kind: row.kind,
            hunk_index: row.hunk_index,
            semantic_key: row.semantic_key.clone(),
            semantic_parent_key: row.semantic_parent_key.clone(),
            semantic_occurrence: row.semantic_occurrence,
            semantic_total: row.semantic_total,
            logical_range: row.logical_range.clone(),
            old_logical_range: row.old_logical_range.clone(),
        }
    }
}

fn resolve_diff_row_identity(
    rows: &[DiffRow],
    identity: Option<&DiffRowIdentity>,
    fallback: usize,
) -> (usize, bool) {
    if rows.is_empty() {
        return (0, false);
    }
    let Some(identity) = identity else {
        return (fallback.min(rows.len() - 1), false);
    };
    // Source location is stronger than projection occurrence: inserting or
    // removing an earlier duplicate renumbers later occurrences.
    if let Some(key) = identity.semantic_key.as_ref() {
        let source_matches = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                row.semantic_key.as_ref() == Some(key)
                    && ((identity.logical_range.is_some()
                        && row.logical_range == identity.logical_range)
                        || (identity.new_lineno.is_some()
                            && row.new_lineno == identity.new_lineno
                            && row.old_lineno == identity.old_lineno))
            })
            .collect::<Vec<_>>();
        if let Some(parent) = identity.semantic_parent_key.as_ref()
            && let Some((index, _)) = source_matches
                .iter()
                .copied()
                .find(|(_, row)| row.semantic_parent_key.as_ref() == Some(parent))
        {
            return (index, true);
        }
        if let [(index, _)] = source_matches.as_slice() {
            return (*index, true);
        }
    }
    if let Some(key) = identity.semantic_key.as_ref() {
        let mut candidates = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.semantic_key.as_ref() == Some(key))
            .collect::<Vec<_>>();
        if let Some(parent) = identity.semantic_parent_key.as_ref() {
            let parent_matches = candidates
                .iter()
                .copied()
                .filter(|(_, row)| row.semantic_parent_key.as_ref() == Some(parent))
                .collect::<Vec<_>>();
            if !parent_matches.is_empty() {
                candidates = parent_matches;
                if let [(index, _)] = candidates.as_slice() {
                    return (*index, true);
                }
            }
        }
        if candidates.len() == identity.semantic_total {
            if let Some((index, _)) = candidates
                .iter()
                .copied()
                .find(|(_, row)| row.semantic_occurrence == identity.semantic_occurrence)
            {
                return (index, true);
            }
        } else if candidates.len() > identity.semantic_total {
            let target = identity
                .logical_range
                .as_ref()
                .map(|range| range.start)
                .or(identity.new_lineno)
                .or(identity.old_lineno)
                .unwrap_or(fallback);
            let mut by_distance = candidates
                .into_iter()
                .map(|(index, row)| {
                    let candidate = row
                        .logical_range
                        .as_ref()
                        .map(|range| range.start)
                        .or(row.new_lineno)
                        .or(row.old_lineno)
                        .unwrap_or(index);
                    (index, candidate.abs_diff(target))
                })
                .collect::<Vec<_>>();
            by_distance.sort_by_key(|(index, distance)| (*distance, index.abs_diff(fallback)));
            if let [best, rest @ ..] = by_distance.as_slice()
                && rest.first().is_none_or(|next| next.1 != best.1)
            {
                return (best.0, true);
            }
        }
    }
    if matches!(
        identity.kind,
        DiffRowKind::ContextFold | DiffRowKind::ExpandGap { .. }
    ) && let Some(range) = identity.logical_range.as_ref()
        && let Some((index, _)) = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let candidate = row.logical_range.as_ref()?;
                let overlap = range.start < candidate.end && candidate.start < range.end;
                overlap.then_some((index, candidate.start.abs_diff(range.start)))
            })
            .min_by_key(|(index, distance)| (*distance, index.abs_diff(fallback)))
    {
        return (index, true);
    }
    if matches!(
        identity.kind,
        DiffRowKind::ContextFold | DiffRowKind::ExpandGap { .. }
    ) && let Some(range) = identity.logical_range.as_ref()
        && let Some(index) = rows.iter().position(|row| {
            row.new_lineno
                .or(row.old_lineno)
                .is_some_and(|line| range.contains(&line))
        })
    {
        return (index, true);
    }
    if identity.semantic_key.is_none()
        && let Some(index) = rows.iter().position(|row| {
            row.kind == identity.kind
                && row.old_lineno == identity.old_lineno
                && row.new_lineno == identity.new_lineno
                && row.text == identity.text
        })
    {
        return (index, true);
    }
    let old_line = identity
        .old_lineno
        .or(identity.new_lineno)
        .unwrap_or(fallback);
    if identity.semantic_key.is_none()
        && let Some((index, _)) = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind == identity.kind && row.text == identity.text)
            .map(|(index, row)| {
                let line = row.old_lineno.or(row.new_lineno).unwrap_or(index);
                (index, line.abs_diff(old_line))
            })
            .min_by_key(|(index, distance)| (*distance, index.abs_diff(fallback)))
    {
        return (index, true);
    }
    if let Some(line) = identity.new_lineno
        && let Some((index, _)) = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let range = row.logical_range.as_ref()?;
                range
                    .contains(&line)
                    .then_some((index, range.end.saturating_sub(range.start)))
            })
            .min_by_key(|(index, width)| (*width, index.abs_diff(fallback)))
    {
        return (index, true);
    }
    if let Some(line) = identity.old_lineno
        && let Some((index, _)) = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let range = row.old_logical_range.as_ref()?;
                range
                    .contains(&line)
                    .then_some((index, range.end.saturating_sub(range.start)))
            })
            .min_by_key(|(index, width)| (*width, index.abs_diff(fallback)))
    {
        return (index, true);
    }
    if matches!(identity.kind, DiffRowKind::HunkHeader) {
        if let Some(range) = identity.logical_range.as_ref()
            && let Some(index) = rows
                .iter()
                .position(|row| row.new_lineno.is_some_and(|line| range.contains(&line)))
        {
            return (index, true);
        }
        if let Some(range) = identity.old_logical_range.as_ref()
            && let Some(index) = rows
                .iter()
                .position(|row| row.old_lineno.is_some_and(|line| range.contains(&line)))
        {
            return (index, true);
        }
    }
    if identity.semantic_key.is_some() {
        return (fallback.min(rows.len() - 1), false);
    }
    if let Some(index) = rows.iter().position(|row| {
        row.kind == identity.kind
            && row.old_lineno == identity.old_lineno
            && row.new_lineno == identity.new_lineno
    }) {
        return (index, true);
    }
    (fallback.min(rows.len() - 1), false)
}

struct ReviewSessionOptions {
    syntax: SyntaxConfig,
    limits: LimitsConfig,
    diff_cues: DiffConfig,
    comment_initial_state: CommentState,
    comment_default_channel: Option<Channel>,
    human_identity: Identity,
    configured_human_name: Option<String>,
    configured_human_email: Option<String>,
    agent_identity: Identity,
    target_author: crate::jj::TargetAuthor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Files,
    Diff,
}

/// Viewport placement required after selecting a located comment.
///
/// This is logical navigation only: terminal geometry remains owned by the
/// TUI viewport controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommentSelection {
    File,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum NavigationPlacement {
    Keep,
    Top,
    Cursor,
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

/// Active single-file range selection in the normal review stream.
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
    pub fn set_target_author(&mut self, author: crate::jj::TargetAuthor) {
        self.target_author_name = author.name.filter(|name| !name.trim().is_empty());
        self.target_author_email = author.email.filter(|email| !email.trim().is_empty());
    }

    /// Stable logical identity of the row currently anchoring the diff top.
    /// Terminal geometry deliberately does not participate in this identity.
    pub(crate) fn diff_top_identity(&self) -> Option<DiffRowIdentity> {
        self.diff_rows_for_file_index(self.selected)
            .get(self.diff_scroll as usize)
            .map(DiffRowIdentity::from)
    }

    pub(crate) fn diff_cursor_identity(&self) -> Option<DiffRowIdentity> {
        self.diff_rows_for_file_index(self.selected)
            .get(self.diff_cursor)
            .map(DiffRowIdentity::from)
    }

    pub(crate) fn stream_top_identity(&self) -> Option<DiffRowIdentity> {
        self.review_stream_rows()
            .get(self.stream_scroll as usize)
            .map(DiffRowIdentity::from)
    }

    pub(crate) fn stream_cursor_identity(&self) -> Option<DiffRowIdentity> {
        self.review_stream_rows()
            .get(self.stream_cursor)
            .map(DiffRowIdentity::from)
    }

    pub(crate) fn restore_stream_top_identity(
        &mut self,
        identity: Option<&DiffRowIdentity>,
        fallback: u16,
    ) -> bool {
        let rows = self.review_stream_rows();
        let (top, recovered) = resolve_diff_row_identity(&rows, identity, fallback as usize);
        self.stream_scroll = top.min(u16::MAX as usize) as u16;
        recovered
    }

    pub(crate) fn restore_stream_cursor_identity(
        &mut self,
        identity: Option<&DiffRowIdentity>,
        fallback: usize,
    ) -> bool {
        let rows = self.review_stream_rows();
        let (cursor, recovered) = resolve_diff_row_identity(&rows, identity, fallback);
        self.stream_cursor = cursor;
        recovered
    }

    /// Re-resolve a durable logical top after the selected file's row
    /// projection changes. Visual continuation remains TUI-owned.
    pub(crate) fn restore_diff_top_identity(
        &mut self,
        identity: Option<&DiffRowIdentity>,
        fallback: u16,
    ) -> bool {
        let rows = self.diff_rows_for_file_index(self.selected);
        let (top, recovered) = resolve_diff_row_identity(&rows, identity, fallback as usize);
        self.diff_scroll = top.min(u16::MAX as usize) as u16;
        recovered
    }

    pub(crate) fn restore_diff_cursor_identity(
        &mut self,
        identity: Option<&DiffRowIdentity>,
        fallback: usize,
    ) -> bool {
        let rows = self.diff_rows_for_file_index(self.selected);
        let (cursor, recovered) = resolve_diff_row_identity(&rows, identity, fallback);
        self.diff_cursor = cursor;
        recovered
    }

    /// App-owned logical viewport anchors for active and inactive files.
    /// The TUI uses this only to pair its visual continuation with a durable
    /// logical identity across refreshes.
    pub(crate) fn logical_viewport_anchors(
        &self,
    ) -> BTreeMap<String, (u16, Option<DiffRowIdentity>)> {
        let mut anchors = self
            .viewport_by_path
            .iter()
            .map(|(path, viewport)| {
                (
                    path.clone(),
                    (viewport.diff_scroll, viewport.top_identity.clone()),
                )
            })
            .collect::<BTreeMap<_, _>>();
        if let Some(file) = self.selected_file() {
            anchors.insert(
                file.path.clone(),
                (self.diff_scroll, self.diff_top_identity()),
            );
        }
        anchors
    }

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
                comment_default_channel: config.comments.default_channel,
                human_identity: config.human_identity(),
                configured_human_name: config.identity.name.clone(),
                configured_human_email: config.identity.email.clone(),
                agent_identity: config.agent_identity(),
                target_author: crate::jj::TargetAuthor::default(),
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
                comment_default_channel: None,
                human_identity: Identity::local_human(),
                configured_human_name: None,
                configured_human_email: None,
                agent_identity: Identity::agent(),
                target_author: crate::jj::TargetAuthor::default(),
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
            comment_default_channel,
            human_identity,
            configured_human_name,
            configured_human_email,
            agent_identity,
            target_author,
        } = options;
        let mut state = state;
        state.normalize_legacy_file_state();
        let ReviewState {
            files,
            comments,
            sessions,
            ..
        } = state;
        let canonical_repo = review::canonical_repo_identity(&repo);
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
            durable_sessions: sessions,
            comment_initial_state,
            comment_default_channel,
            human_identity,
            configured_human_name,
            configured_human_email,
            agent_identity,
            target_author_name: target_author.name,
            target_author_email: target_author.email,
            selected: 0,
            diff_scroll: 0,
            diff_cursor: 0,
            stream_cursor: 0,
            stream_scroll: 0,
            stream_mode: false,
            focus: Focus::Files,
            syntax,
            hide_generated: false,
            viewed_filter: ViewedFilter::default(),
            fold_context: false,
            diff_cues,
            file_pane_visible: true,
            collapsed_dirs: BTreeSet::new(),
            diff_range_selection: None,
            max_diff_lines: limits.max_diff_lines,
            nudge_diff_lines: limits.nudge_diff_lines,
            nudge_files: limits.nudge_files,
            force_rendered: BTreeSet::new(),
            agent_ordering: Vec::new(),
            use_agent_order: true,
            agent_flags: Vec::new(),
            change_diffs: Vec::new(),
            stack_changes: Vec::new(),
            expanded_skim_folds: BTreeSet::new(),
            stream_materialized_files: RefCell::new(BTreeSet::new()),
            stream_cache: RefCell::new(None),
            stream_generation: Cell::new(0),
            durable_generation: Cell::new(0),
            canonical_repo,
            structural_rows_cache: RefCell::new(BTreeMap::new()),
            #[cfg(test)]
            stream_projection_builds: Cell::new(0),
            #[cfg(test)]
            state_snapshots: Cell::new(0),
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
        let previous = self
            .files
            .iter()
            .map(|file| RefreshFileLineage {
                path: file.path.clone(),
                status: file.status,
                old_path: file.old_path.clone(),
            })
            .collect::<Vec<_>>();
        self.replace_diff_without_comment_refresh(target, diff);
        let current = self
            .files
            .iter()
            .map(|file| RefreshFileLineage {
                path: file.path.clone(),
                status: file.status,
                old_path: file.old_path.clone(),
            })
            .collect::<Vec<_>>();
        let mapping = refresh_path_mapping(&previous, &current);
        self.refresh_comment_anchors_for_current_diff(Some(&mapping));
    }

    fn replace_diff_without_comment_refresh(&mut self, target: ReviewTarget, diff: DiffSet) {
        let stream_mode = self.stream_mode;
        // Reconstruction resets the generation counters; carry them forward
        // (plus one) so TUI-side caches keyed on the old generations cannot
        // collide with the fresh session.
        let stream_generation = self.stream_generation.get().wrapping_add(1);
        let durable_generation = self.durable_generation.get().wrapping_add(1);
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
                comment_default_channel: self.comment_default_channel,
                human_identity: self.human_identity.clone(),
                configured_human_name: self.configured_human_name.clone(),
                configured_human_email: self.configured_human_email.clone(),
                agent_identity: self.agent_identity.clone(),
                target_author: crate::jj::TargetAuthor {
                    name: self.target_author_name.clone(),
                    email: self.target_author_email.clone(),
                },
            },
        );
        self.stream_mode = stream_mode;
        self.stream_generation.set(stream_generation);
        self.durable_generation.set(durable_generation);
    }

    /// Like [`Self::replace_diff`], but for background refreshes of the
    /// *same* review: view state (pane visibility, focus, filters, folds,
    /// per-file viewports, and the selected file)
    /// carries over so the reload does not yank the reviewer around.
    pub fn replace_diff_preserving_view(&mut self, target: ReviewTarget, diff: DiffSet) {
        // The active file lives in the public viewport fields until we leave
        // it. Snapshot it before moving the per-file map through replacement.
        self.save_current_viewport();
        let prior_stream = self.review_stream();
        let stream_cursor_id = prior_stream
            .rows
            .get(self.stream_cursor)
            .map(|row| row.id.clone());
        let stream_top_id = prior_stream
            .rows
            .get(self.stream_scroll as usize)
            .map(|row| row.id.clone());
        let prior_stream_cursor = self.stream_cursor;
        let prior_stream_scroll = self.stream_scroll;
        let file_pane_visible = self.file_pane_visible;
        let focus = self.focus;
        let hide_generated = self.hide_generated;
        let viewed_filter = self.viewed_filter;
        let fold_context = self.fold_context;
        let use_agent_order = self.use_agent_order;
        let collapsed_dirs = std::mem::take(&mut self.collapsed_dirs);
        let force_rendered = std::mem::take(&mut self.force_rendered);
        let mut viewports = std::mem::take(&mut self.viewport_by_path);
        let selected_index = self.selected;
        let selected_path = self.selected_file().map(|file| file.path.clone());
        let previous_lineage = self
            .files
            .iter()
            .map(|file| RefreshFileLineage {
                path: file.path.clone(),
                status: file.status,
                old_path: file.old_path.clone(),
            })
            .collect::<Vec<_>>();
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

        self.replace_diff_without_comment_refresh(target, diff);
        let current_lineage = self
            .files
            .iter()
            .map(|file| RefreshFileLineage {
                path: file.path.clone(),
                status: file.status,
                old_path: file.old_path.clone(),
            })
            .collect::<Vec<_>>();
        let path_mapping = refresh_path_mapping(&previous_lineage, &current_lineage);
        self.refresh_comment_anchors_for_current_diff(Some(&path_mapping));

        let mut refresh_changes = Vec::new();
        for file in &mut self.files {
            let new_hunks: Vec<String> = file
                .diff
                .hunks
                .iter()
                .map(|hunk| hunk.content_fingerprint())
                .collect();
            // Freshness tracking retains its existing path-based semantics;
            // rename lineage here is only for logical viewport restoration.
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
        let selected_match = selected_path.as_ref().and_then(|path| {
            self.files.iter().position(|file| {
                path_mapping
                    .get(&file.path)
                    .is_some_and(|prior| prior == path)
            })
        });
        // Use the same deterministic one-to-one lineage mapping for logical
        // viewports. Everything unmapped is stale and deliberately dropped.
        let mut current_viewports = BTreeMap::new();
        for file in &self.files {
            if let Some(prior_path) = path_mapping.get(&file.path)
                && let Some(viewport) = viewports.remove(prior_path)
            {
                current_viewports.insert(file.path.clone(), viewport);
            }
        }
        self.viewport_by_path = current_viewports;
        let visible_selected_match = selected_match.filter(|index| {
            self.files
                .get(*index)
                .is_some_and(|file| self.file_visible(file))
        });
        let restore_index = visible_selected_match.or_else(|| {
            if self.files.is_empty() {
                return None;
            }
            let fallback = selected_match.unwrap_or(selected_index.min(self.files.len() - 1));
            self.files
                .iter()
                .enumerate()
                .filter(|(_, file)| self.file_visible(file))
                .min_by_key(|(index, _)| index.abs_diff(fallback))
                .map(|(index, _)| index)
                .or(Some(fallback))
        });
        if let Some(index) = restore_index {
            self.reveal_file_in_tree(index);
            self.tree_cursor = Some(TreeRowId::File { file_index: index });
            self.selected = index;
            self.restore_current_viewport();
        }
        self.focus = focus;
        if self.focus == Focus::Diff {
            // Row counts may have shifted; keep the cursor on a real row.
            let rows = self.diff_rows_for_file_index(self.selected).len();
            if self.diff_cursor >= rows {
                self.diff_cursor = rows.saturating_sub(1);
            }
            self.ensure_diff_cursor_commentable();
        }
        if self.stream_mode {
            self.materialize_stream_file_reanchored(self.selected);
        }
        let stream = self.review_stream();
        let cursor_by_id = stream_cursor_id
            .as_ref()
            .and_then(|id| stream.rows.iter().position(|row| &row.id == id));
        let numeric_cursor = stream
            .rows
            .get(prior_stream_cursor)
            .filter(|row| row.contains_file(self.selected))
            .map(|_| prior_stream_cursor);
        let fallback_entry = self.stream_entry_for_file(self.selected);
        let next_cursor = cursor_by_id
            .or(numeric_cursor)
            .or(fallback_entry)
            .unwrap_or_else(|| prior_stream_cursor.min(stream.rows.len().saturating_sub(1)));
        let next_row = stream.rows.get(next_cursor).cloned();
        let top_by_id = stream_top_id
            .as_ref()
            .and_then(|id| stream.rows.iter().position(|row| &row.id == id));
        let numeric_top = stream
            .rows
            .get(prior_stream_scroll as usize)
            .filter(|row| row.contains_file(self.selected))
            .map(|_| prior_stream_scroll as usize);
        drop(stream);
        self.stream_cursor = next_cursor;
        if self.stream_mode
            && let Some(row) = next_row.as_ref()
        {
            self.sync_selected_context_from_stream_row(row);
        }
        self.stream_scroll = top_by_id
            .or(numeric_top)
            .or(fallback_entry)
            .unwrap_or(prior_stream_scroll as usize)
            .min(u16::MAX as usize) as u16;
    }

    fn refresh_comment_anchors_for_current_diff(
        &mut self,
        path_mapping: Option<&BTreeMap<String, String>>,
    ) {
        for comment in &mut self.comments {
            let Some(path) = comment.path.clone() else {
                continue;
            };
            let file = path_mapping
                .and_then(|mapping| {
                    self.files.iter().find(|file| {
                        mapping
                            .get(&file.path)
                            .is_some_and(|previous| previous == &path)
                    })
                })
                .or_else(|| self.files.iter().find(|file| file.path == path))
                .or_else(|| {
                    self.files.iter().find(|file| {
                        file.status == FileStatus::Renamed
                            && file.old_path.as_deref() == Some(path.as_str())
                    })
                });
            let Some(file) = file else {
                continue;
            };
            let anchor = match comment.anchor.as_ref() {
                Some(CommentAnchor::Line { side, line, .. }) => {
                    line_anchor_for_side_line(file, *side, *line)
                }
                Some(CommentAnchor::Range { lines, .. }) => comment_anchor_for_sided_lines(
                    file,
                    &lines
                        .iter()
                        .map(|line| (line.side, line.line))
                        .collect::<Vec<_>>(),
                ),
                _ => comment_anchor_for_file_lines(file, comment.line, comment.end_line),
            };
            let anchor = anchor.unwrap_or_else(|| CommentAnchor::File {
                path: file.path.clone(),
                old_path: file.old_path.clone(),
                diff_fingerprint: file.fingerprint.clone(),
            });
            comment.line = anchor.line();
            comment.end_line = match &anchor {
                CommentAnchor::Range { end_line, .. } => Some(*end_line),
                _ => anchor
                    .end_line()
                    .filter(|end_line| Some(*end_line) != anchor.line()),
            };
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
        self.touch_stream_inputs();
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
        self.save_current_viewport();
        state.normalize_legacy_file_state();
        self.persisted_files = state.files;
        self.comments = state.comments;
        self.durable_sessions = state.sessions;
        self.touch_durable_review();
        self.apply_state_files();
        self.ensure_selected_file_visible();
    }

    /// Durable review sessions loaded alongside this review (read-only).
    pub fn durable_sessions(&self) -> &[crate::state::ReviewSession] {
        &self.durable_sessions
    }

    /// Mutable access to the durable review sessions. Acquiring this bumps
    /// the stream and durable generations: attention regions, progress, and
    /// walkthroughs feed the stream projection, and every durable edit must
    /// reach autosave. Do not call this on per-keystroke read paths — use
    /// [`Self::durable_sessions`] or [`Self::active_durable_session`].
    pub fn durable_sessions_mut(&mut self) -> &mut Vec<crate::state::ReviewSession> {
        self.touch_durable_review();
        &mut self.durable_sessions
    }

    /// Canonical (filesystem-resolved) repo identity, computed once at
    /// construction so per-frame lookups never touch the filesystem.
    pub fn canonical_repo(&self) -> &str {
        &self.canonical_repo
    }

    /// The open durable session for this loaded review, resolved against the
    /// cached canonical repo identity. Equivalent to
    /// [`review::active_session_for_loaded_review`] but with zero filesystem
    /// work, so it is safe on per-frame/per-keystroke paths.
    pub fn active_durable_session(&self) -> Option<&crate::state::ReviewSession> {
        let repo = self.canonical_repo.as_str();
        self.durable_sessions.iter().find(|session| {
            session.status == ReviewSessionStatus::Open
                && session.target.repo.as_deref() == Some(repo)
                && session.target.base.as_deref() == Some(self.target.base.as_str())
                && session.target.revision.as_deref() == Some(self.target.rev.as_str())
        })
    }

    /// Mutable variant of [`Self::active_durable_session`]; bumps both
    /// generations like [`Self::durable_sessions_mut`]. Returns `None` when
    /// no open durable session matches this loaded review; use
    /// [`Self::ensure_active_durable_session_mut`] to create one.
    #[allow(dead_code)]
    pub fn active_durable_session_mut(&mut self) -> Option<&mut crate::state::ReviewSession> {
        let index = self.active_durable_session_index()?;
        self.touch_durable_review();
        Some(&mut self.durable_sessions[index])
    }

    pub(crate) fn active_durable_session_index(&self) -> Option<usize> {
        let repo = self.canonical_repo.as_str();
        self.durable_sessions.iter().position(|session| {
            session.status == ReviewSessionStatus::Open
                && session.target.repo.as_deref() == Some(repo)
                && session.target.base.as_deref() == Some(self.target.base.as_str())
                && session.target.revision.as_deref() == Some(self.target.rev.as_str())
        })
    }

    /// Record that a stream-projection input changed (files, force-render
    /// marks, skim folds, stack changes, change diffs, durable sessions).
    /// The next [`Self::review_stream`] access rebuilds the projection.
    pub fn touch_stream_inputs(&self) {
        self.stream_generation
            .set(self.stream_generation.get().wrapping_add(1));
    }

    /// Record that durable review state changed (viewed marks, comments,
    /// durable sessions) so the next autosave pass persists it.
    pub fn touch_durable_state(&self) {
        self.durable_generation
            .set(self.durable_generation.get().wrapping_add(1));
    }

    /// Combined seam for durable-session mutations, which feed both the
    /// stream projection and autosave.
    pub fn touch_durable_review(&self) {
        self.touch_stream_inputs();
        self.touch_durable_state();
    }

    /// Current durable-state generation; autosave compares it against the
    /// generation it last persisted before doing any serialization work.
    pub fn durable_state_generation(&self) -> u64 {
        self.durable_generation.get()
    }

    pub(crate) fn stream_inputs_generation(&self) -> u64 {
        self.stream_generation.get()
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

    /// Move among displayed file rows only, skipping directory rows. The
    /// current file anchors navigation even when focus is in the diff pane.
    pub fn move_file_selection(&mut self, delta: isize) {
        let tree = self.file_tree();
        let file_rows: Vec<_> = tree
            .rows
            .iter()
            .enumerate()
            .filter_map(|(row_index, row)| match row.kind {
                FlatTreeRowKind::File { file_index } => Some((row_index, file_index)),
                FlatTreeRowKind::Directory { .. } => None,
            })
            .collect();
        if file_rows.is_empty() {
            return;
        }
        let selected_tree_path = self
            .selected_file()
            .map(|file| self.tree_path_for_file(file));
        let anchor_row = tree.selected_row_for_file(self.selected).or_else(|| {
            selected_tree_path
                .as_deref()
                .and_then(|path| visible_ancestor_row(&tree, path))
        });
        let next = if let Some(current) = file_rows
            .iter()
            .position(|&(_, index)| index == self.selected)
        {
            (current as isize + delta).clamp(0, file_rows.len() as isize - 1) as usize
        } else if delta > 0 {
            let Some(anchor_row) = anchor_row else {
                return;
            };
            let Some(next) = file_rows
                .iter()
                .position(|&(row_index, _)| row_index > anchor_row)
            else {
                return;
            };
            next
        } else if delta < 0 {
            let Some(anchor_row) = anchor_row else {
                return;
            };
            let Some(next) = file_rows
                .iter()
                .rposition(|&(row_index, _)| row_index < anchor_row)
            else {
                return;
            };
            next
        } else {
            return;
        };
        self.select_file_index(file_rows[next].1);
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

    pub(crate) fn apply_maximum_attention_folding(&mut self) -> FocusFoldingSnapshot {
        let snapshot = FocusFoldingSnapshot {
            fold_context: self.fold_context,
            expanded_skim_folds: self.expanded_skim_folds.clone(),
            context_expansion: self.context_expansion.clone(),
        };
        self.fold_context = true;
        self.expanded_skim_folds.clear();
        self.touch_stream_inputs();
        if !self.context_expansion.is_empty() {
            self.context_expansion.clear();
            self.expansion_epoch = self.expansion_epoch.wrapping_add(1);
            self.rows_cache.borrow_mut().clear();
            self.stream_cache.borrow_mut().take();
        }
        snapshot
    }

    pub(crate) fn restore_attention_folding(&mut self, snapshot: FocusFoldingSnapshot) {
        let expansion_changed = self.context_expansion != snapshot.context_expansion;
        self.fold_context = snapshot.fold_context;
        self.expanded_skim_folds = snapshot.expanded_skim_folds;
        self.context_expansion = snapshot.context_expansion;
        self.touch_stream_inputs();
        if expansion_changed {
            self.expansion_epoch = self.expansion_epoch.wrapping_add(1);
            self.rows_cache.borrow_mut().clear();
            self.stream_cache.borrow_mut().take();
        }
    }

    pub(crate) fn capture_focus_view_and_fold(&mut self) -> FocusViewSnapshot {
        let snapshot = FocusViewSnapshot {
            selected: self.selected,
            diff_scroll: self.diff_scroll,
            diff_cursor: self.diff_cursor,
            stream_scroll: self.stream_scroll,
            stream_cursor: self.stream_cursor,
            focus: self.focus,
            folding: FocusFoldingSnapshot {
                fold_context: self.fold_context,
                expanded_skim_folds: self.expanded_skim_folds.clone(),
                context_expansion: self.context_expansion.clone(),
            },
            viewport_by_path: self.viewport_by_path.clone(),
            tree_cursor: self.tree_cursor.clone(),
            diff_range_selection: self.diff_range_selection.clone(),
            selected_comment_id: self.selected_comment_id.clone(),
        };
        let _ = self.apply_maximum_attention_folding();
        snapshot
    }

    pub(crate) fn restore_focus_view(&mut self, snapshot: FocusViewSnapshot) {
        self.restore_attention_folding(snapshot.folding);
        self.selected = snapshot.selected.min(self.files.len().saturating_sub(1));
        self.diff_scroll = snapshot.diff_scroll;
        self.diff_cursor = snapshot.diff_cursor;
        self.stream_scroll = snapshot.stream_scroll;
        self.stream_cursor = snapshot.stream_cursor;
        self.focus = snapshot.focus;
        self.viewport_by_path = snapshot.viewport_by_path;
        self.tree_cursor = snapshot.tree_cursor;
        self.diff_range_selection = snapshot.diff_range_selection;
        self.selected_comment_id = snapshot.selected_comment_id;
    }

    pub(crate) fn restore_focus_folding_from_view(&mut self, snapshot: FocusViewSnapshot) {
        self.restore_attention_folding(snapshot.folding);
    }

    /// Restore the active Focus-side navigation after the underlying pre-Focus
    /// view has independently passed through a refresh. `prior` is itself
    /// refreshed against the new diff, so these coordinates are current while
    /// the separate Focus restore snapshot remains untouched.
    pub(crate) fn restore_active_focus_view_from(&mut self, prior: &ReviewSession) {
        self.selected = prior.selected.min(self.files.len().saturating_sub(1));
        self.diff_scroll = prior.diff_scroll;
        self.diff_cursor = prior.diff_cursor;
        self.stream_scroll = prior.stream_scroll;
        self.stream_cursor = prior.stream_cursor;
        self.focus = prior.focus;
        self.fold_context = prior.fold_context;
        self.expanded_skim_folds = prior.expanded_skim_folds.clone();
        self.context_expansion = prior.context_expansion.clone();
        self.viewport_by_path = prior.viewport_by_path.clone();
        self.tree_cursor = prior.tree_cursor.clone();
        self.diff_range_selection = prior.diff_range_selection.clone();
        self.selected_comment_id = prior.selected_comment_id.clone();
        self.expansion_epoch = self.expansion_epoch.wrapping_add(1);
        self.rows_cache.borrow_mut().clear();
        self.stream_cache.borrow_mut().take();
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

    /// Toggle measured soft wrapping of diff content. Cursor, selections,
    /// comments, and anchors remain attached to logical rows; only the visual
    /// projection changes.
    pub fn toggle_diff_wrap(&mut self) {
        self.diff_cues.soft_wrap = !self.diff_cues.soft_wrap;
    }

    /// Toggle between the unified and side-by-side diff layouts.
    /// Session-only; config sets the default ([diff] view).
    pub fn toggle_diff_view(&mut self) {
        self.diff_cues.view = match self.diff_cues.view {
            DiffViewModeConfig::Unified => DiffViewModeConfig::SideBySide,
            DiffViewModeConfig::SideBySide => DiffViewModeConfig::Unified,
        };
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

    pub(crate) fn select_file_index(&mut self, index: usize) {
        if index >= self.files.len() {
            return;
        }
        self.reveal_file_in_tree(index);
        self.tree_cursor = Some(TreeRowId::File { file_index: index });
        if index != self.selected {
            self.save_current_viewport();
            self.clear_diff_range_selection();
            self.selected = index;
            self.restore_current_viewport();
        }
        if self.stream_mode {
            self.materialize_stream_file_reanchored(index);
        }
        // File-pane selection is navigation into the one continuous stream,
        // not a request to replace the diff pane's data source.
        if let Some(row) = self.stream_entry_for_file(index) {
            self.stream_cursor = row;
            self.stream_scroll = row.saturating_sub(2).min(u16::MAX as usize) as u16;
        }
    }

    pub(crate) fn select_file_revealed(&mut self, index: usize) -> bool {
        if index >= self.files.len() {
            return false;
        }
        self.reveal_filtered_file(index);
        self.select_file_index(index);
        true
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
    pub fn jump_to_file(&mut self, file_index: usize) -> bool {
        if !self.select_file_revealed(file_index) {
            return false;
        }
        self.focus = Focus::Files;
        true
    }

    pub fn select_diff_row(&mut self, row_index: usize) {
        let rows = self.diff_rows_for_file_index(self.selected);
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
        self.sync_stream_cursor_from_local();
    }

    fn save_current_viewport(&mut self) {
        let Some(path) = self.selected_file().map(|file| file.path.clone()) else {
            return;
        };
        let rows = self.diff_rows_for_file_index(self.selected);
        self.viewport_by_path.insert(
            path,
            FileViewport {
                diff_scroll: self.diff_scroll,
                diff_cursor: self.diff_cursor,
                top_identity: rows
                    .get(self.diff_scroll as usize)
                    .map(DiffRowIdentity::from),
                cursor_identity: rows.get(self.diff_cursor).map(DiffRowIdentity::from),
            },
        );
    }

    fn restore_current_viewport(&mut self) {
        let viewport = self
            .selected_file()
            .and_then(|file| self.viewport_by_path.get(&file.path).cloned())
            .unwrap_or_default();
        let rows = self.diff_rows_for_file_index(self.selected);
        let (top, _) = resolve_diff_row_identity(
            &rows,
            viewport.top_identity.as_ref(),
            viewport.diff_scroll as usize,
        );
        let (cursor, _) = resolve_diff_row_identity(
            &rows,
            viewport.cursor_identity.as_ref(),
            viewport.diff_cursor,
        );
        self.diff_scroll = top.min(u16::MAX as usize) as u16;
        self.diff_cursor = cursor;
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
    }

    /// Fold one-release legacy overlay drafts into durable comments. Pending
    /// drafts keep their ids so a failed overlay cleanup can be retried
    /// without duplication; accepted/discarded entries are intentionally not
    /// recreated.
    pub(crate) fn fold_legacy_agent_drafts(&mut self, overlay: &AgentOverlay) -> usize {
        let pending = overlay
            .legacy_drafts
            .iter()
            .filter(|draft| draft.state == LegacyDraftState::Pending)
            .filter(|draft| !self.comments.iter().any(|comment| comment.id == draft.id))
            .cloned()
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return 0;
        }
        let session_index = self.ensure_active_review_session_index();
        let session_id = self.durable_sessions[session_index].id.clone();
        let now = chrono::Utc::now();
        let mut folded = 0;
        for draft in pending {
            let anchor = self
                .files
                .iter()
                .find(|file| file.path == draft.path)
                .and_then(|file| comment_anchor_for_file_lines(file, draft.line, draft.line));
            let observation = anchor.clone().map(|anchor| {
                crate::provenance::CommentObservation::new(
                    self.provenance_snapshot(session_index),
                    Some(anchor),
                )
            });
            let comment = Comment {
                id: draft.id,
                session_id: Some(session_id.clone()),
                path: Some(draft.path),
                line: draft.line,
                end_line: None,
                anchor,
                observation,
                body: draft.body,
                kind: None,
                action: None,
                state: CommentState::Draft,
                author: self.agent_identity.clone(),
                channel: Channel::Onboarding,
                source_comment_id: None,
                replies: Vec::new(),
                created_at: now,
                updated_at: Some(now),
            };
            self.comments.push(comment);
            folded += 1;
        }
        if folded > 0 {
            self.durable_sessions[session_index].updated_at = Some(now);
        }
        folded
    }

    /// Durable agent-authored comments still awaiting human triage.
    pub fn pending_agent_drafts(&self) -> Vec<Comment> {
        let Some(active_session_id) = self
            .active_durable_session()
            .map(|session| session.id.as_str())
        else {
            return Vec::new();
        };
        self.comments
            .iter()
            .filter(|comment| {
                comment.author.kind == AuthorKind::Agent
                    && comment.state == CommentState::Draft
                    && comment.channel == Channel::Onboarding
                    && comment.belongs_to_session(active_session_id)
            })
            .cloned()
            .collect()
    }

    /// Accept a durable agent draft, preserving its id and anchor while the
    /// TUI-selected channel resolves its lifecycle.
    pub fn accept_agent_draft(
        &mut self,
        draft: &Comment,
        body: String,
        channel: Channel,
    ) -> Option<String> {
        if body.trim().is_empty() {
            return None;
        }
        let path = draft.path.as_deref()?;
        let file_index = self.files.iter().position(|file| file.path == path)?;
        self.reveal_filtered_file(file_index);
        self.select_file_revealed(file_index);
        let index = self.ensure_active_review_session_index();
        review::edit_comment(
            &mut self.durable_sessions[index],
            &mut self.comments,
            &draft.id,
            review::CommentEdits {
                body: Some(body),
                channel: Some(channel),
                ..Default::default()
            },
        )
        .ok()?;
        let accepted_state = match channel {
            Channel::Delegation | Channel::Collaboration => CommentState::Todo,
            Channel::Onboarding => CommentState::Resolved,
            Channel::Note => CommentState::Draft,
        };
        let accepted = review::set_comment_state(
            &mut self.durable_sessions[index],
            &mut self.comments,
            &draft.id,
            accepted_state,
        )
        .ok()?;
        Some(accepted.id)
    }

    /// Discard an agent draft by deleting the durable draft comment.
    pub fn discard_agent_draft(&mut self, draft_id: &str) -> bool {
        if !self
            .pending_agent_drafts()
            .iter()
            .any(|draft| draft.id == draft_id)
        {
            return false;
        }
        self.delete_comment(draft_id)
    }

    /// Jump to a durable review target in the loaded diff. File-only targets
    /// land at the top; line/range targets use the normal projected-row index.
    pub fn jump_to_review_target(
        &mut self,
        target: &StateReviewTarget,
    ) -> Option<NavigationPlacement> {
        let path = target.file.as_deref()?;
        let file_index = self.files.iter().position(|file| file.path == path)?;
        let row_index = target.line.and_then(|start_line| {
            self.projected_row_for_range(
                file_index,
                start_line,
                target.end_line.unwrap_or(start_line),
            )
        });
        if target.line.is_some() && row_index.is_none() {
            return None;
        }
        self.reveal_filtered_file(file_index);
        self.select_file_revealed(file_index);
        let Some(_) = target.line else {
            self.focus = Focus::Files;
            self.diff_scroll = 0;
            return Some(NavigationPlacement::Top);
        };
        self.jump_to_diff_row(row_index?);
        Some(NavigationPlacement::Cursor)
    }

    /// A nudge for large changes: when the diff exceeds the size thresholds
    /// and no agent has organized the review yet (no durable walkthrough or
    /// overlay ordering), point at the harness/CLI collaboration flow.
    /// `None` when the change is small, nudging is disabled, or an agent
    /// already structured the review.
    pub fn large_change_nudge(&self) -> Option<String> {
        let has_walkthrough = self.active_durable_session().is_some_and(|session| {
            session
                .walkthroughs
                .iter()
                .any(|walkthrough| !walkthrough.steps.is_empty())
        });
        if has_walkthrough || !self.agent_ordering.is_empty() {
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
            "large change ({files} files, {lines} changed lines) — an agent harness can organize this review (attention regions + walkthrough); see docs/harness-setup.md"
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
    pub fn jump_to_flag(&mut self, flag: &AgentFlag) -> Option<NavigationPlacement> {
        let file_index = self.files.iter().position(|file| file.path == flag.path)?;
        let row_index = flag
            .line
            .and_then(|line| self.projected_row_for_new_line(file_index, line, true));
        if flag.line.is_some() && row_index.is_none() {
            return None;
        }
        self.reveal_filtered_file(file_index);
        self.select_file_index(file_index);
        let Some(_line) = flag.line else {
            self.focus = Focus::Files;
            self.diff_scroll = 0;
            return Some(NavigationPlacement::Top);
        };
        if let Some(row_index) = row_index {
            self.jump_to_diff_row(row_index);
            Some(NavigationPlacement::Cursor)
        } else {
            None
        }
    }

    /// Resolve a logical new-side destination into the current row
    /// projection. Validity comes from the parsed diff, not from presentation:
    /// folded context and large-diff placeholders may hide the exact row.
    fn projected_row_for_new_line(
        &self,
        file_index: usize,
        line: usize,
        exact: bool,
    ) -> Option<usize> {
        let file = self.files.get(file_index)?;
        let logical_exists = file
            .diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|diff| {
                diff.new_lineno.is_some_and(|candidate| {
                    if exact {
                        candidate == line
                    } else {
                        candidate >= line
                    }
                })
            });
        if !logical_exists {
            return None;
        }

        let rows = self.diff_rows_for_file_index(file_index);
        rows.iter()
            .position(|row| {
                row.new_lineno.is_some_and(|candidate| {
                    if exact {
                        candidate == line
                    } else {
                        candidate >= line
                    }
                })
            })
            .or_else(|| {
                rows.iter()
                    .position(|row| matches!(row.kind, DiffRowKind::Placeholder))
            })
            .or_else(|| {
                rows.iter()
                    .enumerate()
                    .filter_map(|(index, row)| row.new_lineno.map(|candidate| (index, candidate)))
                    .min_by_key(|(_, candidate)| candidate.abs_diff(line))
                    .map(|(index, _)| index)
            })
    }

    pub(crate) fn projected_row_for_range(
        &self,
        file_index: usize,
        start: usize,
        end: usize,
    ) -> Option<usize> {
        let file = self.files.get(file_index)?;
        let (start, end) = (start.min(end), start.max(end));
        let lines = file
            .diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .collect::<Vec<_>>();
        let source = lines
            .iter()
            .copied()
            .find(|line| {
                line.new_lineno
                    .is_some_and(|line| (start..=end).contains(&line))
            })
            .or_else(|| {
                lines.iter().copied().find(|line| {
                    line.old_lineno
                        .is_some_and(|line| (start..=end).contains(&line))
                })
            })?;
        let rows = self.diff_rows_for_file_index(file_index);
        rows.iter()
            .position(|row| {
                row.kind == DiffRowKind::DiffLine(source.kind)
                    && row.old_lineno == source.old_lineno
                    && row.new_lineno == source.new_lineno
                    && row.text == source.text
            })
            .or_else(|| {
                source.new_lineno.and_then(|line| {
                    rows.iter().position(|row| {
                        row.logical_range
                            .as_ref()
                            .is_some_and(|range| range.contains(&line))
                    })
                })
            })
            .or_else(|| {
                rows.iter()
                    .position(|row| matches!(row.kind, DiffRowKind::Placeholder))
            })
    }

    pub(crate) fn projected_row_for_side_line(
        &self,
        file_index: usize,
        side: DiffSide,
        line: usize,
    ) -> Option<usize> {
        self.files.get(file_index)?;
        let rows = self.diff_rows_for_file_index(file_index);
        rows.iter()
            .position(|row| match side {
                DiffSide::Old => row.old_lineno == Some(line),
                DiffSide::New => row.new_lineno == Some(line),
            })
            .or_else(|| {
                rows.iter().position(|row| {
                    match side {
                        DiffSide::Old => row.old_logical_range.as_ref(),
                        DiffSide::New => row.logical_range.as_ref(),
                    }
                    .is_some_and(|range| range.contains(&line))
                })
            })
            .or_else(|| {
                rows.iter()
                    .position(|row| matches!(row.kind, DiffRowKind::Placeholder))
            })
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
        self.touch_durable_state();
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
        self.touch_durable_state();
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
        self.touch_durable_state();
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
        self.touch_durable_state();
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

    /// Whole-file fold-acknowledge viewed effect.
    ///
    /// docs/attention.md requires one whole-file viewed-effect service across
    /// every adapter. The durable `FileState` mutation — viewed fingerprint
    /// insert plus caught-up fingerprint normalization — is performed by the
    /// exact `attention::apply_whole_file_viewed_effects` call CLI and MCP
    /// make, applied here to this session's persisted file records. The
    /// in-memory session flags (viewed/caught-up plus the accumulated
    /// `changed_since_look`/`changed_hunks` freshness clear) are then
    /// projected through the TUI's single mark-viewed helper for the same
    /// path set, so both effects happen in this one place and the persisted
    /// outcome stays byte-identical to the CLI/MCP path.
    pub(crate) fn apply_whole_file_viewed_effects(&mut self, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let files = self
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let mut state = ReviewState {
            files: std::mem::take(&mut self.persisted_files),
            ..ReviewState::default()
        };
        crate::attention::apply_whole_file_viewed_effects(&mut state, &files, paths);
        self.persisted_files = state.files;
        self.touch_durable_state();
        let viewed = paths.iter().map(String::as_str).collect::<BTreeSet<_>>();
        self.mark_files_viewed_where(|file| viewed.contains(file.path.as_str()));
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
        self.touch_durable_state();
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

    fn reveal_filtered_file(&mut self, file_index: usize) {
        let Some(file) = self.files.get(file_index) else {
            return;
        };
        if file.generated {
            self.hide_generated = false;
        }
        if !self.viewed_filter.admits(file.viewed || file.caught_up) {
            self.viewed_filter = ViewedFilter::All;
        }
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
            self.clear_diff_range_selection();
        }
    }

    #[cfg(test)]
    pub fn scroll_diff(&mut self, delta: i16) {
        self.diff_scroll = if delta.is_negative() {
            self.diff_scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.diff_scroll.saturating_add(delta as u16)
        };
        self.clamp_diff_scroll();
    }

    #[cfg(test)]
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
            if let Some(index) = self.selectable_stream_entry_for_file(self.selected) {
                self.select_stream_row(index, true);
            }
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

    pub fn move_diff_cursor(&mut self, delta: isize) -> bool {
        let rows = self.diff_rows_for_selected_file();
        if rows.is_empty() {
            return false;
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
        if rows
            .get(cursor)
            .and_then(|row| row.anchor.as_ref())
            .is_none()
        {
            cursor = self.diff_cursor;
        }

        if cursor == self.diff_cursor {
            return false;
        }
        self.diff_cursor = cursor;
        if self.diff_cursor < self.diff_scroll as usize {
            self.diff_scroll = self.diff_cursor as u16;
        } else if self.diff_cursor > self.diff_scroll as usize + DIFF_CURSOR_SCROLL_MARGIN {
            self.diff_scroll = self.diff_cursor.saturating_sub(DIFF_CURSOR_SCROLL_MARGIN) as u16;
        }
        self.sync_stream_cursor_from_local();
        true
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
    pub fn jump_to_changed_symbol(&mut self, delta: isize) -> bool {
        let before = (
            self.selected,
            self.diff_cursor,
            self.diff_scroll,
            self.focus,
        );
        let targets = self.changed_symbol_targets();
        if targets.is_empty() {
            return false;
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
            if self.focus == Focus::Diff && target.row_index == self.diff_cursor {
                return false;
            }
            self.jump_to_diff_row(target.row_index);
        }
        before
            != (
                self.selected,
                self.diff_cursor,
                self.diff_scroll,
                self.focus,
            )
    }

    pub fn jump_to_changed_hunk(&mut self, delta: isize) -> bool {
        let before = (
            self.selected,
            self.diff_cursor,
            self.diff_scroll,
            self.focus,
        );
        if self.files.is_empty() {
            return false;
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
            let rows = self.diff_rows_for_file_index(file_index);
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
            if targets.is_empty()
                && let Some(placeholder) = rows
                    .iter()
                    .position(|row| matches!(row.kind, DiffRowKind::Placeholder))
            {
                targets.push(placeholder);
            }
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
                if file_index != self.selected {
                    self.select_file_revealed(file_index);
                }
                if file_index == start_file
                    && self.focus == Focus::Diff
                    && target == self.diff_cursor
                {
                    return false;
                }
                self.focus = Focus::Diff;
                self.diff_cursor = target;
                self.diff_scroll = self.diff_cursor.saturating_sub(5) as u16;
                self.sync_stream_cursor_from_local();
                return before
                    != (
                        self.selected,
                        self.diff_cursor,
                        self.diff_scroll,
                        self.focus,
                    );
            }
        }
        before
            != (
                self.selected,
                self.diff_cursor,
                self.diff_scroll,
                self.focus,
            )
    }

    /// Move to a diff row (from outline/symbol navigation), focusing the diff
    /// pane and scrolling the row into view.
    pub fn jump_to_diff_row(&mut self, row_index: usize) {
        self.select_diff_row(row_index);
        self.diff_scroll = self.diff_cursor.saturating_sub(5) as u16;
    }

    fn sync_stream_cursor_from_local(&mut self) {
        let selected = self.selected;
        let local = self.diff_cursor;
        let stream = self.review_stream();
        if let Some(index) = stream
            .rows
            .iter()
            .position(|row| row.file_index == Some(selected) && row.local_row == Some(local))
        {
            self.stream_cursor = index;
        }
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

    /// Canonical full-width card owner for a durable comment. Rendering,
    /// navigation, hit testing, and reflow all resolve through this endpoint.
    /// Range cards belong to their final visual row (the right/new endpoint
    /// when present), while line cards belong to their exact anchored row.
    pub(crate) fn comment_card_owner_for_file(
        &self,
        file_index: usize,
        comment: &Comment,
    ) -> Option<usize> {
        let file = self.files.get(file_index)?;
        let target_path = comment
            .anchor
            .as_ref()
            .map(CommentAnchor::path)
            .or(comment.path.as_deref())?;
        if target_path != file.path {
            return None;
        }
        let rows = self.diff_rows_for_file_index(file_index);
        match &comment.anchor {
            Some(anchor @ CommentAnchor::Line { side, line, .. }) => rows
                .iter()
                .position(|row| row.anchor.as_ref() == Some(anchor))
                .or_else(|| self.projected_row_for_side_line(file_index, *side, *line)),
            Some(CommentAnchor::Range { lines, .. }) => rows
                .iter()
                .rposition(|row| {
                    row.anchor.as_ref().is_some_and(|anchor| {
                        matches!(
                            anchor,
                            CommentAnchor::Line { line_fingerprint, .. }
                                if lines.iter().any(|line| &line.line_fingerprint == line_fingerprint)
                        )
                    })
                })
                .or_else(|| {
                    lines.iter().rev().find_map(|line| {
                        self.projected_row_for_side_line(file_index, line.side, line.line)
                    })
                }),
            _ => None,
        }
    }

    pub(crate) fn selected_comment_card_owner(&self, comment: &Comment) -> Option<usize> {
        self.comment_card_owner_for_file(self.selected, comment)
    }

    /// Canonical owner for a walkthrough target. Walkthrough coordinates are
    /// new-side coordinates; a range card is placed after its final new-side
    /// row, with an anchor-line fallback for legacy targets.
    pub(crate) fn walkthrough_card_owner_for_file(
        &self,
        file_index: usize,
        target: &StateReviewTarget,
    ) -> Option<usize> {
        let file = self.files.get(file_index)?;
        if target.file.as_deref() != Some(file.path.as_str()) {
            return None;
        }
        let rows = self.diff_rows_for_file_index(file_index);
        let Some(start) = target.line else {
            return rows.iter().position(|row| row.anchor.is_some());
        };
        let end = target.end_line.unwrap_or(start);
        rows.iter()
            .enumerate()
            .filter(|(_, row)| {
                row.new_lineno
                    .is_some_and(|line| line >= start && line <= end)
            })
            .map(|(index, _)| index)
            .next_back()
            .or_else(|| {
                rows.iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        row.anchor
                            .as_ref()
                            .and_then(CommentAnchor::line)
                            .is_some_and(|line| line >= start && line <= end)
                    })
                    .map(|(index, _)| index)
                    .next_back()
            })
    }

    pub(crate) fn selected_walkthrough_card_owner(
        &self,
        target: &StateReviewTarget,
    ) -> Option<usize> {
        self.walkthrough_card_owner_for_file(self.selected, target)
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

    pub fn move_to_comment(&mut self, delta: isize) -> Option<CommentSelection> {
        if self.comments.is_empty() {
            return None;
        }

        let current = self.current_comment_index();
        let start = match (current, delta.is_negative()) {
            (Some(index), false) => (index + 1) % self.comments.len(),
            (Some(index), true) => (index + self.comments.len() - 1) % self.comments.len(),
            (None, false) => 0,
            (None, true) => self.comments.len() - 1,
        };
        for offset in 0..self.comments.len() {
            let next = if delta.is_negative() {
                (start + self.comments.len() - offset) % self.comments.len()
            } else {
                (start + offset) % self.comments.len()
            };
            if current == Some(next) && self.comments.len() == 1 {
                return None;
            }
            if let Some(selection) = self.select_comment(next) {
                return Some(selection);
            }
        }
        None
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
        if self.stream_mode && self.focus == Focus::Diff {
            return self.stream_comment_card_owner(comment) == Some(self.stream_cursor);
        }
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

    #[cfg(test)]
    pub fn update_comment_body(&mut self, id: &str, body: String) -> bool {
        let channel = self
            .comments
            .iter()
            .find(|comment| comment.id == id)
            .map(|comment| comment.channel)
            .unwrap_or(Channel::Note);
        self.update_comment_body_and_channel(id, body, channel)
    }

    pub fn update_comment_body_and_channel(
        &mut self,
        id: &str,
        body: String,
        channel: Channel,
    ) -> bool {
        let index = self.ensure_active_review_session_index();
        review::edit_comment(
            &mut self.durable_sessions[index],
            &mut self.comments,
            id,
            review::CommentEdits {
                body: Some(body),
                channel: Some(channel),
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
        review::set_comment_state(
            &mut self.durable_sessions[index],
            &mut self.comments,
            id,
            next,
        )
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
            &mut self.durable_sessions[index],
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
            &mut self.durable_sessions[index],
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
    pub(crate) fn select_comment_by_id(&mut self, id: &str) -> Option<CommentSelection> {
        if let Some(index) = self.comments.iter().position(|comment| comment.id == id) {
            return self.select_comment(index);
        }
        None
    }

    pub fn delete_comment(&mut self, id: &str) -> bool {
        let session_index = self.ensure_active_review_session_index();
        if review::delete_comment(
            &mut self.durable_sessions[session_index],
            &mut self.comments,
            id,
        )
        .is_err()
        {
            return false;
        }
        if self.selected_comment_id.as_deref() == Some(id) {
            self.selected_comment_id = None;
        }
        true
    }

    #[cfg(test)]
    pub fn add_comment(&mut self, body: String) {
        let channel = if self.comment_initial_state == CommentState::Todo {
            Channel::Delegation
        } else {
            Channel::Note
        };
        self.add_comment_in_channel(body, channel);
    }

    #[cfg(test)]
    pub fn add_comment_in_channel(&mut self, body: String, channel: Channel) -> bool {
        self.add_comment_in_channel_linked(body, channel, None)
    }

    pub fn add_comment_in_channel_linked(
        &mut self,
        body: String,
        channel: Channel,
        source_comment_id: Option<String>,
    ) -> bool {
        match self.focus {
            Focus::Files => self.add_file_comment_with_source(body, channel, source_comment_id),
            Focus::Diff => {
                if let Some(anchor) = self.selected_comment_anchor() {
                    let added = self.add_comment_with_anchor_channel_and_source(
                        body,
                        anchor,
                        channel,
                        source_comment_id,
                    );
                    self.clear_diff_range_selection();
                    added
                } else {
                    false
                }
            }
        }
    }

    #[cfg(test)]
    pub fn add_file_comment(&mut self, body: String) {
        let channel = if self.comment_initial_state == CommentState::Todo {
            Channel::Delegation
        } else {
            Channel::Note
        };
        self.add_file_comment_in_channel(body, channel);
    }

    #[cfg(test)]
    pub fn add_file_comment_in_channel(&mut self, body: String, channel: Channel) -> bool {
        self.add_file_comment_with_source(body, channel, None)
    }

    fn add_file_comment_with_source(
        &mut self,
        body: String,
        channel: Channel,
        source_comment_id: Option<String>,
    ) -> bool {
        let Some(file) = self.selected_file() else {
            return false;
        };
        let anchor = CommentAnchor::File {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            diff_fingerprint: file.fingerprint.clone(),
        };
        self.add_comment_with_anchor_channel_and_source(body, anchor, channel, source_comment_id)
    }

    fn add_comment_with_anchor_channel_and_source(
        &mut self,
        body: String,
        anchor: CommentAnchor,
        channel: Channel,
        source_comment_id: Option<String>,
    ) -> bool {
        let index = self.ensure_active_review_session_index();
        let session_id = self.durable_sessions[index].id.clone();
        let observation = crate::provenance::CommentObservation::new(
            self.provenance_snapshot(index),
            Some(anchor.clone()),
        );
        let state = self.initial_state_for_channel(channel);
        let added = review::add_comment(
            &mut self.durable_sessions[index],
            &mut self.comments,
            review::NewComment {
                session_id,
                path: Some(anchor.path().to_owned()),
                line: anchor.line(),
                end_line: match &anchor {
                    CommentAnchor::Range { end_line, .. } => Some(*end_line),
                    _ => anchor
                        .end_line()
                        .filter(|end_line| Some(*end_line) != anchor.line()),
                },
                anchor: Some(anchor),
                observation: Some(observation),
                body,
                kind: None,
                action: None,
                state,
                author: self.human_identity.clone(),
                channel,
            },
        );
        match added {
            Ok(comment) => {
                if let Some(source_comment_id) = source_comment_id
                    && let Some(saved) = self
                        .comments
                        .iter_mut()
                        .find(|saved| saved.id == comment.id)
                {
                    saved.source_comment_id = Some(source_comment_id);
                }
                true
            }
            Err(_) => false,
        }
    }

    /// Add an agent-authored durable draft for TUI triage. This is the shared
    /// live ACP path; the overlay is reserved for non-comment suggestions.
    pub fn add_agent_draft(
        &mut self,
        path: String,
        line: Option<usize>,
        body: String,
    ) -> Option<Comment> {
        let mut state = self.to_state();
        let comment = self
            .add_agent_draft_to_state(&mut state, path, line, body)
            .ok()?;
        self.apply_review_state(state);
        Some(comment)
    }

    /// Apply an agent draft to an arbitrary latest-state snapshot using the
    /// same durable comment service as CLI/MCP/TUI. The loaded diff supplies
    /// the anchor and immutable observation; `state` remains the persistence
    /// authority.
    pub fn add_agent_draft_to_state(
        &self,
        state: &mut ReviewState,
        path: String,
        line: Option<usize>,
        body: String,
    ) -> color_eyre::eyre::Result<Comment> {
        let target = review::SessionTargetSpec {
            repo: Some(self.canonical_repo.clone()),
            base: Some(self.target.base.clone()),
            revision: Some(self.target.rev.clone()),
            revset: Some(self.target.to_string()),
        };
        let session_id = review::ensure_session(state, &target, None).id.clone();
        let session_index = state
            .sessions
            .iter()
            .position(|session| session.id == session_id)
            .expect("ensured session must exist");
        let anchor = self
            .files
            .iter()
            .find(|file| file.path == path)
            .and_then(|file| comment_anchor_for_file_lines(file, line, line));
        let observation = crate::provenance::CommentObservation::new(
            crate::provenance::SnapshotEvidence::capture(
                chrono::Utc::now(),
                session_id.clone(),
                state.sessions[session_index].target.clone(),
                self.files.iter().map(|file| &file.diff),
            ),
            anchor.clone(),
        );
        review::add_comment(
            &mut state.sessions[session_index],
            &mut state.comments,
            review::NewComment {
                session_id,
                path: Some(path),
                line,
                end_line: None,
                anchor,
                observation: Some(observation),
                body,
                kind: None,
                action: None,
                state: CommentState::Draft,
                author: self.agent_identity.clone(),
                channel: Channel::Onboarding,
            },
        )
    }

    #[cfg(test)]
    pub fn add_general_comment(&mut self, body: String) {
        let channel = if self.comment_initial_state == CommentState::Todo {
            Channel::Delegation
        } else {
            Channel::Note
        };
        self.add_general_comment_in_channel(body, channel);
    }

    pub fn add_general_comment_in_channel(&mut self, body: String, channel: Channel) -> bool {
        let index = self.ensure_active_review_session_index();
        let session_id = self.durable_sessions[index].id.clone();
        let observation =
            crate::provenance::CommentObservation::new(self.provenance_snapshot(index), None);
        let state = self.initial_state_for_channel(channel);
        review::add_comment(
            &mut self.durable_sessions[index],
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
                state,
                author: self.human_identity.clone(),
                channel,
            },
        )
        .is_ok()
    }

    fn initial_state_for_channel(&self, channel: Channel) -> CommentState {
        if self.comment_initial_state == CommentState::Todo && !channel.permits_todo() {
            CommentState::Draft
        } else {
            self.comment_initial_state
        }
    }

    fn provenance_snapshot(&self, session_index: usize) -> crate::provenance::SnapshotEvidence {
        let durable = &self.durable_sessions[session_index];
        crate::provenance::SnapshotEvidence::capture(
            chrono::Utc::now(),
            durable.id.clone(),
            durable.target.clone(),
            self.files.iter().map(|file| &file.diff),
        )
    }

    pub fn ready_all_draft_comments(&mut self) -> review::ReadyCommentsResult {
        let index = self.ensure_active_review_session_index();
        review::ready_all_drafts(&mut self.durable_sessions[index], &mut self.comments)
            .unwrap_or_default()
    }

    fn ensure_active_review_session_index(&mut self) -> usize {
        // Callers acquire this index to mutate the durable session (and
        // usually comments alongside); bump the generations once here so
        // every such seam invalidates the stream cache and reaches autosave.
        self.touch_durable_review();
        if let Some(index) = self.active_durable_session_index() {
            return index;
        }
        let mut state = ReviewState {
            sessions: std::mem::take(&mut self.durable_sessions),
            ..ReviewState::default()
        };
        let spec = review::SessionTargetSpec {
            repo: Some(self.canonical_repo.clone()),
            base: Some(self.target.base.clone()),
            revision: Some(self.target.rev.clone()),
            revset: None,
        };
        let id = review::ensure_session(&mut state, &spec, None).id.clone();
        self.durable_sessions = state.sessions;
        self.durable_sessions
            .iter()
            .position(|session| session.id == id)
            .expect("ensured session must exist")
    }

    /// The open durable session for this loaded review, creating one when
    /// none exists yet. Mutation seam: bumps the stream and durable
    /// generations like [`Self::durable_sessions_mut`].
    pub fn ensure_active_durable_session_mut(&mut self) -> &mut crate::state::ReviewSession {
        let index = self.ensure_active_review_session_index();
        &mut self.durable_sessions[index]
    }

    fn current_comment_index(&self) -> Option<usize> {
        self.selected_comment_index()
    }

    fn select_comment(&mut self, index: usize) -> Option<CommentSelection> {
        let comment = self.comments.get(index).cloned()?;
        let target_path = comment
            .anchor
            .as_ref()
            .map(CommentAnchor::path)
            .or(comment.path.as_deref())?;
        let file_index = self
            .files
            .iter()
            .position(|file| file.path == target_path)?;

        let line_anchored = matches!(
            comment.anchor,
            Some(CommentAnchor::Line { .. } | CommentAnchor::Range { .. })
        );
        if self.stream_mode {
            let owner = self.stream_comment_card_owner(&comment);
            if line_anchored && owner.is_none() {
                return None;
            }
            self.select_file_revealed(file_index);
            self.selected_comment_id = Some(comment.id.clone());
            if line_anchored {
                // File materialization can reshape the stream. Resolve the
                // card owner again, then let stream landing rebind both the
                // stream and file-local cursors from the stable anchor.
                let owner = self.stream_comment_card_owner(&comment)?;
                self.select_stream_row(owner, false);
                self.stream_scroll = self.stream_cursor.saturating_sub(5) as u16;
                return Some(CommentSelection::Diff);
            }
            self.focus = Focus::Files;
            return Some(CommentSelection::File);
        }

        let row_index = self.comment_card_owner_for_file(file_index, &comment);
        if line_anchored && row_index.is_none() {
            return None;
        }

        self.select_file_revealed(file_index);
        self.selected_comment_id = Some(comment.id.clone());
        if let Some(row_index) = row_index {
            self.focus = Focus::Diff;
            self.diff_cursor = row_index;
            self.diff_scroll = self.diff_cursor.saturating_sub(5) as u16;
            self.sync_stream_cursor_from_local();
            Some(CommentSelection::Diff)
        } else {
            self.focus = Focus::Files;
            Some(CommentSelection::File)
        }
    }

    fn ensure_diff_cursor_commentable(&mut self) {
        let rows = self.diff_rows_for_file_index(self.selected);
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

    /// Test-only count of session-scale durable-state snapshots; see
    /// [`Self::to_state`].
    #[cfg(test)]
    pub fn state_snapshot_count(&self) -> usize {
        self.state_snapshots.get()
    }

    /// Snapshot the persistable review state (viewed marks and comments)
    /// without consuming the session, so the TUI can autosave mid-session.
    /// Session-scale work: clones every file record, comment, and durable
    /// session. Per-keystroke paths must gate on
    /// [`Self::durable_state_generation`] before calling this.
    pub fn to_state(&self) -> ReviewState {
        #[cfg(test)]
        self.state_snapshots.set(self.state_snapshots.get() + 1);
        ReviewState {
            meta: ReviewStateMeta {
                version: REVIEW_STATE_SCHEMA_VERSION,
                base: Some(self.target.base.clone()),
                revision: Some(self.target.rev.clone()),
                repo: Some(self.canonical_repo.clone()),
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
            sessions: self.durable_sessions.clone(),
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

    /// TUI review-stream summary. File viewed marks remain in the tree and
    /// artifacts, while the footer reports the work units that salience asks
    /// the reviewer to visit or explicitly dismiss.
    pub fn coverage_summary_line(&self) -> String {
        let generated = self.files.iter().filter(|file| file.generated).count();
        let additions: usize = self.files.iter().map(|file| file.additions).sum();
        let deletions: usize = self.files.iter().map(|file| file.deletions).sum();
        format!(
            "{} files ({}, {generated} generated/noisy), +{additions}/-{deletions}, {}",
            self.files.len(),
            self.coverage().label(),
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
        config::{AgentConfig, Config, IdentityConfig},
        diff::DiffSet,
        jj::ReviewTarget,
        state::ReviewState,
        syntax::{HighlightKind, SyntaxSpan},
    };

    #[test]
    fn focusing_the_files_pane_reshows_it() {
        let mut session = session();
        session.toggle_focus();
        session.file_pane_visible = false;
        assert!(!session.file_pane_visible);

        // Never trap: tab back to the files pane brings it back.
        session.toggle_focus();

        assert_eq!(session.focus, Focus::Files);
        assert!(session.file_pane_visible);
    }

    #[test]
    fn configured_identities_stamp_executed_tui_human_comment_and_agent_draft_paths() {
        let config = Config {
            identity: IdentityConfig {
                name: Some("Human Reviewer".into()),
                ..Default::default()
            },
            agent: AgentConfig {
                name: Some("Review Bot".into()),
            },
            ..Default::default()
        };
        let diff = DiffSet::parse("diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
        let mut session = ReviewSession::new_with_config(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
            &config,
        );

        session.add_general_comment("human note".into());
        let draft = session
            .add_agent_draft("src/tui.rs".into(), Some(1), "agent note".into())
            .unwrap();

        assert_eq!(session.comments[0].author.name, "Human Reviewer");
        assert_eq!(session.comments[0].author.kind, AuthorKind::Human);
        assert_eq!(draft.author.name, "Review Bot");
        assert_eq!(draft.author.kind, AuthorKind::Agent);
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
    fn syntax_cache_hits_share_large_per_line_projection() {
        let session = session();
        let file = &session.files[0];
        let (new_source, new_indices) = syntax_source(file, SyntaxSide::New);
        let (old_source, old_indices) = syntax_source(file, SyntaxSide::Old);

        let first = session.syntax_cache_for_file(
            file,
            &new_source,
            &new_indices,
            &old_source,
            &old_indices,
        );
        let second = session.syntax_cache_for_file(
            file,
            &new_source,
            &new_indices,
            &old_source,
            &old_indices,
        );

        assert!(Rc::ptr_eq(&first, &second));
    }

    #[test]
    fn collapsed_file_navigation_scans_tree_order_from_ancestor_with_unviewed_filter() {
        let diff = DiffSet::parse(
            // Parse order is z, mid, a; visible tree order is a, mid, z.
            r#"diff --git a/z/after.rs b/z/after.rs
--- a/z/after.rs
+++ b/z/after.rs
@@ -1 +1 @@
-old
+new
diff --git a/mid/hidden.rs b/mid/hidden.rs
--- a/mid/hidden.rs
+++ b/mid/hidden.rs
@@ -1 +1 @@
-old
+new
diff --git a/a/before.rs b/a/before.rs
--- a/a/before.rs
+++ b/a/before.rs
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.files[2].viewed = true;
        session.viewed_filter = ViewedFilter::Unviewed;
        session.collapsed_dirs.insert("mid".to_owned());
        session.select_file_index(1);
        session.focus = Focus::Files;

        // There is no unviewed file before the collapsed mid row. Do not jump
        // across the ancestor to z/after.rs merely because it was parsed first.
        session.move_file_selection(-1);
        assert_eq!(session.selected_file().unwrap().path, "mid/hidden.rs");
        assert_eq!(session.focus, Focus::Files);

        session.move_file_selection(1);
        assert_eq!(session.selected_file().unwrap().path, "z/after.rs");
    }

    #[test]
    fn collapsed_file_navigation_scans_tree_order_from_ancestor_with_viewed_filter() {
        let diff = DiffSet::parse(
            // Parse order is z, mid, a; visible tree order is a, mid, z.
            r#"diff --git a/z/after.rs b/z/after.rs
--- a/z/after.rs
+++ b/z/after.rs
@@ -1 +1 @@
-old
+new
diff --git a/mid/hidden.rs b/mid/hidden.rs
--- a/mid/hidden.rs
+++ b/mid/hidden.rs
@@ -1 +1 @@
-old
+new
diff --git a/a/before.rs b/a/before.rs
--- a/a/before.rs
+++ b/a/before.rs
@@ -1 +1 @@
-old
+new
"#,
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.files[1].viewed = true;
        session.files[2].viewed = true;
        session.viewed_filter = ViewedFilter::Viewed;
        session.collapsed_dirs.insert("mid".to_owned());
        session.select_file_index(1);
        session.focus = Focus::Diff;

        // There is no viewed file after the collapsed mid row. Do not jump
        // across the ancestor to a/before.rs merely because it was parsed last.
        session.move_file_selection(1);
        assert_eq!(session.selected_file().unwrap().path, "mid/hidden.rs");
        assert_eq!(session.focus, Focus::Diff);

        session.move_file_selection(-1);
        assert_eq!(session.selected_file().unwrap().path, "a/before.rs");
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

        assert_eq!(session.durable_sessions().len(), 2);
        assert_ne!(
            session.comments[0].session_id.as_deref(),
            Some("wrong-repo")
        );
        let owner = session
            .durable_sessions()
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
    fn refresh_remaps_active_viewport_by_logical_row_identity() {
        let original = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10,2 +10,2 @@\n keep\n-old\n+target\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            original,
            ReviewState::default(),
        );
        let old_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.focus = Focus::Diff;
        session.diff_cursor = old_cursor;
        session.diff_scroll = old_cursor as u16;

        let refreshed = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-before\n+after\n@@ -10,2 +10,2 @@\n keep\n-old\n+target\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);

        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_cursor].text, "target");
        assert_eq!(rows[session.diff_scroll as usize].text, "target");
        assert!(session.diff_cursor > old_cursor);
    }

    #[test]
    fn refresh_keeps_inactive_viewport_identity_until_file_returns() {
        let original = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10 +10 @@\n-old\n+target\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            original,
            ReviewState::default(),
        );
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.diff_cursor = target;
        session.diff_scroll = target as u16;
        session.select_file_index(1);

        let refreshed = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-before\n+after\n@@ -10 +10 @@\n-old\n+target\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        session.select_file_index(0);

        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_cursor].text, "target");
        assert_eq!(rows[session.diff_scroll as usize].text, "target");
    }

    #[test]
    fn refresh_preserves_cursor_while_every_file_is_hidden() {
        let mut session = session();
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "new")
            .unwrap();
        session.diff_cursor = target;
        session.diff_scroll = target as u16;
        session.mark_all_viewed();
        session.viewed_filter = ViewedFilter::Unviewed;
        let refreshed = DiffSet::parse(
            "diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        assert!(session.selected_visible_file().is_none());
        assert_eq!(
            session.diff_rows_for_file_index(session.selected)[session.diff_cursor].text,
            "new"
        );
    }

    #[test]
    fn refresh_prunes_removed_logical_viewports_before_same_path_returns() {
        let original = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10 +10 @@\n-old\n+target\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            original,
            ReviewState::default(),
        );
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.diff_cursor = target;
        session.diff_scroll = target as u16;
        session.select_file_index(1);

        let without_a = DiffSet::parse(
            "diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), without_a);

        let unrelated_a = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-before\n+unrelated\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), unrelated_a);
        let a = session
            .files
            .iter()
            .position(|file| file.path == "a.txt")
            .unwrap();
        session.select_file_index(a);

        assert_eq!(session.diff_scroll, 0);
        assert_eq!(session.diff_cursor, 0);
    }

    #[test]
    fn refresh_follows_successive_base_relative_rename_lineage() {
        let mid = DiffSet::parse(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 90%\nrename from old.rs\nrename to mid.rs\n--- a/old.rs\n+++ b/mid.rs\n@@ -10 +10 @@\n-old\n+target\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            mid,
            ReviewState::default(),
        );
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.diff_scroll = target as u16;
        session.diff_cursor = target;

        let new = DiffSet::parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-before\n+after\n@@ -10 +10 @@\n-old\n+target\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), new);

        assert_eq!(session.selected_file().unwrap().path, "new.rs");
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_scroll as usize].text, "target");
        assert_eq!(rows[session.diff_cursor].text, "target");
    }

    #[test]
    fn refresh_reserves_rename_destination_before_recreated_source() {
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -10 +10 @@\n-old\n+target\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.diff_scroll = target as u16;
        session.diff_cursor = target;

        let refreshed = DiffSet::parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -10 +10 @@\n-old\n+target\ndiff --git a/old.rs b/old.rs\nnew file mode 100644\n--- /dev/null\n+++ b/old.rs\n@@ -0,0 +1 @@\n+recreated\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);

        assert_eq!(session.selected_file().unwrap().path, "new.rs");
        assert_eq!(
            session.diff_rows_for_selected_file()[session.diff_cursor].text,
            "target"
        );
    }

    #[test]
    fn successive_rename_with_recreated_source_keeps_lineage_reserved() {
        let first = DiffSet::parse(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 90%\nrename from old.rs\nrename to mid.rs\n--- a/old.rs\n+++ b/mid.rs\n@@ -1 +1 @@\n-old\n+target\ndiff --git a/old.rs b/old.rs\nnew file mode 100644\n--- /dev/null\n+++ b/old.rs\n@@ -0,0 +1 @@\n+recreated\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            first,
            ReviewState::default(),
        );
        let mid = session
            .files
            .iter()
            .position(|file| file.path == "mid.rs")
            .unwrap();
        session.select_file_index(mid);
        session.focus = Focus::Diff;
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.add_comment("rename lineage comment".into());
        let comment_id = session.comments.last().unwrap().id.clone();
        let second = DiffSet::parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+target\ndiff --git a/old.rs b/old.rs\nnew file mode 100644\n--- /dev/null\n+++ b/old.rs\n@@ -0,0 +1 @@\n+recreated again\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), second);
        assert_eq!(session.selected_file().unwrap().path, "new.rs");
        assert_eq!(session.comments[0].path.as_deref(), Some("new.rs"));
        assert_eq!(
            session.select_comment_by_id(&comment_id),
            Some(CommentSelection::Diff)
        );
    }

    #[test]
    fn rename_return_restores_state_and_comment_to_base_path() {
        let first = DiffSet::parse(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 90%\nrename from old.rs\nrename to mid.rs\n--- a/old.rs\n+++ b/mid.rs\n@@ -10 +10 @@\n-old\n+target\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            first,
            ReviewState::default(),
        );
        session.focus = Focus::Diff;
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.diff_scroll = target as u16;
        session.diff_cursor = target;
        session.add_comment("lineage".into());
        let returned = DiffSet::parse(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -10 +10 @@\n-old\n+target\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), returned);
        assert_eq!(session.selected_file().unwrap().path, "old.rs");
        assert_eq!(session.diff_cursor, target);
        assert_eq!(session.comments[0].path.as_deref(), Some("old.rs"));
    }

    #[test]
    fn refresh_disappeared_selection_restores_surviving_fallback_viewport() {
        let original = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+new a\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old b\n+new b\ndiff --git a/c.txt b/c.txt\n--- a/c.txt\n+++ b/c.txt\n@@ -10 +10 @@\n-old c\n+target c\n",
        )
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            original,
            ReviewState::default(),
        );
        session.select_file_index(2);
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target c")
            .unwrap();
        session.diff_scroll = target as u16;
        session.diff_cursor = target;
        session.select_file_index(1);

        let refreshed = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+new a\ndiff --git a/c.txt b/c.txt\n--- a/c.txt\n+++ b/c.txt\n@@ -10 +10 @@\n-old c\n+target c\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);

        assert_eq!(session.selected_file().unwrap().path, "c.txt");
        let rows = session.diff_rows_for_selected_file();
        assert_eq!(rows[session.diff_scroll as usize].text, "target c");
        assert_eq!(rows[session.diff_cursor].text, "target c");
        assert!(matches!(
            session.tree_cursor,
            Some(TreeRowId::File { file_index: 1 })
        ));
    }

    #[test]
    fn refresh_disappeared_selection_prefers_visible_fallback() {
        let mut session = session();
        session.select_file_index(1);
        session.hide_generated = true;
        let refreshed = DiffSet::parse(
            "diff --git a/src/tui.rs b/src/tui.rs\n--- a/src/tui.rs\n+++ b/src/tui.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/generated.rs b/generated.rs\n--- a/generated.rs\n+++ b/generated.rs\n@@ -1 +1 @@\n-old\n+new\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        session.files[1].generated = true;
        session.ensure_selected_file_visible();

        assert_eq!(session.selected_file().unwrap().path, "src/tui.rs");
        assert!(session.selected_visible_file().is_some());
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
    fn comment_refresh_and_navigation_preserve_anchor_side() {
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old text\n+new text\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        session.focus = Focus::Diff;
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "old text")
            .unwrap();
        session.add_comment("old-side".into());
        let id = session.comments[0].id.clone();
        let refreshed = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old changed\n+new changed\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        assert!(matches!(
            session.comments[0].anchor,
            Some(CommentAnchor::Line {
                side: DiffSide::Old,
                ..
            })
        ));
        assert_eq!(
            session.select_comment_by_id(&id),
            Some(CommentSelection::Diff)
        );
        assert_eq!(
            session.diff_rows_for_selected_file()[session.diff_cursor].text,
            "old changed"
        );
        let vanished = DiffSet::parse(
            "diff --git a/a.txt b/renamed.txt\nsimilarity index 90%\nrename from a.txt\nrename to renamed.txt\n--- a/a.txt\n+++ b/renamed.txt\n@@ -50 +50 @@\n-other\n+replacement\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), vanished);
        assert_eq!(session.comments[0].path.as_deref(), Some("renamed.txt"));
        assert!(matches!(
            session.comments[0].anchor,
            Some(CommentAnchor::File { .. })
        ));
    }

    #[test]
    fn replace_diff_comment_lineage_prefers_rename_over_recreated_source() {
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-old\n+target\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        session.focus = Focus::Diff;
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.add_comment("follow rename".into());
        let next = DiffSet::parse(
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+target\ndiff --git a/old.rs b/old.rs\nnew file mode 100644\n--- /dev/null\n+++ b/old.rs\n@@ -0,0 +1 @@\n+recreated\n",
        )
        .unwrap();
        session.replace_diff(ReviewTarget::trunk_to_current(), next);
        assert_eq!(session.comments[0].path.as_deref(), Some("new.rs"));
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
    fn changed_hunk_navigation_targets_large_diff_placeholder() {
        let mut session = session();
        session.max_diff_lines = 1;
        session.files[0].changed_hunks.insert(0);
        session.rows_cache.borrow_mut().clear();
        assert!(session.jump_to_changed_hunk(1));
        assert!(matches!(
            session.diff_rows_for_selected_file()[session.diff_cursor].kind,
            DiffRowKind::Placeholder
        ));
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
        // The nudge points at the harness/CLI flow, never at summoning
        // (docs/decisions.md D9).
        assert!(nudge.contains("agent harness"));
        assert!(nudge.contains("docs/harness-setup.md"));
        assert!(!nudge.contains('@'));

        // 0 disables the line criterion; the file criterion still applies.
        session.nudge_diff_lines = 0;
        assert!(session.large_change_nudge().is_none());
        session.nudge_files = 1;
        assert!(session.large_change_nudge().is_some());
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
    fn preserves_logical_diff_viewport_per_file() {
        let mut session = session();
        session.diff_scroll = 2;
        session.diff_cursor = 3;

        session.move_selection(1);
        session.diff_scroll = 1;
        session.diff_cursor = 1;
        session.move_selection(-1);

        assert_eq!(session.selected_file().unwrap().path, "src/tui.rs");
        assert_eq!(session.diff_scroll, 2);
        assert_eq!(session.diff_cursor, 3);

        session.move_selection(1);

        assert_eq!(session.selected_file().unwrap().path, "README.md");
        assert_eq!(session.diff_scroll, 1);
        assert_eq!(session.diff_cursor, 1);
    }

    #[test]
    fn wrap_toggle_preserves_logical_viewport() {
        let mut session = multi_line_session();
        session.toggle_focus();
        session.diff_scroll = 3;
        let cursor = session.diff_cursor;

        session.toggle_diff_wrap();
        assert!(!session.diff_cues.soft_wrap);
        assert_eq!(session.diff_scroll, 3);
        assert_eq!(session.diff_cursor, cursor);

        session.toggle_diff_wrap();
        assert!(session.diff_cues.soft_wrap);
        assert_eq!(session.diff_scroll, 3);
        assert_eq!(session.diff_cursor, cursor);
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
    fn clamped_cursor_movement_does_not_rewrite_detached_scroll() {
        let mut session = session();
        session.focus = Focus::Diff;
        let rows = session.diff_rows_for_selected_file();
        session.diff_cursor = rows.iter().rposition(|row| row.anchor.is_some()).unwrap();
        session.diff_scroll = 0;
        assert!(!session.move_diff_cursor(1));
        assert_eq!(session.diff_scroll, 0);

        session.diff_cursor = rows.iter().position(|row| row.anchor.is_some()).unwrap();
        session.diff_scroll = 3;
        assert!(!session.move_diff_cursor(-1));
        assert_eq!(session.diff_scroll, 3);
    }

    #[test]
    fn comment_navigation_survives_large_diff_projection() {
        let mut session = multi_line_session();
        let anchor = session
            .diff_rows_for_selected_file()
            .iter()
            .find(|row| row.new_lineno == Some(2))
            .and_then(|row| row.anchor.clone())
            .unwrap();
        session.comments.push(Comment {
            id: "hidden-line".into(),
            path: Some(anchor.path().into()),
            line: anchor.line(),
            anchor: Some(anchor),
            body: "remember this line".into(),
            ..Comment::default()
        });
        session.max_diff_lines = 1;
        session.rows_cache.borrow_mut().clear();

        assert_eq!(
            session.select_comment_by_id("hidden-line"),
            Some(CommentSelection::Diff)
        );
        let rows = session.diff_rows_for_selected_file();
        assert!(matches!(
            rows[session.diff_cursor].kind,
            DiffRowKind::Placeholder
        ));
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
    fn duplicate_diff_line_identity_follows_containing_hunk_across_reorder() {
        let original = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-alpha\n+shared\n context alpha\n@@ -1,2 +1,2 @@\n-beta\n+shared\n context beta\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let original_rows = original.diff_rows_for_selected_file();
        let identity = DiffRowIdentity::from(
            original_rows
                .iter()
                .filter(|row| row.text == "shared")
                .nth(1)
                .unwrap(),
        );

        let reordered = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@\n-beta\n+shared\n context beta\n@@ -1,2 +1,2 @@\n-alpha\n+shared\n context alpha\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let reordered_rows = reordered.diff_rows_for_selected_file();
        let (resolved, recovered) = resolve_diff_row_identity(&reordered_rows, Some(&identity), 0);

        assert!(recovered);
        assert_eq!(reordered_rows[resolved].text, "shared");
        assert_eq!(reordered_rows[resolved].hunk_index, Some(0));
        assert_eq!(reordered_rows[resolved + 1].text, "context beta");
    }

    #[test]
    fn duplicate_semantic_candidates_resolve_by_source_proximity() {
        let original = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10 +10 @@\n-x\n+y\n@@ -30 +30 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let rows = original.diff_rows_for_selected_file();
        let identity = DiffRowIdentity::from(
            rows.iter()
                .filter(|row| matches!(row.kind, DiffRowKind::HunkHeader))
                .nth(1)
                .unwrap(),
        );
        let shifted = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -11 +11 @@\n-x\n+y\n@@ -31 +31 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let shifted_rows = shifted.diff_rows_for_selected_file();
        let (resolved, recovered) = resolve_diff_row_identity(&shifted_rows, Some(&identity), 0);
        assert!(recovered);
        assert_eq!(
            shifted_rows[resolved].logical_range.as_ref().unwrap().start,
            31
        );
        let removed = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -11 +11 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let (_, recovered) =
            resolve_diff_row_identity(&removed.diff_rows_for_selected_file(), Some(&identity), 0);
        assert!(!recovered);

        let earlier_removed = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -30 +30 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let earlier_removed_rows = earlier_removed.diff_rows_for_selected_file();
        let (resolved, recovered) =
            resolve_diff_row_identity(&earlier_removed_rows, Some(&identity), 0);
        assert!(recovered);
        assert_eq!(
            earlier_removed_rows[resolved]
                .logical_range
                .as_ref()
                .unwrap()
                .start,
            30
        );

        let identical_inserted = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n@@ -10 +10 @@\n-x\n+y\n@@ -30 +30 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let identical_rows = identical_inserted.diff_rows_for_selected_file();
        let (resolved, recovered) = resolve_diff_row_identity(&identical_rows, Some(&identity), 0);
        assert!(recovered);
        assert_eq!(
            identical_rows[resolved]
                .logical_range
                .as_ref()
                .unwrap()
                .start,
            30
        );

        let identical_inserted_and_shifted = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n@@ -11 +11 @@\n-x\n+y\n@@ -31 +31 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let inserted_shifted_rows = identical_inserted_and_shifted.diff_rows_for_selected_file();
        let (resolved, recovered) =
            resolve_diff_row_identity(&inserted_shifted_rows, Some(&identity), 0);
        assert!(recovered);
        assert_eq!(
            inserted_shifted_rows[resolved]
                .logical_range
                .as_ref()
                .unwrap()
                .start,
            31
        );

        let mut first = shifted_rows
            .iter()
            .find(|row| row.new_lineno == Some(11))
            .unwrap()
            .clone();
        first.semantic_key = Some("gap-line:repeated".into());
        first.semantic_occurrence = 0;
        first.new_lineno = Some(12);
        let mut second = first.clone();
        second.new_lineno = Some(31);
        second.semantic_occurrence = 1;
        let repeated_identity = DiffRowIdentity::from(&second);
        second.new_lineno = Some(32);
        let repeated = vec![first, second];
        let (resolved, recovered) =
            resolve_diff_row_identity(&repeated, Some(&repeated_identity), 1);
        assert!(recovered);
        assert_eq!(repeated[resolved].new_lineno, Some(32));
    }

    #[test]
    fn explicit_navigation_reveals_filtered_destination() {
        let mut session = session();
        session.files[1].generated = true;
        session.hide_generated = true;
        session.files[1].viewed = true;
        session.viewed_filter = ViewedFilter::Unviewed;
        let path = session.files[1].path.clone();
        let flag = AgentFlag {
            id: "filtered".into(),
            path,
            line: None,
            reason: "navigate".into(),
            priority: crate::agent::FlagPriority::High,
        };

        assert!(session.jump_to_flag(&flag).is_some());
        assert_eq!(session.selected, 1);
        assert!(!session.hide_generated);
        assert_eq!(session.viewed_filter, ViewedFilter::All);
        assert!(session.selected_visible_file().is_some());
    }

    #[test]
    fn external_state_auto_advance_preserves_hidden_file_identity() {
        let mut session = session();
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();
        let target_identity = DiffRowIdentity::from(&session.diff_rows_for_selected_file()[target]);
        session.diff_scroll = target as u16;
        session.diff_cursor = target;
        session.viewed_filter = ViewedFilter::Unviewed;
        let original_path = session.selected_file().unwrap().path.clone();
        let mut external = session.to_state();
        let fingerprint = session.files[0].fingerprint.clone();
        external
            .files
            .entry(original_path.clone())
            .or_default()
            .viewed_fingerprints
            .insert(fingerprint);

        session.apply_review_state(external);
        assert_ne!(session.selected_file().unwrap().path, original_path);
        assert_eq!(
            session.viewport_by_path[&original_path].diff_scroll,
            target as u16
        );
        assert_eq!(
            session.viewport_by_path[&original_path].top_identity,
            Some(target_identity)
        );
        let original = session
            .files
            .iter()
            .position(|file| file.path == original_path)
            .unwrap();
        let rows = session.diff_rows_for_file_index(original);
        let (resolved, _) = resolve_diff_row_identity(
            &rows,
            session.viewport_by_path[&original_path]
                .top_identity
                .as_ref(),
            target,
        );
        assert_eq!(resolved, target);
        session.viewed_filter = ViewedFilter::All;
        session.select_file_index(original);

        assert_eq!(session.diff_scroll, target as u16);
        assert_eq!(session.diff_cursor, target);
    }

    #[test]
    fn folds_only_pending_legacy_overlay_drafts_as_durable_onboarding_comments() {
        let mut session = session();
        let overlay = AgentOverlay {
            legacy_drafts: vec![
                crate::agent::LegacyAgentDraft {
                    id: "pending".into(),
                    path: session.files[0].path.clone(),
                    line: None,
                    body: "agent note".into(),
                    state: LegacyDraftState::Pending,
                    accepted_comment_id: None,
                },
                crate::agent::LegacyAgentDraft {
                    id: "accepted".into(),
                    path: session.files[0].path.clone(),
                    line: None,
                    body: "already accepted".into(),
                    state: LegacyDraftState::Accepted,
                    accepted_comment_id: Some("durable-comment".into()),
                },
                crate::agent::LegacyAgentDraft {
                    id: "discarded".into(),
                    path: session.files[0].path.clone(),
                    line: None,
                    body: "do not recreate".into(),
                    state: LegacyDraftState::Discarded,
                    accepted_comment_id: None,
                },
            ],
            ..Default::default()
        };

        assert_eq!(session.fold_legacy_agent_drafts(&overlay), 1);
        assert_eq!(session.fold_legacy_agent_drafts(&overlay), 0);
        let comment = session
            .comments
            .iter()
            .find(|comment| comment.id == "pending")
            .unwrap();
        assert_eq!(comment.author, Identity::agent());
        assert_eq!(comment.channel, Channel::Onboarding);
        assert_eq!(comment.state, CommentState::Draft);
        assert!(
            !session
                .comments
                .iter()
                .any(|comment| comment.id == "accepted")
        );
        assert!(
            !session
                .comments
                .iter()
                .any(|comment| comment.id == "discarded")
        );
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
    #[test]
    fn large_change_nudge_suppressed_once_an_agent_organized_the_review() {
        let mut session = session();
        session.nudge_files = 1;
        assert!(session.large_change_nudge().is_some());

        let index = session.ensure_active_review_session_index();
        crate::review::add_walkthrough_step(
            &mut session.durable_sessions_mut()[index],
            crate::state::WalkthroughStep {
                title: Some("core".into()),
                ..Default::default()
            },
        );

        assert!(session.large_change_nudge().is_none());
    }
    #[test]
    fn later_hunk_header_identity_does_not_fall_back_to_first_hunk() {
        let original = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -10 +10 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let rows = original.diff_rows_for_selected_file();
        let headers = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row.kind, DiffRowKind::HunkHeader))
            .collect::<Vec<_>>();
        let identity = DiffRowIdentity::from(headers[1].1);

        let refreshed = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-new-before\n+new-after\n@@ -2 +2 @@\n-a\n+b\n@@ -11 +11 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let refreshed_rows = refreshed.diff_rows_for_selected_file();
        let (resolved, recovered) =
            resolve_diff_row_identity(&refreshed_rows, Some(&identity), headers[1].0);

        assert!(recovered);
        assert_eq!(refreshed_rows[resolved].hunk_index, Some(2));
    }
    #[test]
    fn logical_line_navigation_survives_large_diff_projection() {
        let mut session = multi_line_session();
        session.max_diff_lines = 1;
        let target = StateReviewTarget {
            file: Some(session.selected_file().unwrap().path.clone()),
            line: Some(2),
            ..Default::default()
        };

        assert!(session.jump_to_review_target(&target).is_some());
        let rows = session.diff_rows_for_selected_file();
        assert!(matches!(
            rows[session.diff_cursor].kind,
            DiffRowKind::Placeholder
        ));
    }
    #[test]
    fn replace_diff_preserving_view_keeps_the_reviewers_place() {
        let mut session = session();
        session.toggle_focus();
        session.file_pane_visible = false;
        session.hide_generated = true;
        session.viewed_filter = ViewedFilter::Unviewed;
        // Select the second file and mark the first viewed.
        let index = session
            .files
            .iter()
            .position(|file| file.path == "README.md")
            .unwrap();
        session.select_file_index(index);
        session.diff_scroll = 2;
        session.diff_cursor = 3;
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
        assert_eq!(session.diff_scroll, 2);
        assert_eq!(session.diff_cursor, 3);
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
    fn semantic_identity_does_not_retain_large_row_text() {
        let long = "x".repeat(100_000);
        let session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(&format!(
                "diff --git a/a.bin b/a.bin\nold mode 100644\nnew mode 100755\n{long}\n"
            ))
            .unwrap(),
            ReviewState::default(),
        );
        let rows = session.diff_rows_for_selected_file();
        let raw = rows
            .iter()
            .find(|row| matches!(row.kind, DiffRowKind::Raw))
            .unwrap();
        let identity = DiffRowIdentity::from(raw);
        assert!(identity.text.is_empty());
        assert!(identity.semantic_key.unwrap().len() < 80);
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
    fn synthetic_identity_tracks_shifted_gap_and_line_into_containing_fold() {
        let original = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-a\n+b\n@@ -20 +20 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let rows = original.diff_rows_for_selected_file();
        let gap = rows
            .iter()
            .find(|row| matches!(row.kind, DiffRowKind::ExpandGap { gap_id: 1, .. }))
            .unwrap();
        let gap_identity = DiffRowIdentity::from(gap);

        let shifted = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -0,0 +1 @@\n+before\n@@ -2 +2 @@\n-a\n+b\n@@ -21 +21 @@\n-x\n+y\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let shifted_rows = shifted.diff_rows_for_selected_file();
        let (resolved, recovered) =
            resolve_diff_row_identity(&shifted_rows, Some(&gap_identity), 0);
        assert!(recovered);
        assert!(matches!(
            shifted_rows[resolved].kind,
            DiffRowKind::ExpandGap { gap_id: 2, .. }
        ));

        let mut folded = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,10 +1,10 @@\n one\n two\n three\n four\n five\n six\n seven\n eight\n-nine\n+changed\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let unfolded = folded.diff_rows_for_selected_file();
        let line_identity =
            DiffRowIdentity::from(unfolded.iter().find(|row| row.text == "five").unwrap());
        folded.toggle_context_fold();
        let folded_rows = folded.diff_rows_for_selected_file();
        let (resolved, recovered) =
            resolve_diff_row_identity(&folded_rows, Some(&line_identity), 0);
        assert!(recovered);
        assert!(matches!(
            folded_rows[resolved].kind,
            DiffRowKind::ContextFold
        ));
    }
    #[test]
    fn vanished_interior_hunk_header_maps_to_first_hunk_row() {
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old one\n+new one\n@@ -10 +10 @@\n-old ten\n+new ten\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        let rows = session.diff_rows_for_selected_file();
        let second_header = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row.kind, DiffRowKind::HunkHeader))
            .nth(1)
            .unwrap();
        let identity = DiffRowIdentity::from(second_header.1);
        let fallback = second_header.0;
        session.diff_scroll = fallback as u16;
        session.diff_cursor = fallback;
        session.store_file_contents(
            "a.txt",
            Some((1..=10).map(|line| format!("line {line}\n")).collect()),
        );
        assert!(session.expand_nearest_gap(None));
        let expanded = session.diff_rows_for_selected_file();
        assert!(!expanded.iter().any(|row| {
            matches!(row.kind, DiffRowKind::HunkHeader) && row.hunk_index == Some(1)
        }));
        let (resolved, recovered) = resolve_diff_row_identity(&expanded, Some(&identity), fallback);
        assert!(recovered);
        assert_eq!(expanded[resolved].new_lineno, Some(10));
        assert!(session.restore_diff_top_identity(Some(&identity), fallback as u16));
        assert_eq!(session.diff_scroll as usize, resolved);
    }
}

#[cfg(test)]
mod focus_folding_tests {
    use super::*;

    #[test]
    fn maximum_attention_folding_restores_context_expansions_and_skim_peeks_exactly() {
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::parent_to_current(),
            DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10 +10 @@\n-old\n+new\n",
            )
            .unwrap(),
            ReviewState::default(),
        );
        session.fold_context = false;
        session.expanded_skim_folds.insert("fold:peeked".into());
        session
            .context_expansion
            .insert(("a.rs".into(), 0), Expansion { above: 3, below: 2 });
        let expected_expansion = session.context_expansion.clone();

        let snapshot = session.apply_maximum_attention_folding();
        assert!(session.fold_context);
        assert!(session.expanded_skim_folds.is_empty());
        assert!(session.context_expansion.is_empty());
        session.restore_attention_folding(snapshot);
        assert!(!session.fold_context);
        assert_eq!(
            session.expanded_skim_folds,
            BTreeSet::from(["fold:peeked".into()])
        );
        assert_eq!(session.context_expansion, expected_expansion);
    }
}
