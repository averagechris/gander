//! Zen mode: a focused, agent-curated briefing over the review
//! (docs/focused-diff-ux.md §6). It has three surfaces:
//!
//! - **Focus card** (default): a full-screen stop per spotlight chunk that
//!   shows only the critical lines plus the agent's explanation — pop in,
//!   understand, move on. The walkthrough is organized into *chapters*:
//!   each jj change opens with a chapter card (description, bookmarks,
//!   diff stats, and the agent's high-level brief) before its stops, so a
//!   stacked review reads like a guided tour rather than a bare change id.
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

use crate::agent::{Artifact, ChunkImportance, ChunkPart};
use crate::app::{Focus, ReviewSession, ZenFocus};
use crate::jj::{JjChangeSummary, ReviewTarget};

use super::chunks::{ChunkRow, chunk_rows};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ZenState {
    pub(super) stops: Vec<ZenStop>,
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

/// One station of the walkthrough: a chapter intro card for a jj change,
/// or a spotlight stop inside the current chapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ZenStop {
    Chapter(ChapterCard),
    Chunk(ChunkRow),
}

/// The intro card that opens a chapter: what the human should know about a
/// jj change *before* touring its stops. jj metadata comes from the stack
/// (description, bookmarks); the narrative comes from the agent's change
/// brief; diff stats render live from the (retargeted) session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChapterCard {
    /// The jj change this chapter introduces. `None` means the walkthrough's
    /// home target as a whole (unanchored stops, or the chunkless fallback).
    pub(super) change_id: Option<String>,
    /// 1-indexed `(chapter, total chapters)`.
    pub(super) position: (usize, usize),
    /// Spotlight stops that follow this card.
    pub(super) stop_count: usize,
    /// jj description (first line); empty when unknown.
    pub(super) description: String,
    pub(super) bookmarks: String,
    /// The agent's high-level narrative for this change, when briefed.
    pub(super) summary: Option<String>,
    /// Exhibits attached to the change brief, opened with `e`.
    pub(super) artifacts: Vec<Artifact>,
}

/// The zen surfaces. Focus is the default landing surface for each stop;
/// Reading drops into the (dimmed) normal UI; Glance is the bulk-skim board;
/// Artifact is a scrollable viewer over the current stop's exhibits,
/// layered on the focus card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ZenPhase {
    Focus,
    Reading,
    Glance,
    Artifact { index: usize, scroll: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ZenSource {
    Chunks,
    Files,
}

impl ZenState {
    /// Build a walkthrough: spotlight chunks become full-screen stops and
    /// everything else (glance chunks + uncovered files) lands on the
    /// glance board; a chapter card introduces each jj change the stops
    /// flow through (metadata from `stack`, narrative from the agent's
    /// change briefs). Without chunks, every file becomes a stop under a
    /// single opening chapter. `None` when there is nothing to walk through.
    pub(super) fn new(session: &ReviewSession, stack: &[JjChangeSummary]) -> Option<Self> {
        let (chunk_stops, glance_rows, source) = if session.review_chunks.is_empty() {
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
        (!chunk_stops.is_empty()).then(|| Self {
            stops: chaptered_stops(chunk_stops, session, stack),
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
    pub(super) fn refresh(&mut self, session: &ReviewSession, stack: &[JjChangeSummary]) -> bool {
        let Some(rebuilt) = Self::new(session, stack) else {
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

    pub(super) fn current(&self) -> Option<&ZenStop> {
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

    /// Spotlight stops only, chapters excluded — what "N focus stops" means
    /// to the human.
    pub(super) fn chunk_stop_count(&self) -> usize {
        self.stops
            .iter()
            .filter(|stop| matches!(stop, ZenStop::Chunk(_)))
            .count()
    }

    pub(super) fn chapter_count(&self) -> usize {
        self.stops
            .iter()
            .filter(|stop| matches!(stop, ZenStop::Chapter(_)))
            .count()
    }

    /// 1-indexed `(current, total)` counting spotlight stops only, so the
    /// human-facing numbering ignores chapter cards. On a chapter card the
    /// current count is the number of stops already toured.
    pub(super) fn chunk_position(&self) -> (usize, usize) {
        if self.stops.is_empty() {
            return (0, 0);
        }
        let upto = self.index.min(self.stops.len() - 1);
        let current = self.stops[..=upto]
            .iter()
            .filter(|stop| matches!(stop, ZenStop::Chunk(_)))
            .count();
        (current, self.chunk_stop_count())
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

/// The exhibits attached to a stop: chunk artifacts for spotlight stops,
/// change-brief artifacts for chapter cards.
pub(super) fn stop_artifacts(stop: &ZenStop) -> &[Artifact] {
    match stop {
        ZenStop::Chapter(chapter) => &chapter.artifacts,
        ZenStop::Chunk(row) => &row.artifacts,
    }
}

/// Weave chapter cards into the stop list: every run of stops anchored to
/// the same jj change opens with a card introducing that change. Unanchored
/// runs (and the chunkless fallback) open with a card for the home target,
/// so every walkthrough starts with the big picture.
fn chaptered_stops(
    chunk_stops: Vec<ChunkRow>,
    session: &ReviewSession,
    stack: &[JjChangeSummary],
) -> Vec<ZenStop> {
    let mut groups: Vec<(Option<String>, Vec<ChunkRow>)> = Vec::new();
    for row in chunk_stops {
        match groups.last_mut() {
            Some((anchor, rows)) if *anchor == row.change_id => rows.push(row),
            _ => groups.push((row.change_id.clone(), vec![row])),
        }
    }
    let total = groups.len();
    let mut stops = Vec::new();
    for (index, (anchor, rows)) in groups.into_iter().enumerate() {
        stops.push(ZenStop::Chapter(chapter_card(
            anchor,
            (index + 1, total),
            rows.len(),
            session,
            stack,
        )));
        stops.extend(rows.into_iter().map(ZenStop::Chunk));
    }
    stops
}

/// Resolve one chapter's metadata: the jj summary for its change (or for
/// the home target when it cleanly names a single change) plus the agent's
/// brief for that change id.
fn chapter_card(
    anchor: Option<String>,
    position: (usize, usize),
    stop_count: usize,
    session: &ReviewSession,
    stack: &[JjChangeSummary],
) -> ChapterCard {
    let summary = match &anchor {
        Some(change_id) => stack
            .iter()
            .find(|change| change_ids_match(&change.change_id, change_id)),
        None => home_change(session, stack),
    };
    let brief = anchor
        .clone()
        .or_else(|| summary.map(|change| change.change_id.clone()))
        .and_then(|change_id| {
            session
                .change_briefs
                .iter()
                .find(|brief| change_ids_match(&brief.change_id, &change_id))
        });
    ChapterCard {
        change_id: anchor,
        position,
        stop_count,
        description: summary
            .map(|change| change.description.clone())
            .unwrap_or_default(),
        bookmarks: summary
            .map(|change| change.bookmarks.clone())
            .unwrap_or_default(),
        summary: brief.map(|brief| brief.summary.clone()),
        artifacts: brief
            .map(|brief| brief.artifacts.clone())
            .unwrap_or_default(),
    }
}

/// Agents copy change ids from `review/stack_changes`, but tolerate one
/// side being a longer prefix of the other (jj ids abbreviate freely).
fn change_ids_match(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a.starts_with(b) || b.starts_with(a))
}

/// The stack change the home target reviews, when it names exactly one:
/// either the target rev resolves to a stack entry reviewed against its own
/// parent, or the whole stack is a single change. `None` for multi-change
/// ranges — describing those with the tip's description would mislead.
fn home_change<'a>(
    session: &ReviewSession,
    stack: &'a [JjChangeSummary],
) -> Option<&'a JjChangeSummary> {
    let change = stack
        .iter()
        .find(|change| change.matches_rev(&session.target.rev))
        .or_else(|| (session.target.rev == "@").then(|| stack.last()).flatten())?;
    let single_change = stack.len() == 1 || session.target.base == format!("{}-", change.change_id);
    single_change.then_some(change)
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
        artifacts: Vec::new(),
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

/// The review target a stop wants loaded; chapter cards load their change's
/// own diff so stats and the reading view describe that change.
pub(super) fn stop_target(stop: &ZenStop, home: &ReviewTarget) -> ReviewTarget {
    match stop {
        ZenStop::Chapter(chapter) => match &chapter.change_id {
            Some(change_id) => ReviewTarget::new(format!("{change_id}-"), change_id.clone()),
            None => home.clone(),
        },
        ZenStop::Chunk(row) => row_target(row, home),
    }
}

/// Jump the session to a stop's location and frame it: the diff pane takes
/// focus and `zen_focus` records the file/range so out-of-range rows dim.
/// Chapter cards frame nothing — they park at the top of the change with
/// the whole diff undimmed for the reading view.
pub(super) fn jump_to_stop(session: &mut ReviewSession, stop: &ZenStop) {
    let part = match stop {
        ZenStop::Chapter(_) => {
            session.zen_focus = None;
            // Park at the top of the change (first file, no line frame) so
            // the reading view starts at the beginning of the chapter.
            if let Some(path) = session.ordered_visible_file_paths().into_iter().next() {
                session.jump_to_chunk_part(&ChunkPart {
                    path,
                    start_line: None,
                    end_line: None,
                });
            }
            session.focus = Focus::Diff;
            session.diff_scroll = 0;
            return;
        }
        ZenStop::Chunk(row) => &row.part,
    };
    let Some(part) = part else {
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
/// Chapter cards mark nothing — reading an intro is not reading the code.
pub(super) fn mark_stop_viewed(session: &mut ReviewSession, stop: &ZenStop) {
    if let ZenStop::Chunk(row) = stop
        && let Some(part) = &row.part
    {
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
    use crate::agent::{AgentOverlay, ChangeBrief, ChunkPart, ReviewChunk};
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
                artifacts: Vec::new(),
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

    fn spotlight(id: &str, title: &str, change_id: Option<&str>, path: &str) -> ReviewChunk {
        ReviewChunk {
            id: id.to_owned(),
            title: title.to_owned(),
            importance: ChunkImportance::Spotlight,
            change_id: change_id.map(str::to_owned),
            rationale: None,
            explanation: None,
            artifacts: Vec::new(),
            parts: vec![ChunkPart {
                path: path.to_owned(),
                start_line: Some(1),
                end_line: Some(1),
            }],
        }
    }

    fn stack() -> Vec<JjChangeSummary> {
        vec![
            JjChangeSummary {
                change_id: "aaabbbcc".to_owned(),
                bookmarks: "feature".to_owned(),
                description: "feat: first".to_owned(),
            },
            JjChangeSummary {
                change_id: "dddeeeff".to_owned(),
                bookmarks: String::new(),
                description: "feat: second".to_owned(),
            },
        ]
    }

    fn chunk_stop(zen: &ZenState, index: usize) -> &ChunkRow {
        match &zen.stops[index] {
            ZenStop::Chunk(row) => row,
            ZenStop::Chapter(chapter) => panic!("stop {index} is a chapter: {chapter:?}"),
        }
    }

    fn chapter(zen: &ZenState, index: usize) -> &ChapterCard {
        match &zen.stops[index] {
            ZenStop::Chapter(chapter) => chapter,
            ZenStop::Chunk(row) => panic!("stop {index} is a chunk: {row:?}"),
        }
    }

    #[test]
    fn zen_requires_something_to_review() {
        let session = snapshot_session("");
        assert!(ZenState::new(&session, &[]).is_none());
    }

    #[test]
    fn zen_without_chunks_falls_back_to_file_stops_under_one_chapter() {
        let session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session, &[]).unwrap();

        assert_eq!(zen.source, ZenSource::Files);
        assert_eq!(zen.phase, ZenPhase::Focus);
        // An opening chapter for the home target, then one stop per file.
        assert_eq!(zen.stops.len(), 3);
        assert_eq!(zen.chunk_stop_count(), 2);
        assert_eq!(zen.chapter_count(), 1);
        assert_eq!(chapter(&zen, 0).position, (1, 1));
        assert_eq!(chapter(&zen, 0).stop_count, 2);
        assert_eq!(chunk_stop(&zen, 1).title, "a.rs");
        assert_eq!(chunk_stop(&zen, 1).part.as_ref().unwrap().start_line, None);
        assert_eq!(chunk_stop(&zen, 2).title, "b.rs");
        assert!(!zen.has_glance());
    }

    #[test]
    fn zen_prefers_agent_chunks_when_present() {
        let session = session_with_chunks();
        let zen = ZenState::new(&session, &[]).unwrap();

        assert_eq!(zen.source, ZenSource::Chunks);
        assert_eq!(zen.stops.len(), 3);
        assert_eq!(zen.chunk_stop_count(), 2);
        assert!(matches!(zen.current(), Some(ZenStop::Chapter(_))));
        assert_eq!(chunk_stop(&zen, 1).part.as_ref().unwrap().path, "a.rs");
    }

    #[test]
    fn stacked_stops_get_one_chapter_per_change_in_stop_order() {
        let mut session = snapshot_session(two_file_diff());
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![
                spotlight("s1", "first stop", Some("aaabbbcc"), "a.rs"),
                spotlight("s2", "second stop", Some("aaabbbcc"), "a.rs"),
                spotlight("s3", "third stop", Some("dddeeeff"), "b.rs"),
            ],
            briefs: vec![ChangeBrief {
                // A shorter prefix of the stack id still matches.
                change_id: "dddee".to_owned(),
                summary: "Builds the follow-up on the first change.".to_owned(),
                artifacts: Vec::new(),
            }],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &stack()).unwrap();

        assert_eq!(zen.stops.len(), 5);
        assert_eq!(zen.chapter_count(), 2);
        assert_eq!(zen.chunk_stop_count(), 3);

        let first = chapter(&zen, 0);
        assert_eq!(first.change_id.as_deref(), Some("aaabbbcc"));
        assert_eq!(first.position, (1, 2));
        assert_eq!(first.stop_count, 2);
        assert_eq!(first.description, "feat: first");
        assert_eq!(first.bookmarks, "feature");
        assert_eq!(first.summary, None);

        assert_eq!(chunk_stop(&zen, 1).title, "first stop");
        assert_eq!(chunk_stop(&zen, 2).title, "second stop");

        let second = chapter(&zen, 3);
        assert_eq!(second.change_id.as_deref(), Some("dddeeeff"));
        assert_eq!(second.position, (2, 2));
        assert_eq!(second.stop_count, 1);
        assert_eq!(second.description, "feat: second");
        assert_eq!(
            second.summary.as_deref(),
            Some("Builds the follow-up on the first change.")
        );
        assert_eq!(chunk_stop(&zen, 4).title, "third stop");
    }

    #[test]
    fn stop_artifacts_come_from_chunks_and_change_briefs() {
        let mut session = snapshot_session(two_file_diff());
        let mut chunk = spotlight("s1", "first stop", Some("aaabbbcc"), "a.rs");
        chunk.artifacts = vec![Artifact {
            title: "usage".to_owned(),
            kind: crate::agent::ArtifactKind::Example,
            body: "let x = new();".to_owned(),
        }];
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![chunk],
            briefs: vec![ChangeBrief {
                change_id: "aaabbbcc".to_owned(),
                summary: "Lays the groundwork.".to_owned(),
                artifacts: vec![Artifact {
                    title: "flow".to_owned(),
                    kind: crate::agent::ArtifactKind::Diagram,
                    body: "a -> b".to_owned(),
                }],
            }],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &stack()).unwrap();

        // The chapter card exhibits the brief's artifacts, the stop its
        // chunk's.
        assert_eq!(stop_artifacts(&zen.stops[0]).len(), 1);
        assert_eq!(stop_artifacts(&zen.stops[0])[0].title, "flow");
        assert_eq!(stop_artifacts(&zen.stops[1]).len(), 1);
        assert_eq!(stop_artifacts(&zen.stops[1])[0].title, "usage");
    }

    #[test]
    fn home_chapter_describes_a_single_change_target() {
        // Reviewing one change against its parent: the opening chapter
        // carries that change's description and brief.
        let mut session = snapshot_session(two_file_diff());
        session.target = ReviewTarget::new("dddeeeff-", "dddeeeff");
        session.apply_agent_overlay(&AgentOverlay {
            briefs: vec![ChangeBrief {
                change_id: "dddeeeff".to_owned(),
                summary: "Reworks the retry loop.".to_owned(),
                artifacts: Vec::new(),
            }],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &stack()).unwrap();

        let opener = chapter(&zen, 0);
        assert_eq!(opener.change_id, None);
        assert_eq!(opener.description, "feat: second");
        assert_eq!(opener.summary.as_deref(), Some("Reworks the retry loop."));
    }

    #[test]
    fn home_chapter_stays_generic_for_multi_change_ranges() {
        // trunk()..@ over a two-change stack: the tip's description would
        // mislead, so the opener carries no single change's metadata.
        let session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session, &stack()).unwrap();

        let opener = chapter(&zen, 0);
        assert_eq!(opener.change_id, None);
        assert_eq!(opener.description, "");
        assert_eq!(opener.summary, None);
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
                    artifacts: Vec::new(),
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
                    artifacts: Vec::new(),
                    parts: vec![ChunkPart {
                        path: "b.rs".to_owned(),
                        start_line: Some(1),
                        end_line: Some(1),
                    }],
                },
            ],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &[]).unwrap();

        assert_eq!(zen.chunk_stop_count(), 1);
        assert_eq!(chunk_stop(&zen, 1).title, "risky behavior");
        assert_eq!(zen.glance_rows.len(), 1);
        assert_eq!(zen.glance_rows[0].title, "mechanical follow-up");
    }

    #[test]
    fn files_uncovered_by_chunks_join_the_glance_board() {
        let mut session = snapshot_session(two_file_diff());
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![spotlight("spotlight", "the important bit", None, "a.rs")],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &[]).unwrap();

        // b.rs is untouched by any chunk: it must still be reachable via
        // the glance board so the briefing covers the whole change.
        assert_eq!(zen.chunk_stop_count(), 1);
        assert_eq!(zen.glance_rows.len(), 1);
        assert_eq!(zen.glance_rows[0].title, "b.rs");
        assert_eq!(zen.glance_rows[0].part.as_ref().unwrap().path, "b.rs");
    }

    #[test]
    fn glance_board_selection_moves_and_bulk_marks_viewed() {
        let mut session = snapshot_session(two_file_diff());
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![spotlight("spotlight", "the important bit", None, "a.rs")],
            ..Default::default()
        });
        let mut zen = ZenState::new(&session, &[]).unwrap();

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
        let mut zen = ZenState::new(&session, &[]).unwrap();

        assert!(!zen.back());
        assert!(zen.advance());
        assert!(zen.advance());
        match zen.current().unwrap() {
            ZenStop::Chunk(row) => assert_eq!(row.part.as_ref().unwrap().path, "b.rs"),
            other => panic!("expected a chunk stop, got {other:?}"),
        }
        assert!(!zen.advance());
        assert!(zen.back());
        assert_eq!(zen.index, 1);
    }

    #[test]
    fn advancing_marks_the_stop_file_viewed_but_chapters_mark_nothing() {
        let mut session = session_with_chunks();
        let zen = ZenState::new(&session, &[]).unwrap();

        mark_stop_viewed(&mut session, &zen.stops[0]); // the chapter card
        assert!(session.files.iter().all(|file| !file.viewed));

        mark_stop_viewed(&mut session, &zen.stops[1]);

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
        let zen = ZenState::new(&session, &[]).unwrap();

        jump_to_stop(&mut session, &zen.stops[2]);

        assert_eq!(session.selected_file().unwrap().path, "b.rs");
        assert_eq!(session.focus, Focus::Diff);
        let focus = session.zen_focus.as_ref().unwrap();
        assert_eq!(focus.path, "b.rs");
        assert_eq!(focus.lines, Some((1, 1)));
    }

    #[test]
    fn jump_to_a_chapter_parks_at_the_top_with_nothing_framed() {
        let mut session = session_with_chunks();
        let zen = ZenState::new(&session, &[]).unwrap();
        jump_to_stop(&mut session, &zen.stops[2]); // frame something first

        jump_to_stop(&mut session, &zen.stops[0]);

        assert!(session.zen_focus.is_none());
        assert_eq!(session.focus, Focus::Diff);
        assert_eq!(session.diff_scroll, 0);
        assert_eq!(session.selected_file().unwrap().path, "a.rs");
    }

    #[test]
    fn file_fallback_stops_frame_the_whole_file() {
        let mut session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session, &[]).unwrap();

        jump_to_stop(&mut session, &zen.stops[1]);

        assert_eq!(session.focus, Focus::Diff);
        let focus = session.zen_focus.as_ref().unwrap();
        assert_eq!(focus.path, "a.rs");
        assert_eq!(focus.lines, None);
    }

    #[test]
    fn ending_zen_restores_the_file_pane_and_clears_the_frame() {
        let mut session = session_with_chunks();
        let zen = ZenState::new(&session, &[]).unwrap();
        session.file_pane_visible = false;
        jump_to_stop(&mut session, &zen.stops[1]);

        end(&mut session, &zen);

        assert!(session.zen_focus.is_none());
        assert!(session.file_pane_visible);
    }

    #[test]
    fn zen_goes_stale_when_the_target_changes() {
        let session = session_with_chunks();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        assert!(!zen.is_stale(&session));

        zen.target_key = "elsewhere".to_owned();
        assert!(zen.is_stale(&session));
    }

    #[test]
    fn zen_remembers_its_home_target() {
        let session = session_with_chunks();
        let zen = ZenState::new(&session, &[]).unwrap();

        assert_eq!(zen.home_target, session.target);
        assert_eq!(zen.target_key, session.target.to_string());
    }

    #[test]
    fn stop_targets_prefer_the_change_anchor() {
        let home = crate::jj::ReviewTarget::trunk_to_current();
        let mut row = whole_file_row("a.rs".to_owned());
        assert_eq!(row_target(&row, &home), home);
        assert_eq!(stop_target(&ZenStop::Chunk(row.clone()), &home), home);

        row.change_id = Some("xyz".to_owned());
        let anchored = crate::jj::ReviewTarget::new("xyz-", "xyz");
        assert_eq!(row_target(&row, &home), anchored);
        assert_eq!(stop_target(&ZenStop::Chunk(row), &home), anchored);

        let session = session_with_chunks();
        let mut zen = ZenState::new(&session, &stack()).unwrap();
        assert_eq!(stop_target(&zen.stops[0], &home), home);
        if let ZenStop::Chapter(chapter) = &mut zen.stops[0] {
            chapter.change_id = Some("xyz".to_owned());
        }
        assert_eq!(
            stop_target(&zen.stops[0], &home),
            crate::jj::ReviewTarget::new("xyz-", "xyz")
        );
    }

    #[test]
    fn refresh_rebuilds_stops_and_clamps_the_index() {
        let mut session = session_with_chunks();
        let mut zen = ZenState::new(&session, &[]).unwrap();
        zen.index = 2;
        zen.phase = ZenPhase::Reading;

        // The agent trimmed its chunks down to a single one-part spotlight:
        // the stop list shrinks and the index snaps back into range.
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![spotlight("c2", "tightened", None, "a.rs")],
            ..Default::default()
        });

        assert!(zen.refresh(&session, &[]));
        assert_eq!(zen.stops.len(), 2);
        assert_eq!(zen.index, 1);
        assert_eq!(zen.phase, ZenPhase::Reading);
        assert_eq!(chunk_stop(&zen, 1).title, "tightened");
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
        let mut zen = ZenState::new(&session, &[]).unwrap();

        let empty = snapshot_session("");
        assert!(!zen.refresh(&empty, &[]));
    }
}
