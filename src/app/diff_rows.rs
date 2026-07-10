//! Construction of the flattened diff rows rendered in the diff pane,
//! including per-line comment anchors and symbol-aware context folding.

use std::{collections::BTreeMap, ops::Range, rc::Rc};

use crate::{
    anchor::{CommentAnchor, line_anchor_for_diff_row},
    diff::{DiffLineKind, Hunk},
    syntax::{SymbolSpan, SyntaxSpan},
};

use super::{
    ReviewFile, ReviewSession,
    context_expansion::{GapSpec, file_gaps, gap_view},
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
    pub hunk_index: Option<usize>,
    pub anchor: Option<CommentAnchor>,
    /// The context-expansion gap this row belongs to: set on
    /// [`DiffRowKind::ExpandGap`] rows and on the synthetic context rows an
    /// expanded gap reveals, so `+`/`=`/`-` can find the gap nearest the
    /// cursor even when the gap row itself has disappeared.
    pub gap: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffRowKind {
    FileHeader,
    SyntaxSummary,
    HunkHeader,
    DiffLine(DiffLineKind),
    ContextFold,
    /// Hidden file lines between/around hunks that can expand via
    /// `+`/`=`/`-` (docs/focused-diff-ux.md §5). `hidden` is the count
    /// still collapsed.
    ExpandGap {
        gap_id: usize,
        hidden: usize,
    },
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
            self.expansion_epoch,
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
            hunk_index: None,
            anchor: None,
            gap: None,
        }];

        if file.diff.is_binary() {
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
                hunk_index: None,
                anchor: None,
                gap: None,
            });
        }

        // Map (hunk, line) back to its line offset within the concatenated
        // new-side source, for symbol-aware fold labels.
        let new_source_line: BTreeMap<(usize, usize), usize> = new_line_indices
            .iter()
            .enumerate()
            .map(|(source_line, key)| (*key, source_line))
            .collect();

        // Context-expansion gaps: hidden file lines above the first hunk,
        // between hunks, and (once the full contents are known) below the
        // last hunk (docs/focused-diff-ux.md §5).
        let file_lines = self.cached_file_lines(&file.path);
        let gaps = file_gaps(
            &file.diff.hunks,
            file_lines.as_ref().map(|lines| lines.len()),
        );
        let gap_spec = |gap_id: usize| gaps.iter().find(|gap| gap.gap_id == gap_id).copied();

        for (hunk_index, hunk) in file.diff.hunks.iter().enumerate() {
            let mut skip_header = false;
            if let Some(spec) = gap_spec(hunk_index) {
                let closed = self.push_gap_rows(&mut rows, file, &spec, file_lines.as_deref());
                // A fully expanded interior gap merges the two hunks into
                // one contiguous block: drop the interior header. Hunks and
                // anchors are untouched.
                skip_header = closed && hunk_index > 0;
            }
            if !skip_header {
                rows.push(DiffRow {
                    old_lineno: None,
                    new_lineno: None,
                    prefix: " ",
                    text: hunk.header.clone(),
                    syntax: Vec::new(),
                    emphasis: Vec::new(),
                    kind: DiffRowKind::HunkHeader,
                    hunk_index: Some(hunk_index),
                    anchor: None,
                    gap: None,
                });
            }
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
                        hunk_index: Some(hunk_index),
                        anchor: None,
                        gap: None,
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
                    hunk_index: Some(hunk_index),
                    anchor: self.line_anchor(file, hunk_index, line_index),
                    gap: None,
                });
                line_index += 1;
            }
        }

        if let Some(spec) = gap_spec(file.diff.hunks.len()) {
            self.push_gap_rows(&mut rows, file, &spec, file_lines.as_deref());
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
                hunk_index: None,
                anchor: None,
                gap: None,
            });
        }

        rows
    }

    /// Rows for one context-expansion gap: revealed lines at the top edge,
    /// the gap row itself while lines stay hidden, then revealed lines at
    /// the bottom edge. Returns whether the gap is fully expanded (closed).
    fn push_gap_rows(
        &self,
        rows: &mut Vec<DiffRow>,
        file: &ReviewFile,
        spec: &GapSpec,
        lines: Option<&Vec<String>>,
    ) -> bool {
        let expansion = lines.and_then(|_| {
            self.context_expansion
                .get(&(file.path.clone(), spec.gap_id))
                .copied()
        });
        let view = gap_view(spec, expansion);
        if let Some(lines) = lines {
            for new_lineno in view.top.clone() {
                rows.push(expanded_context_row(lines, new_lineno, spec));
            }
        }
        if view.hidden > 0 {
            let text = if self.file_contents_fetchable(&file.path) {
                format!(
                    "⋯ {} lines hidden  (+ expand {}, = expand all)",
                    view.hidden, self.diff_cues.context_step
                )
            } else {
                // The file failed to load (deleted on this side, binary):
                // the gap stays visible but does not offer expansion.
                format!("⋯ {} lines hidden", view.hidden)
            };
            rows.push(DiffRow {
                old_lineno: None,
                new_lineno: None,
                prefix: " ",
                text,
                syntax: Vec::new(),
                emphasis: Vec::new(),
                kind: DiffRowKind::ExpandGap {
                    gap_id: spec.gap_id,
                    hidden: view.hidden,
                },
                hunk_index: None,
                anchor: None,
                gap: Some(spec.gap_id),
            });
        }
        if let Some(lines) = lines {
            for new_lineno in view.bottom.clone() {
                rows.push(expanded_context_row(lines, new_lineno, spec));
            }
        }
        view.hidden == 0
    }

    fn line_anchor(
        &self,
        file: &ReviewFile,
        hunk_index: usize,
        line_index: usize,
    ) -> Option<CommentAnchor> {
        line_anchor_for_diff_row(file, hunk_index, line_index)
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
        hunk_index: None,
        anchor: None,
        gap: None,
    }
}

/// A synthetic context row revealed by gap expansion: real old/new line
/// numbers (old derives from the gap's hunk offset) but no comment anchor —
/// expanded context is not commentable in v1 (docs/focused-diff-ux.md §5).
fn expanded_context_row(lines: &[String], new_lineno: usize, spec: &GapSpec) -> DiffRow {
    let old_lineno = new_lineno as isize - spec.offset;
    DiffRow {
        old_lineno: (old_lineno > 0).then_some(old_lineno as usize),
        new_lineno: Some(new_lineno),
        prefix: " ",
        text: lines.get(new_lineno - 1).cloned().unwrap_or_default(),
        syntax: Vec::new(),
        emphasis: Vec::new(),
        kind: DiffRowKind::DiffLine(DiffLineKind::Context),
        hunk_index: None,
        anchor: None,
        gap: Some(spec.gap_id),
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
