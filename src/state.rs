use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    marker::PhantomData,
    path::{Path, PathBuf},
    rc::Rc,
};

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize, de::Error as _};

use crate::{
    anchor::CommentAnchor,
    provenance::{CommentObservation, CommentReplyResult},
};

/// Current on-disk review-state schema. Version 8 adds conservative durable
/// attention progress for skim acknowledgements and spotlight visits.
pub const REVIEW_STATE_SCHEMA_VERSION: u8 = 8;

/// Deterministic identity used only when reading pre-v4 local review state.
/// Configured identities are stamped by adapters when creating new comments;
/// raw state deserialization deliberately has no config dependency.
pub const LEGACY_LOCAL_HUMAN_NAME: &str = "local";

/// Conservative identity for agent-authored annotations when no `[agent].name`
/// is configured, and for raw migration defaults.
pub const DEFAULT_AGENT_NAME: &str = "agent";

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AuthorKind {
    #[default]
    Human,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
pub struct Identity {
    pub kind: AuthorKind,
    pub name: String,
}

impl Default for Identity {
    fn default() -> Self {
        Self::local_human()
    }
}

impl Identity {
    pub fn local_human() -> Self {
        Self {
            kind: AuthorKind::Human,
            name: LEGACY_LOCAL_HUMAN_NAME.to_owned(),
        }
    }

    pub fn agent() -> Self {
        Self {
            kind: AuthorKind::Agent,
            name: DEFAULT_AGENT_NAME.to_owned(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(eyre!("identity name must not be empty"));
        }
        Ok(())
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Onboarding,
    Delegation,
    Collaboration,
    #[default]
    Note,
}

impl Channel {
    /// Stable editor cycle order documented in `docs/keybindings.md`.
    pub fn next(self) -> Self {
        match self {
            Self::Onboarding => Self::Delegation,
            Self::Delegation => Self::Collaboration,
            Self::Collaboration => Self::Note,
            Self::Note => Self::Onboarding,
        }
    }

    /// Compact audience label used by TUI channel chips and list rows.
    pub fn audience_label(self) -> &'static str {
        match self {
            Self::Onboarding => "reviewer",
            Self::Delegation => "agent",
            Self::Collaboration => "team",
            Self::Note => "note",
        }
    }

    pub fn permits_todo(self) -> bool {
        matches!(self, Self::Delegation | Self::Collaboration)
    }
}

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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Comment {
    pub id: String,
    /// Durable review session that owns this comment.
    ///
    /// This is optional only for compatibility with comments written before
    /// session-scoped comments were introduced. An unscoped legacy comment is
    /// considered visible in every session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<CommentObservation>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CommentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionIntent>,
    #[serde(default)]
    pub state: CommentState,
    pub author: Identity,
    pub channel: Channel,
    /// Exact onboarding comment that prompted this delegation request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_comment_id: Option<String>,
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
            session_id: None,
            path: None,
            line: None,
            end_line: None,
            anchor: None,
            observation: None,
            body: String::new(),
            kind: None,
            action: None,
            state: CommentState::default(),
            author: Identity::default(),
            channel: Channel::default(),
            source_comment_id: None,
            replies: Vec::new(),
            created_at: chrono::Utc::now(),
            updated_at: None,
        }
    }
}

impl Comment {
    /// Whether this is a session-level comment with no file location.
    pub fn is_general(&self) -> bool {
        self.path.is_none()
    }

    /// Whether this comment has a file location (possibly without a line).
    pub fn has_location(&self) -> bool {
        self.path.is_some()
    }

    /// Alias useful to callers that model general and located comments as a
    /// pair of variants.
    pub fn is_located(&self) -> bool {
        self.has_location()
    }

    /// Return whether this comment is visible in `session_id`.
    ///
    /// Comments lacking a session id predate durable comment scoping, so they
    /// remain visible and eligible in every session.
    pub fn belongs_to_session(&self, session_id: &str) -> bool {
        self.session_id
            .as_deref()
            .is_none_or(|owner| owner == session_id)
    }

    /// Validate relationships between the denormalized location fields and
    /// an optional durable diff anchor.
    pub fn validate(&self) -> Result<()> {
        self.author.validate()?;
        for reply in &self.replies {
            reply.validate()?;
        }
        if self.state == CommentState::Todo
            && !matches!(self.channel, Channel::Delegation | Channel::Collaboration)
        {
            return Err(eyre!(
                "todo comment channel must be delegation or collaboration"
            ));
        }
        if self.session_id.as_deref() == Some("") {
            return Err(eyre!("comment session id must not be empty"));
        }

        let Some(path) = self.path.as_deref() else {
            if self.line.is_some() || self.end_line.is_some() || self.anchor.is_some() {
                return Err(eyre!(
                    "general comment cannot have a line, end line, or anchor"
                ));
            }
            return Ok(());
        };

        if path.is_empty() {
            return Err(eyre!("comment path must not be empty"));
        }
        if self.end_line.is_some() && self.line.is_none() {
            return Err(eyre!("comment end line requires a start line"));
        }
        if self.line == Some(0) || self.end_line == Some(0) {
            return Err(eyre!("comment lines must be 1-indexed"));
        }
        if let (Some(line), Some(end_line)) = (self.line, self.end_line)
            && end_line < line
        {
            return Err(eyre!(
                "comment end line must be greater than or equal to its start line"
            ));
        }

        let Some(anchor) = self.anchor.as_ref() else {
            return Ok(());
        };
        if anchor.path() != path {
            return Err(eyre!(
                "comment path `{path}` does not match anchor path `{}`",
                anchor.path()
            ));
        }
        match anchor {
            CommentAnchor::File { .. } => {
                if self.line.is_some() || self.end_line.is_some() {
                    return Err(eyre!("file anchor cannot have line coordinates"));
                }
            }
            CommentAnchor::Line { line, .. } => {
                if self.line != Some(*line)
                    || self.end_line.is_some_and(|end_line| end_line != *line)
                {
                    return Err(eyre!("comment line coordinates do not match line anchor"));
                }
            }
            CommentAnchor::Range {
                start_line,
                end_line,
                ..
            } => {
                if self.line != Some(*start_line) || self.end_line != Some(*end_line) {
                    return Err(eyre!("comment line coordinates do not match range anchor"));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct CompatibleComment {
    id: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
    #[serde(default)]
    anchor: Option<CommentAnchor>,
    #[serde(default)]
    observation: Option<CommentObservation>,
    body: String,
    #[serde(default)]
    kind: Option<CommentKind>,
    #[serde(default)]
    action: Option<ActionIntent>,
    #[serde(default)]
    state: CommentState,
    #[serde(default)]
    author: Option<Identity>,
    #[serde(default)]
    channel: Option<Channel>,
    #[serde(default)]
    source_comment_id: Option<String>,
    #[serde(default)]
    replies: Vec<CommentReply>,
    created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl<'de> Deserialize<'de> for Comment {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let compatible = CompatibleComment::deserialize(deserializer)?;
        let channel = compatible.channel.unwrap_or(match compatible.state {
            CommentState::Todo => Channel::Delegation,
            CommentState::Draft | CommentState::Resolved => Channel::Note,
        });
        let comment = Self {
            id: compatible.id,
            session_id: compatible.session_id,
            path: compatible.path,
            line: compatible.line,
            end_line: compatible.end_line,
            anchor: compatible.anchor,
            observation: compatible.observation,
            body: compatible.body,
            kind: compatible.kind,
            action: compatible.action,
            state: compatible.state,
            author: compatible.author.unwrap_or_default(),
            channel,
            source_comment_id: compatible.source_comment_id,
            replies: compatible.replies,
            created_at: compatible.created_at,
            updated_at: compatible.updated_at,
        };
        comment
            .validate_annotation_fields()
            .map_err(D::Error::custom)?;
        Ok(comment)
    }
}

impl Comment {
    fn validate_annotation_fields(&self) -> Result<()> {
        self.author.validate()?;
        for reply in &self.replies {
            reply.validate()?;
        }
        if self.state == CommentState::Todo
            && !matches!(self.channel, Channel::Delegation | Channel::Collaboration)
        {
            return Err(eyre!(
                "todo comment channel must be delegation or collaboration"
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CommentReply {
    pub id: String,
    pub body: String,
    #[serde(default)]
    pub author: Identity,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<CommentReplyResult>,
}

impl Default for CommentReply {
    fn default() -> Self {
        Self {
            id: String::new(),
            body: String::new(),
            author: Identity::default(),
            created_at: chrono::Utc::now(),
            result: None,
        }
    }
}

impl CommentReply {
    pub fn validate(&self) -> Result<()> {
        self.author.validate()
    }
}

/// Durable local review session over a jj-visible code state.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ReviewSession {
    pub id: String,
    pub title: Option<String>,
    pub target: ReviewTarget,
    pub status: ReviewSessionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<ReviewDisposition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention_regions: Vec<AttentionRegion>,
    /// Append-only review-stream progress. A record only counts while its
    /// fingerprint matches the current projection; older records are retained
    /// as history so drift never silently acknowledges new code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attention_progress: Vec<AttentionProgress>,
    pub walkthroughs: Vec<Walkthrough>,
    pub action_items: Vec<ActionItem>,
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewDisposition {
    Comment,
    Approve,
    RequestChanges,
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
    /// Existing diff anchor/fingerprint evidence for durable located targets.
    ///
    /// This reuses the comment anchoring system rather than introducing a
    /// second region-anchor representation. Older state simply omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchor>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Salience {
    Spotlight,
    #[default]
    Supporting,
    Skim,
}

impl Salience {
    pub fn promote(self) -> Self {
        match self {
            Self::Skim => Self::Supporting,
            Self::Supporting | Self::Spotlight => Self::Spotlight,
        }
    }

    pub fn demote(self) -> Self {
        match self {
            Self::Spotlight => Self::Supporting,
            Self::Supporting | Self::Skim => Self::Skim,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, rmcp::schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SalienceSource {
    Human,
    Agent,
    Heuristic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionRegion {
    pub target: ReviewTarget,
    pub salience: Salience,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    pub source: SalienceSource,
}

/// Stable, fingerprint-independent identity for one member of a rendered
/// attention target. Cross-file skim folds carry one member per covered
/// region, in stream order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct AttentionProgressMember {
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(default)]
pub struct AttentionProgressTarget {
    pub members: Vec<AttentionProgressMember>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AttentionProgressKind {
    SkimAcknowledged,
    SpotlightVisited,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionProgress {
    pub target: AttentionProgressTarget,
    pub kind: AttentionProgressKind,
    /// Aggregate of every current file fingerprint covered by `target`.
    pub fingerprint: String,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
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
    /// Author of this narration. Legacy steps deliberately remain neutral;
    /// callers stamp new steps at their human/agent creation boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<Identity>,
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

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ActionItem {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<ReviewTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionIntent>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comment_ids: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub external_tickets: Vec<ExternalTicket>,
    pub status: ActionItemStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disposition: Option<ClosedDisposition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActionItemStatus {
    #[default]
    Open,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClosedDisposition {
    Completed,
    Dismissed,
    Deferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExternalTicket {
    pub tracker: String,
    pub reference: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CompatibleActionItemStatus {
    #[default]
    Open,
    Closed,
    Done,
    Dismissed,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CompatibleActionItem {
    id: String,
    title: String,
    body: Option<String>,
    target: Option<ReviewTarget>,
    action: Option<ActionIntent>,
    comment_ids: Vec<String>,
    source_comment_id: Option<String>,
    external_tickets: Vec<ExternalTicket>,
    status: CompatibleActionItemStatus,
    disposition: Option<ClosedDisposition>,
    outcome: Option<String>,
    resolution: Option<String>,
    closed_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl<'de> Deserialize<'de> for ActionItem {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let compatible = CompatibleActionItem::deserialize(deserializer)?;
        let (status, legacy_disposition) = match compatible.status {
            CompatibleActionItemStatus::Open => (ActionItemStatus::Open, None),
            CompatibleActionItemStatus::Closed => {
                (ActionItemStatus::Closed, Some(ClosedDisposition::Completed))
            }
            CompatibleActionItemStatus::Done => {
                (ActionItemStatus::Closed, Some(ClosedDisposition::Completed))
            }
            CompatibleActionItemStatus::Dismissed => {
                (ActionItemStatus::Closed, Some(ClosedDisposition::Dismissed))
            }
        };
        let mut comment_ids = compatible.comment_ids;
        if let Some(source_comment_id) = compatible.source_comment_id {
            comment_ids.push(source_comment_id);
        }
        dedupe_strings(&mut comment_ids);

        Ok(Self {
            id: compatible.id,
            title: compatible.title,
            body: compatible.body,
            target: compatible.target,
            action: compatible.action,
            comment_ids,
            external_tickets: compatible.external_tickets,
            status,
            disposition: if status == ActionItemStatus::Closed {
                compatible.disposition.or(legacy_disposition)
            } else {
                None
            },
            outcome: compatible.outcome.or(compatible.resolution),
            closed_at: compatible.closed_at,
            created_at: compatible.created_at,
            updated_at: compatible.updated_at,
        })
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CompatibleReviewSession {
    id: String,
    title: Option<String>,
    target: ReviewTarget,
    status: ReviewSessionStatus,
    disposition: Option<ReviewDisposition>,
    attention_regions: Vec<AttentionRegion>,
    attention_progress: Vec<AttentionProgress>,
    walkthroughs: Vec<Walkthrough>,
    action_items: Vec<ActionItem>,
    tasks: Vec<ActionItem>,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl<'de> Deserialize<'de> for ReviewSession {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let compatible = CompatibleReviewSession::deserialize(deserializer)?;
        let mut action_items = compatible.action_items;
        let mut seen = BTreeSet::new();
        action_items.retain(|item| seen.insert(item.id.clone()));
        for item in compatible.tasks {
            if seen.insert(item.id.clone()) {
                action_items.push(item);
            }
        }

        Ok(Self {
            id: compatible.id,
            title: compatible.title,
            target: compatible.target,
            status: compatible.status,
            disposition: compatible.disposition,
            attention_regions: compatible.attention_regions,
            attention_progress: compatible.attention_progress,
            walkthroughs: compatible.walkthroughs,
            action_items,
            created_at: compatible.created_at,
            updated_at: compatible.updated_at,
        })
    }
}

fn dedupe_strings(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
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
        state.normalize_action_items();
        Ok(state)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let _lock = ReviewStateFileLock::acquire(path)?;
        self.save_locked(path)
    }

    fn save_locked(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Write to a sibling temp file and rename so an interrupted save
        // cannot truncate or corrupt existing review state.
        let mut tmp = path.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let mut persisted = self.clone();
        persisted.meta.version = REVIEW_STATE_SCHEMA_VERSION;
        persisted.normalize_action_items();
        for comment in &persisted.comments {
            comment.validate_annotation_fields()?;
        }
        fs::write(&tmp, serde_json::to_string_pretty(&persisted)?)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Apply only changes made since `base` to the latest state on disk.
    ///
    /// This is the live-instance save handshake: unchanged stale fields are
    /// never written back, while independently changed files and durable
    /// objects compose. Explicit tombstones cover TUI deletions that occurred
    /// before the baseline was refreshed.
    pub(crate) fn merge_changes_since(
        mut latest: Self,
        base: &Self,
        local: Self,
        tombstones: &ReviewStateTombstones,
    ) -> Self {
        latest.meta = local.meta.clone();

        let file_paths = base
            .files
            .keys()
            .chain(local.files.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for path in file_paths {
            if base.files.get(&path) != local.files.get(&path) {
                match (base.files.get(&path), local.files.get(&path)) {
                    (Some(base), Some(local)) => {
                        if let Some(current) = latest.files.get_mut(&path) {
                            merge_file_changes(current, base, local);
                        } else {
                            latest.files.insert(path, local.clone());
                        }
                    }
                    (None, Some(file)) => {
                        latest.files.insert(path, file.clone());
                    }
                    (_, None) => {
                        latest.files.remove(&path);
                    }
                }
            }
        }

        merge_changed_comments(&mut latest.comments, &base.comments, &local.comments);
        merge_changed_sessions(&mut latest.sessions, &base.sessions, &local.sessions);
        apply_tombstones(&mut latest, tombstones);
        latest
    }
}

fn merge_file_changes(current: &mut FileState, base: &FileState, local: &FileState) {
    if local.fingerprint != base.fingerprint {
        current.fingerprint = local.fingerprint.clone();
    }
    if local.viewed != base.viewed {
        current.viewed = local.viewed;
    }
    for fingerprint in local
        .viewed_fingerprints
        .difference(&base.viewed_fingerprints)
    {
        current.viewed_fingerprints.insert(fingerprint.clone());
    }
    for fingerprint in base
        .viewed_fingerprints
        .difference(&local.viewed_fingerprints)
    {
        current.viewed_fingerprints.remove(fingerprint);
    }
    for fingerprint in local
        .caught_up_fingerprints
        .difference(&base.caught_up_fingerprints)
    {
        current.caught_up_fingerprints.insert(fingerprint.clone());
    }
    for fingerprint in base
        .caught_up_fingerprints
        .difference(&local.caught_up_fingerprints)
    {
        current.caught_up_fingerprints.remove(fingerprint);
    }
}

thread_local! {
    static HELD_STATE_LOCKS: RefCell<BTreeMap<PathBuf, usize>> = const { RefCell::new(BTreeMap::new()) };
}

/// Crash-safe, process-wide advisory lock for one review-state file.
///
/// Unix releases `flock` automatically if a process exits. The lock is
/// re-entrant on one thread so service transactions can call `ReviewState::save`.
pub(crate) struct ReviewStateFileLock {
    path: PathBuf,
    #[cfg(unix)]
    file: Option<File>,
    _not_send: PhantomData<Rc<()>>,
}

impl ReviewStateFileLock {
    pub(crate) fn acquire(state_path: &Path) -> Result<Self> {
        let mut lock_path = state_path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let lock_path = PathBuf::from(lock_path);
        let nested = HELD_STATE_LOCKS.with(|held| {
            let mut held = held.borrow_mut();
            if let Some(count) = held.get_mut(&lock_path) {
                *count += 1;
                true
            } else {
                false
            }
        });
        if nested {
            return Ok(Self {
                path: lock_path,
                #[cfg(unix)]
                file: None,
                _not_send: PhantomData,
            });
        }
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        #[cfg(unix)]
        lock_file_exclusive(&file)?;
        HELD_STATE_LOCKS.with(|held| {
            held.borrow_mut().insert(lock_path.clone(), 1);
        });
        Ok(Self {
            path: lock_path,
            #[cfg(unix)]
            file: Some(file),
            _not_send: PhantomData,
        })
    }
}

impl Drop for ReviewStateFileLock {
    fn drop(&mut self) {
        let last = HELD_STATE_LOCKS.with(|held| {
            let mut held = held.borrow_mut();
            let Some(count) = held.get_mut(&self.path) else {
                return false;
            };
            *count -= 1;
            if *count == 0 {
                held.remove(&self.path);
                true
            } else {
                false
            }
        });
        #[cfg(unix)]
        if last && let Some(file) = self.file.as_ref() {
            unlock_file(file);
        }
    }
}

#[cfg(unix)]
fn lock_file_exclusive(file: &File) -> Result<()> {
    use std::{ffi::c_int, os::fd::AsRawFd};
    unsafe extern "C" {
        fn flock(fd: c_int, operation: c_int) -> c_int;
    }
    // LOCK_EX is 2 on the Unix targets supported by the flake.
    if unsafe { flock(file.as_raw_fd(), 2) } == -1 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(unix)]
fn unlock_file(file: &File) {
    use std::{ffi::c_int, os::fd::AsRawFd};
    unsafe extern "C" {
        fn flock(fd: c_int, operation: c_int) -> c_int;
    }
    // LOCK_UN is 8 on the Unix targets supported by the flake.
    let _ = unsafe { flock(file.as_raw_fd(), 8) };
}

fn merge_changed_comments(latest: &mut Vec<Comment>, base: &[Comment], local: &[Comment]) {
    for prior in base {
        if !local.iter().any(|comment| comment.id == prior.id) {
            latest.retain(|comment| comment.id != prior.id);
        }
    }
    for changed in local {
        let prior = base.iter().find(|comment| comment.id == changed.id);
        if prior == Some(changed) {
            continue;
        }
        match latest.iter_mut().find(|comment| comment.id == changed.id) {
            Some(current) if prior.is_some_and(|prior| current != prior) => {
                let mut candidate = changed.clone();
                merge_comment_replies(&mut candidate, current);
                merge_comment_observation(&mut candidate, current);
                if prefer_external_by_updated_at(current.updated_at, candidate.updated_at) {
                    *current = candidate;
                } else {
                    merge_comment_replies(current, &candidate);
                    merge_comment_observation(current, &candidate);
                }
            }
            Some(current) => *current = changed.clone(),
            None => latest.push(changed.clone()),
        }
    }
}

fn merge_changed_sessions(
    latest: &mut Vec<ReviewSession>,
    base: &[ReviewSession],
    local: &[ReviewSession],
) {
    for prior in base {
        if !local.iter().any(|session| session.id == prior.id) {
            latest.retain(|session| session.id != prior.id);
        }
    }
    for changed in local {
        let prior = base.iter().find(|session| session.id == changed.id);
        if prior == Some(changed) {
            continue;
        }
        let Some(prior) = prior else {
            if let Some(current) = latest.iter_mut().find(|session| session.id == changed.id) {
                let mut incoming = changed.clone();
                merge_session_children(current, &mut incoming, &ReviewStateTombstones::default());
                if prefer_external_by_updated_at(current.updated_at, incoming.updated_at) {
                    *current = incoming;
                }
            } else {
                latest.push(changed.clone());
            }
            continue;
        };
        if let Some(current) = latest.iter_mut().find(|session| session.id == changed.id) {
            patch_session(current, prior, changed);
        } else {
            // The local instance changed this object after another writer
            // deleted it. A deliberate local edit is newer than stale absence.
            latest.push(changed.clone());
        }
    }
}

fn patch_session(current: &mut ReviewSession, base: &ReviewSession, local: &ReviewSession) {
    let local_wins_conflict = prefer_external_by_updated_at(current.updated_at, local.updated_at);
    if local.title != base.title && (current.title == base.title || local_wins_conflict) {
        current.title = local.title.clone();
    }
    if local.target != base.target && (current.target == base.target || local_wins_conflict) {
        current.target = local.target.clone();
    }
    if local.status != base.status && (current.status == base.status || local_wins_conflict) {
        current.status = local.status;
    }
    if local.disposition != base.disposition
        && (current.disposition == base.disposition || local_wins_conflict)
    {
        current.disposition = local.disposition;
    }
    if local.attention_regions != base.attention_regions {
        merge_attention_region_changes(
            &mut current.attention_regions,
            &base.attention_regions,
            &local.attention_regions,
            local_wins_conflict,
        );
    }
    for progress in &local.attention_progress {
        if !base.attention_progress.contains(progress)
            && !current.attention_progress.contains(progress)
        {
            current.attention_progress.push(progress.clone());
        }
    }
    merge_changed_by_id(
        &mut current.action_items,
        &base.action_items,
        &local.action_items,
        |item| item.id.as_str(),
        |item| item.updated_at,
    );
    merge_changed_walkthroughs(
        &mut current.walkthroughs,
        &base.walkthroughs,
        &local.walkthroughs,
    );
    if local.created_at != base.created_at
        && (current.created_at == base.created_at || local_wins_conflict)
    {
        current.created_at = local.created_at;
    }
    if local.updated_at != base.updated_at && local_wins_conflict {
        current.updated_at = local.updated_at;
    }
}

fn merge_changed_walkthroughs(
    latest: &mut Vec<Walkthrough>,
    base: &[Walkthrough],
    local: &[Walkthrough],
) {
    for prior in base {
        if !local.iter().any(|walkthrough| walkthrough.id == prior.id) {
            latest.retain(|walkthrough| walkthrough.id != prior.id);
        }
    }
    for changed in local {
        let prior = base.iter().find(|walkthrough| walkthrough.id == changed.id);
        if prior == Some(changed) {
            continue;
        }
        match (prior, latest.iter_mut().find(|item| item.id == changed.id)) {
            (Some(prior), Some(current)) => {
                let local_wins_conflict =
                    prefer_external_by_updated_at(current.updated_at, changed.updated_at);
                if changed.title != prior.title
                    && (current.title == prior.title || local_wins_conflict)
                {
                    current.title = changed.title.clone();
                }
                merge_changed_by_id(
                    &mut current.steps,
                    &prior.steps,
                    &changed.steps,
                    |step| step.id.as_str(),
                    |step| step.updated_at,
                );
                if prior.steps.iter().map(|step| &step.id).collect::<Vec<_>>()
                    != changed
                        .steps
                        .iter()
                        .map(|step| &step.id)
                        .collect::<Vec<_>>()
                {
                    reorder_by_local_ids(
                        &mut current.steps,
                        changed.steps.iter().map(|step| step.id.as_str()),
                        |step| step.id.as_str(),
                    );
                }
                if changed.updated_at != prior.updated_at && local_wins_conflict {
                    current.updated_at = changed.updated_at;
                }
            }
            (_, Some(current)) => {
                if prefer_external_by_updated_at(current.updated_at, changed.updated_at) {
                    *current = changed.clone();
                }
            }
            (_, None) => latest.push(changed.clone()),
        }
    }
}

fn merge_attention_region_changes(
    current: &mut Vec<AttentionRegion>,
    base: &[AttentionRegion],
    local: &[AttentionRegion],
    local_wins_conflict: bool,
) {
    let same_identity = |left: &AttentionRegion, right: &AttentionRegion| {
        left.source == right.source && left.target == right.target
    };
    for prior in base {
        if !local.iter().any(|region| same_identity(region, prior)) {
            current.retain(|region| {
                !same_identity(region, prior) || region != prior && !local_wins_conflict
            });
        }
    }
    for changed in local {
        let prior = base.iter().find(|region| same_identity(region, changed));
        if prior == Some(changed) {
            continue;
        }
        if let Some(existing) = current
            .iter_mut()
            .find(|region| same_identity(region, changed))
        {
            if prior.is_none_or(|prior| existing == prior || local_wins_conflict) {
                *existing = changed.clone();
            }
        } else {
            current.push(changed.clone());
        }
    }
}

fn reorder_by_local_ids<'a, T: Clone + 'a>(
    current: &mut Vec<T>,
    local_ids: impl Iterator<Item = &'a str>,
    id: impl Fn(&T) -> &str,
) {
    let mut remaining = std::mem::take(current);
    let mut reordered = Vec::with_capacity(remaining.len());
    for local_id in local_ids {
        if let Some(index) = remaining.iter().position(|item| id(item) == local_id) {
            reordered.push(remaining.remove(index));
        }
    }
    reordered.extend(remaining);
    *current = reordered;
}

fn merge_changed_by_id<T, I, U>(latest: &mut Vec<T>, base: &[T], local: &[T], id: I, updated_at: U)
where
    T: Clone + PartialEq,
    I: Fn(&T) -> &str,
    U: Fn(&T) -> Option<chrono::DateTime<chrono::Utc>>,
{
    for prior in base {
        if !local.iter().any(|item| id(item) == id(prior)) {
            latest.retain(|item| id(item) != id(prior));
        }
    }
    for changed in local {
        let prior = base.iter().find(|item| id(item) == id(changed));
        if prior == Some(changed) {
            continue;
        }
        match latest.iter_mut().find(|item| id(item) == id(changed)) {
            Some(current)
                if prior.is_some_and(|prior| current != prior)
                    && !prefer_external_by_updated_at(updated_at(current), updated_at(changed)) => {
            }
            Some(current) => *current = changed.clone(),
            None => latest.push(changed.clone()),
        }
    }
}

fn apply_tombstones(state: &mut ReviewState, tombstones: &ReviewStateTombstones) {
    state
        .comments
        .retain(|comment| !tombstones.comments.contains(&comment.id));
    state
        .sessions
        .retain(|session| !tombstones.sessions.contains(&session.id));
    for session in &mut state.sessions {
        session
            .action_items
            .retain(|item| !tombstones.action_items.contains(&item.id));
        session
            .walkthroughs
            .retain(|item| !tombstones.walkthroughs.contains(&item.id));
        for walkthrough in &mut session.walkthroughs {
            walkthrough
                .steps
                .retain(|step| !tombstones.walkthrough_steps.contains(&step.id));
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReviewStateTombstones {
    pub comments: BTreeSet<String>,
    pub sessions: BTreeSet<String>,
    pub action_items: BTreeSet<String>,
    pub walkthroughs: BTreeSet<String>,
    pub walkthrough_steps: BTreeSet<String>,
}

impl ReviewState {
    /// Merge a newer on-disk review state into this in-memory TUI state.
    ///
    /// The TUI remains authoritative for file view-state and untimestamped
    /// conflicts, while externally-added durable objects are adopted so a
    /// later save cannot clobber writes from CLI/MCP processes.
    #[cfg(test)]
    pub fn merge_external(&mut self, external: ReviewState, tombstones: &ReviewStateTombstones) {
        self.merge_external_files(external.files);
        merge_comments(&mut self.comments, external.comments, &tombstones.comments);
        self.merge_external_sessions(external.sessions, tombstones);
    }

    #[cfg(test)]
    fn merge_external_files(&mut self, external: BTreeMap<String, FileState>) {
        for (path, external_file) in external {
            self.files.entry(path).or_insert(external_file);
        }
    }

    #[cfg(test)]
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
                    .action_items
                    .retain(|item| !tombstones.action_items.contains(&item.id));
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
    for progress in &external.attention_progress {
        if !local.attention_progress.iter().any(|candidate| {
            candidate.kind == progress.kind
                && candidate.target == progress.target
                && candidate.fingerprint == progress.fingerprint
        }) {
            local.attention_progress.push(progress.clone());
        }
    }
    merge_vec_by_id(
        &mut local.action_items,
        external.action_items.clone(),
        &tombstones.action_items,
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
    external.action_items = local.action_items.clone();
    external.walkthroughs = local.walkthroughs.clone();
    external.attention_progress = local.attention_progress.clone();
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

#[cfg(test)]
fn merge_comments(local: &mut Vec<Comment>, external: Vec<Comment>, tombstones: &BTreeSet<String>) {
    for mut external_comment in external {
        if tombstones.contains(&external_comment.id) {
            continue;
        }
        if let Some(local_comment) = local
            .iter_mut()
            .find(|comment| comment.id == external_comment.id)
        {
            merge_optional_evidence(
                &mut local_comment.observation,
                &external_comment.observation,
            );
            external_comment.observation = local_comment.observation.clone();
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

pub(crate) fn merge_comment_replies(local: &mut Comment, external: &Comment) {
    for external_reply in &external.replies {
        if let Some(local_reply) = local
            .replies
            .iter_mut()
            .find(|reply| reply.id == external_reply.id)
        {
            let mut result = local_reply.result.clone();
            merge_optional_evidence(&mut result, &external_reply.result);
            let replace = external_reply.created_at > local_reply.created_at
                || (external_reply.created_at == local_reply.created_at
                    && canonical_reply_metadata(external_reply)
                        < canonical_reply_metadata(local_reply));
            if replace {
                *local_reply = external_reply.clone();
            }
            local_reply.result = result;
        } else {
            local.replies.push(external_reply.clone());
        }
    }
    local
        .replies
        .sort_by_key(|reply| (reply.created_at, reply.id.clone()));
}

fn canonical_reply_metadata(reply: &CommentReply) -> String {
    let mut metadata = reply.clone();
    metadata.result = None;
    serde_json::to_string(&metadata).expect("reply metadata must serialize")
}

pub(crate) fn merge_comment_observation(local: &mut Comment, external: &Comment) {
    merge_optional_evidence(&mut local.observation, &external.observation);
}

/// Fill missing immutable evidence. If independently written values conflict,
/// choose the lexicographically smaller canonical JSON value so merge/import
/// order cannot change the winner.
fn merge_optional_evidence<T>(local: &mut Option<T>, external: &Option<T>)
where
    T: Clone + Serialize + PartialEq,
{
    match (&*local, external) {
        (None, Some(value)) => *local = Some(value.clone()),
        (Some(left), Some(right)) if left != right => {
            let left_json = serde_json::to_string(left).expect("evidence must serialize");
            let right_json = serde_json::to_string(right).expect("evidence must serialize");
            if right_json < left_json {
                *local = Some(right.clone());
            }
        }
        _ => {}
    }
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

impl Identified for ActionItem {
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

    pub fn normalize_action_items(&mut self) {
        for session in &mut self.sessions {
            let mut seen = BTreeSet::new();
            session
                .action_items
                .retain(|item| seen.insert(item.id.clone()));
            for item in &mut session.action_items {
                dedupe_strings(&mut item.comment_ids);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::DiffSet;

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
        assert_eq!(loaded.meta.version, REVIEW_STATE_SCHEMA_VERSION);
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

    #[test]
    fn pre_attention_session_migrates_with_empty_regions_and_anchorless_targets() {
        let state: ReviewState = serde_json::from_str(
            r#"{
  "meta": { "version": 5 },
  "sessions": [{
    "id": "legacy",
    "target": { "file": "src/lib.rs", "line": 7 },
    "status": "open",
    "walkthroughs": [],
    "action_items": []
  }]
}"#,
        )
        .unwrap();

        assert!(state.sessions[0].attention_regions.is_empty());
        assert!(state.sessions[0].attention_progress.is_empty());
        assert!(state.sessions[0].target.anchor.is_none());
    }

    #[test]
    fn attention_progress_round_trips_with_stable_target_and_fingerprint_history() {
        let progress = AttentionProgress {
            target: AttentionProgressTarget {
                members: vec![AttentionProgressMember {
                    file: "src/lib.rs".into(),
                    line: Some(7),
                    end_line: Some(9),
                }],
            },
            kind: AttentionProgressKind::SkimAcknowledged,
            fingerprint: "current-diff".into(),
            recorded_at: chrono::Utc::now(),
        };
        let state = ReviewState {
            sessions: vec![ReviewSession {
                id: "progress".into(),
                attention_progress: vec![progress.clone()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let loaded: ReviewState =
            serde_json::from_value(serde_json::to_value(&state).unwrap()).unwrap();
        assert_eq!(loaded.sessions[0].attention_progress, [progress]);
    }

    #[test]
    fn external_merge_unions_current_and_stale_attention_progress_history() {
        let progress = |fingerprint: &str, kind| AttentionProgress {
            target: AttentionProgressTarget {
                members: vec![AttentionProgressMember {
                    file: "src/lib.rs".into(),
                    line: Some(7),
                    end_line: Some(7),
                }],
            },
            kind,
            fingerprint: fingerprint.into(),
            recorded_at: chrono::Utc::now(),
        };
        let mut local = ReviewState {
            sessions: vec![ReviewSession {
                id: "review".into(),
                attention_progress: vec![progress(
                    "old-fingerprint",
                    AttentionProgressKind::SpotlightVisited,
                )],
                ..Default::default()
            }],
            ..Default::default()
        };
        let external = ReviewState {
            sessions: vec![ReviewSession {
                id: "review".into(),
                attention_progress: vec![
                    progress("old-fingerprint", AttentionProgressKind::SpotlightVisited),
                    progress(
                        "current-fingerprint",
                        AttentionProgressKind::SkimAcknowledged,
                    ),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        local.merge_external(external, &ReviewStateTombstones::default());
        assert_eq!(local.sessions[0].attention_progress.len(), 2);
        assert!(
            local.sessions[0]
                .attention_progress
                .iter()
                .any(|entry| entry.fingerprint == "old-fingerprint")
        );
        assert!(
            local.sessions[0]
                .attention_progress
                .iter()
                .any(|entry| entry.fingerprint == "current-fingerprint")
        );
    }

    #[test]
    fn legacy_walkthrough_step_author_remains_neutral_and_new_author_round_trips() {
        let legacy: WalkthroughStep = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "title": "Old narration"
        }))
        .unwrap();
        assert_eq!(legacy.author, None);
        assert!(
            serde_json::to_value(&legacy)
                .unwrap()
                .get("author")
                .is_none()
        );

        let attributed = WalkthroughStep {
            author: Some(Identity {
                kind: AuthorKind::Agent,
                name: "review-agent".into(),
            }),
            ..legacy
        };
        let value = serde_json::to_value(&attributed).unwrap();
        assert_eq!(value["author"]["kind"], "agent");
        assert_eq!(value["author"]["name"], "review-agent");
        assert_eq!(
            serde_json::from_value::<WalkthroughStep>(value)
                .unwrap()
                .author,
            attributed.author
        );
    }

    #[test]
    fn attention_region_anchor_round_trips_and_invalid_loaded_state_can_be_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let anchor = CommentAnchor::File {
            path: "src/lib.rs".into(),
            old_path: None,
            diff_fingerprint: "fingerprint".into(),
        };
        let valid = AttentionRegion {
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                anchor: Some(anchor.clone()),
                ..Default::default()
            },
            salience: Salience::Spotlight,
            rationale: Some("important".into()),
            source: SalienceSource::Human,
        };
        let invalid = AttentionRegion {
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                line: Some(2),
                anchor: Some(anchor),
                ..Default::default()
            },
            salience: Salience::Skim,
            rationale: None,
            source: SalienceSource::Agent,
        };
        let files = DiffSet::parse(
            "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n one\n-old\n+new\n three\n",
        )
        .unwrap()
        .files;
        let line = AttentionRegion {
            target: crate::attention::target_for_diff(&files, "src/lib.rs", Some(2), None).unwrap(),
            salience: Salience::Spotlight,
            rationale: Some("line".into()),
            source: SalienceSource::Human,
        };
        let range = AttentionRegion {
            target: crate::attention::target_for_diff(&files, "src/lib.rs", Some(1), Some(3))
                .unwrap(),
            salience: Salience::Skim,
            rationale: Some("range".into()),
            source: SalienceSource::Agent,
        };
        let expected = vec![valid, line, range, invalid];
        ReviewState {
            sessions: vec![ReviewSession {
                id: "session".into(),
                attention_regions: expected.clone(),
                ..Default::default()
            }],
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let loaded = ReviewState::load_or_default(&path).unwrap();
        assert_eq!(loaded.sessions[0].attention_regions, expected);
        assert!(matches!(
            loaded.sessions[0].attention_regions[1].target.anchor,
            Some(CommentAnchor::Line { .. })
        ));
        assert!(matches!(
            loaded.sessions[0].attention_regions[2].target.anchor,
            Some(CommentAnchor::Range { .. })
        ));
        assert_eq!(loaded.meta.version, REVIEW_STATE_SCHEMA_VERSION);
    }

    fn comment(id: &str, body: &str) -> Comment {
        Comment {
            id: id.to_owned(),
            path: Some("src/lib.rs".to_owned()),
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

    fn action_item(
        id: &str,
        title: &str,
        updated_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> ActionItem {
        ActionItem {
            id: id.to_owned(),
            title: title.to_owned(),
            updated_at,
            ..ActionItem::default()
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
            action_items: vec![action_item("item", "external item", None)],
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
        assert_eq!(local.sessions[0].action_items[0].id, "item");
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
            action_items: vec![action_item("deleted-item", "external item", None)],
            ..ReviewSession::default()
        });
        let tombstones = ReviewStateTombstones {
            comments: BTreeSet::from(["deleted-comment".to_owned()]),
            action_items: BTreeSet::from(["deleted-item".to_owned()]),
            ..ReviewStateTombstones::default()
        };

        local.merge_external(external, &tombstones);

        assert!(local.comments.is_empty());
        assert!(local.sessions[0].action_items.is_empty());
    }

    #[test]
    fn merge_external_uses_newer_updated_at_for_conflicts() {
        let older = chrono::Utc::now();
        let newer = older + chrono::TimeDelta::seconds(5);
        let mut local = ReviewState {
            sessions: vec![ReviewSession {
                id: "session".to_owned(),
                action_items: vec![action_item("item", "old", Some(older))],
                updated_at: Some(older),
                ..ReviewSession::default()
            }],
            ..ReviewState::default()
        };
        let external = ReviewState {
            sessions: vec![ReviewSession {
                id: "session".to_owned(),
                action_items: vec![action_item("item", "new", Some(newer))],
                updated_at: Some(newer),
                ..ReviewSession::default()
            }],
            ..ReviewState::default()
        };

        local.merge_external(external, &ReviewStateTombstones::default());

        assert_eq!(local.sessions[0].action_items[0].title, "new");
    }

    #[test]
    fn merge_external_uses_newer_session_attention_map() {
        let older = chrono::Utc::now();
        let newer = older + chrono::TimeDelta::seconds(5);
        let region = |salience| AttentionRegion {
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                anchor: Some(CommentAnchor::File {
                    path: "src/lib.rs".into(),
                    old_path: None,
                    diff_fingerprint: "fingerprint".into(),
                }),
                ..Default::default()
            },
            salience,
            rationale: None,
            source: SalienceSource::Human,
        };
        let mut local = ReviewState {
            sessions: vec![ReviewSession {
                id: "session".into(),
                attention_regions: vec![region(Salience::Spotlight)],
                updated_at: Some(older),
                ..Default::default()
            }],
            ..Default::default()
        };
        let external = ReviewState {
            sessions: vec![ReviewSession {
                id: "session".into(),
                attention_regions: vec![region(Salience::Skim)],
                updated_at: Some(newer),
                ..Default::default()
            }],
            ..Default::default()
        };

        local.merge_external(external, &ReviewStateTombstones::default());

        assert_eq!(
            local.sessions[0].attention_regions[0].salience,
            Salience::Skim
        );
    }

    #[test]
    fn merge_external_keeps_local_attention_when_local_is_newer_or_timestamps_equal() {
        let base = chrono::Utc::now();
        let region = |salience| AttentionRegion {
            target: ReviewTarget {
                file: Some("src/lib.rs".into()),
                anchor: Some(CommentAnchor::File {
                    path: "src/lib.rs".into(),
                    old_path: None,
                    diff_fingerprint: "fingerprint".into(),
                }),
                ..Default::default()
            },
            salience,
            rationale: None,
            source: SalienceSource::Human,
        };
        for (local_at, external_at) in [(base + chrono::TimeDelta::seconds(1), base), (base, base)]
        {
            let mut local = ReviewState {
                sessions: vec![ReviewSession {
                    id: "session".into(),
                    attention_regions: vec![region(Salience::Spotlight)],
                    updated_at: Some(local_at),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let external = ReviewState {
                sessions: vec![ReviewSession {
                    id: "session".into(),
                    attention_regions: vec![region(Salience::Skim)],
                    updated_at: Some(external_at),
                    ..Default::default()
                }],
                ..Default::default()
            };
            local.merge_external(external, &ReviewStateTombstones::default());
            assert_eq!(
                local.sessions[0].attention_regions[0].salience,
                Salience::Spotlight
            );
        }
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
            author: Identity::local_human(),
            created_at: chrono::Utc::now(),
            result: None,
        });
        let mut external = ReviewState {
            comments: vec![comment("comment", "external")],
            ..Default::default()
        };
        external.comments[0].updated_at = Some(chrono::Utc::now() + chrono::TimeDelta::seconds(1));
        external.comments[0].replies.push(CommentReply {
            id: "external-reply".into(),
            body: "external".into(),
            author: Identity::agent(),
            created_at: chrono::Utc::now(),
            result: None,
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
        assert_eq!(
            local.comments[0]
                .replies
                .iter()
                .find(|reply| reply.id == "external-reply")
                .unwrap()
                .author,
            Identity::agent()
        );
    }

    fn reply_result(session_id: &str) -> crate::provenance::CommentReplyResult {
        let snapshot = crate::provenance::SnapshotEvidence::capture(
            chrono::DateTime::UNIX_EPOCH,
            session_id,
            ReviewTarget::default(),
            std::iter::empty(),
        );
        crate::provenance::CommentReplyResult::compare("comment", None, None, snapshot)
    }

    #[test]
    fn same_id_reply_merge_enriches_result_and_resolves_conflicts_deterministically() {
        let reply = CommentReply {
            id: "reply".into(),
            body: "done".into(),
            author: Identity::local_human(),
            created_at: chrono::DateTime::UNIX_EPOCH,
            result: None,
        };
        let mut local = ReviewState {
            comments: vec![Comment {
                id: "comment".into(),
                replies: vec![reply.clone()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut external = local.clone();
        external.comments[0].replies[0].result = Some(reply_result("enriched"));
        external.comments[0].observation = Some(crate::provenance::CommentObservation::new(
            external.comments[0].replies[0]
                .result
                .as_ref()
                .unwrap()
                .snapshot
                .clone(),
            None,
        ));
        local.merge_external(external, &ReviewStateTombstones::default());
        assert!(local.comments[0].replies[0].result.is_some());
        assert!(local.comments[0].observation.is_some());

        let mut left = local.clone();
        left.comments[0].replies[0].body = "z metadata".into();
        left.comments[0].replies[0].result = Some(reply_result("left"));
        let mut right = local;
        right.comments[0].replies[0].body = "a metadata".into();
        right.comments[0].replies[0].result = Some(reply_result("right"));
        let mut left_first = left.clone();
        left_first.merge_external(right.clone(), &ReviewStateTombstones::default());
        right.merge_external(left, &ReviewStateTombstones::default());
        assert_eq!(
            left_first.comments[0].replies[0],
            right.comments[0].replies[0]
        );
        assert_eq!(left_first.comments[0].replies[0].body, "a metadata");
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

        assert_eq!(state.comments[0].path.as_deref(), Some("src/main.rs"));
        assert!(state.comments[0].session_id.is_none());
        assert!(state.comments[0].anchor.is_none());
        assert!(state.comments[0].kind.is_none());
        assert!(state.comments[0].action.is_none());
        assert!(state.sessions.is_empty());
        assert_eq!(state.meta.version, 0);
    }

    #[test]
    fn state_two_reply_without_provenance_loads_unchanged() {
        let state: ReviewState = serde_json::from_str(
            r#"{
  "meta": { "version": 2 },
  "comments": [{
    "id": "comment",
    "path": "src/lib.rs",
    "body": "legacy comment",
    "created_at": "2026-06-30T00:00:00Z",
    "replies": [{
      "id": "reply",
      "body": "legacy reply",
      "created_at": "2026-06-30T00:01:00Z"
    }]
  }]
}"#,
        )
        .unwrap();

        assert_eq!(state.meta.version, 2);
        assert!(state.comments[0].observation.is_none());
        assert!(state.comments[0].replies[0].result.is_none());
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
        assert_eq!(session.action_items[0].action, None);
        assert_eq!(session.action_items[0].status, ActionItemStatus::Open);
        assert_eq!(
            session.walkthroughs[0].steps[0].target.file.as_deref(),
            Some("src/lib.rs")
        );
    }

    #[test]
    fn legacy_tasks_normalize_to_canonical_action_items() {
        let state: ReviewState = serde_json::from_str(
            r#"{
  "sessions": [{
    "id": "review-1",
    "tasks": [
      {
        "id": "done",
        "title": "Ship fix",
        "action": "fix",
        "status": "done",
        "source_comment_id": "comment-a",
        "resolution": "landed"
      },
      {
        "id": "dismissed",
        "title": "Not needed",
        "status": "dismissed",
        "source_comment_id": "comment-b"
      }
    ]
  }]
}"#,
        )
        .unwrap();

        let done = &state.sessions[0].action_items[0];
        assert_eq!(done.status, ActionItemStatus::Closed);
        assert_eq!(done.disposition, Some(ClosedDisposition::Completed));
        assert_eq!(done.outcome.as_deref(), Some("landed"));
        assert_eq!(done.comment_ids, ["comment-a"]);
        let dismissed = &state.sessions[0].action_items[1];
        assert_eq!(dismissed.disposition, Some(ClosedDisposition::Dismissed));

        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains("\"action_items\""));
        assert!(!json.contains("\"tasks\""));
        assert!(!json.contains("source_comment_id"));
        assert!(!json.contains("resolution"));
    }

    #[test]
    fn mixed_action_items_prefer_canonical_and_dedupe_ids_and_comment_ids() {
        let state: ReviewState = serde_json::from_str(
            r#"{
  "sessions": [{
    "id": "review-1",
    "action_items": [
      {
        "id": "same",
        "title": "canonical",
        "comment_ids": ["one", "one", "two"],
        "source_comment_id": "two"
      },
      { "id": "same", "title": "canonical duplicate" }
    ],
    "tasks": [
      { "id": "same", "title": "legacy loses" },
      { "id": "legacy-only", "title": "legacy retained" },
      { "id": "legacy-only", "title": "legacy duplicate" }
    ]
  }]
}"#,
        )
        .unwrap();

        let items = &state.sessions[0].action_items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "canonical");
        assert_eq!(items[0].comment_ids, ["one", "two"]);
        assert_eq!(items[1].title, "legacy retained");
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
        assert_eq!(
            state.sessions[0].action_items[0].action,
            Some(ActionIntent::Fix)
        );

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
            loaded.sessions[0].action_items[0].comment_ids,
            ["comment-1"]
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

    #[test]
    fn legacy_anchored_comment_loads_without_session_scope() {
        let comment: Comment = serde_json::from_str(
            r#"{
  "id": "legacy",
  "path": "src/lib.rs",
  "line": 4,
  "anchor": {
    "type": "line",
    "path": "src/lib.rs",
    "old_path": null,
    "side": "new",
    "line": 4,
    "old_line": 4,
    "new_line": 4,
    "hunk_header": "@@ -1 +1 @@",
    "hunk_old_start": 1,
    "hunk_old_len": 1,
    "hunk_new_start": 1,
    "hunk_new_len": 1,
    "hunk_index": 0,
    "line_index": 0,
    "line_kind": "context",
    "line_text": "line",
    "line_fingerprint": "line-fingerprint",
    "diff_fingerprint": "diff-fingerprint"
  },
  "body": "legacy anchored comment",
  "created_at": "2026-06-30T00:00:00Z"
}"#,
        )
        .unwrap();

        assert_eq!(comment.path.as_deref(), Some("src/lib.rs"));
        assert!(comment.session_id.is_none());
        assert_eq!(comment.state, CommentState::Draft);
        comment.validate().unwrap();
    }

    #[test]
    fn general_comment_round_trips_and_reports_membership() {
        let comment = Comment {
            id: "general".into(),
            session_id: Some("session-a".into()),
            path: None,
            body: "Overall review note".into(),
            ..Comment::default()
        };

        comment.validate().unwrap();
        assert!(comment.is_general());
        assert!(!comment.has_location());
        assert!(comment.belongs_to_session("session-a"));
        assert!(!comment.belongs_to_session("session-b"));

        let loaded: Comment =
            serde_json::from_str(&serde_json::to_string(&comment).unwrap()).unwrap();
        assert_eq!(loaded, comment);
        assert!(
            !serde_json::to_string(&comment)
                .unwrap()
                .contains("\"path\"")
        );

        let legacy = Comment {
            session_id: None,
            ..comment
        };
        assert!(legacy.belongs_to_session("session-a"));
        assert!(legacy.belongs_to_session("session-b"));
    }

    #[test]
    fn comment_validation_rejects_impossible_locations() {
        let general_with_line = Comment {
            line: Some(1),
            ..Comment::default()
        };
        assert_eq!(
            general_with_line.validate().unwrap_err().to_string(),
            "general comment cannot have a line, end line, or anchor"
        );

        let end_without_start = Comment {
            path: Some("src/lib.rs".into()),
            end_line: Some(2),
            ..Comment::default()
        };
        assert_eq!(
            end_without_start.validate().unwrap_err().to_string(),
            "comment end line requires a start line"
        );

        let path_mismatch = Comment {
            path: Some("src/lib.rs".into()),
            anchor: Some(CommentAnchor::File {
                path: "src/main.rs".into(),
                old_path: None,
                diff_fingerprint: "diff".into(),
            }),
            ..Comment::default()
        };
        assert!(
            path_mismatch
                .validate()
                .unwrap_err()
                .to_string()
                .contains("does not match anchor path")
        );

        let anchored_line_mismatch = Comment {
            path: Some("src/lib.rs".into()),
            line: Some(2),
            anchor: Some(CommentAnchor::Line {
                path: "src/lib.rs".into(),
                old_path: None,
                side: crate::anchor::DiffSide::New,
                line: 3,
                old_line: Some(3),
                new_line: Some(3),
                hunk_header: "@@ -1 +1 @@".into(),
                hunk_old_start: 1,
                hunk_old_len: 1,
                hunk_new_start: 1,
                hunk_new_len: 1,
                hunk_index: 0,
                line_index: 0,
                line_kind: "context".into(),
                line_text: "line".into(),
                line_fingerprint: "line".into(),
                diff_fingerprint: "diff".into(),
            }),
            ..Comment::default()
        };
        assert_eq!(
            anchored_line_mismatch.validate().unwrap_err().to_string(),
            "comment line coordinates do not match line anchor"
        );
    }

    #[test]
    fn legacy_comment_channels_derive_from_state_and_authors_default_locally() {
        let comments: Vec<Comment> = serde_json::from_str(
            r#"[
              {"id":"todo","body":"fix","state":"todo","created_at":"2026-01-01T00:00:00Z"},
              {"id":"draft","body":"note","state":"draft","created_at":"2026-01-01T00:00:00Z"},
              {"id":"resolved","body":"done","state":"resolved","created_at":"2026-01-01T00:00:00Z","replies":[{"id":"reply","body":"ok","created_at":"2026-01-01T00:01:00Z"}]}
            ]"#,
        )
        .unwrap();

        assert_eq!(comments[0].channel, Channel::Delegation);
        assert_eq!(comments[1].channel, Channel::Note);
        assert_eq!(comments[2].channel, Channel::Note);
        assert_eq!(comments[0].author, Identity::local_human());
        assert_eq!(comments[2].replies[0].author, Identity::local_human());
    }

    #[test]
    fn annotation_identity_channel_and_reply_author_round_trip_with_stable_names() {
        let comment = Comment {
            id: "annotation".into(),
            body: "look here".into(),
            author: Identity::agent(),
            channel: Channel::Onboarding,
            replies: vec![CommentReply {
                id: "reply".into(),
                body: "thanks".into(),
                author: Identity {
                    kind: AuthorKind::Human,
                    name: "Reviewer".into(),
                },
                created_at: chrono::DateTime::UNIX_EPOCH,
                result: None,
            }],
            created_at: chrono::DateTime::UNIX_EPOCH,
            ..Default::default()
        };

        let json = serde_json::to_string(&comment).unwrap();
        assert!(json.contains(r#""kind":"agent""#));
        assert!(json.contains(r#""channel":"onboarding""#));
        let loaded: Comment = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, comment);
    }

    #[test]
    fn annotation_validation_rejects_empty_names_and_non_delegation_todos() {
        let empty_name = Comment {
            author: Identity {
                kind: AuthorKind::Human,
                name: "  ".into(),
            },
            ..Default::default()
        };
        assert_eq!(
            empty_name.validate().unwrap_err().to_string(),
            "identity name must not be empty"
        );

        let invalid_todo = Comment {
            state: CommentState::Todo,
            channel: Channel::Note,
            ..Default::default()
        };
        assert_eq!(
            invalid_todo.validate().unwrap_err().to_string(),
            "todo comment channel must be delegation or collaboration"
        );
        let json = serde_json::to_string(&invalid_todo).unwrap();
        assert!(serde_json::from_str::<Comment>(&json).is_err());

        let dir = tempfile::tempdir().unwrap();
        let state = ReviewState {
            comments: vec![invalid_todo],
            ..Default::default()
        };
        assert!(state.save(&dir.path().join("state.json")).is_err());
    }

    #[test]
    fn source_comment_link_is_additive_and_round_trips() {
        let linked = Comment {
            id: "delegation".into(),
            body: "please change this".into(),
            channel: Channel::Delegation,
            source_comment_id: Some("onboarding".into()),
            created_at: chrono::DateTime::UNIX_EPOCH,
            ..Default::default()
        };
        let json = serde_json::to_string(&linked).unwrap();
        assert!(json.contains(r#""source_comment_id":"onboarding""#));
        assert_eq!(serde_json::from_str::<Comment>(&json).unwrap(), linked);

        let mut legacy: serde_json::Value = serde_json::from_str(&json).unwrap();
        legacy.as_object_mut().unwrap().remove("source_comment_id");
        let loaded: Comment = serde_json::from_value(legacy).unwrap();
        assert_eq!(loaded.source_comment_id, None);
    }
}
