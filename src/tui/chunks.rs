//! Walkthrough/tour rows adapted from active agent overlay curation and durable
//! walkthrough steps. Agent overlay chunks remain an internal live-curation input.

use crate::{
    agent::{Artifact, ArtifactKind, ChunkImportance, ChunkPart, ReviewChunk},
    app::ReviewSession,
    state::{ReviewTarget, StepArtifact, StepArtifactKind, StepImportance, WalkthroughStep},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WalkthroughRowList {
    pub(super) rows: Vec<WalkthroughRow>,
    pub(super) selected: usize,
}

/// One selectable row: a tour stop target (or an untargeted stop itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WalkthroughRow {
    /// Stable source curation id. Multi-target stops share this id so zen can
    /// group their glance entry and cross-reference sibling stops.
    pub(super) source_id: String,
    pub(super) title: String,
    pub(super) importance: ChunkImportance,
    /// The jj change the walkthrough stop is anchored to, when the review spans a
    /// stack; jumping to this row retargets the review to that change.
    pub(super) change_id: Option<String>,
    pub(super) rationale: Option<String>,
    /// Teaching text for the zen focus card (spotlight chunks).
    pub(super) explanation: Option<String>,
    /// Agent-produced exhibits (examples, output, diagrams) for the zen
    /// artifact viewer.
    pub(super) artifacts: Vec<Artifact>,
    pub(super) part: Option<ChunkPart>,
    /// Position of this part within its stop, e.g. (1, 3) for "part 1/3".
    pub(super) part_position: Option<(usize, usize)>,
    pub(super) invalid_reason: Option<String>,
}

impl WalkthroughRowList {
    #[allow(dead_code)]
    pub(super) fn new(session: &ReviewSession) -> Self {
        Self {
            rows: session
                .review_chunks
                .iter()
                .flat_map(overlay_chunk_rows)
                .collect(),
            selected: 0,
        }
    }
}

pub(super) fn overlay_chunk_rows(chunk: &ReviewChunk) -> Vec<WalkthroughRow> {
    if chunk.parts.is_empty() {
        return vec![WalkthroughRow {
            source_id: chunk.id.clone(),
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
        .map(|(index, part)| WalkthroughRow {
            source_id: chunk.id.clone(),
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

pub(super) fn durable_walkthrough_rows(step: &WalkthroughStep) -> Vec<WalkthroughRow> {
    let parts: Vec<_> = std::iter::once(&step.target)
        .chain(step.extra_targets.iter())
        .filter_map(target_to_part)
        .collect();
    let importance = match step.importance {
        StepImportance::Spotlight => ChunkImportance::Spotlight,
        StepImportance::Glance => ChunkImportance::Glance,
    };
    let title = step
        .title
        .clone()
        .unwrap_or_else(|| "Walkthrough step".to_owned());
    let artifacts = step.artifacts.iter().map(step_artifact_to_agent).collect();
    if parts.is_empty() {
        return vec![WalkthroughRow {
            source_id: step.id.clone(),
            title,
            importance,
            change_id: step.change_id.clone(),
            rationale: step.why.clone(),
            explanation: step.body.clone(),
            artifacts,
            part: None,
            part_position: None,
            invalid_reason: None,
        }];
    }
    let total = parts.len();
    parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| WalkthroughRow {
            source_id: step.id.clone(),
            title: title.clone(),
            importance,
            change_id: step.change_id.clone(),
            rationale: step.why.clone(),
            explanation: step.body.clone(),
            artifacts: artifacts.clone(),
            part: Some(part),
            part_position: (total > 1).then_some((index + 1, total)),
            invalid_reason: None,
        })
        .collect()
}

fn target_to_part(target: &ReviewTarget) -> Option<ChunkPart> {
    target.file.as_ref().map(|path| ChunkPart {
        path: path.clone(),
        start_line: target.line,
        end_line: target.end_line,
    })
}

fn step_artifact_to_agent(artifact: &StepArtifact) -> Artifact {
    Artifact {
        title: artifact.title.clone(),
        kind: match artifact.kind {
            StepArtifactKind::Example => ArtifactKind::Example,
            StepArtifactKind::Output => ArtifactKind::Output,
            StepArtifactKind::Diagram => ArtifactKind::Diagram,
            StepArtifactKind::Note => ArtifactKind::Note,
        },
        body: artifact.body.clone(),
    }
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

        let state = WalkthroughRowList::new(&session);

        assert_eq!(state.rows.len(), 3);
        assert_eq!(state.rows[0].title, "auth flow");
        assert_eq!(state.rows[0].source_id, "c1");
        assert_eq!(state.rows[1].source_id, "c1");
        assert_eq!(state.rows[0].part_position, Some((1, 2)));
        assert_eq!(state.rows[1].part.as_ref().unwrap().path, "b.rs");
        assert_eq!(state.rows[2].title, "docs only");
        assert_eq!(state.rows[2].source_id, "c2");
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
        let state = WalkthroughRowList::new(&session);
        assert_eq!(state.selected, 0);
        assert_eq!(state.rows[0].title, "single");
        assert_eq!(state.rows[0].part_position, None);
    }
}
