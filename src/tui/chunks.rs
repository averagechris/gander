//! Review chunks: agent-defined reviewable units that can span or subdivide
//! files. The popup lists every chunk part and jumps to its location.

use crate::{
    agent::{ChunkPart, ReviewChunk},
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
    pub(super) title: String,
    pub(super) rationale: Option<String>,
    pub(super) part: Option<ChunkPart>,
    /// Position of this part within its chunk, e.g. (1, 3) for "part 1/3".
    pub(super) part_position: Option<(usize, usize)>,
}

impl ChunkListState {
    pub(super) fn new(session: &ReviewSession) -> Self {
        Self {
            rows: session.review_chunks.iter().flat_map(chunk_rows).collect(),
            selected: 0,
        }
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
            title: chunk.title.clone(),
            rationale: chunk.rationale.clone(),
            part: None,
            part_position: None,
        }];
    }
    let total = chunk.parts.len();
    chunk
        .parts
        .iter()
        .enumerate()
        .map(|(index, part)| ChunkRow {
            title: chunk.title.clone(),
            rationale: chunk.rationale.clone(),
            part: Some(part.clone()),
            part_position: (total > 1).then_some((index + 1, total)),
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
                    rationale: Some("spans two files".to_owned()),
                    parts: vec![part("a.rs", 1, 10), part("b.rs", 5, 20)],
                },
                ReviewChunk {
                    id: "c2".to_owned(),
                    title: "docs only".to_owned(),
                    rationale: None,
                    parts: Vec::new(),
                },
            ],
            ..Default::default()
        });

        let state = ChunkListState::new(&session);

        assert_eq!(state.rows.len(), 3);
        assert_eq!(state.rows[0].title, "auth flow");
        assert_eq!(state.rows[0].part_position, Some((1, 2)));
        assert_eq!(state.rows[1].part.as_ref().unwrap().path, "b.rs");
        assert_eq!(state.rows[2].title, "docs only");
        assert!(state.rows[2].part.is_none());
    }

    #[test]
    fn selection_clamps_and_resolves_rows() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![ReviewChunk {
                id: "c1".to_owned(),
                title: "single".to_owned(),
                rationale: None,
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
