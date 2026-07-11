use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    app::ReviewFile,
    diff::{DiffLineKind, FileDiff},
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffSide {
    Old,
    New,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommentAnchor {
    File {
        path: String,
        old_path: Option<String>,
        diff_fingerprint: String,
    },
    Line {
        path: String,
        old_path: Option<String>,
        side: DiffSide,
        line: usize,
        old_line: Option<usize>,
        new_line: Option<usize>,
        hunk_header: String,
        hunk_old_start: usize,
        hunk_old_len: usize,
        hunk_new_start: usize,
        hunk_new_len: usize,
        hunk_index: usize,
        line_index: usize,
        line_kind: String,
        line_text: String,
        line_fingerprint: String,
        diff_fingerprint: String,
    },
    Range {
        path: String,
        old_path: Option<String>,
        start_line: usize,
        end_line: usize,
        start_row_index: usize,
        end_row_index: usize,
        lines: Vec<RangeLineAnchor>,
        diff_fingerprint: String,
        range_fingerprint: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RangeLineAnchor {
    pub side: DiffSide,
    pub line: usize,
    pub old_line: Option<usize>,
    pub new_line: Option<usize>,
    pub hunk_header: String,
    pub hunk_index: usize,
    pub line_index: usize,
    pub row_index: usize,
    pub line_kind: String,
    pub line_text: String,
    pub line_fingerprint: String,
}

impl CommentAnchor {
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Line { path, .. } | Self::Range { path, .. } => path,
        }
    }

    pub fn line(&self) -> Option<usize> {
        match self {
            Self::File { .. } => None,
            Self::Line { line, .. } => Some(*line),
            Self::Range { start_line, .. } => Some(*start_line),
        }
    }

    pub fn end_line(&self) -> Option<usize> {
        match self {
            Self::File { .. } => None,
            Self::Line { line, .. } => Some(*line),
            Self::Range { end_line, .. } => Some(*end_line),
        }
    }
}

impl DiffSide {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}

pub fn diff_line_kind_label(kind: DiffLineKind) -> &'static str {
    match kind {
        DiffLineKind::Context => "context",
        DiffLineKind::Added => "added",
        DiffLineKind::Removed => "removed",
        DiffLineKind::Meta => "meta",
    }
}

pub fn line_anchor_for_diff_row(
    file: &ReviewFile,
    hunk_index: usize,
    line_index: usize,
) -> Option<CommentAnchor> {
    line_anchor_for_file_diff(&file.diff, hunk_index, line_index)
}

pub fn line_anchor_for_file_diff(
    file: &FileDiff,
    hunk_index: usize,
    line_index: usize,
) -> Option<CommentAnchor> {
    let hunk = file.hunks.get(hunk_index)?;
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
        line_kind: diff_line_kind_label(line.kind).to_owned(),
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

pub fn comment_anchor_for_file_lines(
    file: &ReviewFile,
    line: Option<usize>,
    end_line: Option<usize>,
) -> Option<CommentAnchor> {
    comment_anchor_for_file_diff(&file.diff, line, end_line)
}

pub fn comment_anchor_for_file_diff(
    file: &FileDiff,
    line: Option<usize>,
    end_line: Option<usize>,
) -> Option<CommentAnchor> {
    let Some(line) = line else {
        return Some(CommentAnchor::File {
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            diff_fingerprint: file.fingerprint.clone(),
        });
    };
    let end_line = end_line.unwrap_or(line);
    let anchors = anchors_in_line_range(file, line, end_line);
    match anchors.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        _ => range_anchor_from_lines(file, anchors),
    }
}

fn anchors_in_line_range(file: &FileDiff, start: usize, end: usize) -> Vec<CommentAnchor> {
    let (start, end) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let mut anchors = Vec::new();
    for wanted in start..=end {
        if let Some(anchor) = find_line_anchor(file, wanted, true) {
            anchors.push(anchor);
        } else if let Some(anchor) = find_line_anchor(file, wanted, false) {
            anchors.push(anchor);
        }
    }
    anchors
}

fn find_line_anchor(file: &FileDiff, wanted: usize, new_side: bool) -> Option<CommentAnchor> {
    for (hunk_index, hunk) in file.hunks.iter().enumerate() {
        for (line_index, line) in hunk.lines.iter().enumerate() {
            let matches = if new_side {
                line.new_lineno == Some(wanted)
            } else {
                line.old_lineno == Some(wanted)
            };
            if matches {
                return line_anchor_for_file_diff(file, hunk_index, line_index);
            }
        }
    }
    None
}

pub(crate) fn line_anchor_for_side_line(
    file: &ReviewFile,
    side: DiffSide,
    wanted: usize,
) -> Option<CommentAnchor> {
    let mut anchor = find_line_anchor(&file.diff, wanted, matches!(side, DiffSide::New))?;
    if let CommentAnchor::Line {
        side: anchor_side,
        line,
        line_text,
        line_fingerprint,
        ..
    } = &mut anchor
    {
        *anchor_side = side;
        *line = wanted;
        *line_fingerprint =
            fingerprint_line(&file.path, side, wanted, line_text, &file.fingerprint);
    }
    Some(anchor)
}

pub(crate) fn comment_anchor_for_sided_lines(
    file: &ReviewFile,
    lines: &[(DiffSide, usize)],
) -> Option<CommentAnchor> {
    let anchors = lines
        .iter()
        .filter_map(|(side, line)| line_anchor_for_side_line(file, *side, *line))
        .collect::<Vec<_>>();
    match anchors.as_slice() {
        [] => None,
        [single] => Some(single.clone()),
        _ => range_anchor_from_lines(&file.diff, anchors),
    }
}

fn range_anchor_from_lines(file: &FileDiff, anchors: Vec<CommentAnchor>) -> Option<CommentAnchor> {
    let mut range_lines = Vec::new();
    for (row_index, anchor) in anchors.into_iter().enumerate() {
        if let CommentAnchor::Line {
            side,
            line,
            old_line,
            new_line,
            hunk_header,
            hunk_index,
            line_index,
            line_kind,
            line_text,
            line_fingerprint,
            ..
        } = anchor
        {
            range_lines.push(RangeLineAnchor {
                side,
                line,
                old_line,
                new_line,
                hunk_header,
                hunk_index,
                line_index,
                row_index,
                line_kind,
                line_text,
                line_fingerprint,
            });
        }
    }
    let line_fingerprints: Vec<_> = range_lines
        .iter()
        .map(|line| line.line_fingerprint.clone())
        .collect();
    let start_line = range_lines.first()?.line;
    let end_line = range_lines.last()?.line;
    let start_row_index = range_lines.first()?.row_index;
    let end_row_index = range_lines.last()?.row_index;
    Some(CommentAnchor::Range {
        path: file.path.clone(),
        old_path: file.old_path.clone(),
        start_line,
        end_line,
        start_row_index,
        end_row_index,
        lines: range_lines,
        diff_fingerprint: file.fingerprint.clone(),
        range_fingerprint: fingerprint_range(&file.path, &file.fingerprint, &line_fingerprints),
    })
}

pub fn fingerprint_line(
    path: &str,
    side: DiffSide,
    line: usize,
    text: &str,
    diff_fingerprint: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hasher.update([0]);
    hasher.update(side.label().as_bytes());
    hasher.update([0]);
    hasher.update(line.to_string().as_bytes());
    hasher.update([0]);
    hasher.update(text.as_bytes());
    hasher.update([0]);
    hasher.update(diff_fingerprint.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn fingerprint_range(
    path: &str,
    diff_fingerprint: &str,
    line_fingerprints: &[String],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hasher.update([0]);
    hasher.update(diff_fingerprint.as_bytes());
    for fingerprint in line_fingerprints {
        hasher.update([0]);
        hasher.update(fingerprint.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::ReviewSession, diff::DiffSet, jj::ReviewTarget, state::ReviewState};
    use std::path::PathBuf;

    #[test]
    fn line_fingerprint_changes_when_text_changes() {
        let left = fingerprint_line("src/main.rs", DiffSide::New, 10, "left", "diff");
        let right = fingerprint_line("src/main.rs", DiffSide::New, 10, "right", "diff");

        assert_ne!(left, right);
    }

    #[test]
    fn exposes_line_anchor_path_and_line() {
        let anchor = CommentAnchor::Line {
            path: "src/main.rs".to_owned(),
            old_path: None,
            side: DiffSide::New,
            line: 10,
            old_line: None,
            new_line: Some(10),
            hunk_header: "@@ -1 +1 @@".to_owned(),
            hunk_old_start: 1,
            hunk_old_len: 1,
            hunk_new_start: 1,
            hunk_new_len: 1,
            hunk_index: 0,
            line_index: 0,
            line_kind: "added".to_owned(),
            line_text: "new".to_owned(),
            line_fingerprint: "line".to_owned(),
            diff_fingerprint: "diff".to_owned(),
        };

        assert_eq!(anchor.path(), "src/main.rs");
        assert_eq!(anchor.line(), Some(10));
    }

    #[test]
    fn range_anchor_exposes_path_and_lines() {
        let anchor = CommentAnchor::Range {
            path: "src/main.rs".to_owned(),
            old_path: None,
            start_line: 10,
            end_line: 12,
            start_row_index: 3,
            end_row_index: 5,
            lines: Vec::new(),
            diff_fingerprint: "diff".to_owned(),
            range_fingerprint: "range".to_owned(),
        };

        assert_eq!(anchor.path(), "src/main.rs");
        assert_eq!(anchor.line(), Some(10));
        assert_eq!(anchor.end_line(), Some(12));
    }

    #[test]
    fn range_fingerprint_changes_when_line_set_changes() {
        let left = fingerprint_range("a.rs", "diff", &["one".to_owned()]);
        let right = fingerprint_range("a.rs", "diff", &["one".to_owned(), "two".to_owned()]);

        assert_ne!(left, right);
    }

    #[test]
    fn shared_line_anchor_matches_diff_row_anchor_shape() {
        let file = sample_file();
        let direct = line_anchor_for_diff_row(&file, 0, 2).unwrap();
        let derived = comment_anchor_for_file_lines(&file, Some(2), None).unwrap();

        assert_eq!(derived, direct);
    }

    #[test]
    fn derives_removed_line_anchor_from_old_side_when_new_line_absent() {
        let file = removed_line_file();
        let anchor = comment_anchor_for_file_lines(&file, Some(2), None).unwrap();

        assert!(matches!(
            anchor,
            CommentAnchor::Line {
                side: DiffSide::Old,
                line_kind,
                ..
            } if line_kind == "removed"
        ));
    }

    #[test]
    fn requested_old_side_is_preserved_for_context_line() {
        let file = sample_file();
        let anchor = line_anchor_for_side_line(&file, DiffSide::Old, 1).unwrap();
        assert!(matches!(
            anchor,
            CommentAnchor::Line {
                side: DiffSide::Old,
                line: 1,
                old_line: Some(1),
                new_line: Some(1),
                ..
            }
        ));
    }

    fn sample_file() -> ReviewFile {
        ReviewSession::new(
            PathBuf::from("/repo"),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,3 @@\n fn main() {\n-    old();\n+    new();\n+    extra();\n }",
            )
            .unwrap(),
            ReviewState::default(),
        )
        .files
        .remove(0)
    }

    fn removed_line_file() -> ReviewFile {
        ReviewSession::new(
            PathBuf::from("/repo"),
            ReviewTarget::trunk_to_current(),
            DiffSet::parse(
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1 @@\n keep\n-remove",
            )
            .unwrap(),
            ReviewState::default(),
        )
        .files
        .remove(0)
    }
}
