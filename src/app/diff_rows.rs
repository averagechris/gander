//! Construction of the flattened diff rows rendered in the diff pane,
//! including per-line comment anchors and symbol-aware context folding.

use std::{collections::BTreeMap, ops::Range, rc::Rc};

use crate::{
    anchor::{CommentAnchor, DiffSide, fingerprint_line},
    diff::{DiffLineKind, Hunk},
    syntax::{SymbolSpan, SyntaxSpan},
};

use super::{
    ReviewFile, ReviewSession,
    syntax_cache::{SyntaxCacheKey, SyntaxCacheStatus, SyntaxSide, syntax_source},
    word_diff::hunk_emphasis,
};

/// Context lines kept visible on each side of a fold.
const FOLD_KEEP_CONTEXT: usize = 2;
/// Minimum number of hidden lines for a fold to be worth a placeholder row.
const FOLD_MIN_HIDDEN: usize = 3;

#[derive(Debug, Clone)]
pub struct DiffRow {
    pub old_lineno: Option<usize>,
    pub new_lineno: Option<usize>,
    pub prefix: &'static str,
    pub text: String,
    pub syntax: Vec<SyntaxSpan>,
    /// Byte ranges of `text` to emphasize as changed words (word-level
    /// diff within a modified line pair). Empty when word highlighting is
    /// off or the line has no counterpart.
    pub emphasis: Vec<Range<usize>>,
    pub kind: DiffRowKind,
    pub anchor: Option<CommentAnchor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    FileHeader,
    SyntaxSummary,
    HunkHeader,
    DiffLine(DiffLineKind),
    ContextFold,
    /// Stand-in for content that is intentionally not rendered (binary
    /// files, diffs over the size threshold).
    Placeholder,
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

        let key = (
            SyntaxCacheKey::for_file(self, file),
            self.fold_context,
            self.force_rendered.contains(&file.path),
            self.diff_cues.word_highlight,
        );
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
            emphasis: Vec::new(),
            kind: DiffRowKind::FileHeader,
            anchor: None,
        }];

        if file.status == crate::diff::FileStatus::Binary {
            rows.push(placeholder_row(
                "binary file: contents not rendered (mark viewed with v/enter)",
            ));
            return rows;
        }

        let diff_lines: usize = file.diff.hunks.iter().map(|hunk| hunk.lines.len()).sum();
        if diff_lines > self.max_diff_lines && !self.force_rendered.contains(&file.path) {
            rows.push(placeholder_row(&format!(
                "large diff hidden: {diff_lines} lines exceed the {} line threshold (press L to render)",
                self.max_diff_lines
            )));
            return rows;
        }

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
                emphasis: Vec::new(),
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
                emphasis: Vec::new(),
                kind: DiffRowKind::SyntaxSummary,
                anchor: None,
            });
        }

        // Map (hunk, line) back to its line offset within the concatenated
        // new-side source, for symbol-aware fold labels.
        let new_source_line: BTreeMap<(usize, usize), usize> = new_line_indices
            .iter()
            .enumerate()
            .map(|(source_line, key)| (*key, source_line))
            .collect();

        for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: hunk.header.clone(),
                syntax: Vec::new(),
                emphasis: Vec::new(),
                kind: DiffRowKind::HunkHeader,
                anchor: None,
            });
            let folds = if self.fold_context {
                context_folds(hunk)
            } else {
                BTreeMap::new()
            };
            let emphasis = if self.diff_cues.word_highlight {
                hunk_emphasis(hunk)
            } else {
                BTreeMap::new()
            };
            let mut line_index = 0;
            while line_index < hunk.lines.len() {
                if let Some(fold_end) = folds.get(&line_index).copied() {
                    let hidden = fold_end - line_index;
                    let middle = line_index + hidden / 2;
                    let symbol = new_source_line
                        .get(&(hunk_index, middle))
                        .and_then(|source_line| {
                            enclosing_symbol(&syntax_cache.symbol_spans, *source_line)
                        })
                        .map(|symbol| format!(" (in {} {})", symbol.kind, symbol.name))
                        .unwrap_or_default();
                    rows.push(DiffRow {
                        old_lineno: None,
                        new_lineno: None,
                        prefix: " ",
                        text: format!("⋯ {hidden} unchanged lines{symbol}"),
                        syntax: Vec::new(),
                        emphasis: Vec::new(),
                        kind: DiffRowKind::ContextFold,
                        anchor: None,
                    });
                    line_index = fold_end;
                    continue;
                }

                let line = &hunk.lines[line_index];
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
                    emphasis: emphasis.get(&line_index).cloned().unwrap_or_default(),
                    kind: DiffRowKind::DiffLine(line.kind),
                    anchor: self.line_anchor(file, hunk_index, line_index),
                });
                line_index += 1;
            }
        }

        if file.diff.hunks.is_empty() {
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text: file.diff.raw.clone(),
                syntax: Vec::new(),
                emphasis: Vec::new(),
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

fn placeholder_row(text: &str) -> DiffRow {
    DiffRow {
        old_lineno: None,
        new_lineno: None,
        prefix: " ",
        text: text.to_owned(),
        syntax: Vec::new(),
        emphasis: Vec::new(),
        kind: DiffRowKind::Placeholder,
        anchor: None,
    }
}

pub(super) fn nearest_commentable_row(rows: &[DiffRow], target: usize) -> Option<usize> {
    rows.iter()
        .enumerate()
        .filter(|(_, row)| row.anchor.is_some())
        .min_by_key(|(index, _)| index.abs_diff(target))
        .map(|(index, _)| index)
}

/// Fold ranges for one hunk: map from run start line index to run end
/// (exclusive). Only long runs of context lines fold, keeping
/// [`FOLD_KEEP_CONTEXT`] lines next to surrounding changes.
fn context_folds(hunk: &Hunk) -> BTreeMap<usize, usize> {
    let mut folds = BTreeMap::new();
    let mut run_start: Option<usize> = None;
    for index in 0..=hunk.lines.len() {
        let is_context = hunk
            .lines
            .get(index)
            .is_some_and(|line| line.kind == DiffLineKind::Context);
        match (run_start, is_context) {
            (None, true) => run_start = Some(index),
            (Some(start), false) => {
                let fold_start = start + FOLD_KEEP_CONTEXT;
                let fold_end = index.saturating_sub(FOLD_KEEP_CONTEXT);
                if fold_end > fold_start && fold_end - fold_start >= FOLD_MIN_HIDDEN {
                    folds.insert(fold_start, fold_end);
                }
                run_start = None;
            }
            _ => {}
        }
    }
    folds
}

/// Innermost symbol whose span contains `source_line`.
fn enclosing_symbol(symbols: &[SymbolSpan], source_line: usize) -> Option<&SymbolSpan> {
    symbols
        .iter()
        .filter(|symbol| source_line >= symbol.start_line && source_line <= symbol.end_line)
        .min_by_key(|symbol| symbol.end_line - symbol.start_line)
}
