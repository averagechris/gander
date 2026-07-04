//! Zen mode: a focused, agent-curated briefing over the review
//! (docs/focused-diff-ux.md §6). It has three surfaces:
//!
//! - **Focus card** (default): a full-screen stop per spotlight chunk that
//!   shows only the critical lines plus the agent's explanation — pop in,
//!   understand, move on.
//! - **Reading view** (`tab`): the normal review UI with out-of-range rows
//!   dimmed, for surrounding context and precise commenting. The full
//!   normal-mode vocabulary works here.
//! - **Glance board** (`g`, or automatically after the last stop): every
//!   glance chunk and uncovered file on one skimmable screen, so routine
//!   hunks are acknowledged in bulk instead of toured one-by-one.
//!
//! Stops come from agent-suggested spotlight chunks when present, or fall
//! back to one stop per file in display order, so zen works without an
//! agent and gets dramatically better with one. Gander-native: it only
//! reads the overlay, no live agent required (docs/decisions.md D4).

use crate::agent::{ChunkImportance, ChunkPart};
use crate::app::{Focus, ReviewSession, ZenFocus};
use crate::jj::ReviewTarget;

use super::chunks::{ChunkRow, chunk_rows};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ZenState {
    pub(super) stops: Vec<ChunkRow>,
    /// Rows intentionally left out of the stop-by-stop tour: glance chunks
    /// plus files no chunk covers. Skimmed in bulk on the glance board.
    pub(super) glance_rows: Vec<ChunkRow>,
    pub(super) index: usize,
    /// Which zen surface is showing.
    pub(super) phase: ZenPhase,
    /// Selection on the glance board.
    pub(super) glance_selected: usize,
    /// File-pane visibility to restore when zen ends.
    pub(super) restore_file_pane: bool,
    /// Rendered target the session currently shows; zen updates this when it
    /// retargets itself (change-anchored stops), so a mismatch means the
    /// review was retargeted underneath the walkthrough and zen must end.
    pub(super) target_key: String,
    /// The target the walkthrough started from. Stops without a change
    /// anchor jump within it, and ending zen returns to it.
    pub(super) home_target: ReviewTarget,
    /// Whether the stops came from agent chunks or the file-order fallback.
    pub(super) source: ZenSource,
}

/// The zen surfaces. Focus is the default landing surface for each stop;
/// Reading drops into the (dimmed) normal UI; Glance is the bulk-skim board.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ZenPhase {
    Focus,
    Reading,
    Glance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ZenSource {
    Chunks,
    Files,
}

impl ZenState {
    /// Build a walkthrough: spotlight chunks become full-screen stops and
    /// everything else (glance chunks + uncovered files) lands on the
    /// glance board. Without chunks, every file becomes a stop.
    /// `None` when there is nothing at all to walk through.
    pub(super) fn new(session: &ReviewSession) -> Option<Self> {
        let (stops, glance_rows, source) = if session.review_chunks.is_empty() {
            (file_stops(session), Vec::new(), ZenSource::Files)
        } else {
            let rows: Vec<ChunkRow> = session.review_chunks.iter().flat_map(chunk_rows).collect();
            let (spotlight, glance): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|row| row.importance == ChunkImportance::Spotlight);
            let mut glance_rows = glance;
            let stops = if spotlight.is_empty() {
                // If an agent marks everything as glance, keep zen useful by
                // allowing a lightweight skim rather than refusing to start.
                std::mem::take(&mut glance_rows)
            } else {
                spotlight
            };
            glance_rows.extend(uncovered_file_rows(session));
            (stops, glance_rows, ZenSource::Chunks)
        };
        (!stops.is_empty()).then_some(Self {
            stops,
            glance_rows,
            index: 0,
            phase: ZenPhase::Focus,
            glance_selected: 0,
            restore_file_pane: session.file_pane_visible,
            target_key: session.target.to_string(),
            home_target: session.target.clone(),
            source,
        })
    }

    /// Rebuild the stops and glance rows from the (freshly reloaded)
    /// session, keeping position, phase, and the home target. Returns
    /// `false` when nothing is left to walk through and zen should end.
    pub(super) fn refresh(&mut self, session: &ReviewSession) -> bool {
        let Some(rebuilt) = Self::new(session) else {
            return false;
        };
        self.stops = rebuilt.stops;
        self.glance_rows = rebuilt.glance_rows;
        self.source = rebuilt.source;
        self.index = self.index.min(self.stops.len() - 1);
        self.glance_selected = self
            .glance_selected
            .min(self.glance_rows.len().saturating_sub(1));
        self.target_key = session.target.to_string();
        true
    }

    pub(super) fn current(&self) -> Option<&ChunkRow> {
        self.stops.get(self.index)
    }

    /// Move to the next stop. Returns `false` when already at the last stop
    /// (the walkthrough is complete).
    pub(super) fn advance(&mut self) -> bool {
        if self.index + 1 < self.stops.len() {
            self.index += 1;
            true
        } else {
            false
        }
    }

    pub(super) fn back(&mut self) -> bool {
        if self.index > 0 {
            self.index -= 1;
            true
        } else {
            false
        }
    }

    pub(super) fn len(&self) -> usize {
        self.stops.len()
    }

    pub(super) fn has_glance(&self) -> bool {
        !self.glance_rows.is_empty()
    }

    pub(super) fn move_glance_selection(&mut self, delta: isize) {
        if self.glance_rows.is_empty() {
            self.glance_selected = 0;
            return;
        }
        let max = self.glance_rows.len() as isize - 1;
        self.glance_selected = (self.glance_selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_glance(&self) -> Option<&ChunkRow> {
        self.glance_rows.get(self.glance_selected)
    }

    /// True once the review target no longer matches the one the stops
    /// were built against.
    pub(super) fn is_stale(&self, session: &ReviewSession) -> bool {
        self.target_key != session.target.to_string()
    }
}

/// Chunkless fallback: one stop per visible file, in the same order the
/// tree shows (agent ordering respected when active).
fn file_stops(session: &ReviewSession) -> Vec<ChunkRow> {
    session
        .ordered_visible_file_paths()
        .into_iter()
        .map(whole_file_row)
        .collect()
}

/// Files no chunk part mentions: they join the glance board so the briefing
/// covers the entire change even when the agent's chunks do not.
fn uncovered_file_rows(session: &ReviewSession) -> Vec<ChunkRow> {
    let covered: std::collections::BTreeSet<&str> = session
        .review_chunks
        .iter()
        .flat_map(|chunk| chunk.parts.iter().map(|part| part.path.as_str()))
        .collect();
    session
        .ordered_visible_file_paths()
        .into_iter()
        .filter(|path| !covered.contains(path.as_str()))
        .map(whole_file_row)
        .collect()
}

fn whole_file_row(path: String) -> ChunkRow {
    ChunkRow {
        title: path.clone(),
        importance: ChunkImportance::Glance,
        change_id: None,
        rationale: None,
        explanation: None,
        part: Some(ChunkPart {
            path,
            start_line: None,
            end_line: None,
        }),
        part_position: None,
    }
}

/// The review target a zen row wants loaded: a change-anchored row reviews
/// that change against its parent (stacked-PR style); anything else reads
/// within the walkthrough's home target.
pub(super) fn row_target(row: &ChunkRow, home: &ReviewTarget) -> ReviewTarget {
    match &row.change_id {
        Some(change_id) => ReviewTarget::new(format!("{change_id}-"), change_id.clone()),
        None => home.clone(),
    }
}

/// Jump the session to a stop's location and frame it: the diff pane takes
/// focus and `zen_focus` records the file/range so out-of-range rows dim.
pub(super) fn jump_to_stop(session: &mut ReviewSession, stop: &ChunkRow) {
    let Some(part) = &stop.part else {
        session.zen_focus = None;
        return;
    };
    session.jump_to_chunk_part(part);
    if part.start_line.is_some() {
        session.diff_scroll = session.diff_cursor.saturating_sub(12) as u16;
    }
    // Chunk parts without line info leave focus on the files pane, which
    // would re-show the hidden pane (never-trap); zen reads in the diff.
    session.focus = Focus::Diff;
    session.zen_focus = Some(ZenFocus {
        path: part.path.clone(),
        lines: part
            .start_line
            .map(|start| (start, part.end_line.unwrap_or(start))),
    });
}

/// Mark the file a stop belongs to as viewed (used when advancing past it).
pub(super) fn mark_stop_viewed(session: &mut ReviewSession, stop: &ChunkRow) {
    if let Some(part) = &stop.part {
        let path = part.path.clone();
        session.mark_files_viewed_where(|file| file.path == path);
    }
}

/// Mark every file the glance board covers as viewed (the "I skimmed the
/// boilerplate" bulk acknowledgement).
pub(super) fn mark_glance_viewed(session: &mut ReviewSession, zen: &ZenState) {
    let paths: std::collections::BTreeSet<String> = zen
        .glance_rows
        .iter()
        .filter_map(|row| row.part.as_ref().map(|part| part.path.clone()))
        .collect();
    session.mark_files_viewed_where(|file| paths.contains(&file.path));
}

/// True when every file a glance row touches is already viewed.
pub(super) fn glance_row_viewed(session: &ReviewSession, row: &ChunkRow) -> bool {
    row.part.as_ref().is_some_and(|part| {
        session
            .files
            .iter()
            .find(|file| file.path == part.path)
            .is_some_and(|file| file.viewed)
    })
}

/// Clear the zen layer's session-side view state, restoring the file pane.
pub(super) fn end(session: &mut ReviewSession, zen: &ZenState) {
    session.zen_focus = None;
    session.file_pane_visible = zen.restore_file_pane;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentOverlay, ChunkPart, ReviewChunk};
    use crate::tui::test_support::snapshot_session;

    fn two_file_diff() -> &'static str {
        r#"diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+new
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-old
+new
"#
    }

    fn session_with_chunks() -> ReviewSession {
        let mut session = snapshot_session(two_file_diff());
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![ReviewChunk {
                id: "c1".to_owned(),
                title: "core flow".to_owned(),
                importance: ChunkImportance::Spotlight,
                change_id: None,
                rationale: Some("read these together".to_owned()),
                explanation: Some("The rename changes the contract.".to_owned()),
                parts: vec![
                    ChunkPart {
                        path: "a.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    },
                    ChunkPart {
                        path: "b.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    },
                ],
            }],
            ..Default::default()
        });
        session
    }

    #[test]
    fn zen_requires_something_to_review() {
        let session = snapshot_session("");
        assert!(ZenState::new(&session).is_none());
    }

    #[test]
    fn zen_without_chunks_falls_back_to_file_stops() {
        let session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session).unwrap();

        assert_eq!(zen.source, ZenSource::Files);
        assert_eq!(zen.phase, ZenPhase::Focus);
        assert_eq!(zen.len(), 2);
        assert_eq!(zen.stops[0].title, "a.rs");
        assert_eq!(zen.stops[0].part.as_ref().unwrap().start_line, None);
        assert_eq!(zen.stops[1].title, "b.rs");
        assert!(!zen.has_glance());
    }

    #[test]
    fn zen_prefers_agent_chunks_when_present() {
        let session = session_with_chunks();
        let zen = ZenState::new(&session).unwrap();

        assert_eq!(zen.source, ZenSource::Chunks);
        assert_eq!(zen.len(), 2);
        assert_eq!(zen.current().unwrap().part.as_ref().unwrap().path, "a.rs");
    }

    #[test]
    fn zen_tours_spotlights_and_sends_glance_chunks_to_the_board() {
        let mut session = snapshot_session(two_file_diff());
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![
                ReviewChunk {
                    id: "spotlight".to_owned(),
                    title: "risky behavior".to_owned(),
                    importance: ChunkImportance::Spotlight,
                    change_id: None,
                    rationale: None,
                    explanation: Some("This changes the retry loop.".to_owned()),
                    parts: vec![ChunkPart {
                        path: "a.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                },
                ReviewChunk {
                    id: "glance".to_owned(),
                    title: "mechanical follow-up".to_owned(),
                    importance: ChunkImportance::Glance,
                    change_id: None,
                    rationale: None,
                    explanation: None,
                    parts: vec![ChunkPart {
                        path: "b.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                },
            ],
            ..Default::default()
        });

        let zen = ZenState::new(&session).unwrap();

        assert_eq!(zen.len(), 1);
        assert_eq!(zen.current().unwrap().title, "risky behavior");
        assert_eq!(zen.glance_rows.len(), 1);
        assert_eq!(zen.glance_rows[0].title, "mechanical follow-up");
    }

    #[test]
    fn files_uncovered_by_chunks_join_the_glance_board() {
        let mut session = snapshot_session(two_file_diff());
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![ReviewChunk {
                id: "spotlight".to_owned(),
                title: "the important bit".to_owned(),
                importance: ChunkImportance::Spotlight,
                change_id: None,
                rationale: None,
                explanation: None,
                parts: vec![ChunkPart {
                    path: "a.rs".to_owned(),
                    start_line: Some(1),
                    end_line: Some(1),
                }],
            }],
            ..Default::default()
        });

        let zen = ZenState::new(&session).unwrap();

        // b.rs is untouched by any chunk: it must still be reachable via
        // the glance board so the briefing covers the whole change.
        assert_eq!(zen.len(), 1);
        assert_eq!(zen.glance_rows.len(), 1);
        assert_eq!(zen.glance_rows[0].title, "b.rs");
        assert_eq!(zen.glance_rows[0].part.as_ref().unwrap().path, "b.rs");
    }

    #[test]
    fn glance_board_selection_moves_and_bulk_marks_viewed() {
        let mut session = snapshot_session(two_file_diff());
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![ReviewChunk {
                id: "spotlight".to_owned(),
                title: "the important bit".to_owned(),
                importance: ChunkImportance::Spotlight,
                change_id: None,
                rationale: None,
                explanation: None,
                parts: vec![ChunkPart {
                    path: "a.rs".to_owned(),
                    start_line: Some(1),
                    end_line: Some(1),
                }],
            }],
            ..Default::default()
        });
        let mut zen = ZenState::new(&session).unwrap();

        zen.move_glance_selection(5);
        assert_eq!(zen.glance_selected, 0); // clamped: one row
        assert!(!glance_row_viewed(&session, &zen.glance_rows[0]));

        mark_glance_viewed(&mut session, &zen);
        assert!(glance_row_viewed(&session, &zen.glance_rows[0]));
        // Only glance files were marked; the spotlight file is untouched.
        assert!(
            !session
                .files
                .iter()
                .find(|file| file.path == "a.rs")
                .unwrap()
                .viewed
        );
    }

    #[test]
    fn zen_steps_through_stops_and_stops_at_ends() {
        let session = session_with_chunks();
        let mut zen = ZenState::new(&session).unwrap();

        assert!(!zen.back());
        assert!(zen.advance());
        assert_eq!(zen.current().unwrap().part.as_ref().unwrap().path, "b.rs");
        assert!(!zen.advance());
        assert!(zen.back());
        assert_eq!(zen.index, 0);
    }

    #[test]
    fn advancing_marks_the_stop_file_viewed() {
        let mut session = session_with_chunks();
        let zen = ZenState::new(&session).unwrap();

        mark_stop_viewed(&mut session, &zen.stops[0]);

        assert!(
            session
                .files
                .iter()
                .find(|file| file.path == "a.rs")
                .unwrap()
                .viewed
        );
        assert!(
            !session
                .files
                .iter()
                .find(|file| file.path == "b.rs")
                .unwrap()
                .viewed
        );
    }

    #[test]
    fn jump_to_stop_selects_the_file_and_sets_the_focus_frame() {
        let mut session = session_with_chunks();
        let zen = ZenState::new(&session).unwrap();

        jump_to_stop(&mut session, &zen.stops[1]);

        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert_eq!(session.focus, Focus::Diff);
        let focus = session.zen_focus.as_ref().unwrap();
        assert_eq!(focus.path, "b.rs");
        assert_eq!(focus.lines, Some((1, 1)));
    }

    #[test]
    fn file_fallback_stops_frame_the_whole_file() {
        let mut session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session).unwrap();

        jump_to_stop(&mut session, &zen.stops[0]);

        assert_eq!(session.focus, Focus::Diff);
        let focus = session.zen_focus.as_ref().unwrap();
        assert_eq!(focus.path, "a.rs");
        assert_eq!(focus.lines, None);
    }

    #[test]
    fn ending_zen_restores_the_file_pane_and_clears_the_frame() {
        let mut session = session_with_chunks();
        let zen = ZenState::new(&session).unwrap();
        session.file_pane_visible = false;
        jump_to_stop(&mut session, &zen.stops[0]);

        end(&mut session, &zen);

        assert!(session.zen_focus.is_none());
        assert!(session.file_pane_visible);
    }

    #[test]
    fn zen_goes_stale_when_the_target_changes() {
        let session = session_with_chunks();
        let mut zen = ZenState::new(&session).unwrap();
        assert!(!zen.is_stale(&session));

        zen.target_key = "elsewhere".to_owned();
        assert!(zen.is_stale(&session));
    }

    #[test]
    fn zen_remembers_its_home_target() {
        let session = session_with_chunks();
        let zen = ZenState::new(&session).unwrap();

        assert_eq!(zen.home_target, session.target);
        assert_eq!(zen.target_key, session.target.to_string());
    }

    #[test]
    fn row_target_prefers_the_change_anchor() {
        let home = crate::jj::ReviewTarget::trunk_to_current();
        let mut row = whole_file_row("a.rs".to_owned());
        assert_eq!(row_target(&row, &home), home);

        row.change_id = Some("xyz".to_owned());
        assert_eq!(
            row_target(&row, &home),
            crate::jj::ReviewTarget::new("xyz-", "xyz")
        );
    }

    #[test]
    fn refresh_rebuilds_stops_and_clamps_the_index() {
        let mut session = session_with_chunks();
        let mut zen = ZenState::new(&session).unwrap();
        zen.index = 1;
        zen.phase = ZenPhase::Reading;

        // The agent trimmed its chunks down to a single one-part spotlight:
        // the stop list shrinks and the index snaps back into range.
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![ReviewChunk {
                id: "c2".to_owned(),
                title: "tightened".to_owned(),
                importance: ChunkImportance::Spotlight,
                change_id: None,
                rationale: None,
                explanation: None,
                parts: vec![ChunkPart {
                    path: "a.rs".to_owned(),
                    start_line: Some(1),
                    end_line: Some(1),
                }],
            }],
            ..Default::default()
        });

        assert!(zen.refresh(&session));
        assert_eq!(zen.len(), 1);
        assert_eq!(zen.index, 0);
        assert_eq!(zen.phase, ZenPhase::Reading);
        assert_eq!(zen.stops[0].title, "tightened");
        // b.rs is no longer covered: it joins the glance board.
        assert!(
            zen.glance_rows
                .iter()
                .any(|row| row.part.as_ref().is_some_and(|part| part.path == "b.rs"))
        );
    }

    #[test]
    fn refresh_reports_when_nothing_is_left_to_review() {
        let session = session_with_chunks();
        let mut zen = ZenState::new(&session).unwrap();

        let empty = snapshot_session("");
        assert!(!zen.refresh(&empty));
    }
}
