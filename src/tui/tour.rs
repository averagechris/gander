//! Tour mode: step through agent-suggested review chunks in order with
//! their rationale displayed. Advancing marks the current stop's file
//! viewed; Esc returns to free navigation. Gander-native: it only reads
//! the overlay, no live agent required (docs/decisions.md D4).

use crate::app::ReviewSession;

use super::chunks::{ChunkRow, chunk_rows};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TourState {
    pub(super) stops: Vec<ChunkRow>,
    pub(super) index: usize,
}

impl TourState {
    /// Build a tour from the session's chunks, flattened part-by-part in
    /// the order the agent gave. `None` when no chunks are suggested.
    pub(super) fn new(session: &ReviewSession) -> Option<Self> {
        let stops: Vec<ChunkRow> = session.review_chunks.iter().flat_map(chunk_rows).collect();
        (!stops.is_empty()).then_some(Self { stops, index: 0 })
    }

    pub(super) fn current(&self) -> Option<&ChunkRow> {
        self.stops.get(self.index)
    }

    /// Move to the next stop. Returns `false` when already at the last stop
    /// (the tour is complete).
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
}

/// Jump the session to a tour stop's location.
pub(super) fn jump_to_stop(session: &mut ReviewSession, stop: &ChunkRow) {
    if let Some(part) = &stop.part {
        session.jump_to_chunk_part(part);
    }
}

/// Mark the file a stop belongs to as viewed (used when advancing past it).
pub(super) fn mark_stop_viewed(session: &mut ReviewSession, stop: &ChunkRow) {
    if let Some(part) = &stop.part {
        let path = part.path.clone();
        session.mark_files_viewed_where(|file| file.path == path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentOverlay, ChunkPart, ReviewChunk};
    use crate::tui::test_support::snapshot_session;

    fn session_with_chunks() -> ReviewSession {
        let mut session = snapshot_session(
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
"#,
        );
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![ReviewChunk {
                id: "c1".to_owned(),
                title: "core flow".to_owned(),
                rationale: Some("read these together".to_owned()),
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
    fn tour_requires_chunks() {
        let session = snapshot_session("");
        assert!(TourState::new(&session).is_none());
    }

    #[test]
    fn tour_steps_through_parts_and_stops_at_ends() {
        let session = session_with_chunks();
        let mut tour = TourState::new(&session).unwrap();

        assert_eq!(tour.len(), 2);
        assert_eq!(tour.current().unwrap().part.as_ref().unwrap().path, "a.rs");
        assert!(!tour.back());
        assert!(tour.advance());
        assert_eq!(tour.current().unwrap().part.as_ref().unwrap().path, "b.rs");
        assert!(!tour.advance());
        assert!(tour.back());
        assert_eq!(tour.index, 0);
    }

    #[test]
    fn advancing_marks_the_stop_file_viewed() {
        let mut session = session_with_chunks();
        let tour = TourState::new(&session).unwrap();

        mark_stop_viewed(&mut session, &tour.stops[0]);

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
    fn jump_to_stop_selects_the_part_file() {
        let mut session = session_with_chunks();
        let tour = TourState::new(&session).unwrap();

        jump_to_stop(&mut session, &tour.stops[1]);

        assert_eq!(session.selected_file().unwrap().path, "b.rs");
    }
}
