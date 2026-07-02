use std::{collections::BTreeMap, fs, path::Path};

use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::anchor::CommentAnchor;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewState {
    pub meta: ReviewStateMeta,
    pub files: BTreeMap<String, FileState>,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReviewStateMeta {
    pub version: u8,
    pub base: Option<String>,
    pub revision: Option<String>,
    pub repo: Option<String>,
    pub saved_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileState {
    pub fingerprint: String,
    pub viewed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchor>,
    pub body: String,
    #[serde(default)]
    pub state: CommentState,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Review lifecycle state of a comment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentState {
    #[default]
    Draft,
    Todo,
    Resolved,
}

impl CommentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Todo => "todo",
            Self::Resolved => "resolved",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Draft => Self::Todo,
            Self::Todo => Self::Resolved,
            Self::Resolved => Self::Draft,
        }
    }
}

impl ReviewState {
    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&contents)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write to a sibling temp file and rename so an interrupted save
        // cannot truncate or corrupt existing review state.
        let mut tmp = path.to_path_buf();
        tmp.set_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_round_trips_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("state.json");
        let mut state = ReviewState::default();
        state.files.insert(
            "src/main.rs".to_owned(),
            FileState {
                fingerprint: "abc".to_owned(),
                viewed: true,
            },
        );

        state.save(&path).unwrap();

        let loaded = ReviewState::load_or_default(&path).unwrap();
        assert!(loaded.files["src/main.rs"].viewed);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn loads_comment_without_anchor() {
        let state: ReviewState = serde_json::from_str(
            r#"{
  "files": {},
  "comments": [
    {
      "id": "1",
      "path": "src/main.rs",
      "line": null,
      "body": "legacy",
      "created_at": "2026-06-30T00:00:00Z"
    }
  ]
}"#,
        )
        .unwrap();

        assert_eq!(state.comments[0].path, "src/main.rs");
        assert!(state.comments[0].anchor.is_none());
        assert_eq!(state.meta.version, 0);
    }
}
