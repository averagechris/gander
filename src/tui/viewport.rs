//! Opaque terminal-geometry state for the diff viewport.
//!
//! Logical row/file anchors remain in `ReviewSession`.  This controller owns
//! only visual continuation, horizontal clipping, and measured layout data.
//! Its two `RefCell`s are intentionally separate: layout acquisition returns
//! an `Rc` before viewport state is borrowed, so draw-time interior mutation
//! cannot create nested-borrow hazards.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
    sync::Arc,
};

use ratatui::layout::Rect;

use crate::{
    app::{DiffRow, DiffRowIdentity, ReviewSession},
    diff::FileStatus,
};

use super::annotation_card::AnnotationSource;
#[cfg(test)]
use super::render::MIN_SPLIT_WIDTH;
use super::render::{
    AnnotationLayoutInput, DiffPointHit, MeasuredDiffLayout, annotation_artifact_card_at_owner,
    diff_split_is_active, measured_diff_layout_with_annotations, selected_file_annotation_input,
    selected_file_annotations_for_input,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct LayoutIdentity {
    rows_ptr: usize,
    width: u16,
    split_active: bool,
    soft_wrap: bool,
    annotations_hash: u64,
}

#[derive(Debug, Clone)]
struct CachedDiffLayout {
    identity: LayoutIdentity,
    /// Retain the allocation named by `identity.rows_ptr`; otherwise an
    /// allocator could recycle that address for unrelated logical rows.
    _rows: Arc<Vec<DiffRow>>,
    layout: Rc<MeasuredDiffLayout>,
}

#[derive(Debug, Clone)]
pub(super) struct DiffMeasurement {
    identity: LayoutIdentity,
    rows: Arc<Vec<DiffRow>>,
    layout: Rc<MeasuredDiffLayout>,
    selected_annotation: Option<AnnotationSource>,
}

impl DiffMeasurement {
    pub(super) fn materialize(
        &self,
        session: &ReviewSession,
        start: usize,
        height: usize,
        horizontal: usize,
        theme: &super::theme::AppTheme,
    ) -> Vec<ratatui::text::Line<'static>> {
        super::render::materialize_diff_window(
            session,
            &self.rows,
            &self.layout,
            start,
            height,
            horizontal,
            self.selected_annotation.as_ref(),
            theme,
        )
    }

    fn line_count(&self) -> usize {
        self.layout.line_count()
    }
    fn viewport_start(&self, logical: usize, continuation: usize, height: usize) -> usize {
        self.layout.viewport_start(logical, continuation, height)
    }
    fn viewport_from_line(&self, index: usize) -> (u16, usize) {
        self.layout.viewport_from_line(index)
    }
    fn cursor_bounds(&self, cursor: usize) -> Option<(usize, usize)> {
        self.layout.cursor_bounds(cursor)
    }
    fn cursor_visible(&self, cursor: usize, start: usize, height: usize) -> bool {
        self.layout.cursor_visible(cursor, start, height)
    }
    fn row_at(&self, line: usize, column: usize) -> Option<usize> {
        self.layout.row_at(line, column)
    }
    fn hit_at(&self, line: usize, column: usize) -> Option<DiffPointHit> {
        self.layout.hit_at(line, column)
    }
    fn horizontal_limit(&self) -> usize {
        self.layout.horizontal_limit
    }
    #[cfg(test)]
    pub(super) fn test_layout_rc(&self) -> Rc<MeasuredDiffLayout> {
        Rc::clone(&self.layout)
    }
}

#[derive(Debug, Clone, Default)]
struct LayoutCache {
    entry: Option<CachedDiffLayout>,
    #[cfg(test)]
    builds: usize,
    #[cfg(test)]
    measurement_requests: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct VisualViewport {
    logical_top: u16,
    /// Ephemeral app-owned row identity paired with `logical_top`. This lets
    /// a continuation follow a successfully re-resolved inactive anchor to a
    /// new numeric row while rejecting an unrelated row at the old index.
    bound_top_identity: Option<DiffRowIdentity>,
    continuation: usize,
    horizontal: usize,
    layout_identity: Option<LayoutIdentity>,
    manual_scroll_epoch: u64,
    cursor_placement_epoch: u64,
    pending_placement: Option<PendingPlacement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingPlacement {
    Top,
    Bottom,
    Cursor,
}

#[derive(Debug, Clone, Default)]
struct State {
    by_file: BTreeMap<String, VisualViewport>,
    transition_epoch: u64,
    placement_epoch: u64,
}

#[derive(Debug, Clone)]
pub(super) struct RefreshSnapshot {
    transition: TransitionSnapshot,
    files: Vec<ViewportFileLineage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewportFileLineage {
    path: String,
    status: FileStatus,
    old_path: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct TransitionSnapshot {
    path: Option<String>,
    status: Option<FileStatus>,
    old_path: Option<String>,
    logical_top: u16,
    top_identity: Option<DiffRowIdentity>,
    cursor: usize,
    cursor_identity: Option<DiffRowIdentity>,
    cursor_was_visible: Option<bool>,
    cursor_following: bool,
    transition_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DiffWindow {
    pub(super) start: usize,
    pub(super) horizontal: usize,
}

/// TUI-owned visual viewport and measured-layout cache.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ScopedCardId {
    scope: String,
    source: AnnotationSource,
}

#[derive(Debug, Default)]
pub(super) struct DiffViewportController {
    state: RefCell<State>,
    cache: RefCell<LayoutCache>,
    expanded_cards: RefCell<BTreeSet<ScopedCardId>>,
    selected_annotation: RefCell<Option<ScopedCardId>>,
    artifact_hint: RefCell<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ControllerTransactionSnapshot {
    state: State,
    cache: LayoutCache,
    expanded_cards: BTreeSet<ScopedCardId>,
    selected_annotation: Option<ScopedCardId>,
    artifact_hint: String,
}

impl DiffViewportController {
    #[cfg(test)]
    pub(super) fn annotation_visible_row(
        &self,
        session: &ReviewSession,
        inner: Rect,
        source: &AnnotationSource,
    ) -> Option<usize> {
        let measurement = self.measure(session, inner, diff_split_is_active(session, inner));
        let window = self.window(session, &measurement, inner.height.max(1) as usize);
        measurement
            .layout
            .annotation_line(source)
            .and_then(|line| line.checked_sub(window.start))
    }
    pub(super) fn transaction_snapshot(&self) -> ControllerTransactionSnapshot {
        ControllerTransactionSnapshot {
            state: self.state.borrow().clone(),
            cache: self.cache.borrow().clone(),
            expanded_cards: self.expanded_cards.borrow().clone(),
            selected_annotation: self.selected_annotation.borrow().clone(),
            artifact_hint: self.artifact_hint.borrow().clone(),
        }
    }

    pub(super) fn restore_transaction(&self, snapshot: ControllerTransactionSnapshot) {
        *self.state.borrow_mut() = snapshot.state;
        *self.cache.borrow_mut() = snapshot.cache;
        *self.expanded_cards.borrow_mut() = snapshot.expanded_cards;
        *self.selected_annotation.borrow_mut() = snapshot.selected_annotation;
        *self.artifact_hint.borrow_mut() = snapshot.artifact_hint;
    }
    pub(super) fn measure(
        &self,
        session: &ReviewSession,
        inner: Rect,
        split_active: bool,
    ) -> DiffMeasurement {
        let rows = if session.stream_mode {
            session.review_stream_rows()
        } else {
            session.diff_rows_for_selected_file()
        };
        self.measure_rows(session, rows, inner, split_active)
    }

    pub(super) fn measure_rows(
        &self,
        session: &ReviewSession,
        rows: Arc<Vec<DiffRow>>,
        inner: Rect,
        split_active: bool,
    ) -> DiffMeasurement {
        #[cfg(test)]
        {
            self.cache.borrow_mut().measurement_requests += 1;
        }
        let scope = annotation_scope(session);
        self.prune_annotation_scope(&scope);
        let expanded = self
            .expanded_cards
            .borrow()
            .iter()
            .filter(|card| card.scope == scope)
            .map(|card| card.source.stable_id())
            .collect::<BTreeSet<_>>();
        let hint = self.artifact_hint.borrow().clone();
        let annotation_input = selected_file_annotation_input(session, &expanded, &hint);
        self.expanded_cards
            .borrow_mut()
            .retain(|card| card.scope == scope && annotation_input.contains_source(&card.source));
        let selected_annotation =
            self.revalidate_selected_annotation_with_input(session, &scope, &annotation_input);
        let mut annotation_hasher = DefaultHasher::new();
        annotation_input.hash(&mut annotation_hasher);
        let identity = LayoutIdentity {
            rows_ptr: Arc::as_ptr(&rows) as usize,
            width: inner.width,
            split_active,
            soft_wrap: session.diff_cues.soft_wrap,
            annotations_hash: annotation_hasher.finish(),
        };
        if let Some(cached) = self.cache.borrow().entry.as_ref()
            && cached.identity == identity
        {
            return DiffMeasurement {
                identity,
                rows: Arc::clone(&cached._rows),
                layout: Rc::clone(&cached.layout),
                selected_annotation,
            };
        }

        let annotations = selected_file_annotations_for_input(session, annotation_input, &expanded);
        let layout = Rc::new(measured_diff_layout_with_annotations(
            session,
            &rows,
            inner,
            split_active,
            &annotations,
        ));
        let measurement = DiffMeasurement {
            identity: identity.clone(),
            rows: Arc::clone(&rows),
            layout: Rc::clone(&layout),
            selected_annotation,
        };
        let mut cache = self.cache.borrow_mut();
        cache.entry = Some(CachedDiffLayout {
            identity,
            _rows: rows,
            layout: Rc::clone(&layout),
        });
        #[cfg(test)]
        {
            cache.builds += 1;
        }
        measurement
    }

    /// Toggle the first artifact-bearing card owned by the current logical
    /// diff row. Expansion is presentation-only and intentionally never enters
    /// persisted review state.
    pub(super) fn toggle_annotation_artifacts(&self, session: &ReviewSession) -> Option<bool> {
        let scope = annotation_scope(session);
        self.prune_annotation_scope(&scope);
        let preferred = self.revalidate_selected_annotation(session);
        let source = annotation_artifact_card_at_owner(
            session,
            viewport_cursor(session),
            preferred.as_ref(),
        )?;
        let id = ScopedCardId { scope, source };
        let mut expanded = self.expanded_cards.borrow_mut();
        let now_expanded = if expanded.remove(&id) {
            false
        } else {
            expanded.insert(id);
            true
        };
        self.cache.borrow_mut().entry = None;
        Some(now_expanded)
    }

    pub(super) fn set_annotation_artifact_hint(&self, hint: &str) {
        let hint = hint.to_owned();
        if *self.artifact_hint.borrow() != hint {
            *self.artifact_hint.borrow_mut() = hint;
            self.invalidate_layout();
        }
    }

    pub(super) fn select_annotation(&self, session: &ReviewSession, source: AnnotationSource) {
        *self.selected_annotation.borrow_mut() = Some(ScopedCardId {
            scope: annotation_scope(session),
            source,
        });
    }

    /// Pin the current spotlight narration by selecting the walkthrough card
    /// owned by the stream cursor. Cursor placement then keeps its complete
    /// inline block visible without introducing another rendering primitive.
    pub(super) fn pin_current_spotlight(&self, session: &ReviewSession) -> bool {
        let scope = annotation_scope(session);
        self.prune_annotation_scope(&scope);
        let expanded = self
            .expanded_cards
            .borrow()
            .iter()
            .filter(|card| card.scope == scope)
            .map(|card| card.source.stable_id())
            .collect::<BTreeSet<_>>();
        let hint = self.artifact_hint.borrow().clone();
        let input = selected_file_annotation_input(session, &expanded, &hint);
        let Some(source) = input.walkthrough_source_at_owner(viewport_cursor(session)) else {
            return false;
        };
        *self.selected_annotation.borrow_mut() = Some(ScopedCardId { scope, source });
        true
    }

    pub(super) fn clear_selected_annotation(&self) {
        self.selected_annotation.borrow_mut().take();
    }

    pub(super) fn selected_annotation_source(
        &self,
        session: &ReviewSession,
    ) -> Option<AnnotationSource> {
        self.revalidate_selected_annotation(session)
    }

    fn revalidate_selected_annotation(&self, session: &ReviewSession) -> Option<AnnotationSource> {
        let scope = annotation_scope(session);
        self.prune_annotation_scope(&scope);
        let expanded = self
            .expanded_cards
            .borrow()
            .iter()
            .filter(|card| card.scope == scope)
            .map(|card| card.source.stable_id())
            .collect::<BTreeSet<_>>();
        let hint = self.artifact_hint.borrow().clone();
        let input = selected_file_annotation_input(session, &expanded, &hint);
        self.revalidate_selected_annotation_with_input(session, &scope, &input)
    }

    fn revalidate_selected_annotation_with_input(
        &self,
        session: &ReviewSession,
        scope: &str,
        input: &AnnotationLayoutInput,
    ) -> Option<AnnotationSource> {
        let selected = self.selected_annotation.borrow().clone();
        let valid = selected.as_ref().is_some_and(|selected| {
            selected.scope == scope
                && input.owner_for_source(&selected.source) == Some(viewport_cursor(session))
        });
        if !valid {
            self.selected_annotation.borrow_mut().take();
            return None;
        }
        selected.map(|selected| selected.source)
    }

    fn prune_annotation_scope(&self, scope: &str) {
        self.expanded_cards
            .borrow_mut()
            .retain(|card| card.scope == scope);
        if self
            .selected_annotation
            .borrow()
            .as_ref()
            .is_some_and(|selected| selected.scope != scope)
        {
            self.selected_annotation.borrow_mut().take();
        }
    }

    /// Acquire a normalized visual window.  Draw calls this through `&TuiState`;
    /// only controller-owned visual state is changed.
    pub(super) fn window(
        &self,
        session: &ReviewSession,
        measurement: &DiffMeasurement,
        viewport_height: usize,
    ) -> DiffWindow {
        let Some(path) = selected_path(session) else {
            return DiffWindow {
                start: 0,
                horizontal: 0,
            };
        };
        let identity = Some(measurement.identity.clone());
        let mut state = self.state.borrow_mut();
        let visual = state.by_file.entry(path).or_default();
        bind_logical_top(
            visual,
            viewport_scroll(session),
            viewport_top_identity(session),
        );
        let layout_changed = visual.layout_identity.as_ref() != identity.as_ref();
        let start = measurement.viewport_start(
            viewport_scroll(session) as usize,
            visual.continuation,
            viewport_height,
        );
        let (window_logical, normalized_continuation) = measurement.viewport_from_line(start);
        // A new measured identity can change the height of the same logical
        // block. Keep the detached logical anchor, but normalize its visual
        // continuation to the newly measured block.
        visual.continuation = if window_logical == viewport_scroll(session) {
            normalized_continuation
        } else {
            0
        };
        visual.horizontal = if session.diff_cues.soft_wrap {
            0
        } else {
            visual.horizontal.min(measurement.horizontal_limit())
        };
        if layout_changed && measurement.line_count() == 0 {
            visual.continuation = 0;
        }
        visual.layout_identity = identity;
        DiffWindow {
            start,
            horizontal: visual.horizontal,
        }
    }

    /// Explicit reflow transition. Detached manual scrolling is preserved;
    /// cursor placement happens only when requested by the caller.
    pub(super) fn reflow(
        &self,
        session: &mut ReviewSession,
        inner: Rect,
        keep_cursor_visible: bool,
    ) {
        if !usable_geometry(session, inner) {
            if keep_cursor_visible {
                self.set_pending_if_empty(session, PendingPlacement::Cursor);
            }
            return;
        }
        let pending = self.take_pending(session);
        match pending {
            Some(PendingPlacement::Top) => {
                self.mark_manual_vertical(session);
                set_viewport_scroll(session, 0);
                self.clear_selected_continuation(session);
            }
            Some(PendingPlacement::Bottom) => {
                self.scroll_to_bottom_ready(session, inner);
                return;
            }
            Some(PendingPlacement::Cursor) => {
                self.mark_cursor_placement(session);
                self.clear_selected_continuation(session);
                self.ensure_cursor_visible_inner(session, inner);
                return;
            }
            None => {}
        }
        let layout = self.layout_for(session, inner);
        let window = self.window(session, &layout, inner.height.max(1) as usize);
        self.set_from_line(session, &layout, window.start);
        if keep_cursor_visible {
            self.ensure_cursor_visible_with(session, inner, &layout);
        }
    }

    /// Logical selection transition: bind any new logical top and then place
    /// the cursor according to measured geometry.
    pub(super) fn logical_selection(&self, session: &mut ReviewSession, inner: Rect) {
        self.revalidate_selected_annotation(session);
        self.mark_cursor_placement(session);
        if !usable_geometry(session, inner) {
            self.set_pending(session, PendingPlacement::Cursor);
            return;
        }
        self.take_pending(session);
        self.ensure_cursor_visible_inner(session, inner);
    }

    pub(super) fn visual_scroll(&self, session: &mut ReviewSession, inner: Rect, delta: isize) {
        if !usable_geometry(session, inner) {
            return;
        }
        let layout = self.layout_for(session, inner);
        if layout.line_count() == 0 {
            return;
        }
        let window = self.window(session, &layout, inner.height.max(1) as usize);
        let maximum_top = layout
            .line_count()
            .saturating_sub(inner.height.max(1) as usize);
        let target = window.start.saturating_add_signed(delta).min(maximum_top);
        if target == window.start {
            return;
        }
        self.mark_manual_vertical(session);
        self.take_pending(session);
        self.set_from_line(session, &layout, target);
    }

    pub(super) fn horizontal_scroll(&self, session: &ReviewSession, inner: Rect, delta: isize) {
        if !usable_geometry(session, inner) {
            return;
        }
        let layout = self.layout_for(session, inner);
        let _ = self.window(session, &layout, inner.height.max(1) as usize);
        if session.diff_cues.soft_wrap {
            return;
        }
        let Some(path) = selected_path(session) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        let visual = state.by_file.entry(path).or_default();
        let target = visual
            .horizontal
            .saturating_add_signed(delta)
            .min(layout.horizontal_limit());
        if target == visual.horizontal {
            return;
        }
        visual.horizontal = target;
        visual.pending_placement = None;
        state.transition_epoch = state.transition_epoch.wrapping_add(1);
    }

    pub(super) fn scroll_to_bottom(&self, session: &mut ReviewSession, inner: Rect) {
        self.mark_manual_vertical(session);
        if !usable_geometry(session, inner) {
            scroll_viewport_to_bottom(session);
            self.set_pending(session, PendingPlacement::Bottom);
            return;
        }
        self.take_pending(session);
        self.scroll_to_bottom_ready(session, inner);
    }

    fn scroll_to_bottom_ready(&self, session: &mut ReviewSession, inner: Rect) {
        scroll_viewport_to_bottom(session);
        let layout = self.layout_for(session, inner);
        let target = layout
            .line_count()
            .saturating_sub(inner.height.max(1) as usize);
        self.set_from_line(session, &layout, target);
    }

    fn ensure_cursor_visible_inner(&self, session: &mut ReviewSession, inner: Rect) {
        if !usable_geometry(session, inner) {
            return;
        }
        let layout = self.layout_for(session, inner);
        self.ensure_cursor_visible_with(session, inner, &layout);
    }

    fn ensure_cursor_visible_with(
        &self,
        session: &mut ReviewSession,
        inner: Rect,
        layout: &DiffMeasurement,
    ) {
        let window = self.window(session, layout, inner.height.max(1) as usize);
        // Persist final-page normalization even when the cursor already fits.
        self.set_from_line(session, layout, window.start);
        let start = window.start;
        let end = start.saturating_add(inner.height.max(1) as usize);
        let Some((first, last)) = layout.cursor_bounds(viewport_cursor(session)) else {
            return;
        };
        let height = inner.height.max(1) as usize;
        let target = if last - first + 1 > height && first != start {
            Some(first)
        } else if last - first + 1 > height {
            None
        } else if first < start {
            Some(first)
        } else if last >= end {
            Some(last.saturating_add(1).saturating_sub(height))
        } else {
            None
        };
        if let Some(target) = target {
            self.set_from_line(session, layout, target);
        }
    }

    pub(super) fn cursor_is_visible(&self, session: &ReviewSession, inner: Rect) -> bool {
        if !usable_geometry(session, inner) {
            return false;
        }
        let layout = self.layout_for(session, inner);
        let window = self.window(session, &layout, inner.height.max(1) as usize);
        layout.cursor_visible(
            viewport_cursor(session),
            window.start,
            inner.height.max(1) as usize,
        )
    }

    pub(super) fn row_at_point(
        &self,
        session: &ReviewSession,
        inner: Rect,
        x: u16,
        visible_row: usize,
    ) -> Option<usize> {
        if !usable_geometry(session, inner) {
            return None;
        }
        let layout = self.layout_for(session, inner);
        let window = self.window(session, &layout, inner.height.max(1) as usize);
        layout.row_at(
            window.start + visible_row,
            x.saturating_sub(inner.x) as usize,
        )
    }

    pub(super) fn hit_at_point(
        &self,
        session: &ReviewSession,
        inner: Rect,
        x: u16,
        visible_row: usize,
    ) -> Option<DiffPointHit> {
        if !usable_geometry(session, inner) {
            return None;
        }
        let layout = self.layout_for(session, inner);
        let window = self.window(session, &layout, inner.height.max(1) as usize);
        layout.hit_at(
            window.start + visible_row,
            x.saturating_sub(inner.x) as usize,
        )
    }

    /// File switch/restore transition. Per-file visual state is restored only
    /// when it is still paired with the session's logical top.
    pub(super) fn file_restored(&self, session: &ReviewSession) {
        self.revalidate_selected_annotation(session);
        let Some(path) = selected_path(session) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        bind_logical_top(
            state.by_file.entry(path).or_default(),
            viewport_scroll(session),
            viewport_top_identity(session),
        );
    }

    pub(super) fn place_top(&self, session: &mut ReviewSession, inner: Rect) {
        self.mark_manual_vertical(session);
        set_viewport_scroll(session, 0);
        self.clear_selected_continuation(session);
        if !usable_geometry(session, inner) {
            self.set_pending(session, PendingPlacement::Top);
            return;
        }
        self.take_pending(session);
        self.reflow(session, inner, false);
    }

    /// Placement transition used by jumps: the logical top is
    /// chosen by the caller, while stale continuation is discarded here.
    pub(super) fn place_cursor(&self, session: &mut ReviewSession, inner: Rect) {
        self.mark_cursor_placement(session);
        self.clear_selected_continuation(session);
        if !usable_geometry(session, inner) {
            self.set_pending(session, PendingPlacement::Cursor);
            return;
        }
        self.take_pending(session);
        self.ensure_cursor_visible_inner(session, inner);
    }

    pub(super) fn transition_snapshot(
        &self,
        session: &ReviewSession,
        inner: Rect,
    ) -> TransitionSnapshot {
        let (transition_epoch, cursor_following) = {
            let state = self.state.borrow();
            let following = selected_path(session)
                .and_then(|path| state.by_file.get(&path))
                .is_none_or(|visual| visual.cursor_placement_epoch >= visual.manual_scroll_epoch);
            (state.transition_epoch, following)
        };
        TransitionSnapshot {
            path: selected_path(session),
            status: session.selected_file().map(|file| file.status),
            old_path: session
                .selected_file()
                .and_then(|file| file.old_path.clone()),
            logical_top: viewport_scroll(session),
            top_identity: viewport_top_identity(session),
            cursor: viewport_cursor(session),
            cursor_identity: viewport_cursor_identity(session),
            cursor_was_visible: usable_geometry(session, inner)
                .then(|| self.cursor_is_visible(session, inner)),
            cursor_following,
            transition_epoch,
        }
    }

    pub(super) fn finish_transition(
        &self,
        snapshot: TransitionSnapshot,
        session: &mut ReviewSession,
        inner: Rect,
    ) {
        self.finish_transition_policy(snapshot, session, inner, true);
    }

    pub(super) fn finish_transition_top_only(
        &self,
        snapshot: TransitionSnapshot,
        session: &mut ReviewSession,
        inner: Rect,
    ) {
        self.finish_transition_policy(snapshot, session, inner, false);
    }

    fn finish_transition_policy(
        &self,
        snapshot: TransitionSnapshot,
        session: &mut ReviewSession,
        inner: Rect,
        restore_cursor: bool,
    ) {
        let state = self.state.borrow();
        let current_path = selected_path(session);
        let same_file = snapshot.path.is_some() && snapshot.path == current_path
            || session.selected_file().is_some_and(|file| {
                (file.status == FileStatus::Renamed
                    && file.old_path.as_ref() == snapshot.path.as_ref()
                    && current_path.as_ref() == Some(&file.path))
                    || (snapshot.status == Some(FileStatus::Renamed)
                        && !matches!(file.status, FileStatus::Renamed | FileStatus::Copied)
                        && snapshot.old_path.as_ref() == current_path.as_ref())
                    || (snapshot.status == Some(FileStatus::Renamed)
                        && file.status == FileStatus::Renamed
                        && snapshot.old_path.is_some()
                        && snapshot.old_path == file.old_path)
            });
        let explicit_transition = snapshot.transition_epoch != state.transition_epoch;
        drop(state);

        // Row-producing mutations (folds and context expansion in particular)
        // may insert/remove rows before a detached top. Re-resolve the app's
        // durable logical anchor before binding controller-owned continuation.
        // Explicit scroll/placement transitions have already chosen a new top.
        let (top_recovered, cursor_recovered) = if same_file && !explicit_transition {
            (
                restore_viewport_top_identity(
                    session,
                    snapshot.top_identity.as_ref(),
                    snapshot.logical_top,
                ),
                restore_cursor
                    && restore_viewport_cursor_identity(
                        session,
                        snapshot.cursor_identity.as_ref(),
                        snapshot.cursor,
                    ),
            )
        } else {
            (false, false)
        };
        if top_recovered {
            // A semantic duplicate may retain its source identity while its
            // projection occurrence changes. Rebind without discarding the
            // continuation that was attached to the recovered row.
            self.rebind_selected_top(session);
        } else {
            self.file_restored(session);
        }

        let state = self.state.borrow();
        let same_cursor = cursor_recovered
            || snapshot.cursor_identity == viewport_cursor_identity(session)
                && (snapshot.cursor_identity.is_some()
                    || snapshot.cursor == viewport_cursor(session));
        let preserve_cursor = restore_cursor
            && same_file
            && same_cursor
            && !explicit_transition
            && snapshot.cursor_following
            && snapshot.cursor_was_visible == Some(true);
        drop(state);
        self.reflow(session, inner, preserve_cursor);
    }

    pub(super) fn refresh_snapshot(&self, session: &ReviewSession, inner: Rect) -> RefreshSnapshot {
        RefreshSnapshot {
            transition: self.transition_snapshot(session, inner),
            files: session
                .files
                .iter()
                .map(|file| ViewportFileLineage {
                    path: file.path.clone(),
                    status: file.status,
                    old_path: file.old_path.clone(),
                })
                .collect(),
        }
    }

    /// Refresh identity transition. Rename the per-file visual slot, retain a
    /// continuation only when the app-owned logical row identity survived,
    /// and then normalize against fresh measured geometry.
    pub(super) fn refreshed(
        &self,
        snapshot: RefreshSnapshot,
        session: &mut ReviewSession,
        inner: Rect,
    ) {
        let mut state = self.state.borrow_mut();
        let mut previous = std::mem::take(&mut state.by_file);
        let mut current = BTreeMap::new();
        let current_files = session
            .files
            .iter()
            .map(|file| ViewportFileLineage {
                path: file.path.clone(),
                status: file.status,
                old_path: file.old_path.clone(),
            })
            .collect::<Vec<_>>();
        let mapping = viewport_path_mapping(&snapshot.files, &current_files);
        for file in &current_files {
            if let Some(prior_path) = mapping.get(&file.path)
                && let Some(visual) = previous.remove(prior_path)
            {
                current.insert(file.path.clone(), visual);
            }
        }
        state.by_file = current;
        let current_anchors = session.logical_viewport_anchors();
        for (path, (logical_top, current_identity)) in &current_anchors {
            let visual = state.by_file.entry(path.clone()).or_default();
            bind_logical_top(visual, *logical_top, current_identity.clone());
            visual.layout_identity = None;
        }
        drop(state);
        self.expanded_cards.borrow_mut().clear();
        self.invalidate_layout();
        self.finish_transition(snapshot.transition, session, inner);
        self.revalidate_selected_annotation(session);
    }

    /// Fresh target/reset transition. No terminal geometry leaks between
    /// logical review identities.
    pub(super) fn reset(&self, session: &ReviewSession) {
        self.mark_explicit_transition();
        self.state.borrow_mut().by_file.clear();
        self.expanded_cards.borrow_mut().clear();
        self.selected_annotation.borrow_mut().take();
        self.invalidate_layout();
        self.file_restored(session);
    }

    fn clear_selected_continuation(&self, session: &ReviewSession) {
        let Some(path) = selected_path(session) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        let visual = state.by_file.entry(path).or_default();
        visual.logical_top = viewport_scroll(session);
        visual.bound_top_identity = viewport_top_identity(session);
        visual.continuation = 0;
        visual.layout_identity = None;
    }

    fn rebind_selected_top(&self, session: &ReviewSession) {
        let Some(path) = selected_path(session) else {
            return;
        };
        let mut state = self.state.borrow_mut();
        let visual = state.by_file.entry(path).or_default();
        visual.logical_top = viewport_scroll(session);
        visual.bound_top_identity = viewport_top_identity(session);
        visual.layout_identity = None;
    }

    fn set_from_line(
        &self,
        session: &mut ReviewSession,
        measurement: &DiffMeasurement,
        index: usize,
    ) {
        let Some(path) = selected_path(session) else {
            return;
        };
        let (logical_top, continuation) = measurement.viewport_from_line(index);
        set_viewport_scroll(session, logical_top);
        let materialized = session.stream_mode
            && session.materialize_stream_window_reanchored(logical_top as usize, 200) > 0;
        let effective_top = viewport_scroll(session);
        let identity = (!materialized).then(|| measurement.identity.clone());
        let mut state = self.state.borrow_mut();
        let visual = state.by_file.entry(path).or_default();
        visual.logical_top = effective_top;
        visual.bound_top_identity = viewport_top_identity(session);
        visual.continuation = if materialized { 0 } else { continuation };
        visual.layout_identity = identity;
    }

    fn layout_for(&self, session: &ReviewSession, inner: Rect) -> DiffMeasurement {
        let split_active = diff_split_is_active(session, inner);
        self.measure(session, inner, split_active)
    }

    fn invalidate_layout(&self) {
        self.cache.borrow_mut().entry = None;
    }

    fn mark_explicit_transition(&self) {
        let mut state = self.state.borrow_mut();
        state.transition_epoch = state.transition_epoch.wrapping_add(1);
    }

    fn mark_manual_vertical(&self, session: &ReviewSession) {
        let Some(path) = selected_path(session) else {
            self.mark_explicit_transition();
            return;
        };
        let mut state = self.state.borrow_mut();
        state.transition_epoch = state.transition_epoch.wrapping_add(1);
        state.placement_epoch = state.placement_epoch.wrapping_add(1);
        let epoch = state.placement_epoch;
        state.by_file.entry(path).or_default().manual_scroll_epoch = epoch;
    }

    fn mark_cursor_placement(&self, session: &ReviewSession) {
        let Some(path) = selected_path(session) else {
            self.mark_explicit_transition();
            return;
        };
        let mut state = self.state.borrow_mut();
        state.transition_epoch = state.transition_epoch.wrapping_add(1);
        state.placement_epoch = state.placement_epoch.wrapping_add(1);
        let epoch = state.placement_epoch;
        state
            .by_file
            .entry(path)
            .or_default()
            .cursor_placement_epoch = epoch;
    }

    fn set_pending(&self, session: &ReviewSession, pending: PendingPlacement) {
        let Some(path) = selected_path(session) else {
            return;
        };
        self.state
            .borrow_mut()
            .by_file
            .entry(path)
            .or_default()
            .pending_placement = Some(pending);
    }

    fn set_pending_if_empty(&self, session: &ReviewSession, pending: PendingPlacement) {
        let Some(path) = selected_path(session) else {
            return;
        };
        self.state
            .borrow_mut()
            .by_file
            .entry(path)
            .or_default()
            .pending_placement
            .get_or_insert(pending);
    }

    fn take_pending(&self, session: &ReviewSession) -> Option<PendingPlacement> {
        let path = selected_path(session)?;
        self.state
            .borrow_mut()
            .by_file
            .entry(path)
            .or_default()
            .pending_placement
            .take()
    }

    #[cfg(test)]
    pub(super) fn visual_state(&self, session: &ReviewSession) -> (usize, usize) {
        selected_path(session)
            .and_then(|path| self.state.borrow().by_file.get(&path).cloned())
            .map(|visual| (visual.continuation, visual.horizontal))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(super) fn cache_builds(&self) -> usize {
        self.cache.borrow().builds
    }

    #[cfg(test)]
    pub(super) fn measurement_requests(&self) -> usize {
        self.cache.borrow().measurement_requests
    }
}

fn viewport_cursor(session: &ReviewSession) -> usize {
    if session.stream_mode {
        session.stream_cursor
    } else {
        session.diff_cursor
    }
}

fn viewport_scroll(session: &ReviewSession) -> u16 {
    if session.stream_mode {
        session.stream_scroll
    } else {
        session.diff_scroll
    }
}

fn set_viewport_scroll(session: &mut ReviewSession, value: u16) {
    if session.stream_mode {
        session.stream_scroll = value;
    } else {
        session.diff_scroll = value;
    }
}

fn viewport_top_identity(session: &ReviewSession) -> Option<DiffRowIdentity> {
    if session.stream_mode {
        session.stream_top_identity()
    } else {
        session.diff_top_identity()
    }
}

fn viewport_cursor_identity(session: &ReviewSession) -> Option<DiffRowIdentity> {
    if session.stream_mode {
        session.stream_cursor_identity()
    } else {
        session.diff_cursor_identity()
    }
}

fn restore_viewport_top_identity(
    session: &mut ReviewSession,
    identity: Option<&DiffRowIdentity>,
    fallback: u16,
) -> bool {
    if session.stream_mode {
        session.restore_stream_top_identity(identity, fallback)
    } else {
        session.restore_diff_top_identity(identity, fallback)
    }
}

fn restore_viewport_cursor_identity(
    session: &mut ReviewSession,
    identity: Option<&DiffRowIdentity>,
    fallback: usize,
) -> bool {
    if session.stream_mode {
        session.restore_stream_cursor_identity(identity, fallback)
    } else {
        session.restore_diff_cursor_identity(identity, fallback)
    }
}

fn scroll_viewport_to_bottom(session: &mut ReviewSession) {
    if session.stream_mode {
        session.stream_scroll_to_bottom();
    } else {
        session.scroll_diff_to_bottom();
    }
}

fn selected_path(session: &ReviewSession) -> Option<String> {
    if session.stream_mode {
        (!session.review_stream().rows.is_empty()).then(|| "@review-stream".to_owned())
    } else {
        session.selected_file().map(|file| file.path.clone())
    }
}

fn annotation_scope(session: &ReviewSession) -> String {
    let durable = session
        .active_durable_session()
        .map(|review| review.id.as_str())
        .unwrap_or("legacy");
    format!(
        "{}|{}|{}|{}",
        durable,
        session.repo.display(),
        session.target.base,
        session.target.rev
    )
}

fn usable_geometry(session: &ReviewSession, inner: Rect) -> bool {
    inner.width > 0
        && inner.height > 0
        && if session.stream_mode {
            !session.review_stream().rows.is_empty()
        } else {
            session.selected_visible_file().is_some()
        }
}

fn viewport_path_mapping(
    previous: &[ViewportFileLineage],
    current: &[ViewportFileLineage],
) -> BTreeMap<String, String> {
    let mut mapping = BTreeMap::new();
    let mut used = std::collections::BTreeSet::new();
    let mut assign = |matches: &dyn Fn(&ViewportFileLineage, &ViewportFileLineage) -> bool| {
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

fn bind_logical_top(
    visual: &mut VisualViewport,
    logical_top: u16,
    top_identity: Option<DiffRowIdentity>,
) {
    if visual.bound_top_identity != top_identity
        || (top_identity.is_none() && visual.logical_top != logical_top)
    {
        visual.continuation = 0;
        visual.layout_identity = None;
    }
    visual.logical_top = logical_top;
    visual.bound_top_identity = top_identity;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        config::DiffViewModeConfig,
        diff::DiffSet,
        jj::ReviewTarget,
        state::{Comment, ReviewState},
    };

    fn session(diff: &str) -> ReviewSession {
        ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(diff).unwrap(),
            ReviewState::default(),
        )
    }

    fn long_session() -> ReviewSession {
        session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n+{}\n",
            "0123456789 wrapped ".repeat(30)
        ))
    }

    fn long_row(session: &ReviewSession) -> usize {
        session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text.starts_with("0123456789"))
            .unwrap()
    }

    #[test]
    fn transition_matrix_normalizes_wrap_nowrap_and_logical_rebinding() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 24, 2);
        session.diff_cues.soft_wrap = false;
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);

        controller.horizontal_scroll(&session, inner, isize::MAX);
        let nowrap_horizontal = controller.visual_state(&session).1;
        assert!(nowrap_horizontal > 0);
        let limit = controller.layout_for(&session, inner).horizontal_limit();
        assert_eq!(nowrap_horizontal, limit);

        session.toggle_diff_wrap();
        controller.reflow(&mut session, inner, false);
        assert_eq!(controller.visual_state(&session).1, 0, "wrap zeros x");

        controller.visual_scroll(&mut session, Rect::new(0, 0, 24, 1), 1);
        assert!(controller.visual_state(&session).0 > 0);
        session.diff_scroll = 0;
        controller.file_restored(&session);
        assert_eq!(
            controller.visual_state(&session).0,
            0,
            "a continuation never attaches to a different logical top"
        );
    }

    #[test]
    fn clamped_visual_scroll_does_not_detach_cursor_following() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 4);
        controller.logical_selection(&mut session, inner);
        let path = selected_path(&session).unwrap();
        let before = controller.state.borrow().by_file[&path].manual_scroll_epoch;

        controller.visual_scroll(&mut session, inner, -1);

        assert_eq!(
            controller.state.borrow().by_file[&path].manual_scroll_epoch,
            before
        );
    }

    #[test]
    fn detached_scroll_survives_reflow_but_cursor_policy_can_reattach() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=20 {
            body.push_str(&format!(" line {line} {}\n", "wide ".repeat(12)));
        }
        let mut session = session(&body);
        let controller = DiffViewportController::default();
        let old = Rect::new(0, 0, 28, 4);
        let new = Rect::new(0, 0, 70, 4);
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();

        controller.reflow(&mut session, old, false);
        assert!(!controller.cursor_is_visible(&session, old));
        let transition = controller.transition_snapshot(&session, old);
        controller.finish_transition(transition, &mut session, new);
        assert_eq!(session.diff_scroll, 0, "detached top survives reflow");

        session.diff_scroll = session.diff_cursor as u16;
        controller.file_restored(&session);
        controller.logical_selection(&mut session, old);
        let transition = controller.transition_snapshot(&session, old);
        controller.finish_transition(transition, &mut session, new);
        assert!(controller.cursor_is_visible(&session, new));
    }

    #[test]
    fn top_only_transition_never_follows_cursor() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=20 {
            body.push_str(&format!(" line {line}\n"));
        }
        let mut session = session(&body);
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 3);
        let transition = controller.transition_snapshot(&session, inner);
        session.diff_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();
        let top = session.diff_top_identity();
        controller.finish_transition_top_only(transition, &mut session, inner);
        assert_eq!(session.diff_top_identity(), top);
        assert!(!controller.cursor_is_visible(&session, inner));
    }

    #[test]
    fn file_switch_restores_visual_state_per_file() {
        let long = "abcdefghij ".repeat(25);
        let mut session = session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+{long}a\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old b\n+{long}b\n"
        ));
        session.diff_cues.soft_wrap = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 2);
        controller.horizontal_scroll(&session, inner, 7);
        assert_eq!(controller.visual_state(&session).1, 7);

        session.select_file_index(1);
        controller.file_restored(&session);
        assert_eq!(controller.visual_state(&session).1, 0);
        controller.horizontal_scroll(&session, inner, 3);
        session.select_file_index(0);
        controller.file_restored(&session);
        assert_eq!(controller.visual_state(&session).1, 7);
        session.select_file_index(1);
        controller.file_restored(&session);
        assert_eq!(controller.visual_state(&session).1, 3);
    }

    #[test]
    fn refresh_rename_migrates_visual_slot_and_reset_discards_it() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 24, 1);
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, inner, 1);
        let before = controller.visual_state(&session).0;
        assert!(before > 0);
        let snapshot = controller.refresh_snapshot(&session, inner);

        let renamed = DiffSet::parse(&format!(
            "diff --git a/a.txt b/b.txt\nsimilarity index 90%\nrename from a.txt\nrename to b.txt\n--- a/a.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old\n+{}\n",
            "0123456789 wrapped ".repeat(30)
        ))
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), renamed);
        controller.refreshed(snapshot, &mut session, inner);

        assert_eq!(session.selected_file().unwrap().path, "b.txt");
        assert_eq!(controller.visual_state(&session).0, before);
        controller.reset(&session);
        assert_eq!(controller.visual_state(&session), (0, 0));
    }

    #[test]
    fn refresh_rebinds_continuation_when_logical_identity_moves() {
        let long = "0123456789 wrapped ".repeat(30);
        let mut session = session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10 +10 @@\n-old\n+{long}\n"
        ));
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 24, 1);
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, inner, 1);
        let before = controller.visual_state(&session).0;
        let old_top = session.diff_scroll;
        let snapshot = controller.refresh_snapshot(&session, inner);

        let refreshed = DiffSet::parse(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-before\n+after\n@@ -10 +10 @@\n-old\n+{long}\n"
        ))
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        assert_ne!(session.diff_scroll, old_top);
        controller.refreshed(snapshot, &mut session, inner);

        assert_eq!(controller.visual_state(&session).0, before);
    }

    #[test]
    fn preceding_hunk_insertion_restores_shifted_top_and_cursor_identity() {
        let mut session = session(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10 +10 @@\n-x\n+y\n@@ -30 +30 @@\n-x\n+y\n",
        );
        let rows = session.diff_rows_for_selected_file();
        session.diff_scroll = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row.kind, crate::app::DiffRowKind::HunkHeader))
            .nth(1)
            .unwrap()
            .0 as u16;
        session.diff_cursor = rows
            .iter()
            .position(|row| row.new_lineno == Some(30))
            .unwrap();
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 40, 1);
        controller.file_restored(&session);
        let transition = controller.transition_snapshot(&session, inner);

        let shifted = DiffSet::parse(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-x\n+y\n@@ -11 +11 @@\n-x\n+y\n@@ -31 +31 @@\n-x\n+y\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), shifted);
        let restored_rows = session.diff_rows_for_selected_file();
        assert_eq!(
            restored_rows[session.diff_cursor].new_lineno,
            Some(31),
            "app restoration must recover the shifted cursor: cursor={}, rows={:?}",
            session.diff_cursor,
            restored_rows
                .iter()
                .map(|row| (row.kind, row.old_lineno, row.new_lineno, row.text.as_str()))
                .collect::<Vec<_>>()
        );
        controller.finish_transition(transition, &mut session, inner);

        let shifted_rows = session.diff_rows_for_selected_file();
        assert_eq!(
            shifted_rows[session.diff_scroll as usize]
                .logical_range
                .as_ref()
                .unwrap()
                .start,
            31
        );
        assert_eq!(shifted_rows[session.diff_cursor].new_lineno, Some(31));
    }

    #[test]
    fn refresh_rename_preserves_inactive_file_visual_state() {
        let long = "abcdefghij ".repeat(25);
        let mut session = session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+{long}a\ndiff --git a/c.txt b/c.txt\n--- a/c.txt\n+++ b/c.txt\n@@ -1 +1 @@\n-old c\n+new c\n"
        ));
        session.diff_cues.soft_wrap = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 2);
        controller.horizontal_scroll(&session, inner, 7);
        session.select_file_index(1);
        controller.file_restored(&session);
        let snapshot = controller.refresh_snapshot(&session, inner);

        let refreshed = DiffSet::parse(&format!(
            "diff --git a/a.txt b/b.txt\nsimilarity index 90%\nrename from a.txt\nrename to b.txt\n--- a/a.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old a\n+{long}a\ndiff --git a/c.txt b/c.txt\n--- a/c.txt\n+++ b/c.txt\n@@ -1 +1 @@\n-old c\n+new c\n"
        ))
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        controller.refreshed(snapshot, &mut session, inner);
        let renamed = session
            .files
            .iter()
            .position(|file| file.path == "b.txt")
            .unwrap();
        session.select_file_index(renamed);
        controller.file_restored(&session);

        assert_eq!(controller.visual_state(&session).1, 7);
    }

    #[test]
    fn nonzero_continuation_is_clamped_across_narrow_and_wide_layouts() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let narrow = Rect::new(0, 0, 20, 1);
        let wide = Rect::new(0, 0, 80, 1);
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, narrow, 12);
        let narrow_continuation = controller.visual_state(&session).0;
        assert!(narrow_continuation > 1);

        controller.reflow(&mut session, wide, false);
        let wide_continuation = controller.visual_state(&session).0;
        assert!(wide_continuation > 0);
        assert!(wide_continuation < narrow_continuation);
        assert_eq!(session.diff_scroll as usize, long_row(&session));

        controller.reflow(&mut session, narrow, false);
        assert_eq!(controller.visual_state(&session).0, wide_continuation);
    }

    #[test]
    fn vertical_placements_preserve_x_while_wrap_normalizes_it() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 24, 2);
        session.diff_cues.soft_wrap = false;
        controller.horizontal_scroll(&session, inner, 9);
        assert_eq!(controller.visual_state(&session).1, 9);

        controller.place_top(&mut session, inner);
        assert_eq!(controller.visual_state(&session).1, 9);
        session.diff_cursor = long_row(&session);
        controller.place_cursor(&mut session, inner);
        assert_eq!(controller.visual_state(&session).1, 9);
        controller.place_cursor(&mut session, inner);
        assert_eq!(controller.visual_state(&session).1, 9);

        let transition = controller.transition_snapshot(&session, inner);
        session.toggle_diff_wrap();
        controller.finish_transition(transition, &mut session, inner);
        assert_eq!(controller.visual_state(&session).1, 0);
    }

    #[test]
    fn split_identity_tracks_actual_narrow_fallback_without_losing_anchor() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        session.diff_cues.view = DiffViewModeConfig::SideBySide;
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, Rect::new(0, 0, 99, 1), 1);
        let before = controller.visual_state(&session).0;
        let narrow = controller.layout_for(&session, Rect::new(0, 0, 99, 1));
        let _ = controller.window(&session, &narrow, 1);
        let narrow_identity = controller
            .state
            .borrow()
            .by_file
            .get("a.txt")
            .unwrap()
            .layout_identity
            .clone()
            .unwrap();
        assert!(!narrow_identity.split_active);

        controller.reflow(&mut session, Rect::new(0, 0, MIN_SPLIT_WIDTH, 1), false);
        let split_identity = controller
            .state
            .borrow()
            .by_file
            .get("a.txt")
            .unwrap()
            .layout_identity
            .clone()
            .unwrap();
        assert!(split_identity.split_active);
        assert!(controller.visual_state(&session).0 <= before);
        assert_eq!(
            controller.row_at_point(&session, Rect::new(0, 0, MIN_SPLIT_WIDTH, 1), 80, 0,),
            Some(long_row(&session)),
        );
    }

    #[test]
    fn comments_growing_and_shrinking_above_detached_top_keep_its_anchor() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,12 +1,12 @@\n",
        );
        for line in 1..=12 {
            body.push_str(&format!(" line {line} {}\n", "wrapped ".repeat(5)));
        }
        let mut session = session(&body);
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 3);
        let rows = session.diff_rows_for_selected_file();
        let above = rows
            .iter()
            .position(|row| row.text.starts_with("line 2 "))
            .unwrap();
        let top = rows
            .iter()
            .position(|row| row.text.starts_with("line 8 "))
            .unwrap();
        session.diff_scroll = top as u16;
        session.diff_cursor = rows.iter().rposition(|row| row.anchor.is_some()).unwrap();
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, inner, 1);
        let top_identity = session.diff_top_identity();
        let continuation = controller.visual_state(&session).0;
        let anchor = rows[above].anchor.clone().unwrap();

        let transition = controller.transition_snapshot(&session, inner);
        session.comments.push(Comment {
            id: "above".into(),
            path: Some(anchor.path().into()),
            line: anchor.line(),
            anchor: Some(anchor),
            body: "comment summary that wraps above the detached top".into(),
            ..Comment::default()
        });
        controller.finish_transition(transition, &mut session, inner);
        assert_eq!(session.diff_top_identity(), top_identity);
        assert_eq!(controller.visual_state(&session).0, continuation);

        let transition = controller.transition_snapshot(&session, inner);
        session.comments.clear();
        controller.finish_transition(transition, &mut session, inner);
        assert_eq!(session.diff_top_identity(), top_identity);
        assert_eq!(controller.visual_state(&session).0, continuation);
    }

    #[test]
    fn row_projection_reflow_restores_detached_top_identity() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=11 {
            body.push_str(&format!(" context {line}\n"));
        }
        body.push_str("-before\n+target\n");
        for line in 13..=20 {
            body.push_str(&format!(" context {line}\n"));
        }
        let mut session = session(&body);
        session.fold_context = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 40, 3);
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "target")
            .unwrap();
        session.diff_scroll = target as u16;
        session.diff_cursor = target;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, inner, 1);
        controller.visual_scroll(&mut session, inner, -1);
        let top_identity = session.diff_top_identity();

        let transition = controller.transition_snapshot(&session, inner);
        session.toggle_context_fold();
        controller.finish_transition(transition, &mut session, inner);

        assert_eq!(session.diff_top_identity(), top_identity);
        assert_eq!(
            session.diff_rows_for_selected_file()[session.diff_cursor].text,
            "target"
        );
    }

    #[test]
    fn invalid_same_index_identity_resets_inactive_continuation() {
        let long = "abcdefghij ".repeat(25);
        let mut session = session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+{long}a\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old b\n+new b\n"
        ));
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 24, 1);
        session.diff_scroll = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text.starts_with("abcdefghij"))
            .unwrap() as u16;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, inner, 1);
        assert!(controller.visual_state(&session).0 > 0);
        session.select_file_index(1);
        controller.file_restored(&session);
        let snapshot = controller.refresh_snapshot(&session, inner);

        let replaced = DiffSet::parse(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+unrelated replacement\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old b\n+new b\n",
        )
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), replaced);
        controller.refreshed(snapshot, &mut session, inner);
        session.select_file_index(0);
        controller.file_restored(&session);

        assert_eq!(controller.visual_state(&session).0, 0);
    }

    #[test]
    fn empty_tiny_tall_final_page_and_mouse_reflow_boundaries() {
        let mut empty = session("");
        let empty_controller = DiffViewportController::default();
        empty_controller.logical_selection(&mut empty, Rect::new(0, 0, 0, 0));
        assert_eq!(empty.diff_scroll, 0);

        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=20 {
            body.push_str(&format!(" line {line}\n"));
        }
        let mut session = session(&body);
        session.diff_cues.soft_wrap = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(4, 2, 24, 4);
        let last = session
            .diff_rows_for_selected_file()
            .iter()
            .rposition(|row| row.anchor.is_some())
            .unwrap();
        session.diff_cursor = last;
        session.diff_scroll = last as u16;
        controller.file_restored(&session);
        controller.logical_selection(&mut session, inner);
        assert!(
            session.diff_scroll < last as u16,
            "final page top is durable"
        );
        assert!(controller.cursor_is_visible(&session, inner));

        let first_visible = controller
            .row_at_point(&session, inner, inner.x + 10, 0)
            .unwrap();
        controller.reflow(&mut session, Rect::new(4, 2, 60, 4), false);
        let after_reflow = controller
            .row_at_point(&session, Rect::new(4, 2, 60, 4), 14, 0)
            .unwrap();
        assert_eq!(first_visible, after_reflow);

        let mut tall = long_session();
        let tall_controller = DiffViewportController::default();
        let owner = long_row(&tall);
        tall.diff_cursor = owner;
        tall.diff_scroll = owner as u16;
        tall_controller.file_restored(&tall);
        tall_controller.visual_scroll(&mut tall, Rect::new(0, 0, 20, 3), 5);
        tall_controller.logical_selection(&mut tall, Rect::new(0, 0, 20, 3));
        assert_eq!(tall.diff_scroll as usize, owner);
        assert_eq!(tall_controller.visual_state(&tall).0, 0);
    }

    #[test]
    fn visual_refresh_follows_successive_base_relative_rename_lineage() {
        let long = "lineage wrapped ".repeat(25);
        let mid = DiffSet::parse(&format!(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 90%\nrename from old.rs\nrename to mid.rs\n--- a/old.rs\n+++ b/mid.rs\n@@ -1 +1 @@\n-old\n+{long}\n"
        ))
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            mid,
            ReviewState::default(),
        );
        session.diff_cues.soft_wrap = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 2);
        controller.horizontal_scroll(&session, inner, 7);
        let snapshot = controller.refresh_snapshot(&session, inner);

        let new = DiffSet::parse(&format!(
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+{long}\n"
        ))
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), new);
        controller.refreshed(snapshot, &mut session, inner);

        assert_eq!(session.selected_file().unwrap().path, "new.rs");
        assert_eq!(controller.visual_state(&session).1, 7);
    }

    #[test]
    fn visual_refresh_follows_rename_return_to_base_path() {
        let long = "rename return ".repeat(20);
        let first = DiffSet::parse(&format!(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 90%\nrename from old.rs\nrename to mid.rs\n--- a/old.rs\n+++ b/mid.rs\n@@ -1 +1 @@\n-old\n+{long}\n"
        ))
        .unwrap();
        let mut session = ReviewSession::new(
            ".".into(),
            ReviewTarget::trunk_to_current(),
            first,
            ReviewState::default(),
        );
        session.diff_cues.soft_wrap = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 2);
        controller.horizontal_scroll(&session, inner, 7);
        let snapshot = controller.refresh_snapshot(&session, inner);
        let returned = DiffSet::parse(&format!(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-old\n+{long}\n"
        ))
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), returned);
        controller.refreshed(snapshot, &mut session, inner);
        assert_eq!(session.selected_file().unwrap().path, "old.rs");
        assert_eq!(controller.visual_state(&session).1, 7);
    }

    #[test]
    fn rename_return_keeps_a_followed_cursor_visible_after_annotation_reflow() {
        let mut body = String::from(
            "diff --git a/old.rs b/mid.rs\nsimilarity index 90%\nrename from old.rs\nrename to mid.rs\n--- a/old.rs\n+++ b/mid.rs\n@@ -1,8 +1,8 @@\n",
        );
        for line in 1..=8 {
            body.push_str(&format!(" line {line}\n"));
        }
        let mut session = session(&body);
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 32, 4);
        let target = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "line 8")
            .unwrap();
        session.diff_cursor = target;
        controller.logical_selection(&mut session, inner);
        assert!(controller.cursor_is_visible(&session, inner));
        let snapshot = controller.refresh_snapshot(&session, inner);

        let returned = body
            .replace("a/old.rs b/mid.rs", "a/old.rs b/old.rs")
            .replace(
                "similarity index 90%\nrename from old.rs\nrename to mid.rs\n",
                "",
            )
            .replace("+++ b/mid.rs", "+++ b/old.rs");
        session.replace_diff_preserving_view(
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(&returned).unwrap(),
        );
        let comment_row = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == "line 6")
            .unwrap();
        let anchor = session.diff_rows_for_selected_file()[comment_row]
            .anchor
            .clone()
            .unwrap();
        session.comments.push(Comment {
            id: "reflow".into(),
            path: Some(anchor.path().into()),
            line: anchor.line(),
            anchor: Some(anchor),
            body: "annotation that wraps across several terminal rows ".repeat(8),
            ..Comment::default()
        });

        controller.refreshed(snapshot, &mut session, inner);

        assert_eq!(session.selected_file().unwrap().path, "old.rs");
        assert!(controller.cursor_is_visible(&session, inner));
    }

    #[test]
    fn visual_refresh_never_consumes_copy_source_state() {
        let long = "copy source ".repeat(25);
        let mut session = session(&format!(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.diff_cues.soft_wrap = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 2);
        controller.horizontal_scroll(&session, inner, 7);
        let snapshot = controller.refresh_snapshot(&session, inner);

        let copied = DiffSet::parse(&format!(
            "diff --git a/old.rs b/copied.rs\nsimilarity index 100%\ncopy from old.rs\ncopy to copied.rs\n--- a/old.rs\n+++ b/copied.rs\n@@ -1 +1 @@\n-old\n+{long}\n"
        ))
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), copied);
        controller.refreshed(snapshot, &mut session, inner);

        assert_eq!(session.selected_file().unwrap().path, "copied.rs");
        assert_eq!(controller.visual_state(&session), (0, 0));
    }

    #[test]
    fn visual_refresh_reserves_rename_destination_before_recreated_source() {
        let long = "rename destination ".repeat(20);
        let mut session = session(&format!(
            "diff --git a/old.rs b/old.rs\n--- a/old.rs\n+++ b/old.rs\n@@ -1 +1 @@\n-old\n+{long}\n"
        ));
        session.diff_cues.soft_wrap = false;
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 30, 1);
        let anchored = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text == long)
            .unwrap();
        session.diff_scroll = anchored as u16;
        session.diff_cursor = anchored;
        controller.file_restored(&session);
        controller.horizontal_scroll(&session, inner, 7);
        let snapshot = controller.refresh_snapshot(&session, inner);
        let refreshed = DiffSet::parse(&format!(
            "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\n--- a/old.rs\n+++ b/new.rs\n@@ -1 +1 @@\n-old\n+{long}\ndiff --git a/old.rs b/old.rs\nnew file mode 100644\n--- /dev/null\n+++ b/old.rs\n@@ -0,0 +1 @@\n+recreated\n"
        ))
        .unwrap();
        session.replace_diff_preserving_view(ReviewTarget::trunk_to_current(), refreshed);
        controller.refreshed(snapshot, &mut session, inner);

        assert_eq!(session.selected_file().unwrap().path, "new.rs");
        assert_eq!(controller.visual_state(&session).1, 7);
        let renamed_rows = session.diff_rows_for_selected_file();
        assert_eq!(renamed_rows[session.diff_scroll as usize].text, long);
        assert_eq!(renamed_rows[session.diff_cursor].text, long);
        let recreated = session
            .files
            .iter()
            .position(|file| file.path == "old.rs")
            .unwrap();
        session.select_file_index(recreated);
        controller.file_restored(&session);
        assert_eq!(session.diff_scroll, 0);
        assert_eq!(session.diff_cursor, 0);
        assert_eq!(controller.visual_state(&session).1, 0);
    }

    #[test]
    fn hiding_every_file_does_not_destroy_its_visual_viewport() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let inner = Rect::new(0, 0, 24, 1);
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, inner, 2);
        let logical_top = session.diff_scroll;
        let visual = controller.visual_state(&session);
        assert!(visual.0 > 0);

        session.mark_all_viewed();
        let hide = controller.transition_snapshot(&session, inner);
        session.cycle_viewed_filter();
        controller.finish_transition(hide, &mut session, inner);
        assert!(session.selected_visible_file().is_none());
        assert_eq!(session.diff_scroll, logical_top);

        let show = controller.transition_snapshot(&session, inner);
        session.cycle_viewed_filter();
        controller.finish_transition(show, &mut session, inner);
        assert!(session.selected_visible_file().is_some());
        assert_eq!(session.diff_scroll, logical_top);
        assert_eq!(controller.visual_state(&session), visual);
    }

    #[test]
    fn zero_geometry_preserves_nonempty_viewport_until_recovery() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let usable = Rect::new(0, 0, 24, 2);
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);
        controller.visual_scroll(&mut session, usable, 2);
        let visual = controller.visual_state(&session);

        controller.visual_scroll(&mut session, Rect::new(0, 0, 0, 0), 5);
        assert_eq!(controller.visual_state(&session), visual);
        session.diff_cursor = long_row(&session);
        session.diff_scroll = 0;
        controller.logical_selection(&mut session, Rect::new(0, 0, 0, 0));
        assert!(!controller.cursor_is_visible(&session, Rect::new(0, 0, 0, 0)));
        assert_eq!(
            controller.row_at_point(&session, Rect::new(0, 0, 0, 0), 0, 0),
            None
        );

        controller.reflow(&mut session, usable, false);
        assert!(controller.cursor_is_visible(&session, usable));
        assert_eq!(session.diff_scroll as usize, long_row(&session));
    }

    #[test]
    fn zero_geometry_defers_top_bottom_and_cursor_placement() {
        let mut body = String::from(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1,20 +1,20 @@\n",
        );
        for line in 1..=20 {
            body.push_str(&format!(" line {line} {}\n", "wrapped ".repeat(4)));
        }
        let zero = Rect::new(0, 0, 0, 0);
        let usable = Rect::new(0, 0, 24, 3);

        let mut bottom = session(&body);
        bottom.focus = crate::app::Focus::Diff;
        let bottom_controller = DiffViewportController::default();
        let original_top = bottom.diff_scroll;
        bottom_controller.scroll_to_bottom(&mut bottom, zero);
        assert!(bottom.diff_scroll > original_top);
        bottom_controller.reflow(&mut bottom, usable, false);
        assert!(bottom.diff_scroll > original_top);
        assert_eq!(
            bottom.diff_cursor,
            bottom
                .diff_rows_for_selected_file()
                .iter()
                .rposition(|row| row.anchor.is_some())
                .unwrap()
        );

        let mut top = session(&body);
        let top_controller = DiffViewportController::default();
        top.diff_scroll = 10;
        top_controller.place_top(&mut top, zero);
        assert_eq!(top.diff_scroll, 0);
        top_controller.reflow(&mut top, usable, false);
        assert_eq!(top.diff_scroll, 0);
    }

    #[test]
    fn explicit_pending_placement_precedence_and_usable_cancellation() {
        let mut session = long_session();
        let controller = DiffViewportController::default();
        let zero = Rect::new(0, 0, 0, 0);
        let usable = Rect::new(0, 0, 24, 3);
        session.diff_cursor = long_row(&session);
        session.diff_scroll = session.diff_cursor as u16;

        controller.place_top(&mut session, zero);
        controller.reflow(&mut session, zero, true);
        controller.reflow(&mut session, usable, false);
        assert_eq!(
            session.diff_scroll, 0,
            "implicit follow must not replace explicit top"
        );

        controller.place_top(&mut session, zero);
        controller.place_cursor(&mut session, zero);
        controller.reflow(&mut session, usable, false);
        assert!(controller.cursor_is_visible(&session, usable));
        let after_cursor = session.diff_scroll;
        controller.logical_selection(&mut session, usable);
        controller.reflow(&mut session, usable, false);
        assert_eq!(session.diff_scroll, after_cursor);

        session.diff_cues.soft_wrap = false;
        controller.place_top(&mut session, zero);
        session.diff_scroll = long_row(&session) as u16;
        controller.file_restored(&session);
        controller.horizontal_scroll(&session, usable, 1);
        assert!(
            controller.state.borrow().by_file["a.txt"]
                .pending_placement
                .is_none()
        );
        controller.reflow(&mut session, usable, false);
        assert_ne!(
            session.diff_scroll, 0,
            "successful usable scroll must cancel an older deferred placement"
        );
    }

    #[test]
    fn keep_restore_consumes_pending_placement_for_returned_file() {
        let long = "pending file placement ".repeat(20);
        let mut session = session(&format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old a\n+{long}a\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-old b\n+{long}b\n"
        ));
        let controller = DiffViewportController::default();
        let a_cursor = session
            .diff_rows_for_selected_file()
            .iter()
            .position(|row| row.text.ends_with('a') && row.text.starts_with("pending"))
            .unwrap();
        session.diff_cursor = a_cursor;
        controller.place_cursor(&mut session, Rect::new(0, 0, 0, 0));
        session.select_file_index(1);
        controller.file_restored(&session);
        session.select_file_index(0);
        controller.file_restored(&session);
        let usable = Rect::new(0, 0, 30, 3);
        controller.reflow(&mut session, usable, false);
        assert!(controller.cursor_is_visible(&session, usable));
    }
}
