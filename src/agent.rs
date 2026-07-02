//! Shared state for agent collaboration: the "agent overlay".
//!
//! Agents connected over ACP (see [`crate::acp`]) write suggested review
//! ordering, flagged sections, review chunks, and draft comments into a
//! plain JSON overlay file (`.gander/agent.json` by default). The TUI loads
//! and polls this file, surfaces the suggestions, and writes back draft
//! dispositions so the collaboration is two-way while both processes stay
//! independent.

use std::{fs, path::Path, path::PathBuf};

use color_eyre::eyre::{Context, Result};
use serde::{Deserialize, Serialize};

pub const AGENT_OVERLAY_VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentOverlay {
    pub version: u32,
    /// Suggested review order: file paths, highest priority first. Files not
    /// listed keep their natural order after the listed ones.
    pub ordering: Vec<String>,
    /// Sections the agent flagged as critical.
    pub flags: Vec<AgentFlag>,
    /// Reviewable units that can span or subdivide files.
    pub chunks: Vec<ReviewChunk>,
    /// Agent-drafted comments awaiting human review.
    pub drafts: Vec<AgentDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFlag {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub reason: String,
    #[serde(default)]
    pub priority: FlagPriority,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FlagPriority {
    Critical,
    #[default]
    High,
    Medium,
    Low,
}

impl FlagPriority {
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewChunk {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default)]
    pub parts: Vec<ChunkPart>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkPart {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDraft {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub body: String,
    #[serde(default)]
    pub state: DraftState,
    /// Set when a human accepts the draft: the id of the created comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_comment_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DraftState {
    #[default]
    Pending,
    Accepted,
    Discarded,
}

impl AgentOverlay {
    pub fn default_path(repo: &Path) -> PathBuf {
        repo.join(".gander").join("agent.json")
    }

    pub fn load_or_default(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                version: AGENT_OVERLAY_VERSION,
                ..Self::default()
            });
        }
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read agent overlay {}", path.display()))?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse agent overlay {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Atomic-rename write, same as review state, so a crash cannot
        // corrupt the overlay both processes share.
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
    fn overlay_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".gander").join("agent.json");
        let overlay = AgentOverlay {
            version: AGENT_OVERLAY_VERSION,
            ordering: vec!["src/risky.rs".to_owned(), "src/safe.rs".to_owned()],
            flags: vec![AgentFlag {
                id: "flag-1".to_owned(),
                path: "src/risky.rs".to_owned(),
                line: Some(42),
                reason: "unchecked unwrap on user input".to_owned(),
                priority: FlagPriority::Critical,
            }],
            chunks: vec![ReviewChunk {
                id: "chunk-1".to_owned(),
                title: "auth flow".to_owned(),
                rationale: Some("spans handler and middleware".to_owned()),
                parts: vec![ChunkPart {
                    path: "src/risky.rs".to_owned(),
                    start_line: Some(10),
                    end_line: Some(60),
                }],
            }],
            drafts: vec![AgentDraft {
                id: "draft-1".to_owned(),
                path: "src/risky.rs".to_owned(),
                line: Some(42),
                body: "consider handling the None case".to_owned(),
                state: DraftState::Pending,
                accepted_comment_id: None,
            }],
        };

        overlay.save(&path).unwrap();
        let loaded = AgentOverlay::load_or_default(&path).unwrap();

        assert_eq!(loaded, overlay);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn missing_overlay_defaults_to_current_version() {
        let dir = tempfile::tempdir().unwrap();

        let overlay =
            AgentOverlay::load_or_default(&AgentOverlay::default_path(dir.path())).unwrap();

        assert_eq!(overlay.version, AGENT_OVERLAY_VERSION);
        assert!(overlay.ordering.is_empty());
        assert!(overlay.flags.is_empty());
    }

    #[test]
    fn flag_priorities_order_critical_first() {
        assert!(FlagPriority::Critical < FlagPriority::High);
        assert!(FlagPriority::High < FlagPriority::Medium);
        assert!(FlagPriority::Medium < FlagPriority::Low);
    }
}
