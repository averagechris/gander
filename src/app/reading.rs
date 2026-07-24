//! Toolkit-independent reading projection over the canonical review stream.
//!
//! This adapter groups shared stream rows into stable browser/export-sized
//! regions. It deliberately does not resolve attention, form folds, place
//! chapters, or choose annotation owners; all of those decisions come from
//! `stream.rs`.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::state::{Channel, Comment, Salience, WalkthroughStep};

use super::{DiffRow, ReviewSession, StreamAnnotationSource, StreamRow, StreamRowKind};

#[derive(Debug, Clone)]
pub struct ReadingProjection {
    pub summary: String,
    pub coverage: super::Coverage,
    pub spotlight_count: usize,
    pub skim_count: usize,
    pub skim_files: usize,
    pub chapters: Vec<super::ChapterHeader>,
    pub has_walkthrough: bool,
    pub regions: Vec<ReadingRegion>,
    pub file_region_ids: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct ReadingRegion {
    pub id: String,
    pub kind: ReadingRegionKind,
    pub member_paths: Vec<String>,
    pub rows: Vec<ReadingRow>,
}

#[derive(Debug, Clone)]
pub enum ReadingRegionKind {
    Chapter(super::ChapterHeader),
    File { file_index: usize, path: String },
    Skim(super::SkimFold),
}

#[derive(Debug, Clone)]
pub struct ReadingRow {
    pub id: String,
    pub path: Option<String>,
    pub salience: Option<Salience>,
    pub diff: Option<DiffRow>,
    pub annotations: Vec<ReadingAnnotation>,
}

#[derive(Debug, Clone)]
pub struct ReadingAnnotation {
    pub owner_row_id: String,
    pub source: ReadingAnnotationSource,
}

impl ReadingAnnotation {
    pub fn channel(&self) -> Channel {
        match &self.source {
            ReadingAnnotationSource::Comment(comment) => comment.channel,
            ReadingAnnotationSource::Walkthrough { .. } => Channel::Onboarding,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ReadingAnnotationSource {
    Comment(Comment),
    Walkthrough {
        step: WalkthroughStep,
        target: crate::state::ReviewTarget,
        part: usize,
        rationale: Option<String>,
    },
}

impl ReviewSession {
    pub fn reading_projection(&self) -> ReadingProjection {
        let stream = self.review_stream();
        let mut by_owner = BTreeMap::<usize, Vec<ReadingAnnotation>>::new();
        for annotation in &stream.annotations {
            let source = match &annotation.source {
                StreamAnnotationSource::Comment(comment) => {
                    ReadingAnnotationSource::Comment(comment.clone())
                }
                StreamAnnotationSource::Walkthrough {
                    step,
                    target,
                    part,
                    rationale,
                } => ReadingAnnotationSource::Walkthrough {
                    step: step.clone(),
                    target: target.clone(),
                    part: *part,
                    rationale: rationale.clone(),
                },
            };
            by_owner
                .entry(annotation.owner)
                .or_default()
                .push(ReadingAnnotation {
                    owner_row_id: stream.rows[annotation.owner].id.clone(),
                    source,
                });
        }

        let mut regions = Vec::<ReadingRegion>::new();
        let mut file_region_ids = BTreeMap::new();
        for (index, row) in stream.rows.iter().enumerate() {
            let row_annotations = by_owner.remove(&index).unwrap_or_default();
            match &row.kind {
                StreamRowKind::ChapterHeader(chapter) => regions.push(ReadingRegion {
                    id: stable_region_id("chapter", &row.id),
                    kind: ReadingRegionKind::Chapter(chapter.clone()),
                    member_paths: row.member_paths.clone(),
                    rows: vec![reading_row(row, None, row_annotations)],
                }),
                StreamRowKind::SkimFold(fold) => {
                    let mut rows = vec![reading_row(row, None, row_annotations)];
                    rows.extend(
                        fold.hidden_rows
                            .iter()
                            .map(|hidden| reading_row(hidden, stream_diff(hidden), Vec::new())),
                    );
                    let id = stable_region_id("fold", &fold.id);
                    for path in &fold.files {
                        file_region_ids
                            .entry(path.clone())
                            .or_insert_with(|| id.clone());
                    }
                    regions.push(ReadingRegion {
                        id,
                        kind: ReadingRegionKind::Skim(fold.clone()),
                        member_paths: fold.files.clone(),
                        rows,
                    });
                }
                StreamRowKind::FileHeader(diff) | StreamRowKind::Diff(diff) => {
                    let file_index = row.file_index.expect("diff stream row has a file");
                    let path = row.path.clone().expect("diff stream row has a path");
                    let append = regions.last_mut().filter(|region| {
                        matches!(&region.kind, ReadingRegionKind::File { file_index: existing, .. } if *existing == file_index)
                    });
                    if let Some(region) = append {
                        region
                            .rows
                            .push(reading_row(row, Some(diff.clone()), row_annotations));
                    } else {
                        // A chapter may split one file into multiple stream
                        // regions. Include the first canonical row identity so
                        // every segment has a unique, refresh-stable id.
                        let id = stable_region_id("file", &format!("{path}\0{}", row.id));
                        file_region_ids
                            .entry(path.clone())
                            .or_insert_with(|| id.clone());
                        regions.push(ReadingRegion {
                            id,
                            kind: ReadingRegionKind::File { file_index, path },
                            member_paths: row.member_paths.clone(),
                            rows: vec![reading_row(row, Some(diff.clone()), row_annotations)],
                        });
                    }
                }
            }
        }
        let chapters = stream
            .rows
            .iter()
            .filter_map(|row| match &row.kind {
                StreamRowKind::ChapterHeader(chapter) => Some(chapter.clone()),
                _ => None,
            })
            .collect();
        let skim = stream.rows.iter().filter_map(|row| match &row.kind {
            StreamRowKind::SkimFold(fold) => Some(fold),
            _ => None,
        });
        let skim_count = skim.clone().count();
        let skim_files = skim.map(|fold| fold.files.len()).sum();
        ReadingProjection {
            summary: self.summary_line(),
            coverage: stream.coverage,
            spotlight_count: stream.spotlights.len(),
            skim_count,
            skim_files,
            chapters,
            has_walkthrough: self.active_durable_session().is_some_and(|durable| {
                durable
                    .walkthroughs
                    .iter()
                    .any(|walkthrough| !walkthrough.steps.is_empty())
            }),
            regions,
            file_region_ids,
        }
    }
}

fn stream_diff(row: &StreamRow) -> Option<DiffRow> {
    match &row.kind {
        StreamRowKind::FileHeader(diff) | StreamRowKind::Diff(diff) => Some(diff.clone()),
        StreamRowKind::ChapterHeader(_) | StreamRowKind::SkimFold(_) => None,
    }
}

fn reading_row(
    row: &StreamRow,
    diff: Option<DiffRow>,
    annotations: Vec<ReadingAnnotation>,
) -> ReadingRow {
    ReadingRow {
        id: row.id.clone(),
        path: row.path.clone(),
        salience: row.salience,
        diff,
        annotations,
    }
}

fn stable_region_id(kind: &str, identity: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"gander-reading-region-v1\0");
    hash.update(kind.as_bytes());
    hash.update(b"\0");
    hash.update(identity.as_bytes());
    format!("{kind}-{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        anchor::CommentAnchor,
        diff::{DiffLine, DiffLineKind, DiffSet, FileDiff, FileStatus, Hunk},
        jj::ReviewTarget as JjReviewTarget,
        state::{
            AttentionRegion, Comment, ReviewState, ReviewTarget, SalienceSource, Walkthrough,
            WalkthroughStep,
        },
    };

    fn file(path: &str, fingerprint: &str, text: &str) -> FileDiff {
        FileDiff {
            path: path.into(),
            old_path: None,
            status: FileStatus::Modified,
            additions: 1,
            deletions: 0,
            raw: String::new(),
            fingerprint: fingerprint.into(),
            hunks: vec![Hunk {
                old_start: 1,
                old_len: 0,
                new_start: 1,
                new_len: 1,
                header: "@@ -0,0 +1 @@".into(),
                lines: vec![DiffLine {
                    kind: DiffLineKind::Added,
                    old_lineno: None,
                    new_lineno: Some(1),
                    text: text.into(),
                }],
            }],
        }
    }

    fn fixture() -> ReviewSession {
        let spotlight = file("src/core.rs", "core-fp", "let spotlight = true;");
        let skim = file("generated.lock", "lock-fp", "generated = true");
        let mut state = ReviewState::default();
        state.sessions.push(crate::state::ReviewSession {
            id: "session".into(),
            target: ReviewTarget {
                repo: Some("/repo".into()),
                base: Some("main".into()),
                revision: Some("@".into()),
                ..Default::default()
            },
            attention_regions: vec![
                AttentionRegion {
                    target: ReviewTarget {
                        file: Some("src/core.rs".into()),
                        anchor: Some(CommentAnchor::File {
                            path: "src/core.rs".into(),
                            old_path: None,
                            diff_fingerprint: "core-fp".into(),
                        }),
                        ..Default::default()
                    },
                    salience: Salience::Spotlight,
                    rationale: Some("mental model".into()),
                    source: SalienceSource::Human,
                },
                AttentionRegion {
                    target: ReviewTarget {
                        file: Some("generated.lock".into()),
                        anchor: Some(CommentAnchor::File {
                            path: "generated.lock".into(),
                            old_path: None,
                            diff_fingerprint: "lock-fp".into(),
                        }),
                        ..Default::default()
                    },
                    salience: Salience::Skim,
                    rationale: Some("generated churn".into()),
                    source: SalienceSource::Human,
                },
            ],
            walkthroughs: vec![Walkthrough {
                id: "walkthrough".into(),
                steps: vec![WalkthroughStep {
                    id: "step".into(),
                    title: Some("Understand the core".into()),
                    target: ReviewTarget {
                        file: Some("src/core.rs".into()),
                        anchor: Some(CommentAnchor::File {
                            path: "src/core.rs".into(),
                            old_path: None,
                            diff_fingerprint: "core-fp".into(),
                        }),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut session = ReviewSession::new(
            "/repo".into(),
            JjReviewTarget {
                base: "main".into(),
                rev: "@".into(),
            },
            DiffSet {
                raw_header: Vec::new(),
                files: vec![spotlight, skim],
            },
            state,
        );
        let anchor = session
            .review_stream()
            .rows
            .iter()
            .find(|row| row.path.as_deref() == Some("src/core.rs") && row.anchor.is_some())
            .and_then(|row| row.anchor.clone())
            .unwrap();
        session.comments.push(Comment {
            id: "comment".into(),
            session_id: Some("session".into()),
            path: Some("src/core.rs".into()),
            line: Some(1),
            anchor: Some(anchor),
            body: "A shared card".into(),
            ..Default::default()
        });
        session.touch_stream_inputs();
        session
    }

    #[test]
    fn reading_projection_preserves_guided_stream_rows_and_attention() {
        let session = fixture();
        let stream = session.review_stream();
        let projection = session.reading_projection();
        let guided_ids = projection
            .regions
            .iter()
            .flat_map(|region| match region.kind {
                ReadingRegionKind::Skim(_) => region.rows.iter().take(1),
                _ => region.rows.iter().take(region.rows.len()),
            })
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            guided_ids,
            stream
                .rows
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(projection.spotlight_count, stream.spotlights.len());
        assert_eq!(projection.skim_count, 1);
        assert!(projection.has_walkthrough);
        let spotlight_rows = projection
            .regions
            .iter()
            .flat_map(|region| &region.rows)
            .filter(|row| row.salience == Some(Salience::Spotlight))
            .collect::<Vec<_>>();
        assert!(!spotlight_rows.is_empty());
        assert_eq!(
            spotlight_rows
                .iter()
                .map(|row| row.annotations.len())
                .sum::<usize>(),
            stream.annotations.len()
        );
        assert_eq!(stream.annotations.len(), 2);
        assert!(projection.regions.iter().any(|region| {
            matches!(&region.kind, ReadingRegionKind::Skim(fold) if fold.rationale == "generated churn")
                && region.rows.iter().skip(1).all(|row| row.salience == Some(Salience::Skim))
        }));
    }

    #[test]
    fn region_ids_are_stable_across_equal_projections() {
        let session = fixture();
        let first = session.reading_projection();
        let second = session.reading_projection();
        assert_eq!(
            first
                .regions
                .iter()
                .map(|region| &region.id)
                .collect::<Vec<_>>(),
            second
                .regions
                .iter()
                .map(|region| &region.id)
                .collect::<Vec<_>>()
        );
        assert!(first.regions.iter().all(|region| {
            region
                .id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        }));
        assert_eq!(
            first
                .regions
                .iter()
                .map(|region| &region.id)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            first.regions.len()
        );
    }
}
