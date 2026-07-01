use std::{collections::BTreeMap, fs, path::Path};

use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::anchor::CommentAnchor;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewState {
    pub files: BTreeMap<String, FileState>,
    pub comments: Vec<Comment>,
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
    pub created_at: chrono::DateTime<chrono::Utc>,
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
        fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
