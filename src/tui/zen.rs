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
use crate::diff::{DiffLineKind, FileDiff, Hunk};
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
    /// Chapter cards show the change's full description body by default;
    /// `d` collapses it to the headline (sticky for the walkthrough).
    pub(super) chapter_description_collapsed: bool,
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
    /// jj description (full, multiline); empty when unknown.
    pub(super) description: String,
    pub(super) bookmarks: String,
    /// The agent's high-level narrative for this change, when briefed.
    pub(super) summary: Option<String>,
    /// Exhibits attached to the change brief, opened with `e`.
    pub(super) artifacts: Vec<Artifact>,
    pub(super) derived_lines: Vec<String>,
}

impl ChapterCard {
    /// The description's first line — the card's headline.
    pub(super) fn title(&self) -> &str {
        self.description.lines().next().unwrap_or_default()
    }

    /// Description lines after the headline, outer blank lines trimmed.
    /// What the collapsible body of the chapter card shows.
    pub(super) fn description_body(&self) -> Vec<&str> {
        let mut lines: Vec<&str> = self.description.lines().skip(1).collect();
        while lines.first().is_some_and(|line| line.trim().is_empty()) {
            lines.remove(0);
        }
        while lines.last().is_some_and(|line| line.trim().is_empty()) {
            lines.pop();
        }
        lines
    }
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
        // Agent overlays are workspace-scoped. After `jj ship`, the default
        // review target can be empty while the overlay still contains the
        // previous review's chunks/briefs. Treat an empty diff as nothing to
        // review so zen does not tour stale shipped content.
        if session.files.is_empty() {
            return None;
        }
        let (chunk_stops, glance_rows, source) = if session.review_chunks.is_empty() {
            let (stops, glance) = fallback_rows(session, stack);
            (stops, glance, ZenSource::Files)
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
            chapter_description_collapsed: false,
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
    let mut previous_scopes: Vec<Vec<&FileDiff>> = Vec::new();
    for (index, (anchor, rows)) in groups.into_iter().enumerate() {
        let file_scope = chapter_file_scope(session, &rows, total);
        let mut chapter = chapter_card(
            anchor,
            (index + 1, total),
            rows.len(),
            file_scope.clone(),
            session,
            stack,
        );
        if let Some(files) = file_scope.as_deref()
            && let Some(line) = chapter_dependency_line(session, files, &previous_scopes, &stops)
        {
            chapter.derived_lines.push(line);
        }
        if let Some(files) = file_scope {
            previous_scopes.push(files);
        }
        stops.push(ZenStop::Chapter(chapter));
        stops.extend(rows.into_iter().map(ZenStop::Chunk));
    }
    stops
}

fn chapter_dependency_line(
    session: &ReviewSession,
    files: &[&FileDiff],
    previous_scopes: &[Vec<&FileDiff>],
    existing_stops: &[ZenStop],
) -> Option<String> {
    let current_paths: std::collections::BTreeSet<&str> =
        files.iter().map(|f| f.path.as_str()).collect();
    let current_symbols: std::collections::BTreeSet<String> =
        top_symbols(session, files).into_iter().collect();
    for (prev_idx, prev_files) in previous_scopes.iter().enumerate().rev() {
        let prev_paths: std::collections::BTreeSet<&str> =
            prev_files.iter().map(|f| f.path.as_str()).collect();
        let prev_symbols: std::collections::BTreeSet<String> =
            top_symbols(session, prev_files).into_iter().collect();
        let mut evidence = current_paths
            .intersection(&prev_paths)
            .map(|path| (*path).to_owned())
            .collect::<Vec<_>>();
        evidence.extend(current_symbols.intersection(&prev_symbols).cloned());
        evidence.sort();
        evidence.dedup();
        evidence.truncate(4);
        if !evidence.is_empty() {
            let title = existing_stops
                .iter()
                .filter_map(|stop| match stop {
                    ZenStop::Chapter(chapter) => Some(chapter.title()),
                    ZenStop::Chunk(_) => None,
                })
                .nth(prev_idx)
                .unwrap_or_default();
            return Some(format!(
                "builds on ch.{} \"{}\": {}",
                prev_idx + 1,
                title,
                evidence.join(", ")
            ));
        }
    }
    None
}

/// Resolve one chapter's metadata: the jj summary for its change (or for
/// the home target when it cleanly names a single change) plus the agent's
/// brief for that change id.
fn chapter_card(
    anchor: Option<String>,
    position: (usize, usize),
    stop_count: usize,
    file_scope: Option<Vec<&FileDiff>>,
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
        derived_lines: file_scope
            .as_deref()
            .map(|files| derived_chapter_lines(session, files))
            .unwrap_or_default(),
    }
}

/// The honest file set for chapter-level derived facts.
///
/// A single chapter represents the whole reviewed target (the normal fallback
/// and one-change case), so session-wide facts remain accurate. Multi-chapter
/// curated tours only know chapter membership from the chunk parts that formed
/// that chapter; if those parts do not identify files in the loaded session,
/// we omit derived facts rather than repeating global numbers on every card.
fn chapter_file_scope<'a>(
    session: &'a ReviewSession,
    rows: &[ChunkRow],
    total_chapters: usize,
) -> Option<Vec<&'a FileDiff>> {
    if let Some(change_id) = rows.iter().find_map(|row| row.change_id.as_deref())
        && let Some((_, diff)) = session
            .change_diffs
            .iter()
            .find(|(id, _)| change_ids_match(id, change_id))
    {
        return Some(diff.files.iter().collect());
    }
    if total_chapters == 1 {
        return Some(session.files.iter().map(|file| &file.diff).collect());
    }
    let paths: std::collections::BTreeSet<&str> = rows
        .iter()
        .filter_map(|row| row.part.as_ref().map(|part| part.path.as_str()))
        .collect();
    if paths.is_empty() {
        return None;
    }
    let files = session
        .files
        .iter()
        .filter(|file| paths.contains(file.path.as_str()))
        .map(|file| &file.diff)
        .collect::<Vec<_>>();
    (!files.is_empty()).then_some(files)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileRole {
    Source,
    Tests,
    ConfigManifest,
    Docs,
}

impl FileRole {
    fn label(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Tests => "tests",
            Self::ConfigManifest => "config-manifest",
            Self::Docs => "docs",
        }
    }
}

fn file_role(path: &str) -> FileRole {
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("test")
        || lower.contains("/test")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_tests.rs")
    {
        FileRole::Tests
    } else if lower.ends_with(".md") || lower.starts_with("doc") {
        FileRole::Docs
    } else if is_manifest_path(&lower) {
        FileRole::ConfigManifest
    } else {
        FileRole::Source
    }
}

fn is_manifest_path(lower: &str) -> bool {
    matches!(
        lower,
        "cargo.toml"
            | "cargo.lock"
            | "package.json"
            | "package-lock.json"
            | "flake.nix"
            | "flake.lock"
    ) || lower.ends_with(".toml")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".json")
}

fn derived_chapter_lines(session: &ReviewSession, files: &[&FileDiff]) -> Vec<String> {
    let mut counts = [0usize; 4];
    for file in files {
        counts[file_role(&file.path) as usize] += 1;
    }
    let roles = [
        (FileRole::Source, counts[0]),
        (FileRole::Tests, counts[1]),
        (FileRole::ConfigManifest, counts[2]),
        (FileRole::Docs, counts[3]),
    ]
    .into_iter()
    .filter(|(_, n)| *n > 0)
    .map(|(r, n)| format!("{n} {}", r.label()))
    .collect::<Vec<_>>()
    .join(" · ");
    let additions: usize = files.iter().map(|f| f.additions).sum();
    let deletions: usize = files.iter().map(|f| f.deletions).sum();
    let tests = if files.iter().any(|f| file_role(&f.path) == FileRole::Tests) {
        "tests touched"
    } else {
        "no tests touched"
    };
    let symbols = top_symbols(session, files).join(", ");
    let mut lines = vec![
        format!("stops: roles: {roles}"),
        format!("stops: churn: +{additions} −{deletions} · {tests}"),
        format!(
            "stops: top changed symbols: {}",
            if symbols.is_empty() {
                "none detected"
            } else {
                &symbols
            }
        ),
    ];
    let public_api_files = files.iter().filter(|file| public_api_change(file)).count();
    if public_api_files > 0 {
        lines.push(format!(
            "stops: public API: {public_api_files} file(s) change pub signatures"
        ));
    }
    let error_files = files
        .iter()
        .filter(|file| error_handling_touches(file) >= 2)
        .count();
    if error_files > 0 {
        lines.push(format!(
            "stops: error handling: {error_files} file(s) touch error handling"
        ));
    }
    lines
}

fn top_symbols(session: &ReviewSession, files: &[&FileDiff]) -> Vec<String> {
    let mut out = Vec::new();
    for file in files {
        for s in symbols_for_file(session, file) {
            if !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out.truncate(5);
    out
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
fn fallback_rows(
    session: &ReviewSession,
    stack: &[JjChangeSummary],
) -> (Vec<ChunkRow>, Vec<ChunkRow>) {
    let fallback_stack = if let Some(change) = home_change(session, stack) {
        vec![change]
    } else {
        fallback_chapter_stack(stack)
    };
    if fallback_stack.len() > 1 && !session.change_diffs.is_empty() {
        let mut stops = Vec::new();
        let mut glance = Vec::new();
        for change in &fallback_stack {
            if let Some((_, diff)) = session
                .change_diffs
                .iter()
                .find(|(id, _)| change_ids_match(id, &change.change_id))
            {
                let mut files: Vec<_> = diff.files.iter().collect();
                files.sort_by_key(|f| {
                    (
                        file_role(&f.path),
                        std::cmp::Reverse(f.additions + f.deletions),
                        f.path.clone(),
                    )
                });
                let tests_are_subject = tests_are_review_subject(
                    Some(change.description.as_str()),
                    diff.files.iter().collect::<Vec<_>>().as_slice(),
                );
                for file in files {
                    let role = file_role(&file.path);
                    if (role != FileRole::Source && !(role == FileRole::Tests && tests_are_subject))
                        || is_exports_only(file)
                    {
                        glance.push(whole_file_row(
                            file.path.clone(),
                            Some(role.label().to_owned()),
                        ));
                    } else {
                        stops.push(fallback_file_row(
                            session,
                            file,
                            Some(change.change_id.clone()),
                            Some(change.description.as_str()),
                        ));
                    }
                }
            }
        }
        if !stops.is_empty() || !glance.is_empty() {
            return (stops, glance);
        }
    }
    let mut files: Vec<_> = session.files.iter().collect();
    files.sort_by_key(|f| {
        (
            file_role(&f.path),
            std::cmp::Reverse(f.additions + f.deletions),
            f.path.clone(),
        )
    });
    let mut stops = Vec::new();
    let mut glance = Vec::new();
    let source_count = files
        .iter()
        .filter(|file| file_role(&file.path) == FileRole::Source && !is_exports_only(&file.diff))
        .count();
    let mut source_index = 0usize;
    let tests_are_subject = tests_are_review_subject(
        None,
        &files.iter().map(|file| &file.diff).collect::<Vec<_>>(),
    );
    for file in files {
        let role = file_role(&file.path);
        if (role != FileRole::Source && !(role == FileRole::Tests && tests_are_subject))
            || is_exports_only(&file.diff)
        {
            glance.push(whole_file_row(
                file.path.clone(),
                Some(
                    if is_exports_only(&file.diff) {
                        "exports only"
                    } else {
                        role.label()
                    }
                    .to_owned(),
                ),
            ));
        } else {
            let change = fallback_stack.get(change_bucket(
                source_index,
                source_count,
                fallback_stack.len(),
            ));
            let change_id = change.map(|change| change.change_id.clone());
            let change_description = change.map(|change| change.description.as_str());
            source_index += 1;
            stops.push(fallback_file_row(
                session,
                &file.diff,
                change_id,
                change_description,
            ));
        }
    }
    (stops, glance)
}

fn fallback_chapter_stack(stack: &[JjChangeSummary]) -> Vec<&JjChangeSummary> {
    if stack.len() <= 1 {
        return stack.iter().collect();
    }
    let described = stack
        .iter()
        .filter(|change| !change.description.trim().is_empty())
        .collect::<Vec<_>>();
    if described.is_empty() {
        stack.iter().collect()
    } else {
        described
    }
}

fn tests_are_review_subject(description: Option<&str>, files: &[&FileDiff]) -> bool {
    let described_as_tests = description
        .and_then(first_meaningful_line)
        .is_some_and(|line| {
            let lower = line.to_ascii_lowercase();
            lower.starts_with("test:")
                || lower.starts_with("tests:")
                || lower.starts_with("test(")
                || lower.starts_with("tests(")
                || lower.contains(" test")
                || lower.contains(" tests")
        });
    let test_files = files
        .iter()
        .filter(|file| file_role(&file.path) == FileRole::Tests)
        .count();
    described_as_tests || (test_files > 0 && test_files * 2 >= files.len())
}

fn change_bucket(index: usize, total_rows: usize, total_changes: usize) -> usize {
    if total_changes == 0 || total_rows == 0 {
        return 0;
    }
    (index * total_changes / total_rows).min(total_changes - 1)
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
        .map(|path| {
            let rationale = if is_exports_only_path(session, &path) {
                "exports only".to_owned()
            } else {
                file_role(&path).label().to_owned()
            };
            whole_file_row(path, Some(rationale))
        })
        .collect()
}

fn largest_hunk(file: &FileDiff) -> Option<&Hunk> {
    file.hunks.iter().max_by_key(|h| {
        h.lines
            .iter()
            .filter(|l| matches!(l.kind, DiffLineKind::Added | DiffLineKind::Removed))
            .count()
    })
}

fn fallback_file_row(
    session: &ReviewSession,
    file: &FileDiff,
    change_id: Option<String>,
    change_description: Option<&str>,
) -> ChunkRow {
    let hunk = largest_hunk(file);
    let (adds, dels) = hunk
        .map(hunk_churn)
        .unwrap_or((file.additions, file.deletions));
    let symbols = symbols_touching_hunk(session, file, hunk).join(", ");
    let start = hunk.map(|h| h.new_start.max(1));
    let end = hunk.map(|h| (h.new_start + h.new_len.saturating_sub(1)).max(h.new_start));
    let mut facts = Vec::new();
    if public_api_change(file) {
        let new_api = public_api_is_new(file);
        facts.push(public_api_fact(file).unwrap_or_else(|| "public API change".to_owned()));
        facts.push(if new_api {
            "review question: is this the right surface to expose?".to_owned()
        } else {
            "review question: do callers handle the new signature?".to_owned()
        });
    }
    let error_count = error_handling_touches(file);
    if error_count >= 2 {
        facts.push(
            error_handling_fact(session, file, hunk)
                .unwrap_or_else(|| "touches error handling".to_owned()),
        );
        facts.push("review question: is the new error path covered?".to_owned());
    }
    if let Some(description) = change_description.and_then(first_meaningful_line) {
        facts.push(format!("owning change: {description}"));
    }
    let fact_suffix = if facts.is_empty() {
        String::new()
    } else {
        format!(" · {}", facts.join(" · "))
    };
    ChunkRow {
        chunk_id: format!("file:{}", file.path),
        title: format!("{} · {}", file.path, file_role(&file.path).label()),
        importance: ChunkImportance::Glance,
        change_id,
        rationale: Some(format!(
            "largest hunk +{adds} −{dels} · symbols: {}{fact_suffix}",
            if symbols.is_empty() {
                "none detected"
            } else {
                &symbols
            }
        )),
        explanation: Some(format!(
            "Showing the largest of {} hunk(s); file total +{} −{}. tab opens the full diff.",
            file.hunks.len(),
            file.additions,
            file.deletions,
        )),
        artifacts: Vec::new(),
        part: Some(ChunkPart {
            path: file.path.clone(),
            start_line: start,
            end_line: end,
        }),
        part_position: None,
        invalid_reason: None,
    }
}

fn first_meaningful_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
}

fn public_api_fact(file: &FileDiff) -> Option<String> {
    added_text_lines(file)
        .find(|line| is_public_api_line(line))
        .map(|line| {
            let summary = signature_summary(line);
            if public_api_is_new(file) {
                return format!("new public API: {summary}");
            }
            format!("public API change: {} signature changed", summary)
        })
}

fn public_api_is_new(file: &FileDiff) -> bool {
    file.additions > 0
        && file
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .all(|line| line.kind != DiffLineKind::Removed)
}

fn is_public_api_line(line: &str) -> bool {
    [
        "pub fn ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
        "pub mod ",
        "pub use ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn signature_summary(line: &str) -> String {
    line.split('{')
        .next()
        .unwrap_or(line)
        .split(';')
        .next()
        .unwrap_or(line)
        .trim()
        .to_owned()
}

fn error_handling_fact(
    session: &ReviewSession,
    file: &FileDiff,
    hunk: Option<&Hunk>,
) -> Option<String> {
    let count = error_handling_touches(file);
    (count > 0).then(|| {
        let symbol = symbols_touching_hunk(session, file, hunk)
            .into_iter()
            .next()
            .or_else(|| symbols_for_file(session, file).into_iter().next())
            .unwrap_or_else(|| file.path.clone());
        format!("error handling: {count} changed Result/unwrap/error sites in {symbol}")
    })
}

fn hunk_churn(h: &Hunk) -> (usize, usize) {
    (
        h.lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Added)
            .count(),
        h.lines
            .iter()
            .filter(|l| l.kind == DiffLineKind::Removed)
            .count(),
    )
}

fn whole_file_row(path: String, rationale: Option<String>) -> ChunkRow {
    ChunkRow {
        chunk_id: format!("file:{path}"),
        title: path.clone(),
        importance: ChunkImportance::Glance,
        change_id: None,
        rationale,
        explanation: None,
        artifacts: Vec::new(),
        part: Some(ChunkPart {
            path,
            start_line: None,
            end_line: None,
        }),
        part_position: None,
        invalid_reason: None,
    }
}

fn changed_text_lines(file: &FileDiff) -> impl Iterator<Item = &str> {
    file.hunks.iter().flat_map(|h| &h.lines).filter_map(|line| {
        matches!(line.kind, DiffLineKind::Added | DiffLineKind::Removed).then_some(line.text.trim())
    })
}

fn added_text_lines(file: &FileDiff) -> impl Iterator<Item = &str> {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter_map(|line| (line.kind == DiffLineKind::Added).then_some(line.text.trim()))
}

fn public_api_change(file: &FileDiff) -> bool {
    file.path.ends_with(".rs") && changed_text_lines(file).any(is_public_api_line)
}

fn error_handling_touches(file: &FileDiff) -> usize {
    changed_text_lines(file)
        .filter(|line| {
            line.contains("Result<")
                || line.contains("?;")
                || line.contains(".unwrap()")
                || line.contains(".expect(")
                || line.contains("panic!")
                || line.contains("catch")
                || line.contains("raise")
                || line.contains("except")
        })
        .count()
}

fn is_exports_only(file: &FileDiff) -> bool {
    file.path.ends_with("lib.rs")
        && file
            .hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == DiffLineKind::Added)
            .all(|l| {
                let t = l.text.trim();
                t.starts_with("pub mod ") || t.starts_with("pub use ") || t.is_empty()
            })
}
fn is_exports_only_path(session: &ReviewSession, path: &str) -> bool {
    session
        .files
        .iter()
        .find(|f| f.path == path)
        .is_some_and(|f| is_exports_only(&f.diff))
}

fn new_source(file: &FileDiff) -> String {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| matches!(l.kind, DiffLineKind::Added | DiffLineKind::Context))
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}
fn symbols_for_file(session: &ReviewSession, file: &FileDiff) -> Vec<String> {
    crate::syntax::symbol_spans(&file.path, &new_source(file), &session.syntax)
        .into_iter()
        .map(|s| format!("{} {}", s.kind, s.name))
        .collect()
}
fn symbols_touching_hunk(
    session: &ReviewSession,
    file: &FileDiff,
    hunk: Option<&Hunk>,
) -> Vec<String> {
    let Some(h) = hunk else { return vec![] };
    let start = h.new_start;
    let end = h.new_start + h.new_len.saturating_sub(1);
    crate::syntax::symbol_spans(&file.path, &new_source(file), &session.syntax)
        .into_iter()
        .filter(|s| s.end_line >= start && s.start_line <= end)
        .map(|s| format!("{} {}", s.kind, s.name))
        .collect()
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
            .is_some_and(|file| file.viewed || file.caught_up)
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
    use crate::diff::DiffSet;
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

    fn first_file(diff: &str) -> FileDiff {
        snapshot_session(diff).files.remove(0).diff
    }

    #[test]
    fn public_api_change_detector_is_rust_pub_signature_only() {
        let file = first_file(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-fn old() {}\n+pub fn new_api() {}\n",
        );
        assert!(public_api_change(&file));

        let file = first_file(
            "diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1 +1 @@\n-old\n+pub fn docs only\n",
        );
        assert!(!public_api_change(&file));
    }

    #[test]
    fn error_handling_detector_counts_observed_error_lines() {
        let file = first_file(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,2 @@\n-old\n+let value: Result<u8, Error> = parse()?;\n+panic!(\"bad\");\n",
        );
        assert_eq!(error_handling_touches(&file), 2);

        let file = first_file(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+let value = parse();\n",
        );
        assert_eq!(error_handling_touches(&file), 0);
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
                description: "feat: first\n\nLays the groundwork.\nTwo lines of body.".to_owned(),
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
    fn zen_ignores_stale_chunks_when_diff_is_empty() {
        let mut session = snapshot_session("");
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![spotlight("old", "Old shipped chunk", None, "a.rs")],
            briefs: vec![ChangeBrief {
                change_id: "aaabbbcc".to_owned(),
                summary: "old brief".to_owned(),
                artifacts: Vec::new(),
            }],
            ..Default::default()
        });

        assert!(ZenState::new(&session, &stack()).is_none());
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
        assert_eq!(chunk_stop(&zen, 1).title, "a.rs · source");
        assert_eq!(
            chunk_stop(&zen, 1).part.as_ref().unwrap().start_line,
            Some(1)
        );
        assert_eq!(chunk_stop(&zen, 2).title, "b.rs · source");
        assert!(!zen.has_glance());
    }

    #[test]
    fn fallback_derives_roles_order_largest_hunk_symbols_and_glance_rationale() {
        let session = snapshot_session(
            r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1 +1,2 @@
 pub mod old;
+pub mod queue;
diff --git a/Cargo.toml b/Cargo.toml
--- a/Cargo.toml
+++ b/Cargo.toml
@@ -1 +1,2 @@
 [dependencies]
+itertools = "1"
diff --git a/tests/basic.rs b/tests/basic.rs
--- a/tests/basic.rs
+++ b/tests/basic.rs
@@ -1 +1,2 @@
 fn smoke() {}
+fn queue_orders_priority() {}
diff --git a/src/queue.rs b/src/queue.rs
--- a/src/queue.rs
+++ b/src/queue.rs
@@ -1,2 +1,5 @@
 pub struct Queue;
 impl Queue {
+    pub fn pop(&self) {}
+    pub fn push(&self) {}
+
 }
@@ -20,2 +23,4 @@
 fn helper() {}
+fn tiny() {}
"#,
        );
        let zen = ZenState::new(&session, &[]).unwrap();

        let chapter = chapter(&zen, 0);
        assert!(chapter.derived_lines.iter().any(|l| l.contains("2 source")));
        assert!(
            chapter
                .derived_lines
                .iter()
                .any(|l| l.contains("tests touched"))
        );
        assert_eq!(zen.chunk_stop_count(), 1);
        let stop = chunk_stop(&zen, 1);
        assert_eq!(stop.part.as_ref().unwrap().path, "src/queue.rs");
        assert_eq!(stop.part.as_ref().unwrap().start_line, Some(1));
        assert!(stop.rationale.as_deref().unwrap().contains("+3 −0"));
        assert!(stop.rationale.as_deref().unwrap().contains("symbols:"));
        assert!(stop.explanation.as_deref().unwrap().contains("largest of"));
        assert!(
            zen.glance_rows
                .iter()
                .any(|r| r.title == "Cargo.toml"
                    && r.rationale.as_deref() == Some("config-manifest"))
        );
        assert!(
            zen.glance_rows
                .iter()
                .any(|r| r.title == "src/lib.rs" && r.rationale.as_deref() == Some("exports only"))
        );
        assert!(
            zen.glance_rows
                .iter()
                .any(|r| r.title == "tests/basic.rs" && r.rationale.as_deref() == Some("tests"))
        );
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
        // The card carries the full description: headline + body.
        assert_eq!(first.title(), "feat: first");
        assert_eq!(
            first.description_body(),
            vec!["Lays the groundwork.", "Two lines of body."]
        );
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
    fn multi_change_chapters_scope_derived_lines_to_their_chunk_files() {
        let mut session = snapshot_session(
            r#"diff --git a/src/alpha.rs b/src/alpha.rs
--- a/src/alpha.rs
+++ b/src/alpha.rs
@@ -1 +1 @@
-fn old_alpha() {}
+pub fn alpha() {}
diff --git a/tests/beta.rs b/tests/beta.rs
--- a/tests/beta.rs
+++ b/tests/beta.rs
@@ -1 +1,2 @@
-fn old_beta() {}
+fn beta_smoke() {}
+panic!("beta");
"#,
        );
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![
                spotlight("s1", "alpha", Some("aaabbbcc"), "src/alpha.rs"),
                spotlight("s2", "beta", Some("dddeeeff"), "tests/beta.rs"),
            ],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &stack()).unwrap();

        let first = chapter(&zen, 0);
        assert!(
            first
                .derived_lines
                .iter()
                .any(|l| l == "stops: roles: 1 source")
        );
        assert!(
            first
                .derived_lines
                .iter()
                .any(|l| l == "stops: churn: +1 −1 · no tests touched")
        );
        assert!(first.derived_lines.iter().any(|l| l.contains("public API")));
        assert!(
            !first
                .derived_lines
                .iter()
                .any(|l| l == "stops: roles: 1 tests")
        );

        let second = chapter(&zen, 2);
        assert!(
            second
                .derived_lines
                .iter()
                .any(|l| l == "stops: roles: 1 tests")
        );
        assert!(
            second
                .derived_lines
                .iter()
                .any(|l| l == "stops: churn: +2 −1 · tests touched")
        );
        assert!(
            !second
                .derived_lines
                .iter()
                .any(|l| l.contains("public API"))
        );
    }

    #[test]
    fn multi_change_chapters_narrate_overlap_with_earlier_chapters() {
        let mut session = snapshot_session(
            r#"diff --git a/src/parser.rs b/src/parser.rs
--- a/src/parser.rs
+++ b/src/parser.rs
@@ -1,3 +1,5 @@
 pub fn parse_entry() {
-    old();
+    groundwork();
+    followup();
 }
diff --git a/src/render.rs b/src/render.rs
--- a/src/render.rs
+++ b/src/render.rs
@@ -1 +1,2 @@
 fn render() {}
+fn render_more() {}
"#,
        );
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![
                spotlight("s1", "parser groundwork", Some("aaabbbcc"), "src/parser.rs"),
                spotlight("s2", "parser follow-up", Some("dddeeeff"), "src/parser.rs"),
            ],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &stack()).unwrap();

        let first = chapter(&zen, 0);
        assert!(
            !first
                .derived_lines
                .iter()
                .any(|l| l.starts_with("builds on"))
        );
        let second = chapter(&zen, 2);
        assert!(second.derived_lines.iter().any(
            |l| l.starts_with("builds on ch.1 \"feat: first\":") && l.contains("src/parser.rs")
        ));
    }

    #[test]
    fn fallback_risk_lines_name_specific_evidence_and_review_questions() {
        let session = snapshot_session(
            r#"diff --git a/src/retry.rs b/src/retry.rs
--- a/src/retry.rs
+++ b/src/retry.rs
@@ -1,4 +1,7 @@
-pub fn retry_with_backoff() {}
+pub fn retry_with_backoff(limit: usize) -> Result<(), Error> {
+    run_loop()?;
+    state.unwrap();
+    Ok(())
+}
"#,
        );

        let zen = ZenState::new(&session, &[]).unwrap();
        let rationale = chunk_stop(&zen, 1).rationale.as_deref().unwrap();

        assert!(rationale.contains(
            "public API change: pub fn retry_with_backoff(limit: usize) -> Result<(), Error> signature changed"
        ));
        assert!(rationale.contains("error handling: 3 changed Result/unwrap/error sites in"));
        assert!(rationale.contains("review question: do callers handle the new signature?"));
        assert!(rationale.contains("review question: is the new error path covered?"));
    }

    #[test]
    fn fallback_names_brand_new_public_items_as_new_api() {
        let session = snapshot_session(
            r#"diff --git a/src/priority.rs b/src/priority.rs
--- /dev/null
+++ b/src/priority.rs
@@ -0,0 +1,4 @@
+pub enum Priority {
+    Low,
+    High,
+}
"#,
        );

        let zen = ZenState::new(&session, &[]).unwrap();
        let rationale = chunk_stop(&zen, 1).rationale.as_deref().unwrap();

        assert!(rationale.contains("new public API: pub enum Priority"));
        assert!(!rationale.contains("signature changed"));
        assert!(rationale.contains("review question: is this the right surface to expose?"));
    }

    #[test]
    fn test_titled_changes_spotlight_test_files_instead_of_glancing_them() {
        let whole = r#"diff --git a/src/config.rs b/src/config.rs
--- a/src/config.rs
+++ b/src/config.rs
@@ -1 +1,2 @@
 fn helper() {}
+fn helper_two() {}
diff --git a/tests/basic.rs b/tests/basic.rs
--- a/tests/basic.rs
+++ b/tests/basic.rs
@@ -1 +1,4 @@
 fn smoke() {}
+#[test]
+fn priority_ordering() {}
+fn retry_drops() {}
"#;
        let mut session = snapshot_session(whole);
        session
            .change_diffs
            .push(("dddeeeff".to_owned(), DiffSet::parse(whole).unwrap()));
        let stack = vec![JjChangeSummary {
            change_id: "dddeeeff".to_owned(),
            bookmarks: String::new(),
            description: "test: cover priority ordering and retry drops".to_owned(),
        }];

        let zen = ZenState::new(&session, &stack).unwrap();

        assert!(zen.stops.iter().any(|stop| matches!(stop, ZenStop::Chunk(row) if row.part.as_ref().is_some_and(|part| part.path == "tests/basic.rs"))));
        assert!(!zen.glance_rows.iter().any(|row| {
            row.part
                .as_ref()
                .is_some_and(|part| part.path == "tests/basic.rs")
        }));
    }

    #[test]
    fn multi_change_chapters_omit_derived_lines_when_files_are_unattributable() {
        let mut session = snapshot_session(two_file_diff());
        let mut first = spotlight("s1", "first", Some("aaabbbcc"), "a.rs");
        first.parts.clear();
        session.apply_agent_overlay(&AgentOverlay {
            chunks: vec![first, spotlight("s2", "second", Some("dddeeeff"), "b.rs")],
            ..Default::default()
        });

        let zen = ZenState::new(&session, &stack()).unwrap();

        assert!(chapter(&zen, 0).derived_lines.is_empty());
        assert!(!chapter(&zen, 2).derived_lines.is_empty());
    }

    #[test]
    fn single_chapter_keeps_session_scoped_derived_lines() {
        let session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session, &[]).unwrap();

        let opener = chapter(&zen, 0);
        assert!(
            opener
                .derived_lines
                .iter()
                .any(|l| l == "stops: roles: 2 source")
        );
        assert!(
            opener
                .derived_lines
                .iter()
                .any(|l| l == "stops: churn: +2 −2 · no tests touched")
        );
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
    fn chapter_description_splits_into_headline_and_trimmed_body() {
        let mut chapter = ChapterCard {
            change_id: None,
            position: (1, 1),
            stop_count: 0,
            description: "feat: headline\n\n\nbody one\nbody two\n\n".to_owned(),
            bookmarks: String::new(),
            summary: None,
            artifacts: Vec::new(),
            derived_lines: Vec::new(),
        };

        assert_eq!(chapter.title(), "feat: headline");
        assert_eq!(chapter.description_body(), vec!["body one", "body two"]);

        chapter.description = "just a headline".to_owned();
        assert!(chapter.description_body().is_empty());

        chapter.description = String::new();
        assert_eq!(chapter.title(), "");
        assert!(chapter.description_body().is_empty());
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
        assert_eq!(opener.change_id.as_deref(), Some("dddeeeff"));
        assert_eq!(opener.description, "feat: second");
        assert_eq!(opener.summary.as_deref(), Some("Reworks the retry loop."));
    }

    #[test]
    fn uncurated_fallback_builds_per_stack_change_chapters() {
        // trunk()..@ over a two-change stack: uncurated zen still tells the
        // stack story, one described jj change at a time.
        let session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session, &stack()).unwrap();

        assert_eq!(zen.chapter_count(), 2);
        assert_eq!(chapter(&zen, 0).change_id.as_deref(), Some("aaabbbcc"));
        assert_eq!(chapter(&zen, 0).title(), "feat: first");
        assert_eq!(chapter(&zen, 2).change_id.as_deref(), Some("dddeeeff"));
        assert_eq!(chapter(&zen, 2).title(), "feat: second");
        assert_eq!(chunk_stop(&zen, 1).change_id.as_deref(), Some("aaabbbcc"));
        assert_eq!(chunk_stop(&zen, 3).change_id.as_deref(), Some("dddeeeff"));
    }

    #[test]
    fn uncurated_fallback_scopes_derived_facts_to_each_change_diff() {
        let whole = "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+pub fn api() {}\ndiff --git a/tests/basic.rs b/tests/basic.rs\n--- /dev/null\n+++ b/tests/basic.rs\n@@ -0,0 +1 @@\n+#[test] fn basic() {}\n";
        let mut session = snapshot_session(whole);
        session.change_diffs.push((
            "aaabbbcc".to_owned(),
            DiffSet::parse("diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+pub fn api() {}\n").unwrap(),
        ));
        session.change_diffs.push((
            "dddeeeff".to_owned(),
            DiffSet::parse("diff --git a/tests/basic.rs b/tests/basic.rs\n--- /dev/null\n+++ b/tests/basic.rs\n@@ -0,0 +1 @@\n+#[test] fn basic() {}\n").unwrap(),
        ));

        let zen = ZenState::new(&session, &stack()).unwrap();

        assert!(
            chapter(&zen, 0)
                .derived_lines
                .iter()
                .any(|l| l.contains("no tests touched"))
        );
        assert!(
            chapter(&zen, 0)
                .derived_lines
                .iter()
                .any(|l| l.contains("public API"))
        );
        assert!(
            chapter(&zen, 2)
                .derived_lines
                .iter()
                .any(|l| l.contains("tests touched"))
        );
        assert!(
            !chapter(&zen, 2)
                .derived_lines
                .iter()
                .any(|l| l.contains("public API"))
        );
    }

    #[test]
    fn uncurated_fallback_skips_empty_working_copy_chapter_when_stack_has_descriptions() {
        let mut stack = stack();
        stack.push(JjChangeSummary {
            change_id: "zzzyyyxx".to_owned(),
            bookmarks: String::new(),
            description: String::new(),
        });

        let session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session, &stack).unwrap();

        assert_eq!(zen.chapter_count(), 2);
        assert!(zen.stops.iter().all(|stop| match stop {
            ZenStop::Chapter(chapter) => chapter.change_id.as_deref() != Some("zzzyyyxx"),
            ZenStop::Chunk(row) => row.change_id.as_deref() != Some("zzzyyyxx"),
        }));
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
    fn file_fallback_stops_frame_the_largest_hunk() {
        let mut session = snapshot_session(two_file_diff());
        let zen = ZenState::new(&session, &[]).unwrap();

        jump_to_stop(&mut session, &zen.stops[1]);

        assert_eq!(session.focus, Focus::Diff);
        let focus = session.zen_focus.as_ref().unwrap();
        assert_eq!(focus.path, "a.rs");
        assert_eq!(focus.lines, Some((1, 1)));
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
        let mut row = whole_file_row("a.rs".to_owned(), None);
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
