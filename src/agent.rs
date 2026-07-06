//! Shared state for agent collaboration: the "agent overlay".
//!
//! Agents connected over ACP (see [`crate::acp`]) write suggested review
//! ordering, flagged sections, review chunks, and draft comments into a
//! plain JSON overlay file (`agent.json` in the per-workspace state dir,
//! see [`crate::paths`]). The TUI loads and polls this file, surfaces the
//! suggestions, and writes back draft dispositions so the collaboration is
//! two-way while both processes stay independent.

use std::{fs, path::Path};

use color_eyre::eyre::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::diff::{DiffLineKind, FileDiff};

pub const AGENT_OVERLAY_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentOverlay {
    pub version: u32,
    /// Suggested review order: file paths, highest priority first. Files not
    /// listed keep their natural order after the listed ones.
    pub ordering: Vec<String>,
    /// Sections the agent flagged as critical.
    pub flags: Vec<AgentFlag>,
    /// Reviewable units that can span or subdivide files.
    pub chunks: Vec<ReviewChunk>,
    /// Per-change briefings for stacked reviews: the high-level narrative
    /// of each jj change, shown as a chapter intro in the zen walkthrough.
    pub briefs: Vec<ChangeBrief>,
    /// Agent-drafted comments awaiting human review.
    pub drafts: Vec<AgentDraft>,
}

/// The agent's high-level briefing for one jj change: what it accomplishes,
/// why it exists, and how it builds on the changes before it. The zen
/// walkthrough renders it on the chapter card that introduces that change's
/// stops, alongside jj metadata (description, bookmarks, diff stats).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeBrief {
    /// The jj change this brief describes (a change id from
    /// `review/stack_changes`).
    pub change_id: String,
    /// A few sentences of narrative, prose not bullet points.
    pub summary: String,
    /// Exhibits that show the change rather than describe it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
}

/// An agent-produced exhibit attached to a spotlight chunk or a change
/// brief: a usage example, output the agent captured, a small ASCII
/// diagram, or a free-form note. Zen offers these behind an `e` hint on
/// the focus and chapter cards — show, don't just tell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    pub title: String,
    #[serde(default)]
    pub kind: ArtifactKind,
    /// Plain text, rendered verbatim in a scrollable viewer.
    pub body: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    /// Example usage of the changed code.
    #[default]
    Example,
    /// Captured output: a run, a test, a rendered result.
    Output,
    /// An ASCII/unicode diagram of the flow or structure.
    Diagram,
    /// Anything else worth showing.
    Note,
}

impl ArtifactKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Example => "example",
            Self::Output => "output",
            Self::Diagram => "diagram",
            Self::Note => "note",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFlag {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub reason: String,
    #[serde(default)]
    pub priority: FlagPriority,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagPriority {
    Critical,
    #[default]
    High,
    Medium,
    Low,
}

impl FlagPriority {
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewChunk {
    #[serde(default = "new_chunk_id")]
    pub id: String,
    pub title: String,
    /// How aggressively the UI should feature this chunk. Spotlight chunks
    /// become zen walkthrough stops; glance chunks stay in the overview rail
    /// so routine hunks can be acknowledged without being toured one-by-one.
    #[serde(default)]
    pub importance: ChunkImportance,
    /// The jj change this chunk belongs to, when the review spans a stack.
    /// Part line numbers then refer to that change's own diff
    /// (`<change_id>-..<change_id>`), and the zen walkthrough retargets the
    /// review to that change before visiting the chunk — stacked-PR review,
    /// change by change. `None` anchors the chunk to the loaded target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// The teaching text for a spotlight stop: a few sentences that explain
    /// what the code does, why the change exists, and what could break.
    /// Rendered prominently on the zen focus card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// Exhibits behind an `e` hint on the stop's focus card: examples,
    /// captured output, diagrams (see [`Artifact`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub parts: Vec<ChunkPart>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkImportance {
    /// Worth pausing on in the focused walkthrough.
    #[default]
    Spotlight,
    /// Useful context, but not worth a dedicated tour stop.
    Glance,
}

impl ChunkImportance {
    pub fn label(self) -> &'static str {
        match self {
            Self::Spotlight => "spotlight",
            Self::Glance => "glance",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkPart {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct ChunkValidationContext<'a> {
    pub session_files: &'a [FileDiff],
    pub change_diffs: Vec<ChangeDiffContext<'a>>,
}

#[derive(Debug, Clone)]
pub struct ChangeDiffContext<'a> {
    pub change_id: String,
    pub files: &'a [FileDiff],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvalidChunkPart {
    pub chunk_id: String,
    pub chunk_title: String,
    pub part_index: usize,
    pub path: String,
    pub reason: String,
}

fn new_chunk_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn invalid_chunk_parts_message(invalid: &[InvalidChunkPart]) -> String {
    invalid
        .iter()
        .map(|part| {
            format!(
                "chunk '{}' part {} ({}): {}",
                part.chunk_title, part.part_index, part.path, part.reason
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub fn brief_without_spotlight_warnings(
    briefs: &[ChangeBrief],
    chunks: &[ReviewChunk],
) -> Vec<String> {
    briefs
        .iter()
        .filter(|brief| {
            !chunks.iter().any(|chunk| {
                chunk.importance == ChunkImportance::Spotlight
                    && chunk.change_id.as_deref() == Some(brief.change_id.as_str())
            })
        })
        .map(|brief| {
            format!(
                "brief for change {} has no spotlight chunk yet and will not render on a curated zen chapter right now",
                brief.change_id
            )
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkLineSpaceFile {
    pub path: String,
    pub hunks: Vec<ChunkLineSpaceHunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkLineSpaceHunk {
    pub header: String,
    pub start_line: usize,
    pub end_line: usize,
    pub first_line: String,
    pub last_line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiffLineSpaceRange {
    header: String,
    start_line: usize,
    end_line: usize,
    first_line: String,
    last_line: String,
}

fn file_diff_line_space_ranges(file: &FileDiff) -> Vec<DiffLineSpaceRange> {
    let mut ranges = Vec::new();
    for hunk in &file.hunks {
        for use_new_side in [true, false] {
            let lines = hunk
                .lines
                .iter()
                .filter(|line| line.kind != DiffLineKind::Meta)
                .filter_map(|line| {
                    if use_new_side {
                        line.new_lineno
                    } else {
                        line.old_lineno
                    }
                    .map(|line_no| (line_no, line.text.clone()))
                })
                .collect::<Vec<_>>();
            push_contiguous_line_space_ranges(&mut ranges, &hunk.header, lines);
        }
    }
    ranges.sort_by_key(|range| (range.start_line, range.end_line));
    ranges.dedup_by(|a, b| {
        a.start_line == b.start_line && a.end_line == b.end_line && a.header == b.header
    });
    let mut merged: Vec<DiffLineSpaceRange> = Vec::new();
    for range in ranges {
        if let Some(last) = merged.last_mut()
            && range.start_line <= last.end_line + 1
        {
            if range.end_line > last.end_line {
                last.end_line = range.end_line;
                last.last_line = range.last_line;
            }
            continue;
        }
        merged.push(range);
    }
    merged
}

fn push_contiguous_line_space_ranges(
    ranges: &mut Vec<DiffLineSpaceRange>,
    header: &str,
    mut lines: Vec<(usize, String)>,
) {
    lines.sort_by_key(|(line_no, _)| *line_no);
    let mut iter = lines.into_iter();
    let Some((mut start_line, mut last_line_no, mut first_line)) =
        iter.next().map(|(line_no, text)| (line_no, line_no, text))
    else {
        return;
    };
    let mut last_line = first_line.clone();
    for (line_no, text) in iter {
        if line_no == last_line_no || line_no == last_line_no + 1 {
            last_line_no = line_no;
            last_line = text;
        } else {
            ranges.push(DiffLineSpaceRange {
                header: header.to_owned(),
                start_line,
                end_line: last_line_no,
                first_line: first_line.clone(),
                last_line: last_line.clone(),
            });
            start_line = line_no;
            last_line_no = line_no;
            first_line = text.clone();
            last_line = text;
        }
    }
    ranges.push(DiffLineSpaceRange {
        header: header.to_owned(),
        start_line,
        end_line: last_line_no,
        first_line,
        last_line,
    });
}

pub fn chunk_line_space(files: &[FileDiff], path_filter: Option<&str>) -> Vec<ChunkLineSpaceFile> {
    files
        .iter()
        .filter(|file| path_filter.is_none_or(|path| file.path == path))
        .map(|file| ChunkLineSpaceFile {
            path: file.path.clone(),
            hunks: file_diff_line_space_ranges(file)
                .into_iter()
                .map(|range| ChunkLineSpaceHunk {
                    header: range.header,
                    start_line: range.start_line,
                    end_line: range.end_line,
                    first_line: range.first_line,
                    last_line: range.last_line,
                })
                .collect(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkUpdateSummary {
    pub chunks: usize,
    pub updated: usize,
    pub added: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRemoveSummary {
    pub chunks: usize,
    pub removed: usize,
}

pub fn replace_review_chunks(
    existing: &mut Vec<ReviewChunk>,
    chunks: Vec<ReviewChunk>,
    context: &ChunkValidationContext<'_>,
) -> Result<usize, Vec<InvalidChunkPart>> {
    let invalid = validate_review_chunks(&chunks, context);
    if !invalid.is_empty() {
        return Err(invalid);
    }
    *existing = chunks;
    Ok(existing.len())
}

pub fn update_review_chunks(
    existing: &mut Vec<ReviewChunk>,
    chunks: Vec<ReviewChunk>,
    context: &ChunkValidationContext<'_>,
) -> Result<ChunkUpdateSummary, Vec<InvalidChunkPart>> {
    let invalid = validate_review_chunks(&chunks, context);
    if !invalid.is_empty() {
        return Err(invalid);
    }
    let mut updated = 0;
    let mut added = 0;
    for chunk in chunks {
        if let Some(slot) = existing.iter_mut().find(|existing| existing.id == chunk.id) {
            *slot = chunk;
            updated += 1;
        } else {
            existing.push(chunk);
            added += 1;
        }
    }
    Ok(ChunkUpdateSummary {
        chunks: existing.len(),
        updated,
        added,
    })
}

pub fn remove_review_chunks(
    existing: &mut Vec<ReviewChunk>,
    ids: &[String],
) -> Result<ChunkRemoveSummary, Vec<String>> {
    let unknown = ids
        .iter()
        .filter(|id| !existing.iter().any(|chunk| chunk.id == **id))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        return Err(unknown);
    }
    let before = existing.len();
    existing.retain(|chunk| !ids.iter().any(|id| id == &chunk.id));
    Ok(ChunkRemoveSummary {
        chunks: existing.len(),
        removed: before - existing.len(),
    })
}

pub fn validate_review_chunks(
    chunks: &[ReviewChunk],
    context: &ChunkValidationContext<'_>,
) -> Vec<InvalidChunkPart> {
    let mut invalid = Vec::new();
    for chunk in chunks {
        let files = match &chunk.change_id {
            Some(change_id) => match context
                .change_diffs
                .iter()
                .find(|diff| diff.change_id == *change_id)
            {
                Some(diff) => diff.files,
                None => {
                    for (index, part) in chunk.parts.iter().enumerate() {
                        invalid.push(invalid_part(
                            chunk,
                            index,
                            part,
                            format!("unknown or unresolvable change id: {change_id}"),
                        ));
                    }
                    continue;
                }
            },
            None => context.session_files,
        };
        for (index, part) in chunk.parts.iter().enumerate() {
            let Some(file) = files.iter().find(|file| file.path == part.path) else {
                let scope = chunk
                    .change_id
                    .as_ref()
                    .map(|id| format!("change {id}'s diff"))
                    .unwrap_or_else(|| "session diff".to_owned());
                invalid.push(invalid_part(
                    chunk,
                    index,
                    part,
                    format!("file not present in {scope}: {}", part.path),
                ));
                continue;
            };
            if !part_range_intersects_file_diff(part, file) {
                invalid.push(invalid_part(
                    chunk,
                    index,
                    part,
                    format!("line range outside diff line space for {}", part.path),
                ));
            }
        }
    }
    invalid
}

pub fn remove_invalid_chunk_parts(
    chunks: &[ReviewChunk],
    invalid: &[InvalidChunkPart],
) -> Vec<ReviewChunk> {
    chunks
        .iter()
        .map(|chunk| {
            let mut chunk = chunk.clone();
            let chunk_id = chunk.id.clone();
            chunk.parts = chunk
                .parts
                .into_iter()
                .enumerate()
                .filter(|(index, _)| {
                    !invalid
                        .iter()
                        .any(|part| part.chunk_id == chunk_id && part.part_index == *index + 1)
                })
                .map(|(_, part)| part)
                .collect();
            chunk
        })
        .filter(|chunk| !chunk.parts.is_empty())
        .collect()
}

fn invalid_part(
    chunk: &ReviewChunk,
    index: usize,
    part: &ChunkPart,
    reason: String,
) -> InvalidChunkPart {
    InvalidChunkPart {
        chunk_id: chunk.id.clone(),
        chunk_title: chunk.title.clone(),
        part_index: index + 1,
        path: part.path.clone(),
        reason,
    }
}

fn part_range_intersects_file_diff(part: &ChunkPart, file: &FileDiff) -> bool {
    let start = part.start_line.unwrap_or(1);
    let end = part.end_line.unwrap_or(start);
    if start > end {
        return false;
    }
    file_diff_line_space_ranges(file)
        .iter()
        .any(|range| start <= range.end_line && end >= range.start_line)
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use crate::diff::DiffSet;

    fn diff_files(raw: &str) -> Vec<FileDiff> {
        DiffSet::parse(raw).unwrap().files
    }

    fn chunk(id: &str, change_id: Option<&str>, parts: Vec<ChunkPart>) -> ReviewChunk {
        ReviewChunk {
            id: id.to_owned(),
            title: id.to_owned(),
            importance: ChunkImportance::Spotlight,
            change_id: change_id.map(str::to_owned),
            rationale: None,
            explanation: None,
            artifacts: Vec::new(),
            parts,
        }
    }

    fn part(path: &str, start: usize, end: usize) -> ChunkPart {
        ChunkPart {
            path: path.to_owned(),
            start_line: Some(start),
            end_line: Some(end),
        }
    }

    #[test]
    fn validates_valid_multi_part_chunk() {
        let files = diff_files(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -10 +10 @@\n-old\n+new\n",
        );
        let chunks = vec![chunk(
            "c",
            None,
            vec![part("a.rs", 1, 1), part("b.rs", 10, 10)],
        )];
        let invalid = validate_review_chunks(
            &chunks,
            &ChunkValidationContext {
                session_files: &files,
                change_diffs: Vec::new(),
            },
        );
        assert!(invalid.is_empty());
    }

    #[test]
    fn rejects_file_not_present() {
        let files = diff_files(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let chunks = vec![chunk("c", None, vec![part("missing.rs", 1, 1)])];
        let invalid = validate_review_chunks(
            &chunks,
            &ChunkValidationContext {
                session_files: &files,
                change_diffs: Vec::new(),
            },
        );
        assert!(invalid[0].reason.contains("file not present"));
    }

    #[test]
    fn rejects_line_range_outside_diff_line_space() {
        let files = diff_files(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let chunks = vec![chunk("c", None, vec![part("a.rs", 50, 60)])];
        let invalid = validate_review_chunks(
            &chunks,
            &ChunkValidationContext {
                session_files: &files,
                change_diffs: Vec::new(),
            },
        );
        assert!(invalid[0].reason.contains("outside diff line space"));
    }

    #[test]
    fn rejects_unknown_change_id() {
        let files = diff_files(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let chunks = vec![chunk("c", Some("bad"), vec![part("a.rs", 1, 1)])];
        let invalid = validate_review_chunks(
            &chunks,
            &ChunkValidationContext {
                session_files: &files,
                change_diffs: Vec::new(),
            },
        );
        assert!(
            invalid[0]
                .reason
                .contains("unknown or unresolvable change id")
        );
    }

    #[test]
    fn brief_warnings_reflect_current_spotlight_state() {
        let brief = ChangeBrief {
            change_id: "abc".to_owned(),
            summary: "Summary".to_owned(),
            artifacts: Vec::new(),
        };
        let glance = ReviewChunk {
            importance: ChunkImportance::Glance,
            change_id: Some("abc".to_owned()),
            ..chunk("g", Some("abc"), Vec::new())
        };
        assert_eq!(
            brief_without_spotlight_warnings(std::slice::from_ref(&brief), std::slice::from_ref(&glance)),
            vec!["brief for change abc has no spotlight chunk yet and will not render on a curated zen chapter right now".to_owned()]
        );
        let spotlight = ReviewChunk {
            importance: ChunkImportance::Spotlight,
            change_id: Some("abc".to_owned()),
            ..chunk("s", Some("abc"), Vec::new())
        };
        assert!(brief_without_spotlight_warnings(&[brief], &[glance, spotlight]).is_empty());
    }

    #[test]
    fn chunk_line_space_ranges_validate_round_trip_and_filter_by_path() {
        let files = diff_files(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10,2 +10,3 @@\n ctx\n-old\n+new\n+extra\n@@ -25,11 +45,14 @@\n ctx25\n ctx26\n ctx27\n-old28\n-old29\n-old30\n-old31\n-old32\n-old33\n-old34\n ctx35\n+new48\n+new49\n+new50\n+new51\n+new52\n+new53\n+new54\n+new55\n+new56\n+new57\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -20 +20 @@\n-bye\n+hi\n",
        );
        let listed = chunk_line_space(&files, Some("a.rs"));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].path, "a.rs");
        let hunks = &listed[0].hunks;
        assert!(!hunks.is_empty());
        for hunk in hunks {
            // Every listed range is exactly the same coordinate space accepted by chunk validation.
            let chunks = vec![chunk(
                "c",
                None,
                vec![part("a.rs", hunk.start_line, hunk.end_line)],
            )];
            assert!(
                validate_review_chunks(
                    &chunks,
                    &ChunkValidationContext {
                        session_files: &files,
                        change_diffs: Vec::new()
                    }
                )
                .is_empty(),
                "listed range should validate: {hunk:?}"
            );
        }
        for pair in hunks.windows(2) {
            assert!(
                pair[0].end_line < pair[1].start_line,
                "listed line-space ranges must not overlap: {pair:?}"
            );
        }
        let gap_start = 36;
        let gap_end = 44;
        assert!(hunks.iter().any(|hunk| hunk.end_line < gap_start));
        assert!(hunks.iter().any(|hunk| hunk.start_line > gap_end));
        let invalid = validate_review_chunks(
            &[chunk("gap", None, vec![part("a.rs", gap_start, gap_end)])],
            &ChunkValidationContext {
                session_files: &files,
                change_diffs: Vec::new(),
            },
        );
        assert_eq!(invalid.len(), 1);
        assert!(invalid[0].reason.contains("outside diff line space"));
        assert!(
            !hunks
                .iter()
                .any(|hunk| hunk.start_line == 28 && hunk.end_line == 58),
            "old/new mixed range from evaluator repro must never be listed"
        );
        assert_eq!(
            hunks
                .iter()
                .map(|hunk| (hunk.start_line, hunk.end_line))
                .collect::<Vec<_>>(),
            vec![(10, 12), (25, 35), (45, 58)]
        );
    }

    #[test]
    fn update_chunks_preserves_position_and_appends_new_ids() {
        let files = diff_files(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let context = ChunkValidationContext {
            session_files: &files,
            change_diffs: Vec::new(),
        };
        let mut chunks = vec![
            chunk("a", None, vec![part("a.rs", 1, 1)]),
            chunk("b", None, vec![part("a.rs", 1, 1)]),
        ];
        let summary = update_review_chunks(
            &mut chunks,
            vec![
                chunk("a", None, vec![part("a.rs", 1, 1)]),
                chunk("c", None, vec![part("a.rs", 1, 1)]),
            ],
            &context,
        )
        .unwrap();
        assert_eq!((summary.updated, summary.added, summary.chunks), (1, 1, 3));
        assert_eq!(
            chunks
                .iter()
                .map(|chunk| chunk.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn remove_chunks_rejects_unknown_ids_without_applying() {
        let mut chunks = vec![chunk("a", None, Vec::new())];
        assert_eq!(
            remove_review_chunks(&mut chunks, &["missing".to_owned()]),
            Err(vec!["missing".to_owned()])
        );
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn update_chunks_rejects_invalid_parts_without_applying() {
        let files = diff_files(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n-old\n+new\n",
        );
        let context = ChunkValidationContext {
            session_files: &files,
            change_diffs: Vec::new(),
        };
        let mut chunks = vec![chunk("a", None, vec![part("a.rs", 1, 1)])];
        let result = update_review_chunks(
            &mut chunks,
            vec![chunk("b", None, vec![part("missing.rs", 1, 1)])],
            &context,
        );
        assert!(result.is_err());
        assert_eq!(chunks[0].id, "a");
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDraft {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub body: String,
    #[serde(default)]
    pub state: DraftState,
    /// Set when a human accepts the draft: the id of the created comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_comment_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftState {
    #[default]
    Pending,
    Accepted,
    Discarded,
}

impl DraftState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Discarded => "discarded",
        }
    }
}

impl AgentOverlay {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                version: AGENT_OVERLAY_VERSION,
                ..Self::default()
            });
        }
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read agent overlay {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse agent overlay {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Atomic-rename write, same as review state, so a crash cannot
        // corrupt the overlay both processes share.
        let mut tmp = path.to_path_buf();
        tmp.set_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// A review agent summoned by the TUI. Agent-agnostic: any CLI that takes a
/// prompt works (`opencode run`, `claude -p`, ...). The child is killed when
/// this handle drops so quitting the TUI does not leak agents.
#[derive(Debug)]
pub struct AgentProcess {
    child: std::process::Child,
    exited: bool,
}

impl AgentProcess {
    /// Spawn `command` through the shell with `prompt` appended as a final
    /// shell-quoted argument (or substituted for a `{prompt}` placeholder).
    /// Output goes to `log_path`; stdio stays free for the TUI.
    pub fn spawn(repo: &Path, command: &str, prompt: &str, log_path: &Path) -> Result<Self> {
        let quoted = shell_quote(prompt);
        let command_line = if command.contains("{prompt}") {
            command.replace("{prompt}", &quoted)
        } else {
            format!("{command} {quoted}")
        };
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let log = fs::File::create(log_path)
            .with_context(|| format!("failed to create agent log {}", log_path.display()))?;
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command_line)
            .current_dir(repo)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .with_context(|| format!("failed to spawn agent command `{command}`"))?;
        Ok(Self {
            child,
            exited: false,
        })
    }

    /// `None` while the agent is still running, otherwise its exit status.
    pub fn try_status(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.try_wait().ok().flatten();
        if status.is_some() {
            self.exited = true;
        }
        status
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Build the prompt handed to a summoned agent. A custom template may use
/// `{repo}`, `{base}`, and `{rev}`; the default explains the ACP workflow.
pub fn review_prompt(template: Option<&str>, repo: &Path, base: &str, rev: &str) -> String {
    let repo = repo.display().to_string();
    match template {
        Some(template) => template
            .replace("{repo}", &repo)
            .replace("{base}", base)
            .replace("{rev}", rev),
        None => format!(
            "You are assisting a human who is reviewing a code change in the gander TUI. \
             Repository: {repo}. Review target (jj revsets): {base}..{rev}.\n\
             Interact by running `gander acp` from the repository root and speaking \
             line-delimited JSON-RPC 2.0 over its stdio, one JSON object per line \
             (a running TUI is served live through it automatically).\n\
             1. Call initialize, then review/files, then review/file_diff \
             (params: {{\"path\": ...}}) for each file that matters.\n\
             2. Call review/stack_changes: when the target spans several jj \
             changes the human treats them like stacked PRs (or logical \
             groupings that flow into each other), so review the stack \
             change-by-change with review/change_diff \
             (params: {{\"change_id\": ...}}) instead of only reading the \
             squashed diff.\n\
             3. Brief the human on each change with review/set_change_briefs \
             (params: {{\"briefs\": [{{\"change_id\", \"summary\"}}]}}): one \
             brief per change in the reviewed range (change ids from \
             review/stack_changes), 2-4 sentences of prose that teach the \
             change at a high level — what it accomplishes, why it exists, \
             and how it builds on the changes before it. The walkthrough \
             shows each brief as a chapter intro card before that change's \
             stops, so the human is never dropped into a bare change id.\n\
             4. Suggest a review order with review/set_ordering \
             (params: {{\"paths\": [...]}}), riskiest or most central files first.\n\
             5. Flag sections needing extra scrutiny with review/flag_section \
             (params: {{\"path\", \"line\", \"reason\", \"priority\": \
             critical|high|medium|low}}).\n\
             6. Curate a focused walkthrough with review/set_chunks. The human \
             sees spotlight chunks as full-screen stops (a code excerpt plus \
             your explanation) and skims everything else on a glance board, \
             so budget their attention: pick at most 3-7 chunks with \
             importance=spotlight — only the places a reviewer must actually \
             understand (new invariants, tricky logic, security-sensitive \
             paths, the heart of the change). Each spotlight needs a title, \
             precise start_line/end_line parts covering only the critical \
             lines (not whole files), and an `explanation` of 2-5 sentences \
             that teaches the change: what the code does, why it changed, and \
             what could break. When the target is a stack, anchor each chunk \
             to its jj change with `change_id` (from review/stack_changes), \
             take part line numbers from that change's own diff \
             (review/change_diff), and order chunks in stack order — the \
             walkthrough then flows through the stack change by change. \
             Group ALL remaining hunks into a few \
             importance=glance chunks (mechanical renames, boilerplate, \
             config churn, test scaffolding) with a one-line rationale each; \
             the human acknowledges those in bulk without visiting them.\n\
             7. Optionally attach artifacts to change briefs and spotlight \
             chunks (\"artifacts\": [{{\"title\", \"kind\": \
             example|output|diagram|note, \"body\"}}]): show, don't just \
             tell — a usage example of the changed API, output you captured \
             by exercising the code, or a small ASCII diagram of the new \
             flow. The human opens them from the walkthrough card with `e`.\n\
             8. For concrete issues, add review/draft_comment \
             (params: {{\"path\", \"line\", \"body\"}}); the human accepts or \
             discards these in the TUI.\n\
             Your suggestions appear live in the reviewer's terminal. \
             Do not modify the repository. Exit when your review is complete."
        ),
    }
}

/// Minimal POSIX shell single-quoting.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".gander").join("agent.json");
        let overlay = AgentOverlay {
            version: AGENT_OVERLAY_VERSION,
            ordering: vec!["src/risky.rs".to_owned(), "src/safe.rs".to_owned()],
            flags: vec![AgentFlag {
                id: "flag-1".to_owned(),
                path: "src/risky.rs".to_owned(),
                line: Some(42),
                reason: "unchecked unwrap on user input".to_owned(),
                priority: FlagPriority::Critical,
            }],
            chunks: vec![ReviewChunk {
                id: "chunk-1".to_owned(),
                title: "auth flow".to_owned(),
                importance: ChunkImportance::Spotlight,
                change_id: Some("xyzkwqrs".to_owned()),
                rationale: Some("spans handler and middleware".to_owned()),
                explanation: Some("The middleware now refuses tokens without an audience claim; the handler assumes that invariant.".to_owned()),
                artifacts: vec![Artifact {
                    title: "rejecting a token without an audience".to_owned(),
                    kind: ArtifactKind::Output,
                    body: "$ curl -H \"Authorization: Bearer $NO_AUD\" /api\n401 {\"error\":\"missing aud claim\"}".to_owned(),
                }],
                parts: vec![ChunkPart {
                    path: "src/risky.rs".to_owned(),
                    start_line: Some(10),
                    end_line: Some(60),
                }],
            }],
            briefs: vec![ChangeBrief {
                change_id: "xyzkwqrs".to_owned(),
                summary: "Tightens the auth middleware so downstream handlers can assume an audience claim.".to_owned(),
                artifacts: vec![Artifact {
                    title: "token flow".to_owned(),
                    kind: ArtifactKind::Diagram,
                    body: "client -> middleware(aud?) -> handler".to_owned(),
                }],
            }],
            drafts: vec![AgentDraft {
                id: "draft-1".to_owned(),
                path: "src/risky.rs".to_owned(),
                line: Some(42),
                body: "consider handling the None case".to_owned(),
                state: DraftState::Pending,
                accepted_comment_id: None,
            }],
        };

        overlay.save(&path).unwrap();
        let loaded = AgentOverlay::load_or_default(&path).unwrap();

        assert_eq!(loaded, overlay);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn missing_overlay_defaults_to_current_version() {
        let dir = tempfile::tempdir().unwrap();

        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();

        assert_eq!(overlay.version, AGENT_OVERLAY_VERSION);
        assert!(overlay.ordering.is_empty());
        assert!(overlay.flags.is_empty());
    }

    #[test]
    fn flag_priorities_order_critical_first() {
        assert!(FlagPriority::Critical < FlagPriority::High);
        assert!(FlagPriority::High < FlagPriority::Medium);
        assert!(FlagPriority::Medium < FlagPriority::Low);
    }

    #[test]
    fn agent_process_runs_command_with_quoted_prompt_and_logs_output() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("logs").join("agent.log");

        let mut process = AgentProcess::spawn(
            dir.path(),
            "printf '%s'",
            "it's a prompt with 'quotes'",
            &log_path,
        )
        .unwrap();

        let status = wait_for_exit(&mut process);
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            "it's a prompt with 'quotes'"
        );
    }

    #[test]
    fn agent_process_substitutes_prompt_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("agent.log");

        let mut process =
            AgentProcess::spawn(dir.path(), "printf '%s' {prompt} tail", "middle", &log_path)
                .unwrap();

        let status = wait_for_exit(&mut process);
        assert!(status.success());
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "middletail");
    }

    #[test]
    fn agent_process_reports_failure_status() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("agent.log");

        let mut process = AgentProcess::spawn(dir.path(), "false", "unused", &log_path).unwrap();

        assert!(!wait_for_exit(&mut process).success());
    }

    fn wait_for_exit(process: &mut AgentProcess) -> std::process::ExitStatus {
        for _ in 0..400 {
            if let Some(status) = process.try_status() {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("agent process never exited");
    }

    #[test]
    fn default_review_prompt_mentions_acp_workflow_and_target() {
        let prompt = review_prompt(None, Path::new("/repo"), "trunk()", "@");

        assert!(prompt.contains("/repo"));
        assert!(prompt.contains("trunk()..@"));
        assert!(prompt.contains("gander acp"));
        assert!(prompt.contains("review/set_ordering"));
        assert!(prompt.contains("review/stack_changes"));
        assert!(prompt.contains("review/change_diff"));
        assert!(prompt.contains("change_id"));
        assert!(prompt.contains("stacked PRs"));
        assert!(prompt.contains("review/set_change_briefs"));
        assert!(prompt.contains("chapter intro"));
        assert!(prompt.contains("artifacts"));
        assert!(prompt.contains("example|output|diagram|note"));
        assert!(prompt.contains("review/set_chunks"));
        assert!(prompt.contains("importance=spotlight"));
        assert!(prompt.contains("importance=glance"));
        assert!(prompt.contains("explanation"));
        assert!(prompt.contains("3-7"));
        assert!(prompt.contains("review/draft_comment"));
        assert!(prompt.contains("Do not modify the repository."));
    }

    #[test]
    fn custom_prompt_template_substitutes_placeholders() {
        let prompt = review_prompt(
            Some("review {repo} from {base} to {rev}"),
            Path::new("/repo"),
            "main",
            "@",
        );

        assert_eq!(prompt, "review /repo from main to @");
    }
}
