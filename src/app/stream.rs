//! Salience-driven continuous cross-file review-stream projection.
//!
//! This is a render projection only: file diffs, anchors, comments, syntax
//! spans, and durable assignments remain unchanged. Stable row ids combine the
//! logical target with current fingerprint evidence so refresh can re-anchor
//! conservatively without treating changed code as acknowledged.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::{
    anchor::{CommentAnchor, line_anchor_for_diff_row},
    attention,
    diff::DiffLineKind,
    state::{
        AttentionProgressKind, AttentionProgressTarget, ReviewTarget as StateReviewTarget,
        Salience, StepKind,
    },
};

use super::{DiffRow, DiffRowKind, ReviewSession};

/// Cache key for the memoized stream projection. Every field is either a
/// monotonic generation counter (bumped explicitly at each mutation seam) or
/// a small view-option scalar compared by value. See
/// [`ReviewSession::stream_cache_key`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct StreamCacheKey {
    stream_generation: u64,
    expansion_epoch: u64,
    fold_context: bool,
    word_highlight: bool,
    max_diff_lines: usize,
    syntax: crate::syntax::SyntaxConfig,
}

impl StreamCacheKey {
    /// True when the cached projection is still valid for `session`. This is
    /// a handful of scalar compares (plus one small config equality); it must
    /// never hash or walk session-scale content.
    fn matches(&self, session: &ReviewSession) -> bool {
        self.stream_generation == session.stream_inputs_generation()
            && self.expansion_epoch == session.expansion_epoch
            && self.fold_context == session.fold_context
            && self.word_highlight == session.diff_cues.word_highlight
            && self.max_diff_lines == session.max_diff_lines
            && self.syntax == session.syntax
    }
}

fn structural_rows(file: &super::ReviewFile) -> Vec<DiffRow> {
    let mut rows = vec![structural_row(
        DiffRowKind::FileHeader,
        format!("{}  +{} -{}", file.path, file.additions, file.deletions),
        None,
        None,
        None,
        format!("file:{}", file.path),
    )];
    for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
        rows.push(structural_row(
            DiffRowKind::HunkHeader,
            hunk.header.clone(),
            None,
            None,
            Some(hunk_index),
            format!("hunk:{}:{}", hunk_index, hunk.header),
        ));
        for (line_index, line) in hunk.lines.iter().enumerate() {
            let mut row = structural_row(
                DiffRowKind::DiffLine(line.kind),
                line.text.clone(),
                line.old_lineno,
                line.new_lineno,
                Some(hunk_index),
                format!("line:{hunk_index}:{line_index}"),
            );
            row.prefix = match line.kind {
                DiffLineKind::Context => " ",
                DiffLineKind::Added => "+",
                DiffLineKind::Removed => "-",
                DiffLineKind::Meta => "\\",
            };
            row.anchor = line_anchor_for_diff_row(file, hunk_index, line_index);
            rows.push(row);
        }
    }
    if file.diff.hunks.is_empty() {
        rows.push(structural_row(
            DiffRowKind::Raw,
            file.diff.raw.clone(),
            None,
            None,
            None,
            format!("raw:{}", file.path),
        ));
    }
    rows
}

fn structural_row(
    kind: DiffRowKind,
    text: String,
    old_lineno: Option<usize>,
    new_lineno: Option<usize>,
    hunk_index: Option<usize>,
    semantic_key: String,
) -> DiffRow {
    DiffRow {
        old_lineno,
        new_lineno,
        prefix: " ",
        text,
        syntax: Vec::new(),
        emphasis: Vec::new(),
        kind,
        hunk_index,
        anchor: None,
        gap: None,
        semantic_key: Some(semantic_key),
        semantic_parent_key: None,
        semantic_occurrence: 0,
        semantic_total: 1,
        logical_range: None,
        old_logical_range: None,
    }
}

#[derive(Debug, Clone)]
pub struct StreamRow {
    pub id: String,
    pub path: Option<String>,
    pub anchor: Option<CommentAnchor>,
    pub file_index: Option<usize>,
    pub local_row: Option<usize>,
    /// Every file represented by this row. Ordinary rows contain one member;
    /// a cross-file skim fold contains all of its member paths and indexes.
    pub member_paths: Vec<String>,
    pub member_file_indexes: Vec<usize>,
    /// Effective attention classification supplied by the shared stream
    /// resolver. Adapters may style it, but must not resolve salience again.
    pub salience: Option<Salience>,
    pub kind: StreamRowKind,
}

impl StreamRow {
    pub fn selectable(&self) -> bool {
        matches!(self.kind, StreamRowKind::SkimFold(_)) || self.anchor.is_some()
    }

    pub fn contains_file(&self, file_index: usize) -> bool {
        self.member_file_indexes.binary_search(&file_index).is_ok()
    }
}

#[derive(Debug, Clone)]
struct StreamRowIdentity {
    id: String,
    path: Option<String>,
    anchor: Option<CommentAnchor>,
    file_index: Option<usize>,
}

impl StreamRowIdentity {
    fn for_row(row: &StreamRow, preferred_file: Option<usize>) -> Self {
        Self {
            id: row.id.clone(),
            path: row.path.clone(),
            anchor: row.anchor.clone(),
            file_index: preferred_file.or(row.file_index),
        }
    }
}

#[derive(Debug, Clone)]
pub enum StreamRowKind {
    ChapterHeader(ChapterHeader),
    FileHeader(DiffRow),
    Diff(DiffRow),
    SkimFold(SkimFold),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChapterHeader {
    pub change_id: String,
    pub description: String,
    pub bookmarks: String,
    pub additions: usize,
    pub deletions: usize,
}

#[derive(Debug, Clone)]
pub struct SkimFold {
    pub id: String,
    pub target: AttentionProgressTarget,
    pub rationale: String,
    pub files: Vec<String>,
    pub file_indexes: Vec<usize>,
    pub additions: usize,
    pub deletions: usize,
    pub expanded: bool,
    pub acknowledged: bool,
    /// Files whose entire current diff is represented by this fold. Only these
    /// become viewed when the fold is acknowledged.
    pub whole_files: BTreeSet<String>,
    pub hidden_rows: Vec<StreamRow>,
}

impl SkimFold {
    pub fn label(&self) -> String {
        let arrow = if self.expanded { '⌃' } else { '⌄' };
        let files = if self.files.len() == 1 {
            "file"
        } else {
            "files"
        };
        let acknowledged = if self.acknowledged {
            " · ✓ acknowledged"
        } else {
            ""
        };
        format!(
            "{arrow} {} {files} · {} · +{} −{}{}",
            self.files.len(),
            self.rationale,
            self.additions,
            self.deletions,
            acknowledged
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotlightTarget {
    pub step_id: String,
    pub part: usize,
    pub target: StateReviewTarget,
    pub change_id: Option<String>,
    pub progress_target: AttentionProgressTarget,
    pub visited: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    pub covered: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkimAcknowledgeResult {
    Acknowledged,
    NotFold,
    Unavailable,
}

impl Coverage {
    pub fn label(self) -> String {
        format!("coverage {}/{}", self.covered, self.total)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReviewStream {
    pub rows: Vec<StreamRow>,
    pub spotlights: Vec<SpotlightTarget>,
    pub coverage: Coverage,
    /// Shared annotation-card owners and durable sources. Render adapters may
    /// format these differently, but must not place or filter cards again.
    pub annotations: Vec<StreamAnnotation>,
    render_rows: Arc<Vec<DiffRow>>,
    owner_by_row_id: HashMap<String, usize>,
    owner_by_anchor: HashMap<(usize, String), usize>,
    owner_by_line_fingerprint: HashMap<(usize, String), usize>,
    owner_by_old_line: HashMap<(usize, usize), usize>,
    owner_by_new_line: HashMap<(usize, usize), usize>,
    projected_ranges_by_file: HashMap<usize, Vec<ProjectedRangeOwner>>,
    entries_by_file: HashMap<usize, Vec<usize>>,
}

#[derive(Debug, Clone)]
pub struct StreamAnnotation {
    pub owner: usize,
    pub source: StreamAnnotationSource,
}

#[derive(Debug, Clone)]
pub enum StreamAnnotationSource {
    Comment(crate::state::Comment),
    Walkthrough {
        step: crate::state::WalkthroughStep,
        target: StateReviewTarget,
        part: usize,
        rationale: Option<String>,
    },
}

#[derive(Debug, Clone)]
struct ProjectedRangeOwner {
    owner: usize,
    old: Option<std::ops::Range<usize>>,
    new: Option<std::ops::Range<usize>>,
}

impl ReviewStream {
    /// Adapter for the existing measured unified/side-by-side renderer. Stream
    /// metadata remains in `rows`; this vector preserves the established diff
    /// styling, syntax spans, comments, wrapping, and mouse geometry.
    fn rendered(rows: &[StreamRow]) -> Vec<DiffRow> {
        rows.iter()
            .map(|row| match &row.kind {
                StreamRowKind::ChapterHeader(chapter) => synthetic_row(
                    DiffRowKind::ChapterHeader,
                    format!(
                        "◆ {} · {}{} · +{} −{}",
                        chapter.change_id,
                        chapter.description.lines().next().unwrap_or_default(),
                        if chapter.bookmarks.is_empty() {
                            String::new()
                        } else {
                            format!(" · {}", chapter.bookmarks)
                        },
                        chapter.additions,
                        chapter.deletions
                    ),
                    &row.id,
                ),
                StreamRowKind::FileHeader(diff) | StreamRowKind::Diff(diff) => diff.clone(),
                StreamRowKind::SkimFold(fold) => {
                    synthetic_row(DiffRowKind::SkimFold, fold.label(), &fold.id)
                }
            })
            .collect()
    }

    fn finalize(
        rows: Vec<StreamRow>,
        spotlights: Vec<SpotlightTarget>,
        coverage: Coverage,
    ) -> Self {
        let mut owner_by_row_id = HashMap::with_capacity(rows.len());
        let mut owner_by_anchor = HashMap::with_capacity(rows.len());
        let mut owner_by_line_fingerprint = HashMap::with_capacity(rows.len());
        let mut owner_by_old_line = HashMap::with_capacity(rows.len());
        let mut owner_by_new_line = HashMap::with_capacity(rows.len());
        let mut projected_ranges_by_file = HashMap::<usize, Vec<ProjectedRangeOwner>>::new();
        let mut entries_by_file = HashMap::<usize, Vec<usize>>::new();
        for (index, row) in rows.iter().enumerate() {
            owner_by_row_id.insert(row.id.clone(), index);
            if let StreamRowKind::SkimFold(fold) = &row.kind {
                for hidden in &fold.hidden_rows {
                    index_structural_owner(
                        hidden,
                        index,
                        &mut owner_by_anchor,
                        &mut owner_by_line_fingerprint,
                        &mut owner_by_old_line,
                        &mut owner_by_new_line,
                        &mut projected_ranges_by_file,
                    );
                }
            }
            index_structural_owner(
                row,
                index,
                &mut owner_by_anchor,
                &mut owner_by_line_fingerprint,
                &mut owner_by_old_line,
                &mut owner_by_new_line,
                &mut projected_ranges_by_file,
            );
            for file in &row.member_file_indexes {
                let entries = entries_by_file.entry(*file).or_default();
                if entries.last() != Some(&index) {
                    entries.push(index);
                }
            }
        }
        let render_rows = Arc::new(Self::rendered(&rows));
        Self {
            rows,
            spotlights,
            coverage,
            annotations: Vec::new(),
            render_rows,
            owner_by_row_id,
            owner_by_anchor,
            owner_by_line_fingerprint,
            owner_by_old_line,
            owner_by_new_line,
            projected_ranges_by_file,
            entries_by_file,
        }
    }

    fn resolve_identity(&self, identity: &StreamRowIdentity, fallback: usize) -> usize {
        identity
            .anchor
            .as_ref()
            .and_then(|anchor| {
                self.owner_by_anchor
                    .get(&(identity.file_index?, anchor_key(anchor)))
                    .copied()
            })
            .or_else(|| {
                let file_index = identity.file_index?;
                match identity.anchor.as_ref()? {
                    CommentAnchor::Line { side, line, .. } => {
                        self.side_line_owner(file_index, *side, *line)
                    }
                    CommentAnchor::Range { lines, .. } => lines
                        .iter()
                        .rev()
                        .find_map(|line| self.side_line_owner(file_index, line.side, line.line)),
                    CommentAnchor::File { .. } => self.entry_for_file(file_index),
                }
            })
            .or_else(|| self.owner_by_row_id.get(&identity.id).copied())
            .or_else(|| {
                identity
                    .file_index
                    .and_then(|file| self.entry_for_file(file))
            })
            .or_else(|| {
                identity.path.as_deref().and_then(|path| {
                    self.rows
                        .iter()
                        .position(|row| row.path.as_deref() == Some(path))
                })
            })
            .unwrap_or(fallback.min(self.rows.len().saturating_sub(1)))
    }

    fn comment_owner(&self, file_index: usize, comment: &crate::state::Comment) -> Option<usize> {
        match &comment.anchor {
            Some(anchor @ CommentAnchor::Line { side, line, .. }) => self
                .owner_by_anchor
                .get(&(file_index, anchor_key(anchor)))
                .copied()
                .or_else(|| self.side_line_owner(file_index, *side, *line)),
            Some(CommentAnchor::Range { lines, .. }) => lines
                .iter()
                .rev()
                .find_map(|line| {
                    self.owner_by_line_fingerprint
                        .get(&(file_index, line.line_fingerprint.clone()))
                        .copied()
                })
                .or_else(|| {
                    lines
                        .iter()
                        .rev()
                        .find_map(|line| self.side_line_owner(file_index, line.side, line.line))
                }),
            _ => None,
        }
    }

    fn walkthrough_owner(&self, file_index: usize, target: &StateReviewTarget) -> Option<usize> {
        let Some(start) = target.line else {
            return self.entries_by_file.get(&file_index).and_then(|entries| {
                entries.iter().copied().find(|index| {
                    self.rows[*index].anchor.is_some()
                        || matches!(self.rows[*index].kind, StreamRowKind::SkimFold(_))
                })
            });
        };
        let end = target.end_line.unwrap_or(start);
        (start..=end)
            .rev()
            .find_map(|line| self.owner_by_new_line.get(&(file_index, line)).copied())
            .or_else(|| self.projected_owner(file_index, crate::anchor::DiffSide::New, end))
    }

    fn side_line_owner(
        &self,
        file_index: usize,
        side: crate::anchor::DiffSide,
        line: usize,
    ) -> Option<usize> {
        let exact = match side {
            crate::anchor::DiffSide::Old => &self.owner_by_old_line,
            crate::anchor::DiffSide::New => &self.owner_by_new_line,
        };
        exact
            .get(&(file_index, line))
            .copied()
            .or_else(|| self.projected_owner(file_index, side, line))
    }

    fn projected_owner(
        &self,
        file_index: usize,
        side: crate::anchor::DiffSide,
        line: usize,
    ) -> Option<usize> {
        self.projected_ranges_by_file
            .get(&file_index)?
            .iter()
            .find_map(|projection| {
                let range = match side {
                    crate::anchor::DiffSide::Old => &projection.old,
                    crate::anchor::DiffSide::New => &projection.new,
                };
                range
                    .as_ref()
                    .is_some_and(|range| range.contains(&line))
                    .then_some(projection.owner)
            })
    }

    fn entry_for_file(&self, file_index: usize) -> Option<usize> {
        self.entries_by_file
            .get(&file_index)
            .into_iter()
            .flatten()
            .copied()
            .find(|index| self.rows[*index].selectable())
            .or_else(|| self.entries_by_file.get(&file_index)?.first().copied())
    }

    fn selectable_entry_for_file(&self, file_index: usize) -> Option<usize> {
        self.entries_by_file
            .get(&file_index)
            .into_iter()
            .flatten()
            .copied()
            .find(|index| self.rows[*index].selectable())
    }
}

/// Runtime-only owner-map key. Anchor fingerprints are content-derived sha256
/// evidence, so composing them is both unique per file diff generation and
/// dramatically cheaper than serializing the whole anchor per row.
fn anchor_key(anchor: &CommentAnchor) -> String {
    match anchor {
        CommentAnchor::File {
            diff_fingerprint, ..
        } => format!("file:{diff_fingerprint}"),
        CommentAnchor::Line {
            side,
            line,
            line_fingerprint,
            ..
        } => format!("line:{}:{line}:{line_fingerprint}", side.label()),
        CommentAnchor::Range {
            start_line,
            end_line,
            range_fingerprint,
            ..
        } => format!("range:{start_line}:{end_line}:{range_fingerprint}"),
    }
}

fn index_structural_owner(
    row: &StreamRow,
    owner: usize,
    by_anchor: &mut HashMap<(usize, String), usize>,
    by_line_fingerprint: &mut HashMap<(usize, String), usize>,
    by_old_line: &mut HashMap<(usize, usize), usize>,
    by_new_line: &mut HashMap<(usize, usize), usize>,
    projected_ranges: &mut HashMap<usize, Vec<ProjectedRangeOwner>>,
) {
    let Some(file_index) = row.file_index else {
        return;
    };
    if let Some(anchor) = row.anchor.as_ref() {
        by_anchor.insert((file_index, anchor_key(anchor)), owner);
        if let CommentAnchor::Line {
            line_fingerprint, ..
        } = anchor
        {
            by_line_fingerprint.insert((file_index, line_fingerprint.clone()), owner);
        }
    }
    let diff = match &row.kind {
        StreamRowKind::FileHeader(diff) | StreamRowKind::Diff(diff) => Some(diff),
        StreamRowKind::ChapterHeader(_) | StreamRowKind::SkimFold(_) => None,
    };
    let Some(diff) = diff else {
        return;
    };
    if let Some(line) = diff.old_lineno {
        by_old_line.insert((file_index, line), owner);
    }
    if let Some(line) = diff.new_lineno {
        by_new_line.insert((file_index, line), owner);
    }
    if matches!(
        diff.kind,
        DiffRowKind::ContextFold | DiffRowKind::ExpandGap { .. } | DiffRowKind::Placeholder
    ) && (diff.old_logical_range.is_some() || diff.logical_range.is_some())
    {
        projected_ranges
            .entry(file_index)
            .or_default()
            .push(ProjectedRangeOwner {
                owner,
                old: diff.old_logical_range.clone(),
                new: diff.logical_range.clone(),
            });
    }
}

#[derive(Debug)]
struct PendingFold {
    rationale: String,
    targets: Vec<StateReviewTarget>,
    files: Vec<String>,
    file_indexes: Vec<usize>,
    additions: usize,
    deletions: usize,
    whole_files: BTreeSet<String>,
    hidden_rows: Vec<StreamRow>,
}

#[derive(Debug, Clone)]
struct EffectiveSpotlightSpan {
    jump_target: StateReviewTarget,
    coverage: AttentionProgressTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AttentionSignature {
    salience: Salience,
    rationale: Option<String>,
    source: Option<crate::state::SalienceSource>,
}

impl ReviewSession {
    const MAX_MATERIALIZED_FILES_PER_WINDOW: usize = 4;

    pub fn materialize_stream_file_reanchored(&mut self, file_index: usize) -> bool {
        self.materialize_stream_files_reanchored([file_index], None, None) > 0
    }

    /// Batch-promote several files in one stream rebuild. Static adapters
    /// (`tour render`) know every destination up front; materializing them one
    /// jump at a time would pay one full projection rebuild per slide.
    pub(crate) fn materialize_stream_files(
        &mut self,
        candidates: impl IntoIterator<Item = usize>,
    ) -> usize {
        self.materialize_stream_files_reanchored(candidates, None, None)
    }

    /// Promote only the bounded set of ordinary files represented near a
    /// logical stream window to full syntax/folding rows. Fold and chapter
    /// headers already contain all data they need and never force their member
    /// files to materialize.
    pub fn materialize_stream_window_reanchored(
        &mut self,
        start: usize,
        logical_rows: usize,
    ) -> usize {
        let stream = self.review_stream();
        let top = stream
            .rows
            .get(start)
            .map(|row| StreamRowIdentity::for_row(row, None));
        let mut candidates = Vec::new();
        for row in stream.rows.iter().skip(start).take(logical_rows.max(1)) {
            if matches!(
                row.kind,
                StreamRowKind::SkimFold(_) | StreamRowKind::ChapterHeader(_)
            ) {
                continue;
            }
            for file_index in &row.member_file_indexes {
                if !candidates.contains(file_index) {
                    candidates.push(*file_index);
                }
                if candidates.len() == Self::MAX_MATERIALIZED_FILES_PER_WINDOW {
                    break;
                }
            }
            if candidates.len() == Self::MAX_MATERIALIZED_FILES_PER_WINDOW {
                break;
            }
        }
        drop(stream);
        self.materialize_stream_files_reanchored(candidates, top, None)
    }

    /// The only mutation point for stream materialization. Any promotion from
    /// cheap structural rows to detailed syntax/fold/gap rows must pass through
    /// here so both viewport coordinates are rebound in the rebuilt stream.
    fn materialize_stream_files_reanchored(
        &mut self,
        candidates: impl IntoIterator<Item = usize>,
        top_target: Option<StreamRowIdentity>,
        cursor_target: Option<StreamRowIdentity>,
    ) -> usize {
        let before = self.review_stream();
        let top_fallback = self.stream_scroll as usize;
        let cursor_fallback = self.stream_cursor;
        let selected = self.selected;
        let top = top_target.or_else(|| {
            before
                .rows
                .get(top_fallback)
                .map(|row| StreamRowIdentity::for_row(row, None))
        });
        let cursor = cursor_target.or_else(|| {
            before.rows.get(cursor_fallback).map(|row| {
                StreamRowIdentity::for_row(row, row.contains_file(selected).then_some(selected))
            })
        });
        drop(before);

        let mut materialized = self.stream_materialized_files.borrow_mut();
        let count_before = materialized.len();
        materialized.extend(
            candidates
                .into_iter()
                .filter(|file_index| *file_index < self.files.len()),
        );
        let added = materialized.len() - count_before;
        drop(materialized);

        if added > 0 {
            self.stream_cache.borrow_mut().take();
        }
        let after = self.review_stream();
        let fallback = after.entry_for_file(self.selected);
        let top_index = top
            .as_ref()
            .map(|identity| after.resolve_identity(identity, top_fallback))
            .or(fallback)
            .unwrap_or(top_fallback.min(after.rows.len().saturating_sub(1)));
        let cursor_index = cursor
            .as_ref()
            .map(|identity| after.resolve_identity(identity, cursor_fallback))
            .or(fallback)
            .unwrap_or(cursor_fallback.min(after.rows.len().saturating_sub(1)));
        let cursor_row = after.rows.get(cursor_index).cloned();
        drop(after);
        self.stream_scroll = top_index.min(u16::MAX as usize) as u16;
        self.stream_cursor = cursor_index;
        if let Some(row) = cursor_row.as_ref() {
            self.sync_selected_context_from_stream_row(row);
        }
        added
    }

    pub fn review_stream_rows(&self) -> Arc<Vec<DiffRow>> {
        Arc::clone(&self.review_stream().render_rows)
    }

    /// Cheap cache key for the memoized stream projection. Everything here is
    /// either a monotonic generation counter (bumped explicitly at each
    /// mutation seam — see [`ReviewSession::touch_stream_inputs`]) or a small
    /// scalar/config compared by value. Validating a cache hit must never
    /// hash or walk session-scale content: per-keystroke cost is O(key), not
    /// O(session).
    fn stream_cache_key(&self) -> StreamCacheKey {
        StreamCacheKey {
            stream_generation: self.stream_inputs_generation(),
            expansion_epoch: self.expansion_epoch,
            fold_context: self.fold_context,
            word_highlight: self.diff_cues.word_highlight,
            max_diff_lines: self.max_diff_lines,
            syntax: self.syntax.clone(),
        }
    }

    pub fn review_stream(&self) -> Arc<ReviewStream> {
        if let Some((cached_key, stream)) = self.stream_cache.borrow().as_ref()
            && cached_key.matches(self)
        {
            return Arc::clone(stream);
        }
        let key = self.stream_cache_key();
        let stream = Arc::new(self.build_review_stream());
        #[cfg(test)]
        self.stream_projection_builds
            .set(self.stream_projection_builds.get() + 1);
        *self.stream_cache.borrow_mut() = Some((key, Arc::clone(&stream)));
        stream
    }

    fn build_review_stream(&self) -> ReviewStream {
        let diff_files = self.files.iter().map(|file| &file.diff).collect::<Vec<_>>();
        let durable = self.active_durable_session();
        // One staleness pass for the whole projection; every row query below
        // resolves against the same precomputed effective attention map.
        let resolver =
            durable.map(|session| attention::EffectiveAttentionResolver::new(session, &diff_files));
        let mut rows = Vec::new();
        let mut pending: Option<PendingFold> = None;
        let mut spotlight_spans = Vec::new();
        let materialized = self.stream_materialized_files.borrow();

        for (file_index, file) in self.files.iter().enumerate() {
            let local = if materialized.contains(&file_index) {
                self.diff_rows_for_file_index(file_index)
            } else {
                self.structural_rows_for_file(file_index, file)
            };
            let file_target =
                attention::target_for_file_diff(&file.diff, &file.path, None, None).ok();
            let effective = resolver
                .as_ref()
                .and_then(|resolver| file_target.as_ref().map(|target| resolver.resolve(target)));

            // Resolve every anchorable row first. A broad file Skim is only a
            // default; narrower effective Supporting/Spotlight rows punch
            // through before any fold is formed.
            let row_attention = local
                .iter()
                .map(|row| {
                    let target = row.anchor.as_ref().and_then(target_from_anchor)?;
                    let resolved = resolver
                        .as_ref()
                        .map(|resolver| resolver.resolve(&target))
                        .unwrap_or(crate::attention::EffectiveAttentionRegion {
                            target: target.clone(),
                            salience: Salience::Supporting,
                            rationale: None,
                            source: None,
                        });
                    Some((target, resolved))
                })
                .collect::<Vec<_>>();

            collect_spotlight_spans(&row_attention, &mut spotlight_spans);

            let anchor_attention = row_attention.iter().flatten().collect::<Vec<_>>();
            let whole_signature = anchor_attention
                .first()
                .map(|(_, region)| attention_signature(region));
            let whole_file_skim = if anchor_attention.is_empty() {
                effective
                    .as_ref()
                    .is_some_and(|region| region.salience == Salience::Skim)
            } else {
                whole_signature.as_ref().is_some_and(|signature| {
                    signature.salience == Salience::Skim
                        && anchor_attention
                            .iter()
                            .all(|(_, region)| attention_signature(region) == *signature)
                })
            };
            if whole_file_skim {
                let rationale = fold_rationale(
                    anchor_attention
                        .first()
                        .and_then(|(_, region)| region.rationale.clone())
                        .or_else(|| {
                            effective
                                .as_ref()
                                .and_then(|region| region.rationale.clone())
                        }),
                );
                let target = file_target.expect("current file target");
                let hidden = local
                    .iter()
                    .enumerate()
                    .map(|(local_row, row)| {
                        let mut row = stream_diff_row(file_index, local_row, &file.path, row);
                        row.salience = Some(Salience::Skim);
                        row
                    })
                    .collect::<Vec<_>>();
                append_fold(
                    &mut pending,
                    PendingFold {
                        rationale,
                        targets: vec![target],
                        files: vec![file.path.clone()],
                        file_indexes: vec![file_index],
                        additions: file.additions,
                        deletions: file.deletions,
                        whole_files: BTreeSet::from([file.path.clone()]),
                        hidden_rows: hidden,
                    },
                    &mut rows,
                    self,
                    durable,
                    &diff_files,
                );
                continue;
            }

            for (local_row, row) in local.iter().enumerate() {
                let mut stream_row = stream_diff_row(file_index, local_row, &file.path, row);
                stream_row.salience = row_attention[local_row]
                    .as_ref()
                    .map(|(_, attention)| attention.salience)
                    .or_else(|| effective.as_ref().map(|attention| attention.salience));
                if let Some((target, line_attention)) = &row_attention[local_row]
                    && line_attention.salience == Salience::Skim
                {
                    let additions = usize::from(matches!(
                        row.kind,
                        DiffRowKind::DiffLine(DiffLineKind::Added)
                    ));
                    let deletions = usize::from(matches!(
                        row.kind,
                        DiffRowKind::DiffLine(DiffLineKind::Removed)
                    ));
                    append_fold(
                        &mut pending,
                        PendingFold {
                            rationale: fold_rationale(line_attention.rationale.clone()),
                            targets: vec![target.clone()],
                            files: vec![file.path.clone()],
                            file_indexes: vec![file_index],
                            additions,
                            deletions,
                            whole_files: BTreeSet::new(),
                            hidden_rows: vec![stream_row],
                        },
                        &mut rows,
                        self,
                        durable,
                        &diff_files,
                    );
                } else if row.anchor.is_none()
                    && pending.as_ref().is_some_and(|fold| {
                        next_skim_rationale(&row_attention, local_row + 1)
                            .is_some_and(|rationale| rationale == fold.rationale)
                    })
                {
                    let fold = pending.as_mut().expect("checked above");
                    fold.hidden_rows.push(stream_row);
                    insert_fold_member(fold, file_index, &file.path);
                } else {
                    flush_fold(&mut pending, &mut rows, self, durable, &diff_files);
                    rows.push(stream_row);
                }
            }
        }
        flush_fold(&mut pending, &mut rows, self, durable, &diff_files);

        let spotlights = spotlight_targets(durable, &diff_files, &spotlight_spans);
        rows = insert_chapters(self, rows, &spotlights);
        let skim_total = rows
            .iter()
            .filter(|row| matches!(row.kind, StreamRowKind::SkimFold(_)))
            .count();
        let skim_covered = rows
            .iter()
            .filter(|row| matches!(&row.kind, StreamRowKind::SkimFold(fold) if fold.acknowledged))
            .count();
        let spotlight_covered = spotlights.iter().filter(|target| target.visited).count();
        let coverage = Coverage {
            covered: skim_covered + spotlight_covered,
            total: skim_total + spotlights.len(),
        };
        let mut stream = ReviewStream::finalize(rows, spotlights, coverage);
        stream.annotations = self.build_stream_annotations(&stream);
        stream
    }

    fn build_stream_annotations(&self, stream: &ReviewStream) -> Vec<StreamAnnotation> {
        let mut output = self
            .comments
            .iter()
            .filter_map(|comment| {
                let file_index = self.files.iter().position(|file| {
                    comment
                        .anchor
                        .as_ref()
                        .map(CommentAnchor::path)
                        .or(comment.path.as_deref())
                        == Some(file.path.as_str())
                })?;
                stream
                    .comment_owner(file_index, comment)
                    .map(|owner| StreamAnnotation {
                        owner,
                        source: StreamAnnotationSource::Comment(comment.clone()),
                    })
            })
            .collect::<Vec<_>>();
        let Some(durable) = self.active_durable_session() else {
            return output;
        };
        let files = self.files.iter().map(|file| &file.diff).collect::<Vec<_>>();
        for step in durable
            .walkthroughs
            .iter()
            .flat_map(|walkthrough| &walkthrough.steps)
        {
            for (part, target) in std::iter::once(&step.target)
                .chain(&step.extra_targets)
                .enumerate()
            {
                if !attention::target_is_current(target, &files) {
                    continue;
                }
                let effective =
                    attention::resolve_effective_attention_refs(durable, target, &files);
                if effective.salience != Salience::Spotlight {
                    continue;
                }
                let Some(file_index) = target
                    .file
                    .as_deref()
                    .and_then(|path| self.files.iter().position(|file| file.path == path))
                else {
                    continue;
                };
                let Some(owner) = stream.walkthrough_owner(file_index, target) else {
                    continue;
                };
                output.push(StreamAnnotation {
                    owner,
                    source: StreamAnnotationSource::Walkthrough {
                        step: step.clone(),
                        target: target.clone(),
                        part,
                        rationale: effective.rationale,
                    },
                });
            }
        }
        output
    }

    #[cfg(test)]
    pub fn stream_projection_build_count(&self) -> usize {
        self.stream_projection_builds.get()
    }

    /// Cheap structural stream rows, memoized per file diff fingerprint so
    /// stream rebuilds triggered by navigation or progress updates reuse the
    /// per-line anchors instead of re-deriving them.
    fn structural_rows_for_file(
        &self,
        file_index: usize,
        file: &super::ReviewFile,
    ) -> Arc<Vec<DiffRow>> {
        if let Some((fingerprint, rows)) = self.structural_rows_cache.borrow().get(&file_index)
            && *fingerprint == file.diff.fingerprint
        {
            return Arc::clone(rows);
        }
        let rows = Arc::new(structural_rows(file));
        self.structural_rows_cache.borrow_mut().insert(
            file_index,
            (file.diff.fingerprint.clone(), Arc::clone(&rows)),
        );
        rows
    }

    #[cfg(test)]
    pub fn stream_materialized_file_count(&self) -> usize {
        self.stream_materialized_files.borrow().len()
    }

    #[cfg(test)]
    pub fn detailed_diff_row_cache_count(&self) -> usize {
        self.rows_cache.borrow().len()
    }

    pub fn coverage(&self) -> Coverage {
        self.review_stream().coverage
    }

    pub fn stream_scroll_to_bottom(&mut self) {
        let before = self.review_stream();
        if before.rows.is_empty() {
            self.stream_cursor = 0;
            self.stream_scroll = 0;
            return;
        }
        let destination = (self.focus == super::Focus::Diff).then(|| {
            let index = before
                .rows
                .iter()
                .rposition(StreamRow::selectable)
                .unwrap_or(before.rows.len() - 1);
            (index, before.rows[index].clone())
        });
        drop(before);
        if self.focus == super::Focus::Diff {
            let (index, row) = destination.expect("diff focus has a bottom destination");
            self.land_on_stream_row(&row, index, true);
        }
        // Landing can promote the destination file from structural rows to a
        // detailed gap/fold/syntax projection. Bottom is a projection intent,
        // not a pre-materialization row count, so derive it only afterward.
        let rebuilt = self.review_stream();
        self.stream_scroll = rebuilt
            .rows
            .len()
            .saturating_sub(super::DIFF_CURSOR_SCROLL_MARGIN)
            .min(rebuilt.rows.len() - 1)
            .min(u16::MAX as usize) as u16;
    }

    pub fn selected_stream_row(&self) -> Option<StreamRow> {
        self.review_stream().rows.get(self.stream_cursor).cloned()
    }

    pub(crate) fn stream_comment_card_owner(
        &self,
        comment: &crate::state::Comment,
    ) -> Option<usize> {
        let file_index = self.files.iter().position(|file| {
            comment
                .anchor
                .as_ref()
                .map(CommentAnchor::path)
                .or(comment.path.as_deref())
                == Some(file.path.as_str())
        })?;
        self.review_stream().comment_owner(file_index, comment)
    }

    pub(crate) fn stream_walkthrough_card_owner(
        &self,
        target: &StateReviewTarget,
    ) -> Option<usize> {
        let path = target.file.as_deref()?;
        let file_index = self.files.iter().position(|file| file.path == path)?;
        self.review_stream().walkthrough_owner(file_index, target)
    }

    pub(crate) fn stream_row_in_active_range(&self, row_index: usize) -> bool {
        let stream = self.review_stream();
        let Some(row) = stream.rows.get(row_index) else {
            return false;
        };
        row.file_index == Some(self.selected)
            && row
                .local_row
                .is_some_and(|local| self.diff_row_in_active_range(local))
    }

    pub(crate) fn stream_row_flagged(&self, row_index: usize, row: &DiffRow) -> bool {
        let stream = self.review_stream();
        let Some(path) = stream
            .rows
            .get(row_index)
            .and_then(|entry| entry.path.as_deref())
        else {
            return false;
        };
        self.agent_flags.iter().any(|flag| {
            flag.path == path
                && match flag.line {
                    Some(line) => row.new_lineno == Some(line),
                    None => matches!(row.kind, DiffRowKind::FileHeader),
                }
        })
    }

    pub(crate) fn stream_entry_for_file(&self, file_index: usize) -> Option<usize> {
        self.review_stream().entry_for_file(file_index)
    }

    pub(crate) fn selectable_stream_entry_for_file(&self, file_index: usize) -> Option<usize> {
        self.review_stream().selectable_entry_for_file(file_index)
    }

    pub(crate) fn stream_row_path(&self, row_index: usize) -> Option<String> {
        let stream = self.review_stream();
        let row = stream.rows.get(row_index)?;
        if row.contains_file(self.selected) {
            return self.files.get(self.selected).map(|file| file.path.clone());
        }
        row.path.clone()
    }

    pub fn move_stream_cursor(&mut self, delta: isize) -> bool {
        let stream = self.review_stream();
        if stream.rows.is_empty() || delta == 0 {
            return false;
        }
        let old = self.stream_cursor.min(stream.rows.len() - 1);
        let mut next = old;
        loop {
            let candidate = next.saturating_add_signed(delta.signum());
            if candidate == next || candidate >= stream.rows.len() {
                break;
            }
            next = candidate;
            if stream.rows[next].selectable() {
                break;
            }
        }
        if next == old || !stream.rows[next].selectable() {
            return false;
        }
        self.land_on_stream_row(&stream.rows[next], next, true);
        true
    }

    pub fn select_stream_row(&mut self, row_index: usize, mark_visit: bool) -> bool {
        let stream = self.review_stream();
        let Some(row) = stream.rows.get(row_index) else {
            return false;
        };
        self.land_on_stream_row(row, row_index, mark_visit);
        true
    }

    fn land_on_stream_row(&mut self, row: &StreamRow, row_index: usize, mark_visit: bool) -> usize {
        let file_index = if row.contains_file(self.selected) {
            Some(self.selected)
        } else {
            row.file_index
        };
        let target = StreamRowIdentity::for_row(row, file_index);
        if let Some(file_index) = file_index {
            let destination_path = self.files.get(file_index).map(|file| file.path.clone());
            if self.diff_range_selection.as_ref().is_some_and(|selection| {
                destination_path.as_deref() != Some(selection.file_path.as_str())
            }) {
                self.clear_diff_range_selection();
            }
            if file_index != self.selected {
                self.save_current_viewport();
                self.selected = file_index;
                self.tree_cursor = Some(crate::file_tree::TreeRowId::File { file_index });
            }
            self.materialize_stream_files_reanchored([file_index], None, Some(target));
        } else {
            self.stream_cursor = self.review_stream().resolve_identity(&target, row_index);
        }
        self.focus = super::Focus::Diff;
        self.clear_selected_freshness_marks();
        if mark_visit && let Some(row) = self.selected_stream_row() {
            self.mark_spotlight_visited_at(&row);
        }
        self.stream_cursor
    }

    pub(crate) fn sync_selected_context_from_stream_row(&mut self, row: &StreamRow) {
        if !row.contains_file(self.selected) {
            return;
        }
        let local = if row.file_index == Some(self.selected) {
            row.local_row
        } else if let StreamRowKind::SkimFold(fold) = &row.kind {
            fold.hidden_rows
                .iter()
                .find(|hidden| hidden.file_index == Some(self.selected))
                .and_then(|hidden| hidden.local_row)
        } else {
            None
        };
        if let Some(local) = local {
            self.diff_cursor = local;
        }
    }

    pub fn toggle_selected_skim_fold(&mut self) -> Option<bool> {
        let row = self.selected_stream_row()?;
        let StreamRowKind::SkimFold(fold) = row.kind else {
            return None;
        };
        let expanded = if self.expanded_skim_folds.remove(&fold.id) {
            false
        } else {
            self.expanded_skim_folds.insert(fold.id);
            true
        };
        // Peeked folds change the stream projection but not any cache-key
        // scalar; bump the generation explicitly.
        self.touch_stream_inputs();
        Some(expanded)
    }

    pub fn acknowledge_selected_skim_fold(&mut self) -> SkimAcknowledgeResult {
        let Some(row) = self.selected_stream_row() else {
            return SkimAcknowledgeResult::NotFold;
        };
        let StreamRowKind::SkimFold(fold) = row.kind else {
            return SkimAcknowledgeResult::NotFold;
        };
        self.acknowledge_skim_fold(fold)
    }

    fn acknowledge_skim_fold(&mut self, fold: SkimFold) -> SkimAcknowledgeResult {
        let expected_whole_files = fold.whole_files;
        match self.acknowledge_skim_selection_outcome(&attention::SkimSelection::StableId(fold.id))
        {
            Some(outcome) if outcome.matched > 0 && outcome.stale == 0 => {
                if outcome.acknowledged > 0 {
                    debug_assert_eq!(
                        outcome
                            .whole_files_viewed
                            .iter()
                            .cloned()
                            .collect::<BTreeSet<_>>(),
                        expected_whole_files
                    );
                }
                SkimAcknowledgeResult::Acknowledged
            }
            _ => SkimAcknowledgeResult::Unavailable,
        }
    }

    pub fn acknowledge_skim_fold_id(&mut self, id: &str) -> attention::SkimAcknowledgeOutcome {
        self.acknowledge_skim_selection_outcome(&attention::SkimSelection::StableId(id.to_owned()))
            .unwrap_or_default()
    }

    pub fn acknowledge_all_current_skims(&mut self) -> attention::SkimAcknowledgeOutcome {
        self.acknowledge_skim_selection_outcome(&attention::SkimSelection::AllCurrent)
            .unwrap_or_default()
    }

    fn acknowledge_skim_selection_outcome(
        &mut self,
        selection: &attention::SkimSelection,
    ) -> Option<attention::SkimAcknowledgeOutcome> {
        let files = self
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let index = self.active_durable_session_index()?;
        let outcome =
            attention::acknowledge_skim_folds(&mut self.durable_sessions[index], &files, selection)
                .ok()?;
        if outcome.acknowledged > 0 {
            // Direct durable-session mutation seam: acknowledged folds change
            // both the stream projection and persisted attention progress.
            self.touch_durable_review();
        }
        // Same shared whole-file viewed-effect service as CLI/MCP; see
        // `ReviewSession::apply_whole_file_viewed_effects`.
        self.apply_whole_file_viewed_effects(&outcome.whole_files_viewed);
        Some(outcome)
    }

    #[cfg(test)]
    pub fn jump_spotlight(&mut self, delta: isize) -> bool {
        self.jump_spotlight_with_identity(delta).is_some()
    }

    /// Jump through the walkthrough ordering and return the exact narration
    /// card identity chosen by that jump. Focus uses this to repin colocated
    /// spotlight cards without guessing from the destination row.
    pub(crate) fn jump_spotlight_with_identity(&mut self, delta: isize) -> Option<(String, usize)> {
        let stream = self.review_stream();
        if stream.spotlights.is_empty() {
            return None;
        }
        let current = stream.rows.get(self.stream_cursor);
        let current_target = current
            .and_then(|row| row.anchor.as_ref())
            .and_then(target_from_anchor);
        let current_index = current_target.and_then(|target| {
            stream.spotlights.iter().position(|candidate| {
                coverage_intersects_target(&candidate.progress_target, &target)
            })
        });
        let next = match (current_index, delta.is_negative()) {
            (Some(index), false) => (index + 1) % stream.spotlights.len(),
            (Some(index), true) => (index + stream.spotlights.len() - 1) % stream.spotlights.len(),
            (None, false) => 0,
            (None, true) => stream.spotlights.len() - 1,
        };
        drop(stream);
        self.jump_to_spotlight_index(next)
    }

    /// Advance through Spotlight order without wrapping at the end of the tour.
    pub(crate) fn advance_review_spotlight(&mut self) -> Option<(String, usize)> {
        let stream = self.review_stream();
        let current_target = stream
            .rows
            .get(self.stream_cursor)
            .and_then(|row| row.anchor.as_ref())
            .and_then(target_from_anchor);
        let current_index = current_target.and_then(|target| {
            stream.spotlights.iter().position(|candidate| {
                coverage_intersects_target(&candidate.progress_target, &target)
            })
        });
        let next = current_index.map_or_else(
            || {
                stream
                    .spotlights
                    .iter()
                    .position(|spotlight| {
                        stream.rows.iter().enumerate().any(|(row_index, row)| {
                            row_index > self.stream_cursor
                                && row.anchor.as_ref().is_some_and(|anchor| {
                                    target_overlaps_anchor(&spotlight.target, anchor)
                                })
                        })
                    })
                    .unwrap_or(stream.spotlights.len())
            },
            |index| index + 1,
        );
        if next >= stream.spotlights.len() {
            return None;
        }
        drop(stream);
        self.jump_to_spotlight_index(next)
    }

    /// Jump to one zero-based durable/effective Spotlight in stream order.
    /// Presenter and static tour adapters use the same normal-stream landing
    /// path as Alt-N/Alt-P; no separate presentation coordinate system exists.
    pub(crate) fn jump_to_spotlight_index(&mut self, index: usize) -> Option<(String, usize)> {
        let stream = self.review_stream();
        let spotlight = stream.spotlights.get(index)?.clone();
        let identity = (spotlight.step_id.clone(), spotlight.part);
        let target = &spotlight.target;
        let (row_index, row) = stream.rows.iter().enumerate().find(|(_, row)| {
            row.anchor
                .as_ref()
                .is_some_and(|anchor| target_overlaps_anchor(target, anchor))
        })?;
        let resolved = self.land_on_stream_row(row, row_index, true);
        // The destination file may just have acquired gap/fold/syntax rows.
        // Frame the resolved row in that rebuilt projection, never its stale
        // structural index.
        self.stream_scroll = resolved.saturating_sub(5).min(u16::MAX as usize) as u16;
        Some(identity)
    }

    pub(crate) fn spotlight_count(&self) -> usize {
        self.review_stream().spotlights.len()
    }

    pub(crate) fn spotlight_index_for_step(&self, step_id: &str) -> Option<usize> {
        self.review_stream()
            .spotlights
            .iter()
            .position(|spotlight| spotlight.step_id == step_id)
    }

    pub(crate) fn spotlight_index_for_identity(&self, step_id: &str, part: usize) -> Option<usize> {
        self.review_stream()
            .spotlights
            .iter()
            .position(|spotlight| spotlight.step_id == step_id && spotlight.part == part)
    }

    pub fn change_selected_salience(&mut self, promote: bool) -> bool {
        let Some(row) = self.selected_stream_row() else {
            return false;
        };
        let targets = match row.kind {
            StreamRowKind::SkimFold(fold) => fold
                .target
                .members
                .iter()
                .map(|member| (member.file.clone(), member.line, member.end_line))
                .collect::<Vec<_>>(),
            _ => row
                .anchor
                .as_ref()
                .and_then(target_from_anchor)
                .and_then(|target| Some((target.file?, target.line, target.end_line)))
                .into_iter()
                .collect(),
        };
        if targets.is_empty() {
            return false;
        }
        let files = self.files.iter().map(|file| &file.diff).collect::<Vec<_>>();
        let Some(index) = self.active_durable_session_index() else {
            return false;
        };
        let mut changed = false;
        for (path, line, end_line) in targets {
            let Some(file) = files.iter().copied().find(|file| file.path == path) else {
                continue;
            };
            let Ok(target) = attention::target_for_file_diff(file, &path, line, end_line) else {
                continue;
            };
            let result = if promote {
                attention::promote_human_attention_refs(
                    &mut self.durable_sessions[index],
                    target,
                    None,
                    &files,
                )
            } else {
                attention::demote_human_attention_refs(
                    &mut self.durable_sessions[index],
                    target,
                    None,
                    &files,
                )
            };
            changed |= result.is_ok();
        }
        if changed {
            // Direct durable-session mutation seam: salience edits feed the
            // stream projection, and the reanchor below must observe them.
            self.touch_durable_review();
            self.reanchor_stream_cursor(&row.id);
        }
        changed
    }

    pub(crate) fn reanchor_stream_cursor(&mut self, id: &str) {
        let stream = self.review_stream();
        if let Some(index) = stream.rows.iter().position(|row| row.id == id) {
            self.stream_cursor = index;
        } else if !stream.rows.is_empty() {
            self.stream_cursor = self.stream_cursor.min(stream.rows.len() - 1);
        } else {
            self.stream_cursor = 0;
        }
    }

    fn mark_spotlight_visited_at(&mut self, row: &StreamRow) {
        let Some(anchor) = row.anchor.as_ref() else {
            return;
        };
        let Some(target) = target_from_anchor(anchor) else {
            return;
        };
        let stream = self.review_stream();
        let Some(spotlight) = stream
            .spotlights
            .iter()
            .find(|spotlight| coverage_intersects_target(&spotlight.progress_target, &target))
        else {
            return;
        };
        let files = self.files.iter().map(|file| &file.diff).collect::<Vec<_>>();
        let progress_target = spotlight.progress_target.clone();
        if let Some(index) = self.active_durable_session_index()
            && attention::record_attention_progress_refs(
                &mut self.durable_sessions[index],
                progress_target,
                AttentionProgressKind::SpotlightVisited,
                &files,
            )
            .is_ok_and(|recorded| recorded)
        {
            // Direct durable-session mutation seam: newly recorded progress
            // must reach both the stream projection and autosave.
            self.touch_durable_review();
        }
    }
}

fn attention_signature(region: &crate::attention::EffectiveAttentionRegion) -> AttentionSignature {
    AttentionSignature {
        salience: region.salience,
        rationale: region.rationale.clone(),
        source: region.source,
    }
}

fn collect_spotlight_spans(
    rows: &[Option<(
        StateReviewTarget,
        crate::attention::EffectiveAttentionRegion,
    )>],
    output: &mut Vec<EffectiveSpotlightSpan>,
) {
    let mut pending: Vec<StateReviewTarget> = Vec::new();
    let mut signature: Option<AttentionSignature> = None;
    let flush = |pending: &mut Vec<StateReviewTarget>, output: &mut Vec<EffectiveSpotlightSpan>| {
        if pending.is_empty() {
            return;
        }
        output.push(EffectiveSpotlightSpan {
            jump_target: pending[0].clone(),
            coverage: AttentionProgressTarget::from_targets(pending.iter()),
        });
        pending.clear();
    };
    for (target, region) in rows.iter().flatten() {
        let next_signature = attention_signature(region);
        if region.salience == Salience::Spotlight {
            if signature
                .as_ref()
                .is_some_and(|current| current != &next_signature)
            {
                flush(&mut pending, output);
            }
            signature = Some(next_signature);
            pending.push(target.clone());
        } else {
            flush(&mut pending, output);
            signature = None;
        }
    }
    flush(&mut pending, output);
}

fn next_skim_rationale(
    rows: &[Option<(
        StateReviewTarget,
        crate::attention::EffectiveAttentionRegion,
    )>],
    start: usize,
) -> Option<String> {
    rows.iter()
        .skip(start)
        .flatten()
        .next()
        .and_then(|(_, region)| {
            (region.salience == Salience::Skim).then(|| fold_rationale(region.rationale.clone()))
        })
}

fn insert_fold_member(fold: &mut PendingFold, file_index: usize, path: &str) {
    if !fold.file_indexes.contains(&file_index) {
        fold.file_indexes.push(file_index);
        fold.file_indexes.sort_unstable();
    }
    if !fold.files.iter().any(|file| file == path) {
        fold.files.push(path.to_owned());
        fold.files.sort();
    }
}

fn append_fold(
    pending: &mut Option<PendingFold>,
    next: PendingFold,
    rows: &mut Vec<StreamRow>,
    app: &ReviewSession,
    durable: Option<&crate::state::ReviewSession>,
    files: &[&crate::diff::FileDiff],
) {
    if let Some(current) = pending.as_mut()
        && current.rationale == next.rationale
    {
        current.targets.extend(next.targets);
        current.files.extend(next.files);
        current.files.sort();
        current.files.dedup();
        current.file_indexes.extend(next.file_indexes);
        current.file_indexes.sort_unstable();
        current.file_indexes.dedup();
        current.additions += next.additions;
        current.deletions += next.deletions;
        current.whole_files.extend(next.whole_files);
        current.hidden_rows.extend(next.hidden_rows);
        return;
    }
    flush_fold(pending, rows, app, durable, files);
    *pending = Some(next);
}

fn flush_fold(
    pending: &mut Option<PendingFold>,
    rows: &mut Vec<StreamRow>,
    app: &ReviewSession,
    durable: Option<&crate::state::ReviewSession>,
    files: &[&crate::diff::FileDiff],
) {
    let Some(pending) = pending.take() else {
        return;
    };
    let target = AttentionProgressTarget::from_targets(pending.targets.iter());
    let id = attention::skim_fold_id(&target, &pending.rationale);
    let acknowledged = durable.is_some_and(|session| {
        attention::attention_progress_is_current_refs(
            session,
            &target,
            AttentionProgressKind::SkimAcknowledged,
            files,
        )
    });
    let expanded = app.expanded_skim_folds.contains(&id);
    let first_path = pending.hidden_rows.first().and_then(|row| row.path.clone());
    let first_file = pending.hidden_rows.first().and_then(|row| row.file_index);
    let first_local = pending.hidden_rows.first().and_then(|row| row.local_row);
    let fold = SkimFold {
        id: id.clone(),
        target,
        rationale: pending.rationale,
        files: pending.files,
        file_indexes: pending.file_indexes.clone(),
        additions: pending.additions,
        deletions: pending.deletions,
        expanded,
        acknowledged,
        whole_files: pending.whole_files,
        hidden_rows: pending.hidden_rows,
    };
    rows.push(StreamRow {
        id: id.clone(),
        path: first_path,
        anchor: None,
        file_index: first_file,
        local_row: first_local,
        member_paths: fold.files.clone(),
        member_file_indexes: fold.file_indexes.clone(),
        salience: Some(Salience::Skim),
        kind: StreamRowKind::SkimFold(fold.clone()),
    });
    if expanded {
        rows.extend(fold.hidden_rows);
    }
}

fn stream_diff_row(file_index: usize, local_row: usize, path: &str, row: &DiffRow) -> StreamRow {
    let kind = if row.kind == DiffRowKind::FileHeader {
        StreamRowKind::FileHeader(row.clone())
    } else {
        StreamRowKind::Diff(row.clone())
    };
    StreamRow {
        id: row_id(path, row),
        path: Some(path.to_owned()),
        anchor: row.anchor.clone(),
        file_index: Some(file_index),
        local_row: Some(local_row),
        member_paths: vec![path.to_owned()],
        member_file_indexes: vec![file_index],
        salience: None,
        kind,
    }
}

fn row_id(path: &str, row: &DiffRow) -> String {
    // Anchored rows reuse their anchor's content fingerprints: unique per
    // (file diff, side, line), stable across rebuilds, and free of the
    // per-row serde+sha cost that dominated large stream rebuilds. The `:`
    // separators cannot collide with the hex-only hashed ids below.
    if let Some(anchor) = &row.anchor {
        return format!("row:{}", anchor_key(anchor));
    }
    let mut hash = Sha256::new();
    hash.update(b"gander-stream-row-v1\0");
    hash.update(path.as_bytes());
    hash.update(b"\0");
    hash.update(format!("{:?}", row.kind).as_bytes());
    match row.kind {
        DiffRowKind::FileHeader => hash.update(path.as_bytes()),
        DiffRowKind::HunkHeader => {
            hash.update(row.hunk_index.unwrap_or_default().to_le_bytes());
            hash.update(row.text.as_bytes());
        }
        _ => {
            hash.update(row.semantic_key.as_deref().unwrap_or(&row.text).as_bytes());
            hash.update(row.semantic_occurrence.to_le_bytes());
        }
    }
    format!("row:{:x}", hash.finalize())
}

fn fold_rationale(rationale: Option<String>) -> String {
    attention::skim_rationale(rationale)
}

fn target_from_anchor(anchor: &CommentAnchor) -> Option<StateReviewTarget> {
    Some(StateReviewTarget {
        file: Some(anchor.path().to_owned()),
        line: anchor.line(),
        end_line: anchor.end_line().filter(|end| Some(*end) != anchor.line()),
        anchor: Some(anchor.clone()),
        ..Default::default()
    })
}

fn spotlight_targets(
    durable: Option<&crate::state::ReviewSession>,
    files: &[&crate::diff::FileDiff],
    candidates: &[EffectiveSpotlightSpan],
) -> Vec<SpotlightTarget> {
    let Some(durable) = durable else {
        return Vec::new();
    };
    let mut output = Vec::new();
    let mut seen = BTreeSet::<AttentionProgressTarget>::new();
    for step in durable
        .walkthroughs
        .iter()
        .flat_map(|walkthrough| &walkthrough.steps)
    {
        if step.kind == StepKind::Chapter {
            continue;
        }
        for (part, target) in std::iter::once(&step.target)
            .chain(step.extra_targets.iter())
            .enumerate()
        {
            if !attention::target_is_current(target, files) {
                continue;
            }
            for candidate in candidates
                .iter()
                .filter(|candidate| coverage_intersects_target(&candidate.coverage, target))
            {
                let progress_target = candidate.coverage.clone();
                if !seen.insert(progress_target.clone()) {
                    continue;
                }
                let visited = attention::attention_progress_is_current_refs(
                    durable,
                    &progress_target,
                    AttentionProgressKind::SpotlightVisited,
                    files,
                );
                output.push(SpotlightTarget {
                    step_id: step.id.clone(),
                    part,
                    target: candidate.jump_target.clone(),
                    change_id: step.change_id.clone(),
                    progress_target,
                    visited,
                });
            }
        }
    }
    for candidate in candidates {
        let progress_target = candidate.coverage.clone();
        if !seen.insert(progress_target.clone()) {
            continue;
        }
        let visited = attention::attention_progress_is_current_refs(
            durable,
            &progress_target,
            AttentionProgressKind::SpotlightVisited,
            files,
        );
        let member = progress_target.members.first();
        output.push(SpotlightTarget {
            step_id: format!(
                "attention:{}:{}-{}",
                member
                    .map(|member| member.file.as_str())
                    .unwrap_or("unknown"),
                member.and_then(|member| member.line).unwrap_or(0),
                member.and_then(|member| member.end_line).unwrap_or(0)
            ),
            part: 0,
            target: candidate.jump_target.clone(),
            change_id: None,
            progress_target,
            visited,
        });
    }
    output
}

fn coverage_intersects_target(
    coverage: &AttentionProgressTarget,
    target: &StateReviewTarget,
) -> bool {
    coverage_intersects_bounds(
        coverage,
        target.file.as_deref(),
        target.line,
        target.end_line,
    )
}

/// Anchor-driven variant of [`coverage_intersects_target`] that avoids
/// cloning the anchor into a throwaway `ReviewTarget` per stream row.
fn coverage_intersects_anchor(coverage: &AttentionProgressTarget, anchor: &CommentAnchor) -> bool {
    coverage_intersects_bounds(
        coverage,
        Some(anchor.path()),
        anchor.line(),
        anchor.end_line().filter(|end| Some(*end) != anchor.line()),
    )
}

fn coverage_intersects_bounds(
    coverage: &AttentionProgressTarget,
    file: Option<&str>,
    line: Option<usize>,
    end_line: Option<usize>,
) -> bool {
    coverage.members.iter().any(|member| {
        file == Some(member.file.as_str())
            && match line {
                None => true,
                Some(start) => {
                    let end = end_line.unwrap_or(start);
                    let member_start = member.line.unwrap_or(0);
                    let member_end = member.end_line.unwrap_or(member_start);
                    start <= member_end && member_start <= end
                }
            }
    })
}

/// Whether a spotlight jump target overlaps a row's anchored line range,
/// without cloning the anchor into a throwaway `ReviewTarget` per scanned
/// stream row.
fn target_overlaps_anchor(target: &StateReviewTarget, anchor: &CommentAnchor) -> bool {
    if target.file.as_deref() != Some(anchor.path()) {
        return false;
    }
    match (target.line, anchor.line()) {
        (None, _) | (_, None) => true,
        (Some(target_start), Some(anchor_start)) => {
            let target_end = target.end_line.unwrap_or(target_start);
            let anchor_end = anchor.end_line().unwrap_or(anchor_start);
            target_start <= anchor_end && anchor_start <= target_end
        }
    }
}

fn insert_chapters(
    session: &ReviewSession,
    rows: Vec<StreamRow>,
    spotlights: &[SpotlightTarget],
) -> Vec<StreamRow> {
    let mut output = Vec::with_capacity(rows.len());
    let mut current_change: Option<String> = None;
    for row in rows {
        let change = row
            .anchor
            .as_ref()
            .and_then(|anchor| {
                spotlights
                    .iter()
                    .find(|spotlight| {
                        coverage_intersects_anchor(&spotlight.progress_target, anchor)
                    })
                    .and_then(|spotlight| spotlight.change_id.clone())
            })
            .or_else(|| {
                matches!(row.kind, StreamRowKind::FileHeader(_)).then(|| {
                    let path = row.path.as_deref()?;
                    spotlights
                        .iter()
                        .find(|spotlight| spotlight.target.file.as_deref() == Some(path))
                        .and_then(|spotlight| spotlight.change_id.clone())
                })?
            });
        if let Some(change_id) = change.as_ref() {
            if current_change.as_ref() != Some(change_id) {
                let chapter = chapter_header(session, change_id);
                output.push(StreamRow {
                    id: format!("chapter:{change_id}:{}", row.id),
                    path: row.path.clone(),
                    anchor: None,
                    file_index: row.file_index,
                    local_row: row.local_row,
                    member_paths: row.member_paths.clone(),
                    member_file_indexes: row.member_file_indexes.clone(),
                    salience: Some(Salience::Spotlight),
                    kind: StreamRowKind::ChapterHeader(chapter),
                });
            }
            current_change = Some(change_id.clone());
        } else if row.anchor.is_some() || matches!(row.kind, StreamRowKind::SkimFold(_)) {
            current_change = None;
        }
        output.push(row);
    }
    output
}

fn chapter_header(session: &ReviewSession, change_id: &str) -> ChapterHeader {
    let metadata = session
        .stack_changes
        .iter()
        .find(|change| ids_match(&change.change_id, change_id));
    let diff = session
        .change_diffs
        .iter()
        .find(|(id, _)| ids_match(id, change_id));
    let (additions, deletions) = diff.map_or((0, 0), |(_, diff)| {
        (
            diff.files.iter().map(|file| file.additions).sum(),
            diff.files.iter().map(|file| file.deletions).sum(),
        )
    });
    ChapterHeader {
        change_id: change_id.to_owned(),
        description: metadata
            .map(|change| change.description.clone())
            .filter(|description| !description.trim().is_empty())
            .unwrap_or_else(|| "(no description)".to_owned()),
        bookmarks: metadata
            .map(|change| change.bookmarks.clone())
            .unwrap_or_default(),
        additions,
        deletions,
    }
}

fn ids_match(left: &str, right: &str) -> bool {
    !left.is_empty() && !right.is_empty() && (left.starts_with(right) || right.starts_with(left))
}

fn synthetic_row(kind: DiffRowKind, text: String, id: &str) -> DiffRow {
    DiffRow {
        old_lineno: None,
        new_lineno: None,
        prefix: " ",
        text,
        syntax: Vec::new(),
        emphasis: Vec::new(),
        kind,
        hunk_index: None,
        anchor: None,
        gap: None,
        semantic_key: Some(id.to_owned()),
        semantic_parent_key: None,
        semantic_occurrence: 0,
        semantic_total: 1,
        logical_range: None,
        old_logical_range: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        attention::target_for_diff,
        diff::DiffSet,
        jj::ReviewTarget,
        state::{AttentionRegion, Comment, ReviewState, SalienceSource},
    };

    fn session_with_diff(raw: &str) -> ReviewSession {
        ReviewSession::new(
            ".".into(),
            ReviewTarget::parent_to_current(),
            DiffSet::parse(raw).unwrap(),
            ReviewState::default(),
        )
    }

    fn attach_review(session: &mut ReviewSession, mut review: crate::state::ReviewSession) {
        review.target.base = Some(session.target.base.clone());
        review.target.revision = Some(session.target.rev.clone());
        review.target.repo = Some(session.canonical_repo().to_owned());
        if review.id.is_empty() {
            review.id = "review".into();
        }
        session.durable_sessions_mut().push(review);
    }

    fn reshaping_file_diff(path: &str, suffix: usize) -> String {
        let context = (0..8)
            .map(|line| format!(" fn before_{suffix}_{line}() {{}}\n"))
            .collect::<String>();
        format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -10,12 +10,12 @@\n{context}-fn old_{suffix}() {{}}\n+fn target_{suffix}() {{}}\n fn after_{suffix}_0() {{}}\n fn after_{suffix}_1() {{}}\n fn after_{suffix}_2() {{}}\n"
        )
    }

    fn assert_stream_and_local_cursor_align(session: &ReviewSession, path: &str, line: usize) {
        let stream = session.review_stream();
        let row = &stream.rows[session.stream_cursor];
        assert_eq!(row.path.as_deref(), Some(path));
        assert_eq!(
            row.anchor.as_ref().and_then(CommentAnchor::line),
            Some(line)
        );
        assert_eq!(session.selected_file().unwrap().path, path);
        let local = session.diff_rows_for_selected_file();
        assert_eq!(local[session.diff_cursor].anchor, row.anchor);
        assert_eq!(session.selected_comment_anchor(), row.anchor);
    }

    fn assert_stream_top_matches_scroll(session: &ReviewSession) {
        let rows = session.review_stream_rows();
        assert_eq!(
            session.stream_top_identity(),
            rows.get(session.stream_scroll as usize)
                .map(super::super::DiffRowIdentity::from)
        );
    }

    fn force_syntax_summary(session: &ReviewSession, file_index: usize) {
        let syntax_key = super::super::syntax_cache::SyntaxCacheKey::for_file(
            session,
            &session.files[file_index],
        );
        session.syntax_cache.borrow_mut().insert(
            syntax_key,
            Arc::new(super::super::syntax_cache::SyntaxFileCache {
                new_status: super::super::syntax_cache::SyntaxCacheStatus::Failed,
                old_status: super::super::syntax_cache::SyntaxCacheStatus::Failed,
                ..Default::default()
            }),
        );
    }

    #[test]
    fn generated_files_collapse_into_exact_multi_file_fold() {
        let mut session = session_with_diff(
            "diff --git a/a.gen.rs b/a.gen.rs\n--- a/a.gen.rs\n+++ b/a.gen.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.gen.rs b/b.gen.rs\n--- a/b.gen.rs\n+++ b/b.gen.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let mut review = crate::state::ReviewSession {
            id: "r".into(),
            ..Default::default()
        };
        for file in &files {
            review.attention_regions.push(AttentionRegion {
                target: target_for_diff(&files, &file.path, None, None).unwrap(),
                salience: Salience::Skim,
                rationale: Some("Skim: generated path policy".into()),
                source: SalienceSource::Heuristic,
            });
        }
        attach_review(&mut session, review);
        let stream = session.review_stream();
        let fold = stream
            .rows
            .iter()
            .find_map(|row| match &row.kind {
                StreamRowKind::SkimFold(fold) => Some(fold),
                _ => None,
            })
            .unwrap();
        assert_eq!(fold.label(), "⌄ 2 files · generated churn · +2 −2");
        assert_eq!(fold.whole_files.len(), 2);
        assert_eq!(fold.file_indexes, [0, 1]);
        assert_eq!(stream.rows[0].member_paths, ["a.gen.rs", "b.gen.rs"]);
        assert_eq!(stream.rows[0].member_file_indexes, [0, 1]);
        let unavailable = fold.clone();
        drop(stream);
        session.durable_sessions_mut().clear();
        assert_eq!(
            session.acknowledge_skim_fold(unavailable),
            SkimAcknowledgeResult::Unavailable
        );
        assert!(session.files.iter().all(|file| !file.viewed));
    }

    #[test]
    fn narrow_human_supporting_and_spotlight_punch_through_file_skim() {
        for (salience, expected_spotlights) in [
            (Salience::Supporting, 0usize),
            (Salience::Spotlight, 1usize),
        ] {
            let mut session = session_with_diff(
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n one\n-old\n+new\n three\n",
            );
            let files = session
                .files
                .iter()
                .map(|file| file.diff.clone())
                .collect::<Vec<_>>();
            attach_review(
                &mut session,
                crate::state::ReviewSession {
                    id: format!("punch-{salience:?}"),
                    attention_regions: vec![
                        AttentionRegion {
                            target: target_for_diff(&files, "src/lib.rs", None, None).unwrap(),
                            salience: Salience::Skim,
                            rationale: Some("generated churn".into()),
                            source: SalienceSource::Heuristic,
                        },
                        AttentionRegion {
                            target: target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap(),
                            salience,
                            rationale: Some("human override".into()),
                            source: SalienceSource::Human,
                        },
                    ],
                    ..Default::default()
                },
            );
            let stream = session.review_stream();
            let folds = stream
                .rows
                .iter()
                .filter_map(|row| match &row.kind {
                    StreamRowKind::SkimFold(fold) => Some(fold),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert!(!folds.is_empty());
            assert!(folds.iter().all(|fold| fold.whole_files.is_empty()));
            assert!(stream.rows.iter().any(|row| {
                matches!(&row.kind, StreamRowKind::Diff(diff) if diff.new_lineno == Some(2) || diff.old_lineno == Some(2))
            }));
            assert_eq!(stream.spotlights.len(), expected_spotlights);
            session.stream_cursor = stream
                .rows
                .iter()
                .position(|row| matches!(row.kind, StreamRowKind::SkimFold(_)))
                .unwrap();
            assert_eq!(
                session.acknowledge_selected_skim_fold(),
                SkimAcknowledgeResult::Acknowledged
            );
            assert!(!session.files[0].viewed);
        }
    }

    #[test]
    fn every_multi_file_fold_member_selects_the_same_fold_honestly() {
        let mut session = session_with_diff(
            "diff --git a/a.gen.rs b/a.gen.rs\n--- a/a.gen.rs\n+++ b/a.gen.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.gen.rs b/b.gen.rs\n--- a/b.gen.rs\n+++ b/b.gen.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let review = crate::state::ReviewSession {
            id: "members".into(),
            attention_regions: files
                .iter()
                .map(|file| AttentionRegion {
                    target: target_for_diff(&files, &file.path, None, None).unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("generated churn".into()),
                    source: SalienceSource::Heuristic,
                })
                .collect(),
            ..Default::default()
        };
        attach_review(&mut session, review);
        session.stream_mode = true;
        let fold_row = session.stream_entry_for_file(0).unwrap();
        for (member, file) in files.iter().enumerate().take(2) {
            session.focus = crate::app::Focus::Files;
            session.select_file_index(member);
            assert_eq!(session.stream_cursor, fold_row);
            assert_eq!(session.selected, member);
            session.toggle_focus();
            assert_eq!(session.stream_cursor, fold_row);
            assert_eq!(session.selected, member);
            assert_eq!(session.selected_file().unwrap().path, file.path);
        }
    }

    #[test]
    fn partially_folded_file_jump_uses_first_selectable_entry_in_stream_order() {
        let mut session = session_with_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,4 +1,4 @@\n one\n-old2\n+new2\n three\n-old4\n+new4\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "partial-order".into(),
                attention_regions: vec![
                    AttentionRegion {
                        target: target_for_diff(&files, "src/lib.rs", None, None).unwrap(),
                        salience: Salience::Skim,
                        rationale: Some("generated churn".into()),
                        source: SalienceSource::Heuristic,
                    },
                    AttentionRegion {
                        target: target_for_diff(&files, "src/lib.rs", Some(1), None).unwrap(),
                        salience: Salience::Supporting,
                        rationale: Some("first visible".into()),
                        source: SalienceSource::Human,
                    },
                    AttentionRegion {
                        target: target_for_diff(&files, "src/lib.rs", Some(3), None).unwrap(),
                        salience: Salience::Supporting,
                        rationale: Some("second visible".into()),
                        source: SalienceSource::Human,
                    },
                ],
                ..Default::default()
            },
        );
        let stream = session.review_stream();
        assert!(
            stream
                .rows
                .iter()
                .filter(|row| matches!(row.kind, StreamRowKind::SkimFold(_)))
                .count()
                >= 2
        );
        let expected = stream
            .rows
            .iter()
            .position(|row| {
                row.path.as_deref() == Some("src/lib.rs")
                    && row.anchor.as_ref().and_then(CommentAnchor::line) == Some(1)
            })
            .unwrap();
        drop(stream);
        session.stream_mode = true;
        session.focus = crate::app::Focus::Files;
        session.select_file_index(0);
        assert_eq!(session.stream_cursor, expected);
        assert!(matches!(
            session.selected_stream_row().unwrap().kind,
            StreamRowKind::Diff(_)
        ));
    }

    #[test]
    fn partial_fold_acknowledges_region_without_marking_file_viewed() {
        let mut session = session_with_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n keep\n-old\n+new\n tail\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "partial".into(),
                attention_regions: vec![AttentionRegion {
                    target: target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("routine branch".into()),
                    source: SalienceSource::Human,
                }],
                ..Default::default()
            },
        );
        session.stream_cursor = session
            .review_stream()
            .rows
            .iter()
            .position(|row| matches!(row.kind, StreamRowKind::SkimFold(_)))
            .unwrap();
        assert_eq!(
            session.acknowledge_selected_skim_fold(),
            SkimAcknowledgeResult::Acknowledged
        );
        assert!(!session.files[0].viewed);
        assert_eq!(session.durable_sessions()[0].attention_progress.len(), 1);
        assert_eq!(
            session.coverage(),
            Coverage {
                covered: 1,
                total: 1
            }
        );
    }

    #[test]
    fn whole_file_fold_ack_marks_only_that_current_file_viewed() {
        let mut session = session_with_diff(
            "diff --git a/generated.rs b/generated.rs\n--- a/generated.rs\n+++ b/generated.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/main.rs b/main.rs\n--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "whole".into(),
                attention_regions: vec![AttentionRegion {
                    target: target_for_diff(&files, "generated.rs", None, None).unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("generated churn".into()),
                    source: SalienceSource::Heuristic,
                }],
                ..Default::default()
            },
        );
        session.stream_cursor = session
            .review_stream()
            .rows
            .iter()
            .position(|row| matches!(row.kind, StreamRowKind::SkimFold(_)))
            .unwrap();
        assert_eq!(
            session.acknowledge_selected_skim_fold(),
            SkimAcknowledgeResult::Acknowledged
        );
        assert!(session.files[0].viewed);
        assert!(!session.files[1].viewed);
        let state = session.to_state();
        assert!(state.files["generated.rs"].is_viewed_fingerprint(&files[0].fingerprint));
    }

    #[test]
    fn tui_fold_ack_file_state_mutation_is_byte_identical_to_shared_service() {
        let mut session = session_with_diff(
            "diff --git a/generated.rs b/generated.rs\n--- a/generated.rs\n+++ b/generated.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/main.rs b/main.rs\n--- a/main.rs\n+++ b/main.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "drift-guard".into(),
                attention_regions: vec![AttentionRegion {
                    target: target_for_diff(&files, "generated.rs", None, None).unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("generated churn".into()),
                    source: SalienceSource::Heuristic,
                }],
                ..Default::default()
            },
        );

        // Reference path: acknowledge on a snapshot exactly the way CLI and
        // MCP do, through the shared attention service functions.
        let mut expected = session.to_state();
        let outcome = crate::attention::acknowledge_skim_folds(
            &mut expected.sessions[0],
            &files,
            &attention::SkimSelection::AllCurrent,
        )
        .unwrap();
        assert_eq!(outcome.whole_files_viewed, ["generated.rs"]);
        crate::attention::apply_whole_file_viewed_effects(
            &mut expected,
            &files,
            &outcome.whole_files_viewed,
        );

        // TUI path: acknowledge the selected fold in the stream.
        session.stream_cursor = session
            .review_stream()
            .rows
            .iter()
            .position(|row| matches!(row.kind, StreamRowKind::SkimFold(_)))
            .unwrap();
        assert_eq!(
            session.acknowledge_selected_skim_fold(),
            SkimAcknowledgeResult::Acknowledged
        );

        // Progress timestamps legitimately differ; the persisted per-file
        // viewed/caught-up records must not.
        assert_eq!(session.to_state().files, expected.files);
        assert_eq!(
            session.persisted_files.get("generated.rs"),
            expected.files.get("generated.rs"),
            "the service mutation is applied to the TUI's persisted record directly"
        );
    }

    #[test]
    fn space_peek_is_ephemeral_and_preserves_fold_heading() {
        let mut session = session_with_diff(
            "diff --git a/generated.rs b/generated.rs\n--- a/generated.rs\n+++ b/generated.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "peek".into(),
                attention_regions: vec![AttentionRegion {
                    target: target_for_diff(&files, "generated.rs", None, None).unwrap(),
                    salience: Salience::Skim,
                    rationale: Some("generated churn".into()),
                    source: SalienceSource::Heuristic,
                }],
                ..Default::default()
            },
        );
        session.stream_cursor = 0;
        let collapsed = session.review_stream().rows.len();
        assert_eq!(session.toggle_selected_skim_fold(), Some(true));
        let expanded = session.review_stream();
        assert!(expanded.rows.len() > collapsed);
        assert!(matches!(expanded.rows[0].kind, StreamRowKind::SkimFold(_)));
        assert_eq!(session.toggle_selected_skim_fold(), Some(false));
        assert_eq!(session.review_stream().rows.len(), collapsed);
        assert!(session.durable_sessions()[0].attention_progress.is_empty());
    }

    #[test]
    fn walkthrough_order_drives_spotlight_jumps_visits_and_chapters() {
        let mut session = session_with_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let a = target_for_diff(&files, "a.rs", Some(1), None).unwrap();
        let b = target_for_diff(&files, "b.rs", Some(1), None).unwrap();
        let steps = vec![
            crate::state::WalkthroughStep {
                id: "b-first".into(),
                target: b.clone(),
                change_id: Some("change-b".into()),
                title: Some("Read B".into()),
                ..Default::default()
            },
            crate::state::WalkthroughStep {
                id: "a-second".into(),
                target: a.clone(),
                change_id: Some("change-a".into()),
                title: Some("Read A".into()),
                ..Default::default()
            },
        ];
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "tour".into(),
                attention_regions: vec![
                    AttentionRegion {
                        target: a,
                        salience: Salience::Spotlight,
                        rationale: Some("A".into()),
                        source: SalienceSource::Agent,
                    },
                    AttentionRegion {
                        target: b,
                        salience: Salience::Spotlight,
                        rationale: Some("B".into()),
                        source: SalienceSource::Agent,
                    },
                ],
                walkthroughs: vec![crate::state::Walkthrough {
                    id: "walk".into(),
                    steps,
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        session.stack_changes = vec![
            crate::jj::JjChangeSummary {
                change_id: "change-a".into(),
                bookmarks: "a-bookmark".into(),
                description: "feat: A".into(),
            },
            crate::jj::JjChangeSummary {
                change_id: "change-b".into(),
                bookmarks: "b-bookmark".into(),
                description: "feat: B".into(),
            },
        ];
        let stream = session.review_stream();
        assert_eq!(
            stream
                .spotlights
                .iter()
                .map(|target| target.step_id.as_str())
                .collect::<Vec<_>>(),
            ["b-first", "a-second"]
        );
        assert_eq!(
            stream.coverage,
            Coverage {
                covered: 0,
                total: 2
            }
        );
        assert!(stream.rows.iter().any(|row| matches!(&row.kind, StreamRowKind::ChapterHeader(chapter) if chapter.description == "feat: A" && chapter.bookmarks == "a-bookmark")));
        assert!(session.jump_spotlight(1));
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert_eq!(
            session.coverage(),
            Coverage {
                covered: 1,
                total: 2
            }
        );
        assert!(session.jump_spotlight(1));
        assert_eq!(session.selected_file().unwrap().path, "a.rs");
        assert_eq!(
            session.coverage(),
            Coverage {
                covered: 2,
                total: 2
            }
        );
    }

    #[test]
    fn broad_walkthrough_normalizes_to_disjoint_effective_spotlight_spans() {
        let mut session = session_with_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,5 +1,5 @@\n-old1\n+new1\n two\n three\n four\n-old5\n+new5\n",
        );
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let broad = target_for_diff(&files, "src/lib.rs", None, None).unwrap();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "partitioned-walkthrough".into(),
                attention_regions: vec![
                    AttentionRegion {
                        target: broad.clone(),
                        salience: Salience::Spotlight,
                        rationale: Some("broad narration".into()),
                        source: SalienceSource::Agent,
                    },
                    AttentionRegion {
                        target: target_for_diff(&files, "src/lib.rs", Some(3), None).unwrap(),
                        salience: Salience::Supporting,
                        rationale: Some("human supporting bridge".into()),
                        source: SalienceSource::Human,
                    },
                ],
                walkthroughs: vec![crate::state::Walkthrough {
                    id: "broad".into(),
                    steps: vec![crate::state::WalkthroughStep {
                        id: "broad-step".into(),
                        target: broad,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let stream = session.review_stream();
        assert_eq!(stream.spotlights.len(), 2);
        assert!(
            stream
                .spotlights
                .iter()
                .all(|spotlight| spotlight.target.line.is_some())
        );
        assert!(
            stream.spotlights[0]
                .progress_target
                .members
                .iter()
                .all(|member| member.line != Some(3))
        );
        assert!(
            stream.spotlights[1]
                .progress_target
                .members
                .iter()
                .all(|member| member.line != Some(3))
        );
        let first = stream.spotlights[0].progress_target.clone();
        let second = stream.spotlights[1].progress_target.clone();
        drop(stream);
        assert!(session.jump_spotlight(1));
        assert_eq!(
            session
                .review_stream()
                .spotlights
                .iter()
                .find(|spotlight| spotlight.visited)
                .unwrap()
                .progress_target,
            first
        );
        assert!(session.jump_spotlight(1));
        let visited = session
            .review_stream()
            .spotlights
            .iter()
            .filter(|spotlight| spotlight.visited)
            .map(|spotlight| spotlight.progress_target.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(visited, BTreeSet::from([first, second]));
        assert_eq!(
            session.coverage(),
            Coverage {
                covered: 2,
                total: 2
            }
        );
        assert!(session.jump_spotlight(-1));
    }

    #[test]
    fn keyboard_range_crossing_cancels_without_resurrection() {
        let mut session = session_with_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n",
        );
        session.stream_mode = true;
        session.select_file_index(0);
        session.toggle_focus();
        session.toggle_diff_range_selection();
        assert!(session.has_active_diff_range());
        while session.selected == 0 {
            assert!(session.move_stream_cursor(1));
        }
        assert!(!session.has_active_diff_range());
        assert!(session.diff_range_selection.is_none());
        while session.selected == 1 {
            if !session.move_stream_cursor(-1) {
                break;
            }
        }
        assert!(session.diff_range_selection.is_none());
    }

    #[test]
    fn stream_rebuilds_over_an_unchanged_diff_do_no_anchor_rederivation() {
        let mut raw = String::new();
        for index in 0..20 {
            raw.push_str(&format!(
                "diff --git a/src/f{index}.rs b/src/f{index}.rs\n--- a/src/f{index}.rs\n+++ b/src/f{index}.rs\n@@ -1,3 +1,3 @@\n one_{index}\n-old_{index}\n+new_{index}\n three_{index}\n"
            ));
        }
        let mut session = session_with_diff(&raw);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "memo".into(),
                attention_regions: vec![
                    AttentionRegion {
                        target: target_for_diff(&files, "src/f1.rs", Some(2), None).unwrap(),
                        salience: Salience::Spotlight,
                        rationale: Some("look here".into()),
                        source: SalienceSource::Human,
                    },
                    AttentionRegion {
                        target: target_for_diff(&files, "src/f2.rs", None, None).unwrap(),
                        salience: Salience::Skim,
                        rationale: Some("generated churn".into()),
                        source: SalienceSource::Heuristic,
                    },
                ],
                ..Default::default()
            },
        );
        session.stream_mode = true;

        // First build derives and memoizes every needed line anchor.
        let _ = session.review_stream();
        let builds = session.stream_projection_build_count();
        let derivations = crate::anchor::line_fingerprint_derivations();

        // A forced rebuild of the same diff must be pure cache reuse.
        session.stream_cache.borrow_mut().take();
        let _ = session.review_stream();
        assert_eq!(session.stream_projection_build_count(), builds + 1);
        assert_eq!(crate::anchor::line_fingerprint_derivations(), derivations);
    }

    #[test]
    fn navigations_over_an_unchanged_diff_do_constant_attention_resolution_work() {
        let mut raw = String::new();
        for index in 0..20 {
            raw.push_str(&format!(
                "diff --git a/src/f{index}.rs b/src/f{index}.rs\n--- a/src/f{index}.rs\n+++ b/src/f{index}.rs\n@@ -1,3 +1,3 @@\n one_{index}\n-old_{index}\n+new_{index}\n three_{index}\n"
            ));
        }
        let mut session = session_with_diff(&raw);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "nav-memo".into(),
                attention_regions: vec![
                    AttentionRegion {
                        target: target_for_diff(&files, "src/f1.rs", Some(2), None).unwrap(),
                        salience: Salience::Spotlight,
                        rationale: Some("first stop".into()),
                        source: SalienceSource::Human,
                    },
                    AttentionRegion {
                        target: target_for_diff(&files, "src/f7.rs", Some(2), None).unwrap(),
                        salience: Salience::Spotlight,
                        rationale: Some("second stop".into()),
                        source: SalienceSource::Human,
                    },
                ],
                ..Default::default()
            },
        );
        session.stream_mode = true;
        let _ = session.review_stream();
        let builds = session.stream_projection_build_count();
        let derivations = crate::anchor::line_fingerprint_derivations();

        // Real navigations: each jump materializes the destination file and
        // records visited progress, forcing genuine projection rebuilds.
        for _ in 0..3 {
            assert!(session.jump_spotlight(1));
            let _ = session.review_stream();
        }
        assert!(
            session.stream_projection_build_count() > builds,
            "navigation must have rebuilt the projection for this test to prove anything"
        );
        // O(1) anchor derivation across N navigations: everything after the
        // first build is served from the fingerprint-keyed memo.
        assert_eq!(crate::anchor::line_fingerprint_derivations(), derivations);
    }

    #[test]
    fn large_stream_projection_and_owner_indexes_build_once_per_signature() {
        let mut raw = String::new();
        for index in 0..100 {
            raw.push_str(&format!(
                "diff --git a/src/f{index}.rs b/src/f{index}.rs\n--- a/src/f{index}.rs\n+++ b/src/f{index}.rs\n@@ -1 +1 @@\n-old_{index}\n+new_{index}\n"
            ));
        }
        let mut session = session_with_diff(&raw);
        let stream = session.review_stream();
        assert_eq!(stream.rows.len(), 400);
        assert_eq!(session.stream_projection_build_count(), 1);
        assert_eq!(session.stream_materialized_file_count(), 0);
        assert_eq!(session.detailed_diff_row_cache_count(), 0);
        for file in 0..100 {
            assert!(session.stream_entry_for_file(file).is_some());
        }
        for _ in 0..20 {
            let _ = session.coverage();
            let _ = session.review_stream_rows();
        }
        assert_eq!(session.stream_projection_build_count(), 1);
        assert_eq!(session.detailed_diff_row_cache_count(), 0);

        let added = session.materialize_stream_window_reanchored(0, 200);
        assert!(added <= ReviewSession::MAX_MATERIALIZED_FILES_PER_WINDOW);
        let _ = session.review_stream_rows();
        assert_eq!(session.stream_projection_build_count(), 2);
        assert!(session.detailed_diff_row_cache_count() <= 4);
        assert!(session.stream_materialized_file_count() <= 4);

        let file_fifty = session.stream_entry_for_file(50).unwrap();
        let before = session.stream_materialized_file_count();
        let added = session.materialize_stream_window_reanchored(file_fifty, 200);
        assert!(added <= ReviewSession::MAX_MATERIALIZED_FILES_PER_WINDOW);
        let _ = session.review_stream_rows();
        assert!(session.stream_materialized_file_count() - before <= 4);
        assert!(session.stream_materialized_file_count() < 100);
    }

    #[test]
    fn many_offscreen_annotations_use_structural_indexes_until_exact_materialization() {
        const FILES: usize = 64;
        let raw = (0..FILES)
            .map(|index| reshaping_file_diff(&format!("src/f{index}.rs"), index))
            .collect::<String>();
        let mut session = session_with_diff(&raw);
        session.fold_context = true;
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let targets = (0..FILES)
            .map(|index| {
                target_for_diff(&files, &format!("src/f{index}.rs"), Some(18), None).unwrap()
            })
            .collect::<Vec<_>>();
        session.comments = targets
            .iter()
            .enumerate()
            .map(|(index, target)| Comment {
                id: format!("comment-{index}"),
                path: target.file.clone(),
                line: target.line,
                anchor: target.anchor.clone(),
                body: format!("note {index}"),
                ..Comment::default()
            })
            .collect();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "many-annotations".into(),
                attention_regions: targets
                    .iter()
                    .cloned()
                    .map(|target| AttentionRegion {
                        target,
                        salience: Salience::Spotlight,
                        rationale: Some("review this line".into()),
                        source: SalienceSource::Agent,
                    })
                    .collect(),
                walkthroughs: vec![crate::state::Walkthrough {
                    id: "many-walkthrough".into(),
                    steps: targets
                        .iter()
                        .enumerate()
                        .map(|(index, target)| crate::state::WalkthroughStep {
                            id: format!("step-{index}"),
                            target: target.clone(),
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        );

        let comment_owners = session
            .comments
            .iter()
            .map(|comment| session.stream_comment_card_owner(comment).unwrap())
            .collect::<Vec<_>>();
        let walkthrough_owners = targets
            .iter()
            .map(|target| session.stream_walkthrough_card_owner(target).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(comment_owners, walkthrough_owners);
        assert_eq!(session.stream_materialized_file_count(), 0);
        assert_eq!(session.detailed_diff_row_cache_count(), 0);
        assert_eq!(session.syntax_cache.borrow().len(), 0);

        // Force the detailed target file to include every projection-only row
        // class that can shift local indices: a leading gap, a context fold,
        // and a syntax failure summary.
        let target_file = 47;
        force_syntax_summary(&session, target_file);
        session.stream_mode = true;
        assert!(session.select_stream_row(comment_owners[target_file], false));

        assert_eq!(session.stream_materialized_file_count(), 1);
        assert!(session.detailed_diff_row_cache_count() <= 2);
        let rows = session.diff_rows_for_selected_file();
        assert!(
            rows.iter()
                .any(|row| matches!(row.kind, DiffRowKind::ExpandGap { .. }))
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row.kind, DiffRowKind::ContextFold))
        );
        assert!(
            rows.iter()
                .any(|row| matches!(row.kind, DiffRowKind::SyntaxSummary))
        );
        let exact_owner = session
            .stream_comment_card_owner(&session.comments[target_file])
            .unwrap();
        assert_eq!(session.stream_cursor, exact_owner);
        assert_ne!(exact_owner, comment_owners[target_file]);
        assert_stream_and_local_cursor_align(&session, "src/f47.rs", 18);

        let id = session.comments[target_file].id.clone();
        assert_eq!(
            session.select_comment_by_id(&id),
            Some(super::super::CommentSelection::Diff)
        );
        assert_eq!(session.stream_cursor, exact_owner);
        assert_stream_and_local_cursor_align(&session, "src/f47.rs", 18);
    }

    #[test]
    fn keyboard_mouse_and_spotlight_landings_reanchor_after_projection_reshape() {
        let raw = format!(
            "{}{}",
            reshaping_file_diff("a.rs", 0),
            reshaping_file_diff("b.rs", 1)
        );

        // Keyboard crossing captures the first B anchor before B is detailed.
        let mut keyboard = session_with_diff(&raw);
        keyboard.fold_context = true;
        keyboard.stream_mode = true;
        let last_a = keyboard
            .review_stream()
            .rows
            .iter()
            .rposition(|row| row.path.as_deref() == Some("a.rs") && row.selectable())
            .unwrap();
        keyboard.select_stream_row(last_a, false);
        keyboard.stream_cursor = keyboard
            .review_stream()
            .rows
            .iter()
            .rposition(|row| row.path.as_deref() == Some("a.rs") && row.selectable())
            .unwrap();
        assert!(keyboard.move_stream_cursor(1));
        assert_stream_and_local_cursor_align(&keyboard, "b.rs", 10);

        // Mouse hit testing funnels through select_stream_row; its stale
        // pre-materialization numeric hit must resolve by anchor afterward.
        let mut mouse = session_with_diff(&raw);
        mouse.fold_context = true;
        mouse.stream_mode = true;
        let mouse_hit = mouse
            .review_stream()
            .rows
            .iter()
            .position(|row| {
                row.path.as_deref() == Some("b.rs")
                    && row.anchor.as_ref().and_then(CommentAnchor::line) == Some(18)
            })
            .unwrap();
        mouse.select_stream_row(mouse_hit, false);
        assert_stream_and_local_cursor_align(&mouse, "b.rs", 18);

        // A structural context line may intentionally disappear into a
        // detailed context fold. Stable side/line identity resolves to that
        // fold rather than falling back to an unrelated file entry.
        let mut folded = session_with_diff(&raw);
        folded.fold_context = true;
        folded.stream_mode = true;
        let folded_hit = folded
            .review_stream()
            .rows
            .iter()
            .position(|row| {
                row.path.as_deref() == Some("b.rs")
                    && row.anchor.as_ref().and_then(CommentAnchor::line) == Some(13)
            })
            .unwrap();
        folded.select_stream_row(folded_hit, false);
        let folded_row = folded.selected_stream_row().unwrap();
        assert!(matches!(
            folded_row.kind,
            StreamRowKind::Diff(ref row) if row.kind == DiffRowKind::ContextFold
        ));
        assert!(matches!(
            folded.diff_rows_for_selected_file()[folded.diff_cursor].kind,
            DiffRowKind::ContextFold
        ));
        assert_eq!(folded.selected_file().unwrap().path, "b.rs");

        // Spotlight navigation uses the same landing invariant after finding
        // its target in the structural stream.
        let mut spotlight = session_with_diff(&raw);
        spotlight.fold_context = true;
        let files = spotlight
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        let target = target_for_diff(&files, "b.rs", Some(18), None).unwrap();
        attach_review(
            &mut spotlight,
            crate::state::ReviewSession {
                id: "spotlight-landing".into(),
                attention_regions: vec![AttentionRegion {
                    target: target.clone(),
                    salience: Salience::Spotlight,
                    rationale: Some("critical".into()),
                    source: SalienceSource::Agent,
                }],
                walkthroughs: vec![crate::state::Walkthrough {
                    id: "walk".into(),
                    steps: vec![crate::state::WalkthroughStep {
                        id: "step".into(),
                        target,
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        force_syntax_summary(&spotlight, 1);
        spotlight.stream_mode = true;
        let structural_spotlight = spotlight
            .review_stream()
            .rows
            .iter()
            .position(|row| {
                row.path.as_deref() == Some("b.rs")
                    && row.anchor.as_ref().and_then(CommentAnchor::line) == Some(18)
            })
            .unwrap();
        assert!(spotlight.jump_spotlight(1));
        assert_stream_and_local_cursor_align(&spotlight, "b.rs", 18);
        let resolved_spotlight = spotlight.stream_cursor;
        let framed_top = resolved_spotlight.saturating_sub(5);
        assert_eq!(spotlight.stream_scroll as usize, framed_top);
        assert_ne!(framed_top, structural_spotlight.saturating_sub(5));
        assert!(resolved_spotlight >= spotlight.stream_scroll as usize);
        assert!(resolved_spotlight - spotlight.stream_scroll as usize <= 5);
        assert_stream_top_matches_scroll(&spotlight);
        let spotlight_rows = spotlight.diff_rows_for_selected_file();
        assert!(
            spotlight_rows
                .iter()
                .any(|row| matches!(row.kind, DiffRowKind::ExpandGap { .. }))
        );
        assert!(
            spotlight_rows
                .iter()
                .any(|row| matches!(row.kind, DiffRowKind::ContextFold))
        );
        assert!(
            spotlight_rows
                .iter()
                .any(|row| matches!(row.kind, DiffRowKind::SyntaxSummary))
        );

        // Bottom captures the last structural destination, materializes it,
        // then derives both the cursor and top from the final projection.
        let bottom_raw = (0..3)
            .map(|index| reshaping_file_diff(&format!("bottom-{index}.rs"), index))
            .collect::<String>();
        let mut bottom = session_with_diff(&bottom_raw);
        bottom.fold_context = true;
        bottom.stream_mode = true;
        bottom.focus = super::super::Focus::Diff;
        force_syntax_summary(&bottom, 2);
        let structural_len = bottom.review_stream().rows.len();
        bottom.stream_scroll_to_bottom();
        let rebuilt = bottom.review_stream();
        let rebuilt_len = rebuilt.rows.len();
        let rebuilt_last = rebuilt_len - 1;
        let expected_top = rebuilt_len
            .saturating_sub(super::super::DIFF_CURSOR_SCROLL_MARGIN)
            .min(rebuilt_last);
        assert_ne!(rebuilt_len, structural_len);
        assert_eq!(bottom.stream_cursor, rebuilt_last);
        assert_eq!(bottom.stream_scroll as usize, expected_top);
        assert!(bottom.stream_cursor >= bottom.stream_scroll as usize);
        assert!(
            rebuilt_len - bottom.stream_scroll as usize <= super::super::DIFF_CURSOR_SCROLL_MARGIN
        );
        drop(rebuilt);
        assert_stream_top_matches_scroll(&bottom);
        assert_stream_and_local_cursor_align(&bottom, "bottom-2.rs", 21);
        let bottom_rows = bottom.diff_rows_for_selected_file();
        assert!(
            bottom_rows
                .iter()
                .any(|row| matches!(row.kind, DiffRowKind::ExpandGap { .. }))
        );
        assert!(
            bottom_rows
                .iter()
                .any(|row| matches!(row.kind, DiffRowKind::ContextFold))
        );
        assert!(
            bottom_rows
                .iter()
                .any(|row| matches!(row.kind, DiffRowKind::SyntaxSummary))
        );
    }

    #[test]
    fn syntax_config_changes_invalidate_materialized_stream_rows() {
        let mut session = session_with_diff(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-fn old() {}\n+fn new() {}\n",
        );
        session.materialize_stream_file_reanchored(0);
        let before = session.review_stream_rows();
        let builds = session.stream_projection_build_count();
        session.syntax.enabled = !session.syntax.enabled;
        let after = session.review_stream_rows();
        assert!(!Arc::ptr_eq(&before, &after));
        assert_eq!(session.stream_projection_build_count(), builds + 1);
    }

    #[test]
    fn file_selection_and_cursor_crossing_share_the_stream_coordinate() {
        let mut session = session_with_diff(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n",
        );
        session.stream_mode = true;
        session.select_file_index(0);
        session.toggle_focus();
        assert_eq!(
            session.selected_stream_row().unwrap().path.as_deref(),
            Some("a.rs")
        );
        while session.selected_file().unwrap().path == "a.rs" {
            assert!(session.move_stream_cursor(1));
        }
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert_eq!(session.selected_line_anchor().unwrap().path(), "b.rs");

        session.focus = crate::app::Focus::Files;
        session.select_file_index(0);
        assert_eq!(
            session.selected_stream_row().unwrap().path.as_deref(),
            Some("a.rs")
        );
    }

    #[test]
    fn refresh_reanchors_unchanged_stream_row_after_an_inserted_file() {
        let mut session = session_with_diff(
            "diff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n",
        );
        session.stream_mode = true;
        session.toggle_focus();
        let wanted = session
            .review_stream()
            .rows
            .iter()
            .position(|row| {
                row.anchor.is_some()
                    && matches!(&row.kind, StreamRowKind::Diff(diff) if diff.text == "new_b")
            })
            .unwrap();
        session.select_stream_row(wanted, false);
        let target = session.target.clone();
        session.replace_diff_preserving_view(
            target,
            crate::diff::DiffSet::parse(
                "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n",
            )
            .unwrap(),
        );
        assert!(matches!(
            session.selected_stream_row().unwrap().kind,
            StreamRowKind::Diff(ref diff) if diff.text == "new_b"
        ));
        assert_eq!(session.selected_file().unwrap().path, "b.rs");
    }

    #[test]
    fn refresh_fallback_keeps_selected_member_when_it_leaves_grouped_fold() {
        let original = "diff --git a/a.gen.rs b/a.gen.rs\n--- a/a.gen.rs\n+++ b/a.gen.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.gen.rs b/b.gen.rs\n--- a/b.gen.rs\n+++ b/b.gen.rs\n@@ -1 +1 @@\n-old_b\n+new_b\n";
        let mut session = session_with_diff(original);
        let files = session
            .files
            .iter()
            .map(|file| file.diff.clone())
            .collect::<Vec<_>>();
        attach_review(
            &mut session,
            crate::state::ReviewSession {
                id: "refresh-member".into(),
                attention_regions: files
                    .iter()
                    .map(|file| AttentionRegion {
                        target: target_for_diff(&files, &file.path, None, None).unwrap(),
                        salience: Salience::Skim,
                        rationale: Some("generated churn".into()),
                        source: SalienceSource::Heuristic,
                    })
                    .collect(),
                ..Default::default()
            },
        );
        session.stream_mode = true;
        session.focus = crate::app::Focus::Files;
        session.select_file_index(1);
        let old_fold = session.stream_cursor;
        assert!(matches!(
            session.selected_stream_row().unwrap().kind,
            StreamRowKind::SkimFold(_)
        ));

        session.replace_diff_preserving_view(
            session.target.clone(),
            DiffSet::parse(
                "diff --git a/a.gen.rs b/a.gen.rs\n--- a/a.gen.rs\n+++ b/a.gen.rs\n@@ -1 +1 @@\n-old_a\n+new_a\ndiff --git a/b.gen.rs b/b.gen.rs\n--- a/b.gen.rs\n+++ b/b.gen.rs\n@@ -1 +1 @@\n-old_b\n+changed_b\n",
            )
            .unwrap(),
        );
        assert_eq!(session.selected_file().unwrap().path, "b.gen.rs");
        let row = session.selected_stream_row().unwrap();
        assert!(row.contains_file(session.selected));
        assert!(!matches!(row.kind, StreamRowKind::SkimFold(_)));
        assert_ne!(session.stream_cursor, old_fold);
        assert_eq!(session.selected_line_anchor().unwrap().path(), "b.gen.rs");
        assert!(session.selected_comment_anchor().is_some());
    }
}
