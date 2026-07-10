use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use color_eyre::eyre::Result;
use serde::{Deserialize, Serialize};

use crate::anchor::CommentAnchor;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReviewState {
    pub meta: ReviewStateMeta,
    pub files: BTreeMap<String, FileState>,
    pub comments: Vec<Comment>,
    pub sessions: Vec<ReviewSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReviewStateMeta {
    pub version: u8,
    pub base: Option<String>,
    pub revision: Option<String>,
    pub repo: Option<String>,
    pub saved_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FileState {
    pub fingerprint: String,
    pub viewed: bool,
    pub viewed_fingerprints: BTreeSet<String>,
    pub caught_up_fingerprints: BTreeSet<String>,
}

impl FileState {
    pub fn normalize_legacy(&mut self) {
        if self.viewed && !self.fingerprint.is_empty() {
            self.viewed_fingerprints.insert(self.fingerprint.clone());
        }
    }

    pub fn is_viewed_fingerprint(&self, fingerprint: &str) -> bool {
        self.viewed_fingerprints.contains(fingerprint)
    }

    pub fn is_caught_up_fingerprint(&self, fingerprint: &str) -> bool {
        self.caught_up_fingerprints.contains(fingerprint)
    }

    pub fn has_any_viewed_fingerprint(&self) -> bool {
        !self.viewed_fingerprints.is_empty() || !self.caught_up_fingerprints.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CommentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionIntent>,
    #[serde(default)]
    pub state: CommentState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<CommentReply>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Default for Comment {
    fn default() -> Self {
        Self {
            id: String::new(),
            path: String::new(),
            line: None,
            end_line: None,
            anchor: None,
            body: String::new(),
            kind: None,
            action: None,
            state: CommentState::default(),
            replies: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CommentReply {
    pub id: String,
    pub body: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Default for CommentReply {
    fn default() -> Self {
        Self {
            id: String::new(),
            body: String::new(),
            created_at: chrono::Utc::now(),
        }
    }
}

/// Durable local review session over a jj-visible code state.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReviewSession {
    pub id: String,
    pub title: Option<String>,
    pub target: ReviewTarget,
    pub status: ReviewSessionStatus,
    pub walkthroughs: Vec<Walkthrough>,
    pub tasks: Vec<ReviewTask>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Stable target for a review session or nested review object.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReviewTarget {
    pub revset: Option<String>,
    pub base: Option<String>,
    pub revision: Option<String>,
    pub repo: Option<String>,
    pub file: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewSessionStatus {
    #[default]
    Open,
    Completed,
    Archived,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Walkthrough {
    pub id: String,
    pub title: Option<String>,
    pub steps: Vec<WalkthroughStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct WalkthroughStep {
    pub id: String,
    pub target: ReviewTarget,
    #[serde(default)]
    pub importance: StepImportance,
    #[serde(default)]
    pub kind: StepKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<StepArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_targets: Vec<ReviewTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepImportance {
    #[default]
    Spotlight,
    Glance,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepKind {
    #[default]
    Step,
    Chapter,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StepArtifact {
    pub title: String,
    pub kind: StepArtifactKind,
    pub body: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepArtifactKind {
    #[default]
    Example,
    Output,
    Diagram,
    Note,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReviewTask {
    pub id: String,
    pub title: String,
    pub body: Option<String>,
    pub target: Option<ReviewTarget>,
    pub action: ActionIntent,
    pub status: ReviewTaskStatus,
    pub source_comment_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewTaskStatus {
    #[default]
    Open,
    Done,
    Dismissed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum CommentKind {
    Note,
    Issue,
    Question,
    Praise,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
pub enum ActionIntent {
    #[serde(rename = "none")]
    #[default]
    None,
    #[serde(rename = "fix")]
    Fix,
    #[serde(rename = "explain")]
    Explain,
    #[serde(rename = "test")]
    Test,
    #[serde(rename = "follow-up", alias = "followup")]
    FollowUp,
}

/// Review lifecycle state of a comment.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
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
        let mut state: Self = serde_json::from_str(&contents)?;
        state.normalize_legacy_file_state();
        Ok(state)
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewStateTombstones {
    pub comments: BTreeSet<String>,
    pub sessions: BTreeSet<String>,
    pub tasks: BTreeSet<String>,
    pub walkthroughs: BTreeSet<String>,
    pub walkthrough_steps: BTreeSet<String>,
}

impl ReviewState {
    /// Merge a newer on-disk review state into this in-memory TUI state.
    ///
    /// The TUI remains authoritative for file view-state and untimestamped
    /// conflicts, while externally-added durable objects are adopted so a
    /// later save cannot clobber writes from CLI/MCP processes.
    pub fn merge_external(&mut self, external: ReviewState, tombstones: &ReviewStateTombstones) {
        self.merge_external_files(external.files);
        merge_comments(&mut self.comments, external.comments, &tombstones.comments);
        self.merge_external_sessions(external.sessions, tombstones);
    }

    fn merge_external_files(&mut self, external: BTreeMap<String, FileState>) {
        for (path, external_file) in external {
            self.files.entry(path).or_insert(external_file);
        }
    }

    fn merge_external_sessions(
        &mut self,
        external: Vec<ReviewSession>,
        tombstones: &ReviewStateTombstones,
    ) {
        for mut external_session in external {
            if tombstones.sessions.contains(&external_session.id) {
                continue;
            }
            if let Some(local_session) = self
                .sessions
                .iter_mut()
                .find(|session| session.id == external_session.id)
            {
                merge_session_children(local_session, &mut external_session, tombstones);
                if *local_session != external_session
                    && prefer_external_by_updated_at(
                        local_session.updated_at,
                        external_session.updated_at,
                    )
                {
                    *local_session = external_session;
                }
            } else {
                external_session
                    .tasks
                    .retain(|task| !tombstones.tasks.contains(&task.id));
                external_session
                    .walkthroughs
                    .retain(|walkthrough| !tombstones.walkthroughs.contains(&walkthrough.id));
                for walkthrough in &mut external_session.walkthroughs {
                    walkthrough
                        .steps
                        .retain(|step| !tombstones.walkthrough_steps.contains(&step.id));
                }
                self.sessions.push(external_session);
            }
        }
    }
}

fn merge_session_children(
    local: &mut ReviewSession,
    external: &mut ReviewSession,
    tombstones: &ReviewStateTombstones,
) {
    merge_vec_by_id(
        &mut local.tasks,
        external.tasks.clone(),
        &tombstones.tasks,
        |local, external| prefer_external_by_updated_at(local.updated_at, external.updated_at),
    );
    merge_vec_by_id(
        &mut local.walkthroughs,
        external.walkthroughs.clone(),
        &tombstones.walkthroughs,
        |local, external| prefer_external_by_updated_at(local.updated_at, external.updated_at),
    );
    for external_walkthrough in &external.walkthroughs {
        if let Some(local_walkthrough) = local
            .walkthroughs
            .iter_mut()
            .find(|walkthrough| walkthrough.id == external_walkthrough.id)
        {
            merge_vec_by_id(
                &mut local_walkthrough.steps,
                external_walkthrough.steps.clone(),
                &tombstones.walkthrough_steps,
                |local, external| {
                    prefer_external_by_updated_at(local.updated_at, external.updated_at)
                },
            );
        }
    }
    external.tasks = local.tasks.clone();
    external.walkthroughs = local.walkthroughs.clone();
}

fn prefer_external_by_updated_at(
    local: Option<chrono::DateTime<chrono::Utc>>,
    external: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    match (local, external) {
        (None, Some(_)) => true,
        (Some(local), Some(external)) => external > local,
        _ => false,
    }
}

fn merge_comments(local: &mut Vec<Comment>, external: Vec<Comment>, tombstones: &BTreeSet<String>) {
    for mut external_comment in external {
        if tombstones.contains(&external_comment.id) {
            continue;
        }
        if let Some(local_comment) = local
            .iter_mut()
            .find(|comment| comment.id == external_comment.id)
        {
            merge_comment_replies(local_comment, &external_comment);
            external_comment.replies = local_comment.replies.clone();
            if *local_comment != external_comment
                && prefer_external_by_updated_at(
                    local_comment.updated_at,
                    external_comment.updated_at,
                )
            {
                *local_comment = external_comment;
            }
        } else {
            local.push(external_comment);
        }
    }
}

fn merge_comment_replies(local: &mut Comment, external: &Comment) {
    for external_reply in &external.replies {
        if let Some(local_reply) = local
            .replies
            .iter_mut()
            .find(|reply| reply.id == external_reply.id)
        {
            if external_reply.created_at > local_reply.created_at {
                *local_reply = external_reply.clone();
            }
        } else {
            local.replies.push(external_reply.clone());
        }
    }
    local
        .replies
        .sort_by_key(|reply| (reply.created_at, reply.id.clone()));
}

trait Identified {
    fn id(&self) -> &str;
}

impl Identified for Comment {
    fn id(&self) -> &str {
        &self.id
    }
}

impl Identified for ReviewSession {
    fn id(&self) -> &str {
        &self.id
    }
}

impl Identified for ReviewTask {
    fn id(&self) -> &str {
        &self.id
    }
}

impl Identified for Walkthrough {
    fn id(&self) -> &str {
        &self.id
    }
}

impl Identified for WalkthroughStep {
    fn id(&self) -> &str {
        &self.id
    }
}

fn merge_vec_by_id<T, F>(
    local: &mut Vec<T>,
    external: Vec<T>,
    tombstones: &BTreeSet<String>,
    prefer_external: F,
) where
    T: Identified + PartialEq,
    F: Fn(&T, &T) -> bool,
{
    for external_item in external {
        if tombstones.contains(external_item.id()) {
            continue;
        }
        if let Some(local_item) = local
            .iter_mut()
            .find(|item| item.id() == external_item.id())
        {
            if *local_item != external_item && prefer_external(local_item, &external_item) {
                *local_item = external_item;
            }
        } else {
            local.push(external_item);
        }
    }
}

impl ReviewState {
    pub fn normalize_legacy_file_state(&mut self) {
        for file in self.files.values_mut() {
            file.normalize_legacy();
        }
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
                ..Default::default()
            },
        );

        state.save(&path).unwrap();

        let loaded = ReviewState::load_or_default(&path).unwrap();
        assert!(loaded.files["src/main.rs"].viewed);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn legacy_viewed_file_state_seeds_viewed_fingerprints() {
        let mut state: ReviewState = serde_json::from_str(
            r#"{
  "files": {
    "src/main.rs": { "fingerprint": "abc", "viewed": true }
  }
}"#,
        )
        .unwrap();

        state.normalize_legacy_file_state();

        let file = &state.files["src/main.rs"];
        assert!(file.viewed);
        assert!(file.viewed_fingerprints.contains("abc"));
        assert!(file.caught_up_fingerprints.is_empty());
    }

    #[test]
    fn caught_up_fingerprints_round_trip_and_legacy_defaults_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = ReviewState::default();
        state.files.insert(
            "src/main.rs".to_owned(),
            FileState {
                fingerprint: "abc".to_owned(),
                caught_up_fingerprints: BTreeSet::from(["abc".to_owned()]),
                ..Default::default()
            },
        );

        state.save(&path).unwrap();
        let loaded = ReviewState::load_or_default(&path).unwrap();
        assert!(
            loaded.files["src/main.rs"]
                .caught_up_fingerprints
                .contains("abc")
        );

        let legacy: ReviewState = serde_json::from_str(
            r#"{ "files": { "src/main.rs": { "fingerprint": "abc", "viewed": false } } }"#,
        )
        .unwrap();
        assert!(
            legacy.files["src/main.rs"]
                .caught_up_fingerprints
                .is_empty()
        );
    }

    fn comment(id: &str, body: &str) -> Comment {
        Comment {
            id: id.to_owned(),
            path: "src/lib.rs".to_owned(),
            line: Some(1),
            end_line: None,
            anchor: None,
            body: body.to_owned(),
            kind: None,
            action: None,
            state: CommentState::Draft,
            created_at: chrono::Utc::now(),
            ..Default::default()
        }
    }

    fn task(
        id: &str,
        title: &str,
        updated_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> ReviewTask {
        ReviewTask {
            id: id.to_owned(),
            title: title.to_owned(),
            updated_at,
            ..ReviewTask::default()
        }
    }

    #[test]
    fn merge_external_adopts_external_additions() {
        let mut local = ReviewState::default();
        let mut external = ReviewState::default();
        external
            .comments
            .push(comment("external-comment", "external"));
        external.sessions.push(ReviewSession {
            id: "session".to_owned(),
            tasks: vec![task("task", "external task", None)],
            walkthroughs: vec![Walkthrough {
                id: "walkthrough".to_owned(),
                steps: vec![WalkthroughStep {
                    id: "step".to_owned(),
                    title: Some("external step".to_owned()),
                    ..WalkthroughStep::default()
                }],
                ..Walkthrough::default()
            }],
            ..ReviewSession::default()
        });

        local.merge_external(external, &ReviewStateTombstones::default());

        assert_eq!(local.comments[0].id, "external-comment");
        assert_eq!(local.sessions[0].tasks[0].id, "task");
        assert_eq!(local.sessions[0].walkthroughs[0].steps[0].id, "step");
    }

    #[test]
    fn merge_external_does_not_resurrect_tombstoned_ids() {
        let mut local = ReviewState::default();
        let mut external = ReviewState::default();
        external
            .comments
            .push(comment("deleted-comment", "external"));
        external.sessions.push(ReviewSession {
            id: "session".to_owned(),
            tasks: vec![task("deleted-task", "external task", None)],
            ..ReviewSession::default()
        });
        let tombstones = ReviewStateTombstones {
            comments: BTreeSet::from(["deleted-comment".to_owned()]),
            tasks: BTreeSet::from(["deleted-task".to_owned()]),
            ..ReviewStateTombstones::default()
        };

        local.merge_external(external, &tombstones);

        assert!(local.comments.is_empty());
        assert!(local.sessions[0].tasks.is_empty());
    }

    #[test]
    fn merge_external_uses_newer_updated_at_for_conflicts() {
        let older = chrono::Utc::now();
        let newer = older + chrono::TimeDelta::seconds(5);
        let mut local = ReviewState {
            sessions: vec![ReviewSession {
                id: "session".to_owned(),
                tasks: vec![task("task", "old", Some(older))],
                updated_at: Some(older),
                ..ReviewSession::default()
            }],
            ..ReviewState::default()
        };
        let external = ReviewState {
            sessions: vec![ReviewSession {
                id: "session".to_owned(),
                tasks: vec![task("task", "new", Some(newer))],
                updated_at: Some(newer),
                ..ReviewSession::default()
            }],
            ..ReviewState::default()
        };

        local.merge_external(external, &ReviewStateTombstones::default());

        assert_eq!(local.sessions[0].tasks[0].title, "new");
    }

    #[test]
    fn merge_external_uses_newer_updated_at_for_walkthrough_steps() {
        let older = chrono::Utc::now() - chrono::Duration::seconds(10);
        let newer = chrono::Utc::now();
        let mut local = ReviewState::default();
        local.sessions.push(ReviewSession {
            id: "s".into(),
            walkthroughs: vec![Walkthrough {
                id: "w".into(),
                steps: vec![WalkthroughStep {
                    id: "st".into(),
                    title: Some("old".into()),
                    updated_at: Some(older),
                    ..Default::default()
                }],
                updated_at: Some(older),
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut external = local.clone();
        external.sessions[0].walkthroughs[0].steps[0].title = Some("new".into());
        external.sessions[0].walkthroughs[0].steps[0].updated_at = Some(newer);
        local.merge_external(external, &ReviewStateTombstones::default());
        assert_eq!(
            local.sessions[0].walkthroughs[0].steps[0].title.as_deref(),
            Some("new")
        );
    }

    #[test]
    fn merge_external_keeps_newer_local_walkthrough_steps() {
        let older = chrono::Utc::now() - chrono::Duration::seconds(10);
        let newer = chrono::Utc::now();
        let mut local = ReviewState::default();
        local.sessions.push(ReviewSession {
            id: "s".into(),
            walkthroughs: vec![Walkthrough {
                id: "w".into(),
                steps: vec![WalkthroughStep {
                    id: "st".into(),
                    title: Some("new".into()),
                    updated_at: Some(newer),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        });
        let mut external = local.clone();
        external.sessions[0].walkthroughs[0].steps[0].title = Some("old".into());
        external.sessions[0].walkthroughs[0].steps[0].updated_at = Some(older);
        local.merge_external(external, &ReviewStateTombstones::default());
        assert_eq!(
            local.sessions[0].walkthroughs[0].steps[0].title.as_deref(),
            Some("new")
        );
    }

    #[test]
    fn merge_external_keeps_in_memory_conflicts_without_timestamps() {
        let mut local = ReviewState {
            comments: vec![comment("comment", "local")],
            sessions: vec![ReviewSession {
                id: "session".to_owned(),
                title: Some("local".to_owned()),
                ..ReviewSession::default()
            }],
            ..ReviewState::default()
        };
        let external = ReviewState {
            comments: vec![comment("comment", "external")],
            sessions: vec![ReviewSession {
                id: "session".to_owned(),
                title: Some("external".to_owned()),
                ..ReviewSession::default()
            }],
            ..ReviewState::default()
        };

        local.merge_external(external, &ReviewStateTombstones::default());

        assert_eq!(local.comments[0].body, "local");
        assert_eq!(local.sessions[0].title.as_deref(), Some("local"));
    }

    #[test]
    fn merge_external_unions_replies_for_same_comment_id() {
        let mut local = ReviewState {
            comments: vec![comment("comment", "local")],
            ..Default::default()
        };
        local.comments[0].replies.push(CommentReply {
            id: "local-reply".into(),
            body: "local".into(),
            created_at: chrono::Utc::now(),
        });
        let mut external = ReviewState {
            comments: vec![comment("comment", "external")],
            ..Default::default()
        };
        external.comments[0].updated_at = Some(chrono::Utc::now() + chrono::TimeDelta::seconds(1));
        external.comments[0].replies.push(CommentReply {
            id: "external-reply".into(),
            body: "external".into(),
            created_at: chrono::Utc::now(),
        });

        local.merge_external(external, &ReviewStateTombstones::default());

        assert_eq!(local.comments[0].body, "external");
        assert!(
            local.comments[0]
                .replies
                .iter()
                .any(|reply| reply.id == "local-reply")
        );
        assert!(
            local.comments[0]
                .replies
                .iter()
                .any(|reply| reply.id == "external-reply")
        );
    }

    #[test]
    fn merge_external_prefers_in_memory_view_state_but_adopts_unknown_files() {
        let mut local = ReviewState::default();
        local.files.insert(
            "known.rs".to_owned(),
            FileState {
                fingerprint: "local".to_owned(),
                viewed: true,
                ..FileState::default()
            },
        );
        let mut external = ReviewState::default();
        external.files.insert(
            "known.rs".to_owned(),
            FileState {
                fingerprint: "external".to_owned(),
                viewed: false,
                ..FileState::default()
            },
        );
        external.files.insert(
            "unknown.rs".to_owned(),
            FileState {
                fingerprint: "external".to_owned(),
                viewed: true,
                ..FileState::default()
            },
        );

        local.merge_external(external, &ReviewStateTombstones::default());

        assert_eq!(local.files["known.rs"].fingerprint, "local");
        assert_eq!(local.files["unknown.rs"].fingerprint, "external");
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
        assert!(state.comments[0].kind.is_none());
        assert!(state.comments[0].action.is_none());
        assert!(state.sessions.is_empty());
        assert_eq!(state.meta.version, 0);
    }

    #[test]
    fn loads_session_defaults_from_minimal_json() {
        let state: ReviewState = serde_json::from_str(
            r#"{
  "sessions": [
    {
      "id": "review-1",
      "tasks": [
        { "id": "task-1", "title": "Explain this hunk" }
      ],
      "walkthroughs": [
        {
          "id": "walkthrough-1",
          "steps": [
            { "id": "step-1", "target": { "file": "src/lib.rs", "line": 10 } }
          ]
        }
      ]
    }
  ]
}"#,
        )
        .unwrap();

        let session = &state.sessions[0];
        assert_eq!(session.status, ReviewSessionStatus::Open);
        assert_eq!(session.tasks[0].action, ActionIntent::None);
        assert_eq!(session.tasks[0].status, ReviewTaskStatus::Open);
        assert_eq!(
            session.walkthroughs[0].steps[0].target.file.as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn round_trips_comment_action_and_session_fields() {
        let created_at = chrono::DateTime::parse_from_rfc3339("2026-07-04T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let state: ReviewState = serde_json::from_str(
            r#"{
  "comments": [
    {
      "id": "comment-1",
      "path": "src/state.rs",
      "line": 32,
      "body": "Needs a follow-up test",
      "kind": "issue",
      "action": "test",
      "created_at": "2026-07-04T12:00:00Z"
    }
  ],
  "sessions": [
    {
      "id": "review-1",
      "title": "Schema slice",
      "target": { "revset": "@", "repo": "gander" },
      "status": "completed",
      "tasks": [
        {
          "id": "task-1",
          "title": "Fix issue",
          "action": "fix",
          "status": "done",
          "source_comment_id": "comment-1"
        }
      ],
      "walkthroughs": [
        {
          "id": "walkthrough-1",
          "title": "Start here",
          "steps": [
            {
              "id": "step-1",
              "target": { "file": "src/state.rs", "line": 1, "end_line": 10, "symbol": "ReviewState" },
              "why": "Introduces durable state"
            }
          ]
        }
      ]
    }
  ]
}"#,
        )
        .unwrap();

        assert_eq!(state.comments[0].kind, Some(CommentKind::Issue));
        assert_eq!(state.comments[0].action, Some(ActionIntent::Test));
        assert_eq!(state.comments[0].created_at, created_at);
        assert_eq!(state.sessions[0].status, ReviewSessionStatus::Completed);
        assert_eq!(state.sessions[0].tasks[0].action, ActionIntent::Fix);

        let json = serde_json::to_string(&state).unwrap();
        let loaded: ReviewState = serde_json::from_str(&json).unwrap();

        assert_eq!(loaded.comments[0].kind, Some(CommentKind::Issue));
        assert_eq!(loaded.comments[0].action, Some(ActionIntent::Test));
        assert_eq!(
            loaded.sessions[0].walkthroughs[0].steps[0]
                .target
                .symbol
                .as_deref(),
            Some("ReviewState")
        );
        assert_eq!(
            loaded.sessions[0].tasks[0].source_comment_id.as_deref(),
            Some("comment-1")
        );
    }

    #[test]
    fn follow_up_action_accepts_legacy_spelling_and_serializes_canonical() {
        for spelling in ["followup", "follow-up"] {
            let action: ActionIntent = serde_json::from_str(&format!("\"{spelling}\"")).unwrap();
            assert_eq!(action, ActionIntent::FollowUp);
        }

        let json = serde_json::to_string(&ActionIntent::FollowUp).unwrap();
        assert_eq!(json, "\"follow-up\"");
        let action: ActionIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(action, ActionIntent::FollowUp);
    }
}
