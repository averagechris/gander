//! Per-gap hunk context expansion (docs/focused-diff-ux.md §5): the math
//! for the runs of file lines hidden between/around hunks, and the session
//! operations that expand (`+`/`=`) and re-collapse (`-`) them using lazily
//! fetched full file contents.
//!
//! Context lines are identical on both sides of the diff, so the new-side
//! contents suffice; old line numbers derive from the hunk offsets. The
//! underlying `Hunk`s and all comment anchors are never modified.

use std::{ops::Range, rc::Rc};

use crate::diff::Hunk;

use super::{DiffRow, DiffRowKind, ReviewSession};

/// How far one gap of hidden context lines has been expanded. `above`
/// counts lines revealed at the top edge of the gap (just below the
/// preceding hunk); `below` counts lines revealed at the bottom edge (just
/// above the following hunk). Session-only state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Expansion {
    pub above: usize,
    pub below: usize,
}

/// One run of file lines the diff did not emit. Gap `i` sits directly above
/// hunk `i`; gap `hunks.len()` is the trailing gap below the last hunk
/// (only known once the full file contents are loaded).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapSpec {
    pub gap_id: usize,
    /// First hidden line, new-side numbering.
    pub new_start: usize,
    /// Total hidden lines when fully collapsed.
    pub hidden: usize,
    /// `new_lineno - old_lineno` for every line in this gap. Constant per
    /// gap because old/new numbering only drifts inside hunks.
    pub offset: isize,
}

/// What one gap currently shows: revealed line ranges (new-side numbering)
/// at the top and bottom edges, and the count still hidden between them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GapView {
    pub top: Range<usize>,
    pub bottom: Range<usize>,
    pub hidden: usize,
}

/// Gaps of hidden context for a file's hunks, in order. The trailing gap
/// below the last hunk is only emitted when `total_lines` (the full
/// new-side line count) is known.
pub fn file_gaps(hunks: &[Hunk], total_lines: Option<usize>) -> Vec<GapSpec> {
    let mut gaps = Vec::new();
    let Some(first) = hunks.first() else {
        return gaps;
    };

    let push = |gaps: &mut Vec<GapSpec>, gap_id, new_start: usize, hidden: usize, offset| {
        if hidden > 0 {
            gaps.push(GapSpec {
                gap_id,
                new_start,
                hidden,
                offset,
            });
        }
    };

    push(
        &mut gaps,
        0,
        1,
        first.new_start.saturating_sub(1),
        first.new_start as isize - first.old_start as isize,
    );
    for (index, window) in hunks.windows(2).enumerate() {
        let (previous, next) = (&window[0], &window[1]);
        let end_new = previous.new_start + previous.new_len;
        let end_old = previous.old_start + previous.old_len;
        push(
            &mut gaps,
            index + 1,
            end_new,
            next.new_start.saturating_sub(end_new),
            end_new as isize - end_old as isize,
        );
    }
    if let (Some(total), Some(last)) = (total_lines, hunks.last()) {
        let end_new = last.new_start + last.new_len;
        let end_old = last.old_start + last.old_len;
        push(
            &mut gaps,
            hunks.len(),
            end_new,
            (total + 1).saturating_sub(end_new),
            end_new as isize - end_old as isize,
        );
    }
    gaps
}

/// Project an expansion onto a gap, clamping so the revealed edges never
/// overlap: `above` wins the remainder, `below` takes what is left.
pub fn gap_view(spec: &GapSpec, expansion: Option<Expansion>) -> GapView {
    let expansion = expansion.unwrap_or_default();
    let above = expansion.above.min(spec.hidden);
    let below = expansion.below.min(spec.hidden - above);
    GapView {
        top: spec.new_start..spec.new_start + above,
        bottom: spec.new_start + spec.hidden - below..spec.new_start + spec.hidden,
        hidden: spec.hidden - above - below,
    }
}

impl ReviewSession {
    /// Expand the gap nearest the diff cursor by `step` lines (`None`
    /// expands fully). Requires the file contents to be loaded (see
    /// [`ReviewSession::store_file_contents`]); returns `false` when there
    /// is no expandable gap.
    pub fn expand_nearest_gap(&mut self, step: Option<usize>) -> bool {
        let Some((path, spec, trailing)) =
            self.nearest_gap(|row| matches!(row.kind, DiffRowKind::ExpandGap { .. }))
        else {
            return false;
        };
        let anchor = self.selected_line_anchor();
        let expansion = self
            .context_expansion
            .entry((path, spec.gap_id))
            .or_default();
        // Middle/top gaps grow upward from the hunk below (the code being
        // read); the trailing gap grows downward from the last hunk.
        match (step, trailing) {
            (Some(step), false) => {
                expansion.below = (expansion.below + step).min(spec.hidden - expansion.above);
            }
            (Some(step), true) => {
                expansion.above = (expansion.above + step).min(spec.hidden - expansion.below);
            }
            (None, false) => expansion.below = spec.hidden - expansion.above,
            (None, true) => expansion.above = spec.hidden - expansion.below,
        }
        self.expansion_epoch += 1;
        self.restore_diff_cursor_anchor(anchor);
        true
    }

    /// Re-collapse the expanded gap nearest the diff cursor to its original
    /// state. Returns `false` when no nearby gap is expanded.
    pub fn collapse_nearest_gap(&mut self) -> bool {
        let Some(file_path) = self.selected_visible_file().map(|file| file.path.clone()) else {
            return false;
        };
        let expanded: std::collections::BTreeSet<usize> = self
            .context_expansion
            .keys()
            .filter(|(path, _)| *path == file_path)
            .map(|(_, gap_id)| *gap_id)
            .collect();
        let Some((path, spec, _)) =
            self.nearest_gap(|row| row.gap.is_some_and(|gap_id| expanded.contains(&gap_id)))
        else {
            return false;
        };
        let anchor = self.selected_line_anchor();
        self.context_expansion.remove(&(path, spec.gap_id));
        self.expansion_epoch += 1;
        self.restore_diff_cursor_anchor(anchor);
        true
    }

    /// The gap whose row (matched by `candidate`) is nearest the diff
    /// cursor, resolved to its spec. `true` marks the trailing gap.
    fn nearest_gap(&self, candidate: impl Fn(&DiffRow) -> bool) -> Option<(String, GapSpec, bool)> {
        let file = self.selected_visible_file()?;
        let rows = self.diff_rows_for_selected_file();
        let gap_id = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let gap_id = row.gap.filter(|_| candidate(row))?;
                Some((index.abs_diff(self.diff_cursor), index, gap_id))
            })
            .min_by_key(|(distance, index, _)| (*distance, *index))
            .map(|(_, _, gap_id)| gap_id)?;
        let lines = self.cached_file_lines(&file.path)?;
        let gaps = file_gaps(&file.diff.hunks, Some(lines.len()));
        let spec = gaps.iter().find(|gap| gap.gap_id == gap_id)?;
        Some((file.path.clone(), *spec, gap_id == file.diff.hunks.len()))
    }

    /// Record fetched full file contents (new side) for a path; `None`
    /// records a failed fetch so gap rows stop offering expansion. Bumps
    /// the expansion epoch so cached rows rebuild.
    pub fn store_file_contents(&mut self, path: &str, contents: Option<String>) {
        let lines = contents
            .map(|contents| Rc::new(contents.lines().map(str::to_owned).collect::<Vec<String>>()));
        self.file_contents.insert(path.to_owned(), lines);
        self.expansion_epoch += 1;
    }

    /// Whether a fetch (successful or failed) has been recorded for a path.
    pub fn has_file_contents_entry(&self, path: &str) -> bool {
        self.file_contents.contains_key(path)
    }

    /// Whether full file contents are loaded and usable for a path.
    pub fn file_contents_loaded(&self, path: &str) -> bool {
        matches!(self.file_contents.get(path), Some(Some(_)))
    }

    pub(super) fn cached_file_lines(&self, path: &str) -> Option<Rc<Vec<String>>> {
        self.file_contents.get(path)?.clone()
    }

    /// Whether expansion may still be offered for a path: contents are
    /// loaded or simply not fetched yet (only a recorded failure disables).
    pub(super) fn file_contents_fetchable(&self, path: &str) -> bool {
        !matches!(self.file_contents.get(path), Some(None))
    }

    /// Row indices shift when gaps expand/collapse; put the cursor back on
    /// the row carrying the same anchor and keep it scrolled into view.
    fn restore_diff_cursor_anchor(&mut self, anchor: Option<crate::anchor::CommentAnchor>) {
        let Some(anchor) = anchor else {
            return;
        };
        let rows = self.diff_rows_for_selected_file();
        if let Some(index) = rows
            .iter()
            .position(|row| row.anchor.as_ref() == Some(&anchor))
        {
            self.diff_cursor = index;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use crate::{app::ReviewSession, diff::DiffSet, jj::ReviewTarget, state::ReviewState};

    fn hunk(old_start: usize, old_len: usize, new_start: usize, new_len: usize) -> Hunk {
        Hunk {
            old_start,
            old_len,
            new_start,
            new_len,
            header: format!("@@ -{old_start},{old_len} +{new_start},{new_len} @@"),
            lines: Vec::new(),
        }
    }

    #[test]
    fn gaps_cover_top_middle_and_bottom() {
        let hunks = [hunk(5, 3, 5, 3), hunk(20, 3, 20, 3)];

        let gaps = file_gaps(&hunks, Some(30));

        assert_eq!(
            gaps,
            vec![
                GapSpec {
                    gap_id: 0,
                    new_start: 1,
                    hidden: 4,
                    offset: 0,
                },
                GapSpec {
                    gap_id: 1,
                    new_start: 8,
                    hidden: 12,
                    offset: 0,
                },
                GapSpec {
                    gap_id: 2,
                    new_start: 23,
                    hidden: 8,
                    offset: 0,
                },
            ]
        );
    }

    #[test]
    fn bottom_gap_requires_total_line_count() {
        let hunks = [hunk(5, 3, 5, 3)];

        let gaps = file_gaps(&hunks, None);

        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].gap_id, 0);
    }

    #[test]
    fn gap_offsets_track_old_new_drift() {
        // First hunk adds two lines: after it, new numbering runs 2 ahead.
        let hunks = [hunk(5, 3, 5, 5), hunk(20, 3, 22, 3)];

        let gaps = file_gaps(&hunks, Some(40));

        let middle = &gaps[1];
        assert_eq!(middle.new_start, 10);
        assert_eq!(middle.offset, 2);
        // new line 10 corresponds to old line 8.
        assert_eq!(middle.new_start as isize - middle.offset, 8);
        let bottom = &gaps[2];
        assert_eq!(bottom.offset, 2);
    }

    #[test]
    fn no_gaps_for_adjacent_hunks_or_file_start() {
        let hunks = [hunk(1, 3, 1, 3), hunk(4, 3, 4, 3)];

        assert!(file_gaps(&hunks, None).is_empty());
    }

    #[test]
    fn gap_view_clamps_and_closes() {
        let spec = GapSpec {
            gap_id: 1,
            new_start: 10,
            hidden: 6,
            offset: 0,
        };

        let collapsed = gap_view(&spec, None);
        assert_eq!(collapsed.top, 10..10);
        assert_eq!(collapsed.bottom, 16..16);
        assert_eq!(collapsed.hidden, 6);

        let partial = gap_view(&spec, Some(Expansion { above: 2, below: 2 }));
        assert_eq!(partial.top, 10..12);
        assert_eq!(partial.bottom, 14..16);
        assert_eq!(partial.hidden, 2);

        // Over-expansion clamps and the gap closes (renders contiguously).
        let closed = gap_view(
            &spec,
            Some(Expansion {
                above: 100,
                below: 100,
            }),
        );
        assert_eq!(closed.top, 10..16);
        assert_eq!(closed.bottom, 16..16);
        assert_eq!(closed.hidden, 0);
    }

    const TWO_HUNK_DIFF: &str = r#"diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -4,3 +4,3 @@
 line 4
-old five
+line 5
 line 6
@@ -12,3 +12,3 @@
 line 12
-old thirteen
+line 13
 line 14
"#;

    fn contents(total: usize) -> String {
        (1..=total)
            .map(|n| format!("line {n}\n"))
            .collect::<String>()
    }

    fn session_with_contents() -> ReviewSession {
        let diff = DiffSet::parse(TWO_HUNK_DIFF).unwrap();
        let mut session = ReviewSession::new(
            PathBuf::from("."),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.syntax = crate::syntax::SyntaxConfig {
            enabled: false,
            ..crate::syntax::SyntaxConfig::default()
        };
        session.store_file_contents("a.txt", Some(contents(20)));
        session
    }

    fn gap_rows(session: &ReviewSession) -> Vec<(usize, usize)> {
        session
            .diff_rows_for_selected_file()
            .iter()
            .filter_map(|row| match row.kind {
                DiffRowKind::ExpandGap { gap_id, hidden } => Some((gap_id, hidden)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn rows_insert_gap_rows_above_between_and_below_hunks() {
        let session = session_with_contents();

        // 3 above the first hunk, 5 between, 6 below (lines 15-20).
        assert_eq!(gap_rows(&session), vec![(0, 3), (1, 5), (2, 6)]);
    }

    #[test]
    fn bottom_gap_appears_only_once_contents_are_known() {
        let diff = DiffSet::parse(TWO_HUNK_DIFF).unwrap();
        let mut session = ReviewSession::new(
            PathBuf::from("."),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.syntax = crate::syntax::SyntaxConfig {
            enabled: false,
            ..crate::syntax::SyntaxConfig::default()
        };

        assert_eq!(gap_rows(&session), vec![(0, 3), (1, 5)]);
        let rows = session.diff_rows_for_selected_file();
        let gap_row = rows
            .iter()
            .find(|row| matches!(row.kind, DiffRowKind::ExpandGap { .. }))
            .unwrap();
        // Not fetched yet: expansion is still offered.
        assert!(gap_row.text.contains("expand"));

        // A failed fetch removes the affordance but keeps the gap visible.
        session.store_file_contents("a.txt", None);
        assert_eq!(gap_rows(&session), vec![(0, 3), (1, 5)]);
        let rows = session.diff_rows_for_selected_file();
        let gap_row = rows
            .iter()
            .find(|row| matches!(row.kind, DiffRowKind::ExpandGap { .. }))
            .unwrap();
        assert!(!gap_row.text.contains("expand"));
    }

    #[test]
    fn expanding_reveals_real_line_numbers_without_anchors() {
        let mut session = session_with_contents();
        // Cursor on the first hunk's first context line so the top gap is
        // nearest.
        let rows = session.diff_rows_for_selected_file();
        session.diff_cursor = rows.iter().position(|row| row.text == "line 4").unwrap();

        assert!(session.expand_nearest_gap(Some(2)));

        let rows = session.diff_rows_for_selected_file();
        let expanded: Vec<_> = rows
            .iter()
            .filter(|row| row.gap == Some(0) && matches!(row.kind, DiffRowKind::DiffLine(_)))
            .collect();
        // Top gap grows upward from the first hunk: lines 2 and 3 revealed.
        assert_eq!(expanded.len(), 2);
        assert_eq!(expanded[0].new_lineno, Some(2));
        assert_eq!(expanded[0].old_lineno, Some(2));
        assert_eq!(expanded[0].text, "line 2");
        assert_eq!(expanded[1].new_lineno, Some(3));
        assert!(expanded.iter().all(|row| row.anchor.is_none()));
        assert_eq!(gap_rows(&session), vec![(0, 1), (1, 5), (2, 6)]);
    }

    #[test]
    fn fully_expanded_middle_gap_drops_the_interior_hunk_header() {
        let mut session = session_with_contents();
        let rows = session.diff_rows_for_selected_file();
        // Cursor on the second hunk's first context line: the middle gap is
        // nearest.
        session.diff_cursor = rows.iter().position(|row| row.text == "line 12").unwrap();

        assert!(session.expand_nearest_gap(None));

        let rows = session.diff_rows_for_selected_file();
        let headers = rows
            .iter()
            .filter(|row| row.kind == DiffRowKind::HunkHeader)
            .count();
        assert_eq!(headers, 1, "interior header should drop when gap closes");
        assert_eq!(gap_rows(&session), vec![(0, 3), (2, 6)]);
        // Numbering runs contiguously across the seam: 7..=11 revealed.
        let expanded: Vec<_> = rows
            .iter()
            .filter(|row| row.gap == Some(1))
            .map(|row| row.new_lineno.unwrap())
            .collect();
        assert_eq!(expanded, vec![7, 8, 9, 10, 11]);
    }

    #[test]
    fn collapse_restores_the_original_gap() {
        let mut session = session_with_contents();
        let rows = session.diff_rows_for_selected_file();
        session.diff_cursor = rows.iter().position(|row| row.text == "line 12").unwrap();
        assert!(session.expand_nearest_gap(None));
        let anchor_before = session.selected_line_anchor();

        assert!(session.collapse_nearest_gap());

        assert_eq!(gap_rows(&session), vec![(0, 3), (1, 5), (2, 6)]);
        // The cursor followed its anchor through the row shifts.
        assert_eq!(session.selected_line_anchor(), anchor_before);
        // Nothing left to collapse.
        assert!(!session.collapse_nearest_gap());
    }

    #[test]
    fn trailing_gap_expands_downward_from_the_last_hunk() {
        let mut session = session_with_contents();
        let rows = session.diff_rows_for_selected_file();
        session.diff_cursor = rows.len() - 1;

        assert!(session.expand_nearest_gap(Some(3)));

        let rows = session.diff_rows_for_selected_file();
        let expanded: Vec<_> = rows
            .iter()
            .filter(|row| row.gap == Some(2) && matches!(row.kind, DiffRowKind::DiffLine(_)))
            .map(|row| row.new_lineno.unwrap())
            .collect();
        assert_eq!(expanded, vec![15, 16, 17]);
        assert_eq!(gap_rows(&session), vec![(0, 3), (1, 5), (2, 3)]);
    }

    #[test]
    fn expansion_requires_loaded_contents() {
        let diff = DiffSet::parse(TWO_HUNK_DIFF).unwrap();
        let mut session = ReviewSession::new(
            PathBuf::from("."),
            ReviewTarget::trunk_to_current(),
            diff,
            ReviewState::default(),
        );
        session.syntax = crate::syntax::SyntaxConfig {
            enabled: false,
            ..crate::syntax::SyntaxConfig::default()
        };

        assert!(!session.expand_nearest_gap(Some(10)));
    }
}
