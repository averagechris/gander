use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
}
