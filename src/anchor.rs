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
}

impl CommentAnchor {
    pub fn path(&self) -> &str {
        match self {
            Self::File { path, .. } | Self::Line { path, .. } => path,
        }
    }

    pub fn line(&self) -> Option<usize> {
        match self {
            Self::File { .. } => None,
            Self::Line { line, .. } => Some(*line),
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
}
