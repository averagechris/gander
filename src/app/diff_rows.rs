//! Construction of the flattened diff rows rendered in the diff pane,
//! including per-line comment anchors.

use std::rc::Rc;

use crate::{
    anchor::{CommentAnchor, DiffSide, fingerprint_line},
    diff::DiffLineKind,
    syntax::SyntaxSpan,
};

use super::{
    ReviewFile, ReviewSession,
    syntax_cache::{SyntaxCacheKey, SyntaxCacheStatus, SyntaxSide, syntax_source},
};

#[derive(Debug, Clone)]
pub struct DiffRow {
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub prefix: &'static str,
    pub text: String,
    pub syntax: Vec<SyntaxSpan>,
    pub kind: DiffRowKind,
    pub anchor: Option<CommentAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    FileHeader,
    SyntaxSummary,
    HunkHeader,
    DiffLine(DiffLineKind),
    Raw,
}

impl ReviewSession {
    /// Diff rows for the selected file, memoized per file/fingerprint/syntax
    /// config. Rebuilding on every draw and cursor move was the main hot spot
    /// on large files.
    pub fn diff_rows_for_selected_file(&self) -> Rc<Vec<DiffRow>> {
        let Some(file) = self.selected_visible_file() else {
            return Rc::new(Vec::new());
        };

        let key = SyntaxCacheKey::for_file(self, file);
        if let Some(cached) = self.rows_cache.borrow().get(&key).cloned() {
            return cached;
        }
        let rows = Rc::new(self.build_diff_rows(file));
        self.rows_cache.borrow_mut().insert(key, Rc::clone(&rows));
        rows
    }

    fn build_diff_rows(&self, file: &ReviewFile) -> Vec<DiffRow> {
        let mut rows = vec![DiffRow {
            old_lineno: None,
            new_lineno: None,
            prefix: " ",
            text: format!("{}  +{} -{}", file.path, file.additions, file.deletions),
            syntax: Vec::new(),
            kind: DiffRowKind::FileHeader,
            anchor: None,
        }];

        let (new_source, new_line_indices) = syntax_source(file, SyntaxSide::New);
        let (old_source, old_line_indices) = syntax_source(file, SyntaxSide::Old);
        let syntax_cache = self.syntax_cache_for_file(
            file,
            &new_source,
            &new_line_indices,
            &old_source,
            &old_line_indices,
        );
        if let Some(summary) = syntax_cache.summary.clone() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: format!(
                    "tree-sitter: {} root={} errors={}",
                    summary.language, summary.root_kind, summary.has_error
                ),
                syntax: Vec::new(),
                kind: DiffRowKind::SyntaxSummary,
                anchor: None,
            });
        }
        if syntax_cache.new_status == SyntaxCacheStatus::Failed
            || syntax_cache.old_status == SyntaxCacheStatus::Failed
        {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: "tree-sitter: highlighting unavailable".to_owned(),
                syntax: Vec::new(),
                kind: DiffRowKind::SyntaxSummary,
                anchor: None,
            });
        }

        for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: hunk.header.clone(),
                syntax: Vec::new(),
                kind: DiffRowKind::HunkHeader,
                anchor: None,
            });
            for (line_index, line) in hunk.lines.iter().enumerate() {
                let prefix = match line.kind {
                    DiffLineKind::Context => " ",
                    DiffLineKind::Added => "+",
                    DiffLineKind::Removed => "-",
                    DiffLineKind::Meta => "\\",
                };
                let syntax = match line.kind {
                    DiffLineKind::Added | DiffLineKind::Context => syntax_cache
                        .new_lines
                        .get(&(hunk_index, line_index))
                        .cloned()
                        .unwrap_or_default(),
                    DiffLineKind::Removed => syntax_cache
                        .old_lines
                        .get(&(hunk_index, line_index))
                        .cloned()
                        .unwrap_or_default(),
                    DiffLineKind::Meta => Vec::new(),
                };
                rows.push(DiffRow {
                    old_lineno: line.old_lineno,
                    new_lineno: line.new_lineno,
                    prefix,
                    text: line.text.clone(),
                    syntax,
                    kind: DiffRowKind::DiffLine(line.kind),
                    anchor: self.line_anchor(file, hunk_index, line_index),
                });
            }
        }

        if file.diff.hunks.is_empty() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: file.diff.raw.clone(),
                syntax: Vec::new(),
                kind: DiffRowKind::Raw,
                anchor: None,
            });
        }

        rows
    }

    fn line_anchor(
        &self,
        file: &ReviewFile,
        hunk_index: usize,
        line_index: usize,
    ) -> Option<CommentAnchor> {
        let hunk = file.diff.hunks.get(hunk_index)?;
        let line = hunk.lines.get(line_index)?;
        let (side, line_number) = match line.kind {
            DiffLineKind::Added => (DiffSide::New, line.new_lineno?),
            DiffLineKind::Removed => (DiffSide::Old, line.old_lineno?),
            DiffLineKind::Context => (DiffSide::New, line.new_lineno?),
            DiffLineKind::Meta => return None,
        };
        Some(CommentAnchor::Line {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            side,
            line: line_number,
            old_line: line.old_lineno,
            new_line: line.new_lineno,
            hunk_header: hunk.header.clone(),
            hunk_old_start: hunk.old_start,
            hunk_old_len: hunk.old_len,
            hunk_new_start: hunk.new_start,
            hunk_new_len: hunk.new_len,
            hunk_index,
            line_index,
            line_kind: match line.kind {
                DiffLineKind::Context => "context",
                DiffLineKind::Added => "added",
                DiffLineKind::Removed => "removed",
                DiffLineKind::Meta => "meta",
            }
            .to_owned(),
            line_text: line.text.clone(),
            line_fingerprint: fingerprint_line(
                &file.path,
                side,
                line_number,
                &line.text,
                &file.fingerprint,
            ),
            diff_fingerprint: file.fingerprint.clone(),
        })
    }
}

pub(super) fn nearest_commentable_row(rows: &[DiffRow], target: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.anchor.is_some())
        .min_by_key(|(index, _)| index.abs_diff(target))
        .map(|(index, _)| index)
}
