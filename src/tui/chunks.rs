//! Review chunks: agent-defined reviewable units that can span or subdivide
//! files. The popup lists every chunk part and jumps to its location.

use crate::{
    agent::{Artifact, ChunkImportance, ChunkPart, InvalidChunkPart, ReviewChunk},
    app::ReviewSession,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChunkListState {
    pub(super) rows: Vec<ChunkRow>,
    pub(super) selected: usize,
}

/// One selectable row: a part of a chunk (or a part-less chunk itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChunkRow {
    /// Stable review chunk id. Multi-part chunks share this id so zen can
    /// group their glance entry and cross-reference sibling stops.
    pub(super) chunk_id: String,
    pub(super) title: String,
    pub(super) importance: ChunkImportance,
    /// The jj change the chunk is anchored to, when the review spans a
    /// stack; jumping to this row retargets the review to that change.
    pub(super) change_id: Option<String>,
    pub(super) rationale: Option<String>,
    /// Teaching text for the zen focus card (spotlight chunks).
    pub(super) explanation: Option<String>,
    /// Agent-produced exhibits (examples, output, diagrams) for the zen
    /// artifact viewer.
    pub(super) artifacts: Vec<Artifact>,
    pub(super) part: Option<ChunkPart>,
    /// Position of this part within its chunk, e.g. (1, 3) for "part 1/3".
    pub(super) part_position: Option<(usize, usize)>,
    pub(super) invalid_reason: Option<String>,
}

impl ChunkListState {
    #[allow(dead_code)]
    pub(super) fn new(session: &ReviewSession) -> Self {
        Self {
            rows: session.review_chunks.iter().flat_map(chunk_rows).collect(),
            selected: 0,
        }
    }

    pub(super) fn new_with_invalid(session: &ReviewSession, invalid: &[InvalidChunkPart]) -> Self {
        let mut rows: Vec<_> = session.review_chunks.iter().flat_map(chunk_rows).collect();
        rows.extend(invalid.iter().map(|part| ChunkRow {
            chunk_id: part.chunk_id.clone(),
            title: part.chunk_title.clone(),
            importance: ChunkImportance::Glance,
            change_id: None,
            rationale: None,
            explanation: None,
            artifacts: Vec::new(),
            part: Some(ChunkPart {
                path: part.path.clone(),
                start_line: None,
                end_line: None,
            }),
            part_position: Some((part.part_index, part.part_index)),
            invalid_reason: Some(part.reason.clone()),
        }));
        Self { rows, selected: 0 }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.rows.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, max) as usize;
    }

    pub(super) fn selected_row(&self) -> Option<&ChunkRow> {
        self.rows.get(self.selected)
    }
}

pub(super) fn chunk_rows(chunk: &ReviewChunk) -> Vec<ChunkRow> {
    if chunk.parts.is_empty() {
        return vec![ChunkRow {
            chunk_id: chunk.id.clone(),
            title: chunk.title.clone(),
            importance: chunk.importance,
            change_id: chunk.change_id.clone(),
            rationale: chunk.rationale.clone(),
            explanation: chunk.explanation.clone(),
            artifacts: chunk.artifacts.clone(),
            part: None,
            part_position: None,
            invalid_reason: None,
        }];
    }
    let total = chunk.parts.len();
    chunk
        .parts
        .iter()
        .enumerate()
        .map(|(index, part)| ChunkRow {
            chunk_id: chunk.id.clone(),
            title: chunk.title.clone(),
            importance: chunk.importance,
            change_id: chunk.change_id.clone(),
            rationale: chunk.rationale.clone(),
            explanation: chunk.explanation.clone(),
            artifacts: chunk.artifacts.clone(),
            part: Some(part.clone()),
            part_position: (total > 1).then_some((index + 1, total)),
            invalid_reason: None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::AgentOverlay;
    use crate::tui::test_support::snapshot_session;

    fn part(path: &str, start: usize, end: usize) -> ChunkPart {
        ChunkPart {
            path: path.to_owned(),
            start_line: Some(start),
            end_line: Some(end),
        }
    }

    #[test]
    fn chunks_flatten_into_part_rows() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![
                ReviewChunk {
                    id: "c1".to_owned(),
                    title: "auth flow".to_owned(),
                    importance: ChunkImportance::Spotlight,
                    change_id: None,
                    explanation: None,
                    rationale: Some("spans two files".to_owned()),
                    artifacts: Vec::new(),
                    parts: vec![part("a.rs", 1, 10), part("b.rs", 5, 20)],
                },
                ReviewChunk {
                    id: "c2".to_owned(),
                    title: "docs only".to_owned(),
                    importance: ChunkImportance::Glance,
                    change_id: None,
                    explanation: None,
                    rationale: None,
                    artifacts: Vec::new(),
                    parts: Vec::new(),
                },
            ],
            ..Default::default()
        });

        let state = ChunkListState::new(&session);

        assert_eq!(state.rows.len(), 3);
        assert_eq!(state.rows[0].title, "auth flow");
        assert_eq!(state.rows[0].chunk_id, "c1");
        assert_eq!(state.rows[1].chunk_id, "c1");
        assert_eq!(state.rows[0].part_position, Some((1, 2)));
        assert_eq!(state.rows[1].part.as_ref().unwrap().path, "b.rs");
        assert_eq!(state.rows[2].title, "docs only");
        assert_eq!(state.rows[2].chunk_id, "c2");
        assert!(state.rows[2].part.is_none());
    }

    #[test]
    fn selection_clamps_and_resolves_rows() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![ReviewChunk {
                id: "c1".to_owned(),
                title: "single".to_owned(),
                importance: ChunkImportance::Spotlight,
                change_id: None,
                explanation: None,
                rationale: None,
                artifacts: Vec::new(),
                parts: vec![part("a.rs", 1, 2)],
            }],
            ..Default::default()
        });
        let mut state = ChunkListState::new(&session);

        state.move_selection(9);
        assert_eq!(state.selected, 0);
        assert_eq!(state.selected_row().unwrap().title, "single");
        assert_eq!(state.selected_row().unwrap().part_position, None);
    }
}
