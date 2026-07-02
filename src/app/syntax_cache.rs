//! Per-file syntax highlighting cache keyed by diff fingerprint and syntax
//! config, so tree-sitter runs at most once per file/config combination.

use std::collections::BTreeMap;

use crate::{
    diff::DiffLineKind,
    syntax::{HighlightOutcome, SyntaxConfig, SyntaxSpan, SyntaxSummary},
};

use super::{ReviewFile, ReviewSession};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct SyntaxCacheKey {
    pub(super) path: String,
    pub(super) fingerprint: String,
    pub(super) config_key: String,
}

impl SyntaxCacheKey {
    pub(super) fn for_file(session: &ReviewSession, file: &ReviewFile) -> Self {
        Self {
            path: file.path.clone(),
            fingerprint: file.fingerprint.clone(),
            config_key: session.syntax.cache_key(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct SyntaxFileCache {
    pub(super) summary: Option<SyntaxSummary>,
    pub(super) new_lines: BTreeMap<(usize, usize), Vec<SyntaxSpan>>,
    pub(super) old_lines: BTreeMap<(usize, usize), Vec<SyntaxSpan>>,
    pub(super) new_status: SyntaxCacheStatus,
    pub(super) old_status: SyntaxCacheStatus,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SyntaxCacheStatus {
    #[default]
    Disabled,
    Unsupported,
    Highlighted,
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SyntaxSide {
    Old,
    New,
}

impl ReviewSession {
    pub(super) fn syntax_cache_for_file(
        &self,
        file: &ReviewFile,
        new_source: &str,
        new_line_indices: &[(usize, usize)],
        old_source: &str,
        old_line_indices: &[(usize, usize)],
    ) -> SyntaxFileCache {
        let key = SyntaxCacheKey::for_file(self, file);
        if let Some(cached) = self.syntax_cache.borrow().get(&key).cloned() {
            return cached;
        }

        let (new_lines, new_status) =
            syntax_highlights_by_diff_line(&file.path, new_source, new_line_indices, &self.syntax);
        let (old_lines, old_status) =
            syntax_highlights_by_diff_line(&file.path, old_source, old_line_indices, &self.syntax);

        let computed = SyntaxFileCache {
            summary: crate::syntax::summarize_with_config(&file.path, new_source, &self.syntax),
            new_lines,
            old_lines,
            new_status,
            old_status,
        };
        self.syntax_cache.borrow_mut().insert(key, computed.clone());
        computed
    }
}

pub(super) fn syntax_source(file: &ReviewFile, side: SyntaxSide) -> (String, Vec<(usize, usize)>) {
    let mut lines = Vec::new();
    let mut indices = Vec::new();
    for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
        for (line_index, line) in hunk.lines.iter().enumerate() {
            let included = match (side, line.kind) {
                (SyntaxSide::New, DiffLineKind::Added | DiffLineKind::Context) => true,
                (SyntaxSide::Old, DiffLineKind::Removed | DiffLineKind::Context) => true,
                (_, DiffLineKind::Meta) => false,
                _ => false,
            };
            if included {
                lines.push(line.text.as_str());
                indices.push((hunk_index, line_index));
            }
        }
    }
    (lines.join("\n"), indices)
}

fn syntax_highlights_by_diff_line(
    path: &str,
    source: &str,
    line_indices: &[(usize, usize)],
    config: &SyntaxConfig,
) -> (BTreeMap<(usize, usize), Vec<SyntaxSpan>>, SyntaxCacheStatus) {
    match crate::syntax::highlight_outcome(path, source, config) {
        HighlightOutcome::Highlighted { lines, .. } => (
            lines
                .into_iter()
                .zip(line_indices.iter())
                .map(|(line, index)| (*index, line.spans))
                .collect(),
            SyntaxCacheStatus::Highlighted,
        ),
        HighlightOutcome::Disabled => (BTreeMap::new(), SyntaxCacheStatus::Disabled),
        HighlightOutcome::Unsupported => (BTreeMap::new(), SyntaxCacheStatus::Unsupported),
        HighlightOutcome::Failed { .. } => (BTreeMap::new(), SyntaxCacheStatus::Failed),
    }
}
