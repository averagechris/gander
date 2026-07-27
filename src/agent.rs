//! Shared state for agent collaboration: the "agent overlay".
//!
//! Agents connected over ACP (see [`crate::acp`]) write suggested review
//! ordering and flagged sections into a
//! plain JSON overlay file (`agent.json` in the per-workspace state dir,
//! see [`crate::paths`]). The TUI loads and polls this file, surfaces the
//! suggestions while both processes stay independent. Pre-M17 overlay drafts
//! are retained only as a one-release, read-only migration input. Unknown
//! legacy chunk and brief fields are intentionally ignored and disappear on
//! the next save; durable walkthroughs and attention regions replaced them.

use std::{fs, path::Path};

use color_eyre::eyre::{Context, Result};
use serde::{Deserialize, Serialize};

pub const AGENT_OVERLAY_VERSION: u32 = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentOverlay {
    pub version: u32,
    /// Suggested review order: file paths, highest priority first. Files not
    /// listed keep their natural order after the listed ones.
    pub ordering: Vec<String>,
    /// Sections the agent flagged as critical.
    pub flags: Vec<AgentFlag>,
    /// One-release compatibility input. Never serialized: pending entries are
    /// folded into durable comments and accepted/discarded history is consumed
    /// without being recreated.
    #[serde(skip)]
    pub(crate) legacy_drafts: Vec<LegacyAgentDraft>,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LegacyAgentDraft {
    pub id: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub body: String,
    #[serde(default)]
    pub state: LegacyDraftState,
    /// Set when a human accepts the draft: the id of the created comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_comment_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LegacyDraftState {
    #[default]
    Pending,
    Accepted,
    Discarded,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CompatibleAgentOverlay {
    version: u32,
    ordering: Vec<String>,
    flags: Vec<AgentFlag>,
    drafts: Vec<LegacyAgentDraft>,
}

impl<'de> Deserialize<'de> for AgentOverlay {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let compatible = CompatibleAgentOverlay::deserialize(deserializer)?;
        Ok(Self {
            version: compatible.version,
            ordering: compatible.ordering,
            flags: compatible.flags,
            legacy_drafts: compatible.drafts,
        })
    }
}

impl AgentOverlay {
    pub(crate) fn has_legacy_drafts(&self) -> bool {
        !self.legacy_drafts.is_empty()
    }

    pub(crate) fn clear_legacy_drafts(&mut self) {
        self.legacy_drafts.clear();
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
        crate::state::ensure_supported_schema(
            &contents,
            path,
            u64::from(AGENT_OVERLAY_VERSION),
            "agent overlay",
        )?;
        serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse agent overlay {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let _lock = crate::state::ReviewStateFileLock::acquire(path)?;
        crate::state::ensure_existing_schema_supported(
            path,
            u64::from(AGENT_OVERLAY_VERSION),
            "agent overlay",
        )?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Atomic-rename write, same as review state, so a crash cannot
        // corrupt the overlay both processes share.
        let mut tmp = path.to_path_buf();
        tmp.set_extension("json.tmp");
        let mut persisted = self.clone();
        persisted.version = AGENT_OVERLAY_VERSION;
        crate::state::atomic_replace(
            path,
            &tmp,
            serde_json::to_string_pretty(&persisted)?.as_bytes(),
        )
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
            legacy_drafts: Vec::new(),
        };

        overlay.save(&path).unwrap();
        let loaded = AgentOverlay::load_or_default(&path).unwrap();

        assert_eq!(loaded, overlay);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn legacy_drafts_deserialize_but_are_not_serialized() {
        let overlay: AgentOverlay = serde_json::from_str(
            r#"{"version":1,"drafts":[{"id":"pending","path":"src/lib.rs","line":4,"body":"check","state":"pending"},{"id":"discarded","path":"src/lib.rs","body":"old","state":"discarded"}]}"#,
        )
        .unwrap();

        assert_eq!(overlay.legacy_drafts.len(), 2);
        assert_eq!(overlay.legacy_drafts[0].state, LegacyDraftState::Pending);
        let json = serde_json::to_string(&overlay).unwrap();
        assert!(!json.contains("drafts"));
    }

    #[test]
    fn legacy_chunks_and_briefs_are_discarded_without_touching_overlay_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        fs::write(
            &path,
            r#"{
  "version": 2,
  "ordering": ["src/risky.rs"],
  "flags": [{"id":"flag-1","path":"src/risky.rs","reason":"review me","priority":"high"}],
  "chunks": [{"id":"old","title":"legacy","parts":[{"path":"src/risky.rs"}]}],
  "briefs": [{"change_id":"abc","summary":"legacy chapter"}]
}"#,
        )
        .unwrap();

        let overlay = AgentOverlay::load_or_default(&path).unwrap();
        assert_eq!(overlay.ordering, ["src/risky.rs"]);
        assert_eq!(overlay.flags.len(), 1);
        overlay.save(&path).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("chunks"));
        assert!(!saved.contains("briefs"));
        let reloaded = AgentOverlay::load_or_default(&path).unwrap();
        assert_eq!(reloaded.ordering, ["src/risky.rs"]);
        assert_eq!(reloaded.flags[0].id, "flag-1");
        assert_eq!(reloaded.version, AGENT_OVERLAY_VERSION);
    }

    #[test]
    fn missing_overlay_defaults_to_current_version() {
        let dir = tempfile::tempdir().unwrap();

        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();

        assert_eq!(overlay.version, AGENT_OVERLAY_VERSION);
        assert!(overlay.ordering.is_empty());
        assert!(overlay.flags.is_empty());
    }

    #[test]
    fn future_overlay_schema_is_rejected_and_never_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.json");
        let future = br#"{"version":9001,"future":{"unknown":"preserve exactly"}}
"#;
        fs::write(&path, future).unwrap();

        assert!(
            AgentOverlay::load_or_default(&path)
                .unwrap_err()
                .to_string()
                .contains("schema version 9001")
        );
        assert_eq!(fs::read(&path).unwrap(), future);
        assert!(AgentOverlay::default().save(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), future);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn flag_priorities_order_critical_first() {
        assert!(FlagPriority::Critical < FlagPriority::High);
        assert!(FlagPriority::High < FlagPriority::Medium);
        assert!(FlagPriority::Medium < FlagPriority::Low);
    }
}
