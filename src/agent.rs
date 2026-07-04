//! Shared state for agent collaboration: the "agent overlay".
//!
//! Agents connected over ACP (see [`crate::acp`]) write suggested review
//! ordering, flagged sections, review chunks, and draft comments into a
//! plain JSON overlay file (`agent.json` in the per-workspace state dir,
//! see [`crate::paths`]). The TUI loads and polls this file, surfaces the
//! suggestions, and writes back draft dispositions so the collaboration is
//! two-way while both processes stay independent.

use std::{fs, path::Path};

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
    /// How aggressively the UI should feature this chunk. Spotlight chunks
    /// become zen walkthrough stops; glance chunks stay in the overview rail
    /// so routine hunks can be acknowledged without being toured one-by-one.
    #[serde(default)]
    pub importance: ChunkImportance,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    /// The teaching text for a spotlight stop: a few sentences that explain
    /// what the code does, why the change exists, and what could break.
    /// Rendered prominently on the zen focus card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    #[serde(default)]
    pub parts: Vec<ChunkPart>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChunkImportance {
    /// Worth pausing on in the focused walkthrough.
    #[default]
    Spotlight,
    /// Useful context, but not worth a dedicated tour stop.
    Glance,
}

impl ChunkImportance {
    pub fn label(self) -> &'static str {
        match self {
            Self::Spotlight => "spotlight",
            Self::Glance => "glance",
        }
    }
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

impl DraftState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Discarded => "discarded",
        }
    }
}

impl AgentOverlay {
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

/// A review agent summoned by the TUI. Agent-agnostic: any CLI that takes a
/// prompt works (`opencode run`, `claude -p`, ...). The child is killed when
/// this handle drops so quitting the TUI does not leak agents.
#[derive(Debug)]
pub struct AgentProcess {
    child: std::process::Child,
    exited: bool,
}

impl AgentProcess {
    /// Spawn `command` through the shell with `prompt` appended as a final
    /// shell-quoted argument (or substituted for a `{prompt}` placeholder).
    /// Output goes to `log_path`; stdio stays free for the TUI.
    pub fn spawn(repo: &Path, command: &str, prompt: &str, log_path: &Path) -> Result<Self> {
        let quoted = shell_quote(prompt);
        let command_line = if command.contains("{prompt}") {
            command.replace("{prompt}", &quoted)
        } else {
            format!("{command} {quoted}")
        };
        if let Some(parent) = log_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let log = fs::File::create(log_path)
            .with_context(|| format!("failed to create agent log {}", log_path.display()))?;
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command_line)
            .current_dir(repo)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .with_context(|| format!("failed to spawn agent command `{command}`"))?;
        Ok(Self {
            child,
            exited: false,
        })
    }

    /// `None` while the agent is still running, otherwise its exit status.
    pub fn try_status(&mut self) -> Option<std::process::ExitStatus> {
        let status = self.child.try_wait().ok().flatten();
        if status.is_some() {
            self.exited = true;
        }
        status
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Build the prompt handed to a summoned agent. A custom template may use
/// `{repo}`, `{base}`, and `{rev}`; the default explains the ACP workflow.
pub fn review_prompt(template: Option<&str>, repo: &Path, base: &str, rev: &str) -> String {
    let repo = repo.display().to_string();
    match template {
        Some(template) => template
            .replace("{repo}", &repo)
            .replace("{base}", base)
            .replace("{rev}", rev),
        None => format!(
            "You are assisting a human who is reviewing a code change in the gander TUI. \
             Repository: {repo}. Review target (jj revsets): {base}..{rev}.\n\
             Interact by running `gander acp` from the repository root and speaking \
             line-delimited JSON-RPC 2.0 over its stdio, one JSON object per line \
             (a running TUI is served live through it automatically).\n\
             1. Call initialize, then review/files, then review/file_diff \
             (params: {{\"path\": ...}}) for each file that matters.\n\
             2. Suggest a review order with review/set_ordering \
             (params: {{\"paths\": [...]}}), riskiest or most central files first.\n\
             3. Flag sections needing extra scrutiny with review/flag_section \
             (params: {{\"path\", \"line\", \"reason\", \"priority\": \
             critical|high|medium|low}}).\n\
             4. Curate a focused walkthrough with review/set_chunks. The human \
             sees spotlight chunks as full-screen stops (a code excerpt plus \
             your explanation) and skims everything else on a glance board, \
             so budget their attention: pick at most 3-7 chunks with \
             importance=spotlight — only the places a reviewer must actually \
             understand (new invariants, tricky logic, security-sensitive \
             paths, the heart of the change). Each spotlight needs a title, \
             precise start_line/end_line parts covering only the critical \
             lines (not whole files), and an `explanation` of 2-5 sentences \
             that teaches the change: what the code does, why it changed, and \
             what could break. Group ALL remaining hunks into a few \
             importance=glance chunks (mechanical renames, boilerplate, \
             config churn, test scaffolding) with a one-line rationale each; \
             the human acknowledges those in bulk without visiting them.\n\
             5. For concrete issues, add review/draft_comment \
             (params: {{\"path\", \"line\", \"body\"}}); the human accepts or \
             discards these in the TUI.\n\
             Your suggestions appear live in the reviewer's terminal. \
             Do not modify the repository. Exit when your review is complete."
        ),
    }
}

/// Minimal POSIX shell single-quoting.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
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
                importance: ChunkImportance::Spotlight,
                rationale: Some("spans handler and middleware".to_owned()),
                explanation: Some("The middleware now refuses tokens without an audience claim; the handler assumes that invariant.".to_owned()),
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

        let overlay = AgentOverlay::load_or_default(&dir.path().join("agent.json")).unwrap();

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

    #[test]
    fn agent_process_runs_command_with_quoted_prompt_and_logs_output() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("logs").join("agent.log");

        let mut process = AgentProcess::spawn(
            dir.path(),
            "printf '%s'",
            "it's a prompt with 'quotes'",
            &log_path,
        )
        .unwrap();

        let status = wait_for_exit(&mut process);
        assert!(status.success());
        assert_eq!(
            fs::read_to_string(&log_path).unwrap(),
            "it's a prompt with 'quotes'"
        );
    }

    #[test]
    fn agent_process_substitutes_prompt_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("agent.log");

        let mut process =
            AgentProcess::spawn(dir.path(), "printf '%s' {prompt} tail", "middle", &log_path)
                .unwrap();

        let status = wait_for_exit(&mut process);
        assert!(status.success());
        assert_eq!(fs::read_to_string(&log_path).unwrap(), "middletail");
    }

    #[test]
    fn agent_process_reports_failure_status() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join("agent.log");

        let mut process = AgentProcess::spawn(dir.path(), "false", "unused", &log_path).unwrap();

        assert!(!wait_for_exit(&mut process).success());
    }

    fn wait_for_exit(process: &mut AgentProcess) -> std::process::ExitStatus {
        for _ in 0..400 {
            if let Some(status) = process.try_status() {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("agent process never exited");
    }

    #[test]
    fn default_review_prompt_mentions_acp_workflow_and_target() {
        let prompt = review_prompt(None, Path::new("/repo"), "trunk()", "@");

        assert!(prompt.contains("/repo"));
        assert!(prompt.contains("trunk()..@"));
        assert!(prompt.contains("gander acp"));
        assert!(prompt.contains("review/set_ordering"));
        assert!(prompt.contains("review/set_chunks"));
        assert!(prompt.contains("importance=spotlight"));
        assert!(prompt.contains("importance=glance"));
        assert!(prompt.contains("explanation"));
        assert!(prompt.contains("3-7"));
        assert!(prompt.contains("review/draft_comment"));
        assert!(prompt.contains("Do not modify the repository."));
    }

    #[test]
    fn custom_prompt_template_substitutes_placeholders() {
        let prompt = review_prompt(
            Some("review {repo} from {base} to {rev}"),
            Path::new("/repo"),
            "main",
            "@",
        );

        assert_eq!(prompt, "review /repo from main to @");
    }
}
