//! Word-level emphasis for modified line pairs: within each hunk, removed
//! runs pair with the added runs that follow them, and the changed tokens
//! of each pair are emphasized in the diff pane (docs/focused-diff-ux.md).

use std::{collections::BTreeMap, ops::Range};

use similar::{Algorithm, ChangeTag, TextDiff, utils::diff_unicode_words};

use crate::diff::{DiffLineKind, Hunk};

/// Pairs whose word-level similarity falls below this ratio render without
/// emphasis: highlighting most of a rewritten line is worse than nothing.
const MIN_SIMILARITY: f32 = 0.4;

/// Byte ranges to emphasize, keyed by line index within the hunk.
pub(super) type HunkEmphasis = BTreeMap<usize, Vec<Range<usize>>>;

/// Compute word-level emphasis ranges for one hunk.
///
/// Change blocks follow the shape the parser emits: a maximal run of
/// `Removed` lines optionally followed by a maximal run of `Added` lines.
/// Removed line *i* pairs with added line *i*; unpaired lines get no
/// emphasis.
pub(super) fn hunk_emphasis(hunk: &Hunk) -> HunkEmphasis {
    let mut emphasis = HunkEmphasis::new();
    let mut index = 0;
    while index < hunk.lines.len() {
        if hunk.lines[index].kind != DiffLineKind::Removed {
            index += 1;
            continue;
        }
        let removed_start = index;
        while index < hunk.lines.len() && hunk.lines[index].kind == DiffLineKind::Removed {
            index += 1;
        }
        let added_start = index;
        while index < hunk.lines.len() && hunk.lines[index].kind == DiffLineKind::Added {
            index += 1;
        }
        let pairs = (index - added_start).min(added_start - removed_start);
        for offset in 0..pairs {
            let removed_index = removed_start + offset;
            let added_index = added_start + offset;
            let (removed_ranges, added_ranges) = pair_emphasis(
                &hunk.lines[removed_index].text,
                &hunk.lines[added_index].text,
            );
            if !removed_ranges.is_empty() {
                emphasis.insert(removed_index, removed_ranges);
            }
            if !added_ranges.is_empty() {
                emphasis.insert(added_index, added_ranges);
            }
        }
    }
    emphasis
}

/// Byte ranges of changed words in `(old, new)`, or empty ranges when the
/// pair is too dissimilar to emphasize meaningfully.
fn pair_emphasis(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if old == new {
        return (Vec::new(), Vec::new());
    }
    let ratio = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_unicode_words(old, new)
        .ratio();
    if ratio < MIN_SIMILARITY {
        return (Vec::new(), Vec::new());
    }

    let changes = diff_unicode_words(Algorithm::Myers, old, new);
    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    let mut old_offset = 0;
    let mut new_offset = 0;
    for (tag, text) in changes {
        match tag {
            ChangeTag::Equal => {
                old_offset += text.len();
                new_offset += text.len();
            }
            ChangeTag::Delete => {
                push_range(&mut old_ranges, old_offset..old_offset + text.len());
                old_offset += text.len();
            }
            ChangeTag::Insert => {
                push_range(&mut new_ranges, new_offset..new_offset + text.len());
                new_offset += text.len();
            }
        }
    }
    (old_ranges, new_ranges)
}

/// Append a range, merging it into the previous one when adjacent.
fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if let Some(last) = ranges.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffLine;

    fn hunk(lines: Vec<(DiffLineKind, &str)>) -> Hunk {
        Hunk {
            old_start: 1,
            old_len: lines.len(),
            new_start: 1,
            new_len: lines.len(),
            header: "@@ test @@".to_owned(),
            lines: lines
                .into_iter()
                .enumerate()
                .map(|(index, (kind, text))| DiffLine {
                    kind,
                    text: text.to_owned(),
                    old_lineno: Some(index + 1),
                    new_lineno: Some(index + 1),
                })
                .collect(),
        }
    }

    #[test]
    fn emphasizes_changed_tokens_in_paired_lines() {
        let hunk = hunk(vec![
            (DiffLineKind::Context, "fn main() {"),
            (DiffLineKind::Removed, "    let count = 1;"),
            (DiffLineKind::Added, "    let count = 2;"),
            (DiffLineKind::Context, "}"),
        ]);

        let emphasis = hunk_emphasis(&hunk);

        let removed = &emphasis[&1];
        let added = &emphasis[&2];
        assert_eq!(&"    let count = 1;"[removed[0].clone()], "1");
        assert_eq!(&"    let count = 2;"[added[0].clone()], "2");
    }

    #[test]
    fn pairs_lines_by_offset_within_a_change_block() {
        let hunk = hunk(vec![
            (DiffLineKind::Removed, "alpha beta"),
            (DiffLineKind::Removed, "gamma delta"),
            (DiffLineKind::Added, "alpha BETA"),
            (DiffLineKind::Added, "gamma DELTA"),
        ]);

        let emphasis = hunk_emphasis(&hunk);

        assert_eq!(&"alpha beta"[emphasis[&0][0].clone()], "beta");
        assert_eq!(&"gamma delta"[emphasis[&1][0].clone()], "delta");
        assert_eq!(&"alpha BETA"[emphasis[&2][0].clone()], "BETA");
        assert_eq!(&"gamma DELTA"[emphasis[&3][0].clone()], "DELTA");
    }

    #[test]
    fn dissimilar_pairs_get_no_emphasis() {
        let hunk = hunk(vec![
            (DiffLineKind::Removed, "use std::collections::BTreeMap;"),
            (DiffLineKind::Added, "let widget = Widget::render(area);"),
        ]);

        assert!(hunk_emphasis(&hunk).is_empty());
    }

    #[test]
    fn unpaired_additions_get_no_emphasis() {
        let hunk = hunk(vec![
            (DiffLineKind::Added, "let alpha = 1;"),
            (DiffLineKind::Added, "let beta = 2;"),
        ]);

        assert!(hunk_emphasis(&hunk).is_empty());
    }

    #[test]
    fn adjacent_changed_words_merge_into_one_range() {
        let hunk = hunk(vec![
            (DiffLineKind::Removed, "value = old_name(alpha)"),
            (DiffLineKind::Added, "value = new_label(alpha)"),
        ]);

        let emphasis = hunk_emphasis(&hunk);

        assert_eq!(emphasis[&0].len(), 1);
        assert_eq!(
            &"value = old_name(alpha)"[emphasis[&0][0].clone()],
            "old_name"
        );
        assert_eq!(
            &"value = new_label(alpha)"[emphasis[&1][0].clone()],
            "new_label"
        );
    }

    #[test]
    fn multibyte_text_produces_valid_char_boundaries() {
        let old = "greeting = \"héllo wörld\";";
        let new = "greeting = \"héllo mönde\";";
        let hunk = hunk(vec![
            (DiffLineKind::Removed, old),
            (DiffLineKind::Added, new),
        ]);

        let emphasis = hunk_emphasis(&hunk);

        for (line, text) in [(0usize, old), (1usize, new)] {
            for range in emphasis.get(&line).into_iter().flatten() {
                assert!(text.is_char_boundary(range.start));
                assert!(text.is_char_boundary(range.end));
            }
        }
    }
}
